//! Live runner: strategy A (lag_arb / momentum CEX direccional) + LiveExecutor.
//!
//! Cambio importante (2026-05-12): el modo live anterior usaba
//! `BilateralPureStrategy` (strategy B) que matemáticamente nunca dispara en
//! mercados eficientes (sum_ask siempre >= 1). Evidencia: trinity 10min DRY-RUN
//! reportó B=0 entries, A=27198 entries. Ahora live usa strategy A.
//!
//! Strategy A es DIRECCIONAL (no arbitraje matemático): compra YES o NO basado
//! en momentum del CEX (Coinbase). PnL depende de que el momentum prediga la
//! resolución, no es garantizado.
//!
//! Por default DRY-RUN: detecta opps y las logguea, NO ejecuta.
//! Con `live` modo ejecuta trades reales (con safety limits).

use crate::book_state::BookState;
use crate::cex_feed::{CexFeed, CexState};
use crate::config::Settings;
use crate::gamma::GammaClient;
use crate::live::{ArbLegOrder, ArbSide, ArbStatus, LiveExecutor, SafetyLimits};
use crate::poly_ws::{BookEvent, PolymarketMarketWS};
use crate::strategies::lag_arb::{LagArbConfig, LagArbStrategy};
use crate::strategies::DecisionKind;
use crate::trinity_runner::TrinityRecorder;
use anyhow::Result;
use rust_decimal_macros::dec;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};
use tracing::{error, info, warn};

/// Modo de ejecución.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiveMode {
    /// Solo detección + logueo. NO envía trades.
    DryRun,
    /// Envía trades reales al CLOB con la wallet.
    Live,
}

pub async fn run(
    settings: Settings,
    duration_seconds: u64,
    mode: LiveMode,
    safety: SafetyLimits,
) -> Result<()> {
    info!(
        mode = ?mode,
        duration_s = duration_seconds,
        max_total = %safety.max_total_capital_usdc,
        max_per_trade = %safety.max_per_trade_usdc,
        max_per_h = safety.max_trades_per_hour,
        "live_runner_starting_strategy_A"
    );

    let out_dir = Path::new("analysis_output");
    std::fs::create_dir_all(out_dir).ok();
    let recorder = Arc::new(TrinityRecorder::open(&out_dir.join("live_decisions.jsonl"))?);
    let trade_recorder = Arc::new(TrinityRecorder::open(&out_dir.join("live_trades.jsonl"))?);

    // Executor solo si Live mode. TP-on-place: 15% por default, configurable
    // via env LAGARB_TP_PCT (en porcentaje, ej "15" para +15%). Si LAGARB_TP_PCT=0
    // o "off", desactiva el TP automatico.
    let tp_pct: Option<rust_decimal::Decimal> = match std::env::var("LAGARB_TP_PCT") {
        Ok(s) if s.trim().eq_ignore_ascii_case("off") || s.trim() == "0" => None,
        Ok(s) => Some(
            rust_decimal::Decimal::from_str_exact(s.trim())
                .map_err(|e| anyhow::anyhow!("LAGARB_TP_PCT invalido '{s}': {e}"))?,
        ),
        Err(_) => Some(dec!(15)),
    };
    let executor: Option<Arc<LiveExecutor>> = match mode {
        LiveMode::Live => {
            let pk = std::env::var("PRIVATE_KEY")
                .or_else(|_| std::env::var("POLYMARKET_PRIVATE_KEY"))
                .map_err(|_| {
                    anyhow::anyhow!("Mode=Live requiere PRIVATE_KEY en env")
                })?;
            info!(tp_pct = ?tp_pct, "live_runner_initializing_executor");
            let ex = LiveExecutor::init(pk, safety.clone(), tp_pct).await?;
            Some(Arc::new(ex))
        }
        LiveMode::DryRun => None,
    };

    // Strategy A: momentum CEX direccional. Threshold 3bps en 30s window —
    // punto medio entre 1bps (ruido) y 5bps (muy selectivo). Captura
    // movimientos algo mas que el bid-ask spread sin esperar eventos grandes.
    let lagarb_cfg = LagArbConfig {
        momentum_threshold: 0.0003,
        base_size_usdc: safety.max_per_trade_usdc, // $3 default
        max_size_usdc: safety.max_per_trade_usdc,
        min_ttr_seconds: 10,
        max_ttr_seconds: 900,
        min_price_yes: dec!(0.03),
        max_price_yes: dec!(0.97),
        momentum_window_s: 30,
        dedup_window_s: 60,
    };
    let strat = Arc::new(Mutex::new(LagArbStrategy::new(lagarb_cfg)));

    // CEX feed (Coinbase). Strategy A requiere precios CEX para calcular
    // momentum. Productos soportados deben coincidir con product_id_from_slug.
    let cex = Arc::new(CexFeed::new(vec![
        "BTC-USD".to_string(),
        "ETH-USD".to_string(),
        "SOL-USD".to_string(),
        "XRP-USD".to_string(),
    ]));
    let cex_state = cex.snapshot_arc();
    let cex_clone = cex.clone();
    let cex_task = tokio::spawn(async move {
        if let Err(e) = cex_clone.run().await {
            error!(error = %e, "cex_feed task failed");
        }
    });

    let gamma = Arc::new(GammaClient::new(&settings.gamma_url)?);
    let markets = gamma
        .discover_hourly_crypto(settings.arb_horizon_seconds, settings.arb_gamma_limit)
        .await?;
    if markets.is_empty() {
        warn!("live_runner_no_markets");
        cex.stop();
        return Ok(());
    }
    let mut assets = HashMap::new();
    {
        let mut s = strat.lock().await;
        for m in markets {
            s.register_market(m.clone());
            assets.insert(m.yes_token_id.clone(), m.condition_id.clone());
            assets.insert(m.no_token_id.clone(), m.condition_id.clone());
        }
    }
    info!(mkts = assets.len() / 2, "live_runner_markets_registered");

    let (event_tx, event_rx) = mpsc::channel::<BookEvent>(8192);
    let poly_ws = Arc::new(PolymarketMarketWS::new(
        &settings.poly_ws_url,
        assets,
        event_tx,
    ));
    let books = poly_ws.books();
    let poly_ws_clone = poly_ws.clone();
    let poly_task = tokio::spawn(async move {
        if let Err(e) = poly_ws_clone.run().await {
            error!(error = %e, "poly_ws task failed");
        }
    });

    let dispatcher = spawn_live_dispatcher(
        event_rx,
        strat.clone(),
        books,
        cex_state,
        recorder.clone(),
        trade_recorder.clone(),
        executor.clone(),
        mode,
    );

    // Esperar duración O state/STOP file. Lo primero que ocurra.
    let stop_path = settings.stop_file_path.clone();
    let timer = tokio::time::sleep(std::time::Duration::from_secs(duration_seconds));
    tokio::pin!(timer);
    loop {
        tokio::select! {
            _ = &mut timer => {
                info!("live_runner_timer_elapsed");
                break;
            }
            _ = tokio::time::sleep(std::time::Duration::from_secs(2)) => {
                if stop_path.exists() {
                    info!(path = %stop_path.display(), "live_runner_stop_file_detected");
                    break;
                }
            }
        }
    }
    poly_ws.stop();
    cex.stop();
    dispatcher.abort();
    let _ = tokio::time::timeout(std::time::Duration::from_secs(5), poly_task).await;
    let _ = tokio::time::timeout(std::time::Duration::from_secs(5), cex_task).await;
    info!("live_runner_stopped");
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn spawn_live_dispatcher(
    mut event_rx: mpsc::Receiver<BookEvent>,
    strat: Arc<Mutex<LagArbStrategy>>,
    books: Arc<Mutex<HashMap<String, BookState>>>,
    cex_state: Arc<Mutex<HashMap<String, CexState>>>,
    decisions_recorder: Arc<TrinityRecorder>,
    trades_recorder: Arc<TrinityRecorder>,
    executor: Option<Arc<LiveExecutor>>,
    mode: LiveMode,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        while let Some(BookEvent::Updated(asset_id)) = event_rx.recv().await {
            let market_id = {
                let s = strat.lock().await;
                s.market_id_for_asset(&asset_id).map(String::from)
            };
            let Some(market_id) = market_id else { continue };

            let books_snap = {
                let b = books.lock().await;
                b.clone()
            };
            let cex_snap = {
                let c = cex_state.lock().await;
                c.clone()
            };
            let dec_opt = {
                let mut s = strat.lock().await;
                s.evaluate_market(&market_id, &books_snap, &cex_snap)
            };
            let Some(decision) = dec_opt else { continue };
            if !matches!(decision.decision, DecisionKind::Enter) {
                continue;
            }

            // Loguear decision siempre que sea Enter
            if let Err(e) = decisions_recorder.record(&decision) {
                warn!(error = %e, "record decision failed");
            }
            info!(
                mkt = &decision.market_id[..decision.market_id.len().min(14)],
                dir = ?decision.direction,
                edge = %decision.edge_per_unit,
                size = %decision.size_usdc,
                p_yes = %decision.price_yes,
                p_no = %decision.price_no,
                mode = ?mode,
                "live_entry_detected"
            );

            if mode == LiveMode::DryRun {
                continue;
            }

            // LIVE MODE: ejecutar 1 leg direccional
            let Some(ex) = executor.as_ref() else { continue };

            let Some(direction) = decision.direction.as_deref() else { continue };
            let (token_id, leg_price) = match direction {
                "YES" => (decision.yes_token_id.clone(), decision.price_yes),
                "NO" => (decision.no_token_id.clone(), decision.price_no),
                _ => {
                    warn!(dir = %direction, "live_unknown_direction");
                    continue;
                }
            };
            if leg_price <= rust_decimal::Decimal::ZERO {
                warn!(
                    mkt = %market_id,
                    "live_skip_zero_price"
                );
                continue;
            }

            let leg = ArbLegOrder {
                token_id,
                side: ArbSide::Buy,
                size_usdc: decision.size_usdc,
                entry_price: leg_price,
            };

            let market_for_log = market_id.clone();
            match ex.execute_single_leg(market_id, leg).await {
                Ok(result) => {
                    info!(
                        mkt = &market_for_log[..market_for_log.len().min(14)],
                        dir = %direction,
                        status = ?result.status,
                        order_id = ?result.order_id_a,
                        tp_order_id = ?result.tp_order_id,
                        tp_price = ?result.tp_price,
                        size = %result.total_usdc,
                        "live_trade_result"
                    );
                    if let Some(e) = &result.error_a {
                        warn!(mkt = %market_for_log, err = %e, "live_order_error");
                    }
                    if let Some(e) = &result.tp_error {
                        warn!(mkt = %market_for_log, err = %e, "live_tp_error");
                    }
                    let trade_record = serde_json::json!({
                        "strategy": "A-LIVE",
                        "timestamp": result.timestamp.to_rfc3339(),
                        "market_id": market_for_log,
                        "direction": direction,
                        "status": format!("{:?}", result.status),
                        "order_id": result.order_id_a,
                        "error": result.error_a,
                        "size_usdc": result.total_usdc.to_string(),
                        "tp_order_id": result.tp_order_id,
                        "tp_price": result.tp_price.map(|p| p.to_string()),
                        "tp_error": result.tp_error,
                    });
                    let line = serde_json::to_string(&trade_record).unwrap_or_default();
                    let _ = std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open("analysis_output/live_trades.jsonl")
                        .and_then(|mut f| {
                            use std::io::Write;
                            writeln!(f, "{line}")
                        });
                    let _ = trades_recorder; // reservado uso futuro
                    if matches!(result.status, ArbStatus::BothFailed) {
                        warn!(
                            mkt = %market_for_log,
                            "live_trade_failed_will_retry_on_next_signal"
                        );
                    }
                }
                Err(e) => {
                    warn!(error = %e, mkt = %market_for_log, "live_trade_failed");
                }
            }
        }
    })
}
