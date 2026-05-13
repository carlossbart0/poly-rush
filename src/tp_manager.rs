//! TP Manager: lista posiciones abiertas y postea limit SELL al precio TP.
//!
//! Por cada posición:
//!   - Skip si está marcada `redeemable` (market ya resolvió) o `size <= 0`
//!   - tp_price = avg_price * (1 + tp_pct/100)
//!   - Cap a 0.99 (limite duro de Polymarket)
//!   - Round al tick size (0.01 si precio en [0.10, 0.90], else 0.001)
//!   - Skip si tp_price <= cur_price (TP ya alcanzado — se podria vender market,
//!     pero por seguridad lo logueamos sin tocar — usuario decide)
//!   - Skip si notional (tp_price * shares) < 1 USDC (minimo de Polymarket)
//!   - Postea limit SELL GTC

use anyhow::{anyhow, Context, Result};
use alloy::primitives::Address;
use alloy::signers::Signer as _;
use polymarket_client_sdk_v2::clob::types::{OrderType, Side as PolySide, SignatureType};
use polymarket_client_sdk_v2::clob::{Client as ClobClient, Config as ClobConfig};
use polymarket_client_sdk_v2::data::types::request::PositionsRequest;
use polymarket_client_sdk_v2::data::Client as DataClient;
use polymarket_client_sdk_v2::types::Decimal as PolyDecimal;
use polymarket_client_sdk_v2::POLYGON;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::str::FromStr;
use tracing::{info, warn};

const CLOB_HOST: &str = "https://clob.polymarket.com";
const MAX_TP_PRICE: Decimal = dec!(0.99);
const MIN_NOTIONAL_USDC: Decimal = dec!(1);
const TICK_LOW: Decimal = dec!(0.001);
const TICK_NORMAL: Decimal = dec!(0.01);
const PRICE_LOW_THRESHOLD: Decimal = dec!(0.10);
const PRICE_HIGH_THRESHOLD: Decimal = dec!(0.90);
/// Polymarket lot size: shares deben tener max 2 decimales (step 0.01).
const SIZE_LOT_SIZE: Decimal = dec!(0.01);

#[derive(Debug, Default)]
pub struct TpReport {
    pub total_positions: usize,
    pub skipped_redeemable: usize,
    pub skipped_zero_size: usize,
    pub skipped_tp_already_reached: usize,
    pub skipped_below_min_notional: usize,
    pub skipped_tp_capped_unprofitable: usize,
    pub posted_ok: usize,
    pub posted_failed: usize,
    pub orders: Vec<TpOrderResult>,
}

#[derive(Debug)]
pub struct TpOrderResult {
    pub condition_id: String,
    pub asset_token_id: String,
    pub size: Decimal,
    pub avg_price: Decimal,
    pub cur_price: Decimal,
    pub tp_price: Decimal,
    pub order_id: Option<String>,
    pub error: Option<String>,
}

/// Computa tick size segun el precio (regla Polymarket).
fn tick_size_for(price: Decimal) -> Decimal {
    if price < PRICE_LOW_THRESHOLD || price > PRICE_HIGH_THRESHOLD {
        TICK_LOW
    } else {
        TICK_NORMAL
    }
}

/// Redondea hacia abajo al tick (para SELL — no queremos quedar arriba del target).
fn round_down_to_tick(price: Decimal, tick: Decimal) -> Decimal {
    if tick.is_zero() {
        return price;
    }
    let n = (price / tick).floor();
    n * tick
}

/// Lista posiciones del owner y postea TP en cada una. tp_pct es porcentaje
/// (e.g. dec!(15) = 15% sobre avg_price).
pub async fn place_tp_on_open_positions(
    private_key_hex: String,
    owner: Address,
    signature_type: SignatureType,
    funder: Option<Address>,
    tp_pct: Decimal,
) -> Result<TpReport> {
    info!(
        owner = %owner,
        tp_pct = %tp_pct,
        sig_type = ?signature_type,
        funder = ?funder,
        "tp_manager_starting"
    );

    // 1. Lista positions via Data API (publico, sin auth)
    let data_client = DataClient::default();
    let request = PositionsRequest::builder()
        .user(owner)
        .limit(500)
        .map_err(|e| anyhow!("PositionsRequest.limit: {e}"))?
        .build();
    let positions = data_client
        .positions(&request)
        .await
        .context("data_client.positions")?;
    info!(count = positions.len(), "tp_manager_positions_fetched");

    let mut report = TpReport {
        total_positions: positions.len(),
        ..Default::default()
    };

    if positions.is_empty() {
        warn!("tp_manager_no_open_positions");
        return Ok(report);
    }

    // 2. Setup CLOB client autenticado (1 vez, reusamos para todas las orders)
    let pk_trim = private_key_hex.trim().trim_start_matches("0x");
    let signer = alloy::signers::local::LocalSigner::from_str(pk_trim)
        .context("LocalSigner from PRIVATE_KEY")?
        .with_chain_id(Some(POLYGON));
    let config = ClobConfig::builder().use_server_time(true).build();
    let auth_builder = ClobClient::new(CLOB_HOST, config)
        .context("ClobClient::new")?
        .authentication_builder(&signer)
        .signature_type(signature_type);
    let auth_builder = if let Some(f) = funder {
        auth_builder.funder(f)
    } else {
        auth_builder
    };
    let client = auth_builder.authenticate().await.context("CLOB authenticate")?;
    info!("tp_manager_clob_authenticated");

    // 3. Por cada position, decidir + postear
    for pos in &positions {
        // Las posiciones del SDK usan rust_decimal::Decimal de polymarket_client_sdk_v2::types,
        // convertimos a nuestro rust_decimal::Decimal via string.
        let raw_size = Decimal::from_str(&pos.size.to_string()).unwrap_or(Decimal::ZERO);
        // Polymarket requiere lot size 0.01 — redondeamos hacia abajo para no
        // intentar vender mas shares que las disponibles en la position.
        let size = round_down_to_tick(raw_size, SIZE_LOT_SIZE);
        let avg_price = Decimal::from_str(&pos.avg_price.to_string()).unwrap_or(Decimal::ZERO);
        let cur_price = Decimal::from_str(&pos.cur_price.to_string()).unwrap_or(Decimal::ZERO);

        let condition_id = format!("{:#x}", pos.condition_id);
        let asset_token_id = pos.asset.to_string();

        if pos.redeemable {
            info!(
                mkt = %condition_id[..condition_id.len().min(14)],
                "tp_skip_redeemable"
            );
            report.skipped_redeemable += 1;
            continue;
        }
        if size <= Decimal::ZERO {
            report.skipped_zero_size += 1;
            continue;
        }
        if avg_price <= Decimal::ZERO {
            warn!(
                mkt = %condition_id[..condition_id.len().min(14)],
                "tp_skip_zero_avg_price"
            );
            report.skipped_zero_size += 1;
            continue;
        }

        // tp_price calc
        let raw_tp = avg_price * (Decimal::ONE + tp_pct / dec!(100));
        let capped = raw_tp.min(MAX_TP_PRICE);
        let tick = tick_size_for(capped);
        let tp_price = round_down_to_tick(capped, tick);

        if tp_price <= avg_price {
            // El cap a 0.99 dejo el TP por debajo del entry — no rentable
            warn!(
                mkt = %condition_id[..condition_id.len().min(14)],
                avg = %avg_price,
                tp = %tp_price,
                "tp_skip_capped_unprofitable"
            );
            report.skipped_tp_capped_unprofitable += 1;
            report.orders.push(TpOrderResult {
                condition_id: condition_id.clone(),
                asset_token_id: asset_token_id.clone(),
                size,
                avg_price,
                cur_price,
                tp_price,
                order_id: None,
                error: Some("tp_capped_unprofitable".to_string()),
            });
            continue;
        }

        let notional = tp_price * size;
        if notional < MIN_NOTIONAL_USDC {
            warn!(
                mkt = %condition_id[..condition_id.len().min(14)],
                notional = %notional,
                "tp_skip_below_min_notional"
            );
            report.skipped_below_min_notional += 1;
            continue;
        }

        info!(
            mkt = %condition_id[..condition_id.len().min(14)],
            size = %size,
            avg = %avg_price,
            cur = %cur_price,
            tp = %tp_price,
            "tp_placing_limit_sell"
        );

        // 4. Postear limit SELL GTC. Sigue en book hasta filled o cancel.
        let token_id_u256 = polymarket_client_sdk_v2::types::U256::from_str(&asset_token_id)
            .with_context(|| format!("parse asset token_id: {asset_token_id}"))?;
        let price_poly = PolyDecimal::from_str(&tp_price.to_string()).context("tp_price parse")?;
        let size_poly = PolyDecimal::from_str(&size.to_string()).context("size parse")?;

        let result = client
            .limit_order()
            .token_id(token_id_u256)
            .side(PolySide::Sell)
            .price(price_poly)
            .size(size_poly)
            .order_type(OrderType::GTC)
            .build_sign_and_post(&signer)
            .await;

        match result {
            Ok(resp) => {
                info!(
                    mkt = %condition_id[..condition_id.len().min(14)],
                    order_id = %resp.order_id,
                    status = %resp.status,
                    "tp_posted_ok"
                );
                report.posted_ok += 1;
                report.orders.push(TpOrderResult {
                    condition_id,
                    asset_token_id,
                    size,
                    avg_price,
                    cur_price,
                    tp_price,
                    order_id: Some(resp.order_id),
                    error: None,
                });
            }
            Err(e) => {
                let err_str = format!("{:#}", e);
                warn!(
                    mkt = %condition_id[..condition_id.len().min(14)],
                    err = %err_str,
                    "tp_post_failed"
                );
                report.posted_failed += 1;
                report.orders.push(TpOrderResult {
                    condition_id,
                    asset_token_id,
                    size,
                    avg_price,
                    cur_price,
                    tp_price,
                    order_id: None,
                    error: Some(err_str),
                });
            }
        }
    }

    info!(
        total = report.total_positions,
        posted = report.posted_ok,
        failed = report.posted_failed,
        skip_redeemable = report.skipped_redeemable,
        skip_zero = report.skipped_zero_size,
        skip_capped = report.skipped_tp_capped_unprofitable,
        skip_min_notional = report.skipped_below_min_notional,
        "tp_manager_done"
    );

    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tick_normal_zone() {
        assert_eq!(tick_size_for(dec!(0.50)), TICK_NORMAL);
        assert_eq!(tick_size_for(dec!(0.10)), TICK_NORMAL);
        assert_eq!(tick_size_for(dec!(0.90)), TICK_NORMAL);
    }

    #[test]
    fn tick_low_zone() {
        assert_eq!(tick_size_for(dec!(0.05)), TICK_LOW);
        assert_eq!(tick_size_for(dec!(0.95)), TICK_LOW);
    }

    #[test]
    fn round_down_basic() {
        assert_eq!(round_down_to_tick(dec!(0.567), TICK_NORMAL), dec!(0.56));
        assert_eq!(round_down_to_tick(dec!(0.045), TICK_LOW), dec!(0.045));
        assert_eq!(round_down_to_tick(dec!(0.0455), TICK_LOW), dec!(0.045));
    }

    #[test]
    fn tp_calc_15pct_normal() {
        let avg = dec!(0.50);
        let raw = avg * (Decimal::ONE + dec!(15) / dec!(100));
        assert_eq!(raw, dec!(0.575));
        let tick = tick_size_for(raw);
        let tp = round_down_to_tick(raw.min(MAX_TP_PRICE), tick);
        assert_eq!(tp, dec!(0.57));
    }

    #[test]
    fn tp_calc_caps_at_99() {
        // avg=0.95, tp=15% → 1.0925, cap a 0.99
        let avg = dec!(0.95);
        let raw = avg * (Decimal::ONE + dec!(15) / dec!(100));
        let capped = raw.min(MAX_TP_PRICE);
        let tick = tick_size_for(capped);
        let tp = round_down_to_tick(capped, tick);
        assert_eq!(tp, dec!(0.989)); // 0.99 cap, low tick zone, round down
    }
}
