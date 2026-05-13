//! Orquestador del bot Rust. Espejo de `runner.py` (sin executor/cooldown
//! ya que no ejecutamos ordenes — DRY_RUN puro persistiendo opps).

use crate::bilateral::{BilateralArbStrategy, BilateralConfig};
use crate::config::Settings;
use crate::gamma::GammaClient;
use crate::poly_ws::{BookEvent, PolymarketMarketWS};
use crate::recorder::Recorder;
use crate::types::Market;
use anyhow::Result;
use rust_decimal_macros::dec;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};
use tokio::task::JoinHandle;
use tracing::{error, info, warn};

pub async fn run(settings: Settings) -> Result<()> {
    info!(
        min_edge = %settings.arb_min_edge_per_unit,
        max_size_usdc = %settings.arb_max_size_usdc,
        fee_bps = settings.arb_fee_rate_bps,
        slippage_bps = settings.arb_slippage_buffer_bps,
        "arb_starting"
    );

    // Max shares (cap aprox worst-case del Python: USDC / 0.5).
    let max_shares = settings.arb_max_size_usdc / dec!(0.5);
    let strategy = Arc::new(Mutex::new(BilateralArbStrategy::new(BilateralConfig {
        fee_rate_bps: settings.arb_fee_rate_bps,
        min_edge_per_unit: settings.arb_min_edge_per_unit,
        slippage_buffer_bps: settings.arb_slippage_buffer_bps,
        max_shares,
    })));

    let recorder = Arc::new(Recorder::open(&settings.db_path)?);
    let gamma = Arc::new(GammaClient::new(&settings.gamma_url)?);

    // Discovery inicial.
    let initial = gamma
        .discover_hourly_crypto(settings.arb_horizon_seconds, settings.arb_gamma_limit)
        .await?;
    if initial.is_empty() {
        warn!("arb_no_initial_markets");
        return Ok(());
    }
    let initial_assets = {
        let mut s = strategy.lock().await;
        for m in initial {
            s.register_market(m);
        }
        s.tracked_assets()
    };
    info!(
        assets = initial_assets.len(),
        "arb_initial_assets_registered"
    );

    // Canal para eventos del WS.
    let (event_tx, event_rx) = mpsc::channel::<BookEvent>(4096);

    let ws = Arc::new(PolymarketMarketWS::new(
        &settings.poly_ws_url,
        initial_assets,
        event_tx,
    ));
    let books = ws.books();

    // Subtasks: WS, refresh, stop watcher, dispatcher.
    let ws_run = {
        let ws = ws.clone();
        tokio::spawn(async move {
            if let Err(e) = ws.run().await {
                error!(error = %e, "ws task failed");
            }
        })
    };

    let refresh_task = spawn_refresh_loop(
        gamma.clone(),
        strategy.clone(),
        ws.clone(),
        settings.arb_markets_refresh_seconds,
        settings.arb_horizon_seconds,
        settings.arb_gamma_limit,
    );

    let stop_signal = Arc::new(tokio::sync::Notify::new());
    let stop_task = spawn_stop_watcher(settings.stop_file_path.clone(), stop_signal.clone());

    let dispatcher = spawn_dispatcher(
        event_rx,
        strategy.clone(),
        books.clone(),
        recorder.clone(),
    );

    // Esperar stop signal.
    stop_signal.notified().await;
    info!("arb_stop_requested");

    ws.stop();
    refresh_task.abort();
    dispatcher.abort();
    stop_task.abort();
    // Cap shutdown wait — si el WS no cierra en 5s, force abort.
    let _ = tokio::time::timeout(std::time::Duration::from_secs(5), ws_run).await;

    let stats = recorder.stats()?;
    info!(
        count = stats.count,
        total_pnl_usdc = stats.total_pnl,
        avg_edge = stats.avg_edge,
        max_edge = stats.max_edge,
        total_notional_usdc = stats.total_notional,
        "arb_stopped"
    );
    Ok(())
}

fn spawn_refresh_loop(
    gamma: Arc<GammaClient>,
    strategy: Arc<Mutex<BilateralArbStrategy>>,
    ws: Arc<PolymarketMarketWS>,
    refresh_seconds: u64,
    horizon_seconds: u64,
    limit: u32,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(refresh_seconds)).await;
            match gamma.discover_hourly_crypto(horizon_seconds, limit).await {
                Ok(markets) => {
                    update_strategy(&strategy, &ws, markets).await;
                }
                Err(e) => {
                    warn!(error = %e, "arb_refresh_failed");
                }
            }
        }
    })
}

async fn update_strategy(
    strategy: &Arc<Mutex<BilateralArbStrategy>>,
    ws: &Arc<PolymarketMarketWS>,
    markets: Vec<Market>,
) {
    let mut additions: HashMap<String, String> = HashMap::new();
    {
        let mut s = strategy.lock().await;
        let existing = s.tracked_assets();
        let new_ids: std::collections::HashSet<String> =
            markets.iter().map(|m| m.condition_id.clone()).collect();

        // Add new
        for m in markets {
            // Check if asset_a y asset_b ya estan registrados.
            if !existing.contains_key(&m.yes_token_id) {
                additions.insert(m.yes_token_id.clone(), m.condition_id.clone());
                additions.insert(m.no_token_id.clone(), m.condition_id.clone());
                s.register_market(m);
            }
        }

        // Cleanup stale: encontrar markets que ya no aparecen.
        // (No removemos del WS para simplicidad; los assets stale dejan de
        // recibir updates pero no hacen daño — discovery se llama cada 120s.)
        let known_market_ids: std::collections::HashSet<String> =
            existing.values().cloned().collect();
        let stale: Vec<String> = known_market_ids
            .into_iter()
            .filter(|mid| !new_ids.contains(mid))
            .collect();
        for mid in &stale {
            s.unregister_market(mid);
        }
        if !stale.is_empty() {
            info!(removed = stale.len(), "arb_markets_stale_removed");
        }
    }

    if !additions.is_empty() {
        info!(new_assets = additions.len(), "arb_markets_subscribe");
        ws.subscribe(additions).await;
    }
}

fn spawn_stop_watcher(
    stop_path: PathBuf,
    stop_signal: Arc<tokio::sync::Notify>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            if stop_path.exists() {
                info!(path = %stop_path.display(), "stop_file_detected");
                stop_signal.notify_one();
                return;
            }
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
    })
}

fn spawn_dispatcher(
    mut event_rx: mpsc::Receiver<BookEvent>,
    strategy: Arc<Mutex<BilateralArbStrategy>>,
    books: Arc<Mutex<HashMap<String, crate::book_state::BookState>>>,
    recorder: Arc<Recorder>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        while let Some(ev) = event_rx.recv().await {
            let asset_id = match ev {
                BookEvent::Updated(a) => a,
            };
            // Snapshot del strategy + books bajo locks separados.
            let market_id = {
                let s = strategy.lock().await;
                s.market_id_for_asset(&asset_id).map(String::from)
            };
            let Some(market_id) = market_id else { continue };

            let books_snapshot = {
                let b = books.lock().await;
                b.clone()
            };
            let opp = {
                let s = strategy.lock().await;
                s.evaluate_market(&market_id, &books_snapshot)
            };

            if let Some(opp) = opp {
                match recorder.insert(&opp) {
                    Ok(id) => {
                        info!(
                            id,
                            strategy = %opp.strategy,
                            market = &opp.market_id[..opp.market_id.len().min(14)],
                            edge = %opp.edge_per_unit,
                            notional = %opp.notional_usdc,
                            pnl_teorico = %opp.expected_pnl_usdc,
                            "arb_opportunity_detected"
                        );
                    }
                    Err(e) => {
                        error!(error = %e, "arb_recorder_insert_failed");
                    }
                }
            }
        }
    })
}
