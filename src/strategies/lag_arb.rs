//! Strategy A v2: MOMENTUM CEX DIRECCIONAL.
//!
//! Replica empirica de lo que hace 0xeebde7a0. NO usa Black-Scholes.
//! Usa momentum simple del CEX en ventanas de 30s/60s.
//!
//! Logica:
//!   1. Si BTC subio mas de 5 bps en los ultimos 30s → momentum bullish
//!      → comprar YES de markets btc-updown si best_ask_yes < umbral_dinamico
//!   2. Si BTC bajo mas de 5 bps en 30s → momentum bearish
//!      → comprar NO de markets btc-updown si best_ask_no < umbral_dinamico
//!   3. Sizing escalado por |momentum|: mas movimiento = mas conviction = mas size
//!   4. Anti-spam: 1 entry per market per direction cada 60s
//!
//! TTR window: 30s-10min (median del bot real fue 11min, p10 98s).

use crate::book_state::BookState;
use crate::cex_feed::CexState;
use crate::pricing_model::product_id_from_slug;
use crate::strategies::{skip_reasons, Decision, DecisionKind};
use crate::types::Market;
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::collections::{HashMap, VecDeque};
use std::str::FromStr;

pub struct LagArbConfig {
    pub momentum_threshold: f64, // 0.0005 = 5bps en 30s
    pub base_size_usdc: Decimal,
    pub max_size_usdc: Decimal,
    pub min_ttr_seconds: i64,
    pub max_ttr_seconds: i64,
    pub min_price_yes: Decimal,
    pub max_price_yes: Decimal,
    pub momentum_window_s: i64,
    pub dedup_window_s: i64,
}

impl Default for LagArbConfig {
    fn default() -> Self {
        Self {
            momentum_threshold: 0.0001,
            base_size_usdc: dec!(10),
            max_size_usdc: dec!(50),
            min_ttr_seconds: 10,
            max_ttr_seconds: 900,
            min_price_yes: dec!(0.03),
            max_price_yes: dec!(0.97),
            momentum_window_s: 30,
            dedup_window_s: 30,
        }
    }
}

/// Track ultimo entry por (market_id, direction) para dedup.
#[derive(Default)]
pub struct DedupState {
    last_entry: HashMap<(String, String), DateTime<Utc>>,
}

impl DedupState {
    pub fn should_emit(&self, mkt: &str, dir: &str, now: DateTime<Utc>, window_s: i64) -> bool {
        if let Some(last) = self.last_entry.get(&(mkt.to_string(), dir.to_string())) {
            if (now - *last).num_seconds() < window_s {
                return false;
            }
        }
        true
    }
    pub fn record(&mut self, mkt: &str, dir: &str, ts: DateTime<Utc>) {
        self.last_entry
            .insert((mkt.to_string(), dir.to_string()), ts);
    }
}

/// Computa momentum CEX como (price_now / price_window_ago - 1).
pub fn momentum(cex: &CexState, window_s: i64, now: DateTime<Utc>) -> Option<f64> {
    let now_p = cex.last_price?;
    // Tomar el log_return acumulado en la ventana
    // Sumamos log returns hasta cubrir window_s segundos
    let mut elapsed = 0.0;
    let mut log_ret_sum = 0.0;
    for (lr, dt) in cex.log_returns.iter().rev().zip(cex.tick_dt_seconds.iter().rev()) {
        elapsed += dt;
        log_ret_sum += lr;
        if elapsed >= window_s as f64 {
            break;
        }
    }
    if elapsed < (window_s as f64) * 0.3 {
        // No suficientes ticks en la ventana
        return None;
    }
    let _ = now_p;
    let _ = now;
    Some((log_ret_sum.exp()) - 1.0)
}

pub fn evaluate(
    market: &Market,
    book_a: &BookState, // YES
    book_b: &BookState, // NO
    cex_state: &HashMap<String, CexState>,
    cfg: &LagArbConfig,
    dedup: &mut DedupState,
) -> Decision {
    let now = Utc::now();
    let mut base = Decision {
        strategy: "A".to_string(),
        timestamp: now,
        market_id: market.condition_id.clone(),
        market_slug: market.slug.clone(),
        decision: DecisionKind::Skip,
        skip_reason: None,
        yes_token_id: market.yes_token_id.clone(),
        no_token_id: market.no_token_id.clone(),
        price_yes: Decimal::ZERO,
        price_no: Decimal::ZERO,
        sum_ask: Decimal::ZERO,
        size_yes_available: Decimal::ZERO,
        size_no_available: Decimal::ZERO,
        ttr_seconds: market.end_date.map(|d| (d - now).num_seconds()),
        neg_risk: market.neg_risk,
        cex_product: None,
        cex_spot: None,
        cex_strike: None,
        cex_sigma_annual: None,
        p_model_yes: None,
        cex_edge: None,
        direction: None,
        size_usdc: Decimal::ZERO,
        edge_per_unit: Decimal::ZERO,
        expected_pnl_usdc: Decimal::ZERO,
    };

    // FILTROS
    if market.neg_risk {
        base.skip_reason = Some(skip_reasons::NEG_RISK.to_string());
        return base;
    }
    let ttr = base.ttr_seconds.unwrap_or(-1);
    if ttr < cfg.min_ttr_seconds || ttr > cfg.max_ttr_seconds {
        base.skip_reason = Some(skip_reasons::TTR_TOO_LOW.to_string());
        return base;
    }

    let (price_yes, size_yes) = match book_a.best_ask() {
        Some(v) => v,
        None => {
            base.skip_reason = Some(skip_reasons::LIQUIDITY_TOO_LOW.to_string());
            return base;
        }
    };
    let (price_no, size_no) = match book_b.best_ask() {
        Some(v) => v,
        None => {
            base.skip_reason = Some(skip_reasons::LIQUIDITY_TOO_LOW.to_string());
            return base;
        }
    };
    base.price_yes = price_yes;
    base.price_no = price_no;
    base.sum_ask = price_yes + price_no;
    base.size_yes_available = size_yes;
    base.size_no_available = size_no;

    if price_yes < cfg.min_price_yes
        || price_yes > cfg.max_price_yes
        || price_no < cfg.min_price_yes
        || price_no > cfg.max_price_yes
    {
        base.skip_reason = Some(skip_reasons::PRICE_OUT_OF_RANGE.to_string());
        return base;
    }

    // CEX product
    let product = match product_id_from_slug(&market.slug) {
        Some(p) => p.to_string(),
        None => {
            base.skip_reason = Some(skip_reasons::NO_CEX_DATA.to_string());
            return base;
        }
    };
    base.cex_product = Some(product.clone());

    let cex = match cex_state.get(&product) {
        Some(c) => c,
        None => {
            base.skip_reason = Some(skip_reasons::NO_CEX_DATA.to_string());
            return base;
        }
    };
    base.cex_spot = cex.last_price;

    // MOMENTUM SIGNAL
    let mom = match momentum(cex, cfg.momentum_window_s, now) {
        Some(m) => m,
        None => {
            base.skip_reason = Some(skip_reasons::NO_CEX_DATA.to_string());
            return base;
        }
    };
    base.cex_edge = Some(mom);

    // DIRECCION segun momentum
    let abs_mom = mom.abs();
    if abs_mom < cfg.momentum_threshold {
        base.skip_reason = Some(skip_reasons::NO_EDGE.to_string());
        return base;
    }

    let (direction, price, size_avail) = if mom > 0.0 {
        // BTC subiendo → YES más probable → comprar YES si precio razonable
        if price_yes >= dec!(0.7) {
            // Ya está caro YES, no entrada
            base.skip_reason = Some(skip_reasons::PRICE_OUT_OF_RANGE.to_string());
            return base;
        }
        ("YES", price_yes, size_yes)
    } else {
        // BTC bajando → NO más probable
        if price_no >= dec!(0.7) {
            base.skip_reason = Some(skip_reasons::PRICE_OUT_OF_RANGE.to_string());
            return base;
        }
        ("NO", price_no, size_no)
    };

    // DEDUP: 1 entry per market per direction per window
    if !dedup.should_emit(&market.condition_id, direction, now, cfg.dedup_window_s) {
        base.skip_reason = Some("dedup".to_string());
        return base;
    }

    // SIZING power-law: more |momentum| → more conviction → bigger size
    // momentum=threshold → 1× base. momentum=2×threshold → 2× base. cap @ max.
    let mom_ratio = (abs_mom / cfg.momentum_threshold).min(5.0);
    let target_usdc = cfg.base_size_usdc
        * Decimal::try_from(mom_ratio).unwrap_or(Decimal::ONE);
    let target_usdc = target_usdc.min(cfg.max_size_usdc);
    let shares_target = if price > Decimal::ZERO {
        target_usdc / price
    } else {
        Decimal::ZERO
    };
    let shares = shares_target.min(size_avail);
    if shares <= Decimal::ZERO {
        base.skip_reason = Some(skip_reasons::LIQUIDITY_TOO_LOW.to_string());
        return base;
    }

    // EDGE: si pago $price y "el modelo" cree que va a ganar, mi edge esperado
    // es (1 - price). PnL esperado bruto = shares × (1 - price).
    // Pero esto solo se realiza si la predicción es correcta.
    // Para reportar PnL teórico CONSERVADOR: asumimos accuracy=55% (más que
    // coinflip, menos que perfect)
    let accuracy = dec!(0.55);
    let pnl_if_win = (Decimal::ONE - price) * shares;
    let pnl_if_lose = -price * shares; // perdés todo el costo
    let expected_pnl = accuracy * pnl_if_win + (Decimal::ONE - accuracy) * pnl_if_lose;

    dedup.record(&market.condition_id, direction, now);

    base.decision = DecisionKind::Enter;
    base.direction = Some(direction.to_string());
    base.size_usdc = price * shares;
    base.edge_per_unit = Decimal::try_from(abs_mom).unwrap_or(Decimal::ZERO);
    base.expected_pnl_usdc = expected_pnl;
    base
}

/// Wrapper stateful con dedup.
pub struct LagArbStrategy {
    cfg: LagArbConfig,
    asset_to_market: HashMap<String, String>,
    markets: HashMap<String, Market>,
    market_pairs: HashMap<String, (String, String)>,
    product_required: HashMap<String, String>,
    pub dedup: DedupState,
}

impl LagArbStrategy {
    pub fn new(cfg: LagArbConfig) -> Self {
        Self {
            cfg,
            asset_to_market: HashMap::new(),
            markets: HashMap::new(),
            market_pairs: HashMap::new(),
            product_required: HashMap::new(),
            dedup: DedupState::default(),
        }
    }

    pub fn register_market(&mut self, market: Market) {
        let a = market.yes_token_id.clone();
        let b = market.no_token_id.clone();
        let cid = market.condition_id.clone();
        if let Some(pid) = product_id_from_slug(&market.slug) {
            self.product_required.insert(cid.clone(), pid.to_string());
        }
        self.asset_to_market.insert(a.clone(), cid.clone());
        self.asset_to_market.insert(b.clone(), cid.clone());
        self.market_pairs.insert(cid.clone(), (a, b));
        self.markets.insert(cid, market);
    }

    pub fn market_id_for_asset(&self, asset_id: &str) -> Option<&str> {
        self.asset_to_market.get(asset_id).map(String::as_str)
    }

    pub fn evaluate_market(
        &mut self,
        market_id: &str,
        books: &HashMap<String, BookState>,
        cex_state: &HashMap<String, CexState>,
    ) -> Option<Decision> {
        let market = self.markets.get(market_id)?;
        let (a, b) = self.market_pairs.get(market_id)?;
        let book_a = books.get(a)?;
        let book_b = books.get(b)?;
        Some(evaluate(market, book_a, book_b, cex_state, &self.cfg, &mut self.dedup))
    }
}

// Stub para que `momentum` sea testeable.
#[allow(dead_code)]
fn _unused_dummy(_v: VecDeque<f64>) {}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn momentum_simple() {
        let mut s = CexState::new("BTC-USD");
        let base = Utc.with_ymd_and_hms(2026, 5, 12, 0, 0, 0).unwrap();
        // 50 ticks de 1s c/u con drift positivo 1bps/tick = ~50 bps en 30s
        for i in 0..50 {
            let p = 63000.0 * (1.0 + 0.0001 * i as f64);
            s.update(p, base + chrono::Duration::seconds(i));
        }
        let m = momentum(&s, 30, base + chrono::Duration::seconds(60));
        let mv = m.expect("momentum");
        assert!(mv > 0.001, "momentum should be positive {:?}", mv);
    }

    #[test]
    fn dedup_blocks_within_window() {
        let mut d = DedupState::default();
        let t = Utc.with_ymd_and_hms(2026, 5, 12, 0, 0, 0).unwrap();
        assert!(d.should_emit("MKT", "YES", t, 60));
        d.record("MKT", "YES", t);
        assert!(!d.should_emit("MKT", "YES", t + chrono::Duration::seconds(30), 60));
        assert!(d.should_emit("MKT", "YES", t + chrono::Duration::seconds(61), 60));
        // Different direction OK
        assert!(d.should_emit("MKT", "NO", t + chrono::Duration::seconds(30), 60));
    }
}
