//! Strategy C: hibrido bilateral + CEX como filtro de sanidad.
//!
//! Toda opp candidate viene de filtros bilaterales (B). Antes de ENTER,
//! consultamos CEX: si el mid de Polymarket esta MUY lejos del P_model
//! (>5% de diferencia), descartamos la opp como probable phantom.
//!
//! Garantia: estrictamente subset de B. Si B entra y CEX confirma → C entra.
//! Si CEX dice "phantom", C skip pero B sigue.

use crate::book_state::BookState;
use crate::cex_feed::CexState;
use crate::pricing_model::{
    parse_unix_close_from_slug, prob_above_threshold, product_id_from_slug, sigma_or_fallback,
    strike_time_for_5m_market,
};
use crate::strategies::bilateral_pure::{self, BilateralPureConfig};
use crate::strategies::{skip_reasons, Decision, DecisionKind};
use crate::types::Market;
use chrono::Utc;
use rust_decimal::Decimal;
use std::collections::HashMap;
use std::str::FromStr;

pub struct HybridConfig {
    pub bilateral: BilateralPureConfig,
    pub cex_tolerance: f64, // 0.05 = 5% max disagreement
}

pub fn evaluate(
    market: &Market,
    book_a: &BookState,
    book_b: &BookState,
    cex_state: &HashMap<String, CexState>,
    strike_cache: &HashMap<String, f64>,
    cfg: &HybridConfig,
) -> Decision {
    // Empezamos con la evaluacion bilateral
    let mut d = bilateral_pure::evaluate(market, book_a, book_b, &cfg.bilateral);
    d.strategy = "C".to_string();

    // Si B ya skip, C tambien skip (mismo reason)
    if matches!(d.decision, DecisionKind::Skip) {
        return d;
    }

    // B aprobaria. Ahora verificamos CEX si aplica.
    let product = match product_id_from_slug(&market.slug) {
        Some(p) => p.to_string(),
        None => {
            // No tenemos product CEX → no podemos confirmar. Aceptamos B.
            return d;
        }
    };
    d.cex_product = Some(product.clone());

    let cex = match cex_state.get(&product) {
        Some(c) => c,
        None => return d,
    };
    let spot = match cex.last_price {
        Some(p) => p,
        None => return d,
    };
    d.cex_spot = Some(spot);

    let strike = match strike_cache.get(&market.condition_id) {
        Some(k) => *k,
        None => return d, // si no hay strike, no podemos confirmar. Acepta B.
    };
    d.cex_strike = Some(strike);

    let sigma = sigma_or_fallback(cex.realized_vol_annual());
    d.cex_sigma_annual = Some(sigma);

    let end_dt = match market.end_date {
        Some(d) => d,
        None => return d.clone(),
    };
    let now = Utc::now();
    let dt_s = (end_dt - now).num_milliseconds() as f64 / 1000.0;
    let p_model = match prob_above_threshold(spot, strike, dt_s, sigma) {
        Some(v) => v,
        None => return d,
    };
    d.p_model_yes = Some(p_model);

    // Mid implícito Polymarket: (price_yes + (1 - price_no)) / 2
    let price_yes_f = f64::from_str(&d.price_yes.to_string()).unwrap_or(0.0);
    let price_no_f = f64::from_str(&d.price_no.to_string()).unwrap_or(0.0);
    let p_market_yes = (price_yes_f + (1.0 - price_no_f)) / 2.0;
    let disagreement = (p_model - p_market_yes).abs();
    d.cex_edge = Some(p_model - p_market_yes);

    // Si CEX disagrees > tolerance → descartar
    if disagreement > cfg.cex_tolerance {
        d.decision = DecisionKind::Skip;
        d.skip_reason = Some(skip_reasons::CEX_DISAGREES.to_string());
        return d;
    }

    // Si pasamos CEX check, mantenemos el ENTER de B.
    d
}

pub struct HybridStrategy {
    cfg: HybridConfig,
    asset_to_market: HashMap<String, String>,
    markets: HashMap<String, Market>,
    market_pairs: HashMap<String, (String, String)>,
    pub strike_cache: HashMap<String, f64>,
    product_required: HashMap<String, String>,
}

impl HybridStrategy {
    pub fn new(cfg: HybridConfig) -> Self {
        Self {
            cfg,
            asset_to_market: HashMap::new(),
            markets: HashMap::new(),
            market_pairs: HashMap::new(),
            strike_cache: HashMap::new(),
            product_required: HashMap::new(),
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

    pub fn try_compute_strike(
        &mut self,
        market_id: &str,
        cex_state: &HashMap<String, CexState>,
    ) -> Option<f64> {
        if let Some(k) = self.strike_cache.get(market_id) {
            return Some(*k);
        }
        let market = self.markets.get(market_id)?;
        let product = self.product_required.get(market_id)?;
        let cex = cex_state.get(product)?;
        let last = cex.last_price?;
        let unix_close = parse_unix_close_from_slug(&market.slug)?;
        let _open_ts = strike_time_for_5m_market(unix_close);
        let now = Utc::now();
        let end = market.end_date?;
        let elapsed_in_bucket = (now - (end - chrono::Duration::seconds(300))).num_seconds();
        if elapsed_in_bucket < 0 || elapsed_in_bucket > 300 {
            return None;
        }
        if elapsed_in_bucket < 30 {
            self.strike_cache.insert(market_id.to_string(), last);
            return Some(last);
        }
        None
    }

    pub fn evaluate_market(
        &mut self,
        market_id: &str,
        books: &HashMap<String, BookState>,
        cex_state: &HashMap<String, CexState>,
    ) -> Option<Decision> {
        let _ = self.try_compute_strike(market_id, cex_state);
        let market = self.markets.get(market_id)?;
        let (a, b) = self.market_pairs.get(market_id)?;
        let book_a = books.get(a)?;
        let book_b = books.get(b)?;
        Some(evaluate(
            market,
            book_a,
            book_b,
            cex_state,
            &self.strike_cache,
            &self.cfg,
        ))
    }

    pub fn tracked_assets(&self) -> HashMap<String, String> {
        self.asset_to_market.clone()
    }
}
