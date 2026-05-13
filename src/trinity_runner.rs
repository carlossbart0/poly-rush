//! Trinity runner: corre A + B + C en paralelo sobre el mismo book stream.
//!
//! Architecture:
//!   - 1 Polymarket WS
//!   - 1 Coinbase WS (CEX)
//!   - 1 Gamma discovery + refresh loop
//!   - 3 strategy states (A, B, C)
//!   - 3 recorders JSONL
//!   - 1 dispatcher que en cada book update evalua las 3 strategies y persiste.

use crate::book_state::BookState;
use crate::cex_feed::{CexFeed, CexState};
use crate::config::Settings;
use crate::gamma::GammaClient;
use crate::poly_ws::{BookEvent, PolymarketMarketWS};
use crate::strategies::bilateral_pure::{BilateralPureConfig, BilateralPureStrategy};
use crate::strategies::hybrid::{HybridConfig, HybridStrategy};
use crate::strategies::lag_arb::{LagArbConfig, LagArbStrategy};
use crate::strategies::{Decision, DecisionKind};
use anyhow::Result;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};
use tracing::{error, info, warn};

/// Recorder JSONL append-only para una strategy.
pub struct TrinityRecorder {
    writer: std::sync::Mutex<File>,
}

impl TrinityRecorder {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        Ok(Self {
            writer: std::sync::Mutex::new(file),
        })
    }

    pub fn record(&self, d: &Decision) -> Result<()> {
        let line = serde_json::to_string(d)?;
        let mut w = self.writer.lock().unwrap();
        writeln!(w, "{line}")?;
        w.flush()?;
        Ok(())
    }
}

pub struct TrinityRunner {
    settings: Settings,
}

impl TrinityRunner {
    pub fn new(settings: Settings) -> Self {
        Self { settings }
    }
}

pub async fn run(settings: Settings, duration_seconds: u64) -> Result<()> {
    info!(
        duration_s = duration_seconds,
        "trinity_starting"
    );

    let out_dir = Path::new("analysis_output");
    std::fs::create_dir_all(out_dir).ok();
    let rec_a = Arc::new(TrinityRecorder::open(&out_dir.join("trinity_A.jsonl"))?);
    let rec_b = Arc::new(TrinityRecorder::open(&out_dir.join("trinity_B.jsonl"))?);
    let rec_c = Arc::new(TrinityRecorder::open(&out_dir.join("trinity_C.jsonl"))?);

    // ===== STRATEGIES =====
    let max_shares = settings.arb_max_size_usdc / dec!(0.5);
    // DEBUG MODE: thresholds extremadamente permisivos para diagnostico.
    let bilateral_cfg = BilateralPureConfig {
        fee_rate_bps: settings.arb_fee_rate_bps,
        min_edge_per_unit: dec!(0.001),
        max_edge_per_unit: dec!(0.30),
        slippage_buffer_bps: settings.arb_slippage_buffer_bps,
        max_shares,
        min_ttr_seconds: 10,
        min_liquidity_usdc: dec!(1),
    };
    let bilateral_cfg_for_c = BilateralPureConfig {
        fee_rate_bps: settings.arb_fee_rate_bps,
        min_edge_per_unit: dec!(0.001),
        max_edge_per_unit: dec!(0.30),
        slippage_buffer_bps: settings.arb_slippage_buffer_bps,
        max_shares,
        min_ttr_seconds: 10,
        min_liquidity_usdc: dec!(1),
    };
    let strat_b = Arc::new(Mutex::new(BilateralPureStrategy::new(bilateral_cfg)));
    let strat_c = Arc::new(Mutex::new(HybridStrategy::new(HybridConfig {
        bilateral: bilateral_cfg_for_c,
        cex_tolerance: 0.05,
    })));
    let strat_a = Arc::new(Mutex::new(LagArbStrategy::new(LagArbConfig::default())));

    // ===== CEX FEED =====
    let cex = Arc::new(CexFeed::new(vec![
        "BTC-USD".to_string(),
        "ETH-USD".to_string(),
    ]));
    let cex_state = cex.snapshot_arc();
    let cex_clone = cex.clone();
    let cex_task = tokio::spawn(async move {
        if let Err(e) = cex_clone.run().await {
            error!(error = %e, "cex_feed task failed");
        }
    });

    // ===== GAMMA DISCOVERY =====
    let gamma = Arc::new(GammaClient::new(&settings.gamma_url)?);
    let markets = gamma
        .discover_hourly_crypto(settings.arb_horizon_seconds, settings.arb_gamma_limit)
        .await?;
    if markets.is_empty() {
        warn!("trinity_no_initial_markets");
        return Ok(());
    }
    let mut initial_assets: HashMap<String, String> = HashMap::new();
    {
        let mut a = strat_a.lock().await;
        let mut b = strat_b.lock().await;
        let mut c = strat_c.lock().await;
        for m in markets {
            a.register_market(m.clone());
            b.register_market(m.clone());
            c.register_market(m.clone());
            initial_assets.insert(m.yes_token_id.clone(), m.condition_id.clone());
            initial_assets.insert(m.no_token_id.clone(), m.condition_id.clone());
        }
        info!(
            mkts = initial_assets.len() / 2,
            assets = initial_assets.len(),
            "trinity_markets_registered"
        );
    }

    // ===== POLYMARKET WS =====
    let (event_tx, event_rx) = mpsc::channel::<BookEvent>(8192);
    let poly_ws = Arc::new(PolymarketMarketWS::new(
        &settings.poly_ws_url,
        initial_assets,
        event_tx,
    ));
    let books = poly_ws.books();
    let poly_ws_clone = poly_ws.clone();
    let poly_task = tokio::spawn(async move {
        if let Err(e) = poly_ws_clone.run().await {
            error!(error = %e, "poly_ws task failed");
        }
    });

    // ===== DISPATCHER =====
    let dispatcher = spawn_dispatcher(
        event_rx,
        strat_a.clone(),
        strat_b.clone(),
        strat_c.clone(),
        books.clone(),
        cex_state.clone(),
        rec_a.clone(),
        rec_b.clone(),
        rec_c.clone(),
    );

    // ===== TIMER =====
    info!(duration_s = duration_seconds, "trinity_running_until_timer");
    tokio::time::sleep(std::time::Duration::from_secs(duration_seconds)).await;

    info!("trinity_timer_elapsed_stopping");
    poly_ws.stop();
    cex.stop();
    dispatcher.abort();
    let _ = tokio::time::timeout(std::time::Duration::from_secs(5), poly_task).await;
    let _ = tokio::time::timeout(std::time::Duration::from_secs(5), cex_task).await;

    info!("trinity_stopped");
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn spawn_dispatcher(
    mut event_rx: mpsc::Receiver<BookEvent>,
    strat_a: Arc<Mutex<LagArbStrategy>>,
    strat_b: Arc<Mutex<BilateralPureStrategy>>,
    strat_c: Arc<Mutex<HybridStrategy>>,
    books: Arc<Mutex<HashMap<String, BookState>>>,
    cex_state: Arc<Mutex<HashMap<String, CexState>>>,
    rec_a: Arc<TrinityRecorder>,
    rec_b: Arc<TrinityRecorder>,
    rec_c: Arc<TrinityRecorder>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        // DEBUG MODE: logueamos TODOS los skips para diagnostico.
        let log_skip_reasons = [
            "neg_risk",
            "ttr_too_low",
            "liquidity_too_low",
            "price_out_of_range",
            "no_edge",
            "edge_too_high_phantom",
            "no_cex_data",
            "cex_disagrees",
            "no_sigma",
            "no_strike",
            "dedup",
        ];

        while let Some(BookEvent::Updated(asset_id)) = event_rx.recv().await {
            // Resolver market_id (todas las strategies tienen mismo mapping)
            let market_id = {
                let s = strat_b.lock().await;
                s.market_id_for_asset(&asset_id).map(String::from)
            };
            let Some(market_id) = market_id else { continue };

            // Snapshot de books y cex
            let books_snapshot = {
                let b = books.lock().await;
                b.clone()
            };
            let cex_snapshot = {
                let c = cex_state.lock().await;
                c.clone()
            };

            // Evaluar las 3 strategies
            // A
            let d_a = {
                let mut a = strat_a.lock().await;
                a.evaluate_market(&market_id, &books_snapshot, &cex_snapshot)
            };
            if let Some(d) = d_a {
                let should_log = matches!(d.decision, DecisionKind::Enter)
                    || d
                        .skip_reason
                        .as_ref()
                        .map(|r| log_skip_reasons.contains(&r.as_str()))
                        .unwrap_or(false);
                if should_log {
                    if let Err(e) = rec_a.record(&d) {
                        warn!(error = %e, "record A failed");
                    }
                    if matches!(d.decision, DecisionKind::Enter) {
                        info!(
                            strategy = "A",
                            mkt = &d.market_id[..d.market_id.len().min(14)],
                            dir = ?d.direction,
                            edge = %d.edge_per_unit,
                            size = %d.size_usdc,
                            pnl_teo = %d.expected_pnl_usdc,
                            p_model = ?d.p_model_yes,
                            "trinity_entry"
                        );
                    }
                }
            }

            // B
            let d_b = {
                let b = strat_b.lock().await;
                b.evaluate_market(&market_id, &books_snapshot)
            };
            if let Some(d) = d_b {
                let should_log = matches!(d.decision, DecisionKind::Enter)
                    || d
                        .skip_reason
                        .as_ref()
                        .map(|r| log_skip_reasons.contains(&r.as_str()))
                        .unwrap_or(false);
                if should_log {
                    if let Err(e) = rec_b.record(&d) {
                        warn!(error = %e, "record B failed");
                    }
                    if matches!(d.decision, DecisionKind::Enter) {
                        info!(
                            strategy = "B",
                            mkt = &d.market_id[..d.market_id.len().min(14)],
                            edge = %d.edge_per_unit,
                            sum = %d.sum_ask,
                            size = %d.size_usdc,
                            pnl_teo = %d.expected_pnl_usdc,
                            "trinity_entry"
                        );
                    }
                }
            }

            // C
            let d_c = {
                let mut c = strat_c.lock().await;
                c.evaluate_market(&market_id, &books_snapshot, &cex_snapshot)
            };
            if let Some(d) = d_c {
                let should_log = matches!(d.decision, DecisionKind::Enter)
                    || d
                        .skip_reason
                        .as_ref()
                        .map(|r| log_skip_reasons.contains(&r.as_str()))
                        .unwrap_or(false);
                if should_log {
                    if let Err(e) = rec_c.record(&d) {
                        warn!(error = %e, "record C failed");
                    }
                    if matches!(d.decision, DecisionKind::Enter) {
                        info!(
                            strategy = "C",
                            mkt = &d.market_id[..d.market_id.len().min(14)],
                            edge = %d.edge_per_unit,
                            cex_edge = ?d.cex_edge,
                            "trinity_entry"
                        );
                    }
                }
            }
        }
    })
}

/// Useful for tests
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recorder_writes_jsonl() {
        let tmp = std::env::temp_dir().join(format!(
            "trinity_test_{}.jsonl",
            chrono::Utc::now().timestamp_micros()
        ));
        let _ = std::fs::remove_file(&tmp);
        let r = TrinityRecorder::open(&tmp).unwrap();
        let d = Decision {
            strategy: "TEST".to_string(),
            timestamp: chrono::Utc::now(),
            market_id: "0xMKT".to_string(),
            market_slug: "btc-updown-5m-X".to_string(),
            decision: DecisionKind::Enter,
            skip_reason: None,
            yes_token_id: "T1".to_string(),
            no_token_id: "T2".to_string(),
            price_yes: Decimal::ZERO,
            price_no: Decimal::ZERO,
            sum_ask: Decimal::ZERO,
            size_yes_available: Decimal::ZERO,
            size_no_available: Decimal::ZERO,
            ttr_seconds: Some(300),
            neg_risk: false,
            cex_product: None,
            cex_spot: None,
            cex_strike: None,
            cex_sigma_annual: None,
            p_model_yes: None,
            cex_edge: None,
            direction: Some("YES".to_string()),
            size_usdc: Decimal::ZERO,
            edge_per_unit: Decimal::ZERO,
            expected_pnl_usdc: Decimal::ZERO,
        };
        r.record(&d).unwrap();
        let content = std::fs::read_to_string(&tmp).unwrap();
        assert!(content.contains("TEST"));
        std::fs::remove_file(&tmp).ok();
    }
}
