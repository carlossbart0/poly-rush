//! Strategy B: bilateral puro mejorado.
//!
//! Diferencias vs runner.rs original:
//! - Filtro neg_risk (markets multi-outcome donde puede resolver a "neither")
//! - Filtro min time-to-resolution (anti-phantom edges de markets resolviendo)
//! - Filtro min liquidity (descarta arbs con notional irrelevante)
//! - Cap upper en edge (descarta phantom edges >8%)
//!
//! Garantia matematica: si dispara, gana (asumiendo fill simultaneo en live).

use crate::book_state::BookState;
use crate::fees::effective_fee_per_share;
use crate::strategies::{skip_reasons, Decision, DecisionKind};
use crate::types::Market;
use chrono::Utc;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::collections::HashMap;

pub struct BilateralPureConfig {
    pub fee_rate_bps: u32,
    pub min_edge_per_unit: Decimal,
    pub max_edge_per_unit: Decimal, // anti-phantom
    pub slippage_buffer_bps: u32,
    pub max_shares: Decimal,
    pub min_ttr_seconds: i64,
    pub min_liquidity_usdc: Decimal,
}

pub fn evaluate(
    market: &Market,
    book_a: &BookState,
    book_b: &BookState,
    cfg: &BilateralPureConfig,
) -> Decision {
    let now = Utc::now();
    let strategy = "B".to_string();

    let mut base = Decision {
        strategy: strategy.clone(),
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

    // FIX 1: neg_risk
    if market.neg_risk {
        base.skip_reason = Some(skip_reasons::NEG_RISK.to_string());
        return base;
    }

    // FIX 2: time-to-resolution
    if let Some(ttr) = base.ttr_seconds {
        if ttr < cfg.min_ttr_seconds {
            base.skip_reason = Some(skip_reasons::TTR_TOO_LOW.to_string());
            return base;
        }
    }

    // Books snapshots
    let (price_a, size_a) = match book_a.best_ask() {
        Some(v) => v,
        None => {
            base.skip_reason = Some(skip_reasons::LIQUIDITY_TOO_LOW.to_string());
            return base;
        }
    };
    let (price_b, size_b) = match book_b.best_ask() {
        Some(v) => v,
        None => {
            base.skip_reason = Some(skip_reasons::LIQUIDITY_TOO_LOW.to_string());
            return base;
        }
    };
    base.price_yes = price_a;
    base.price_no = price_b;
    base.sum_ask = price_a + price_b;
    base.size_yes_available = size_a;
    base.size_no_available = size_b;

    // Price sanity
    if price_a <= dec!(0.01)
        || price_b <= dec!(0.01)
        || price_a >= dec!(0.99)
        || price_b >= dec!(0.99)
    {
        base.skip_reason = Some(skip_reasons::PRICE_OUT_OF_RANGE.to_string());
        return base;
    }

    // FIX 3: liquidez minima (notional disponible al best ask)
    let notional_avail = (price_a * size_a).min(price_b * size_b);
    if notional_avail < cfg.min_liquidity_usdc {
        base.skip_reason = Some(skip_reasons::LIQUIDITY_TOO_LOW.to_string());
        return base;
    }

    // Edge math
    let fee_a = effective_fee_per_share(price_a, cfg.fee_rate_bps);
    let fee_b = effective_fee_per_share(price_b, cfg.fee_rate_bps);
    let cost = price_a + price_b + fee_a + fee_b;
    let edge = Decimal::ONE - cost;
    let slippage = Decimal::from(cfg.slippage_buffer_bps) / dec!(10000);
    let edge_after = edge - slippage;
    base.edge_per_unit = edge_after;

    if edge_after < cfg.min_edge_per_unit {
        base.skip_reason = Some(skip_reasons::NO_EDGE.to_string());
        return base;
    }
    // FIX 4: cap upper anti-phantom
    if edge_after > cfg.max_edge_per_unit {
        base.skip_reason = Some(skip_reasons::EDGE_TOO_HIGH.to_string());
        return base;
    }

    let shares = size_a.min(size_b).min(cfg.max_shares);
    if shares <= Decimal::ZERO {
        base.skip_reason = Some(skip_reasons::LIQUIDITY_TOO_LOW.to_string());
        return base;
    }
    base.decision = DecisionKind::Enter;
    base.direction = Some("BOTH".to_string());
    base.size_usdc = (price_a + price_b) * shares;
    base.expected_pnl_usdc = edge_after * shares;
    base
}

/// Wrapper stateful (mismo patrón que BilateralArbStrategy).
pub struct BilateralPureStrategy {
    cfg: BilateralPureConfig,
    asset_to_market: HashMap<String, String>,
    markets: HashMap<String, Market>,
    market_pairs: HashMap<String, (String, String)>,
}

impl BilateralPureStrategy {
    pub fn new(cfg: BilateralPureConfig) -> Self {
        Self {
            cfg,
            asset_to_market: HashMap::new(),
            markets: HashMap::new(),
            market_pairs: HashMap::new(),
        }
    }

    pub fn register_market(&mut self, market: Market) {
        let a = market.yes_token_id.clone();
        let b = market.no_token_id.clone();
        let cid = market.condition_id.clone();
        self.asset_to_market.insert(a.clone(), cid.clone());
        self.asset_to_market.insert(b.clone(), cid.clone());
        self.market_pairs.insert(cid.clone(), (a, b));
        self.markets.insert(cid, market);
    }

    pub fn market_id_for_asset(&self, asset_id: &str) -> Option<&str> {
        self.asset_to_market.get(asset_id).map(String::as_str)
    }

    pub fn evaluate_market(
        &self,
        market_id: &str,
        books: &HashMap<String, BookState>,
    ) -> Option<Decision> {
        let market = self.markets.get(market_id)?;
        let (a, b) = self.market_pairs.get(market_id)?;
        let book_a = books.get(a)?;
        let book_b = books.get(b)?;
        Some(evaluate(market, book_a, book_b, &self.cfg))
    }

    pub fn tracked_assets(&self) -> HashMap<String, String> {
        self.asset_to_market.clone()
    }

    pub fn cfg(&self) -> &BilateralPureConfig {
        &self.cfg
    }
}
