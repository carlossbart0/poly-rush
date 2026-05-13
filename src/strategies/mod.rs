//! Strategies del trinity harness.
//!
//! A: lag arb direccional (CEX-driven, predictive)
//! B: bilateral puro mejorado (matematico, garantizado si fillea)
//! C: hibrido (bilateral + CEX como filtro de sanidad)
//!
//! Cada una emite Decision a su recorder. Decision puede ser ENTER o SKIP
//! con razon. Todas se loguean para post-analisis comparativo.

pub mod lag_arb;
pub mod bilateral_pure;
pub mod hybrid;

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

/// Una decisión tomada por una estrategia ante un book update.
/// Se persiste en JSONL para análisis post-run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Decision {
    pub strategy: String,
    pub timestamp: DateTime<Utc>,
    pub market_id: String,
    pub market_slug: String,
    pub decision: DecisionKind,
    pub skip_reason: Option<String>,

    // Token IDs (necesarios para envío live)
    #[serde(default)]
    pub yes_token_id: String,
    #[serde(default)]
    pub no_token_id: String,

    // Pricing context
    #[serde(with = "rust_decimal::serde::str")]
    pub price_yes: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub price_no: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub sum_ask: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub size_yes_available: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub size_no_available: Decimal,

    // Market context
    pub ttr_seconds: Option<i64>,
    pub neg_risk: bool,

    // CEX context (solo A y C lo usan)
    pub cex_product: Option<String>,
    pub cex_spot: Option<f64>,
    pub cex_strike: Option<f64>,
    pub cex_sigma_annual: Option<f64>,
    pub p_model_yes: Option<f64>,
    pub cex_edge: Option<f64>,

    // Decision details
    pub direction: Option<String>, // "YES" | "NO" | "BOTH"
    #[serde(with = "rust_decimal::serde::str")]
    pub size_usdc: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub edge_per_unit: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub expected_pnl_usdc: Decimal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum DecisionKind {
    Enter,
    Skip,
}

/// Razones de SKIP estandarizadas para análisis.
pub mod skip_reasons {
    pub const NEG_RISK: &str = "neg_risk";
    pub const TTR_TOO_LOW: &str = "ttr_too_low";
    pub const LIQUIDITY_TOO_LOW: &str = "liquidity_too_low";
    pub const PRICE_OUT_OF_RANGE: &str = "price_out_of_range";
    pub const NO_EDGE: &str = "no_edge";
    pub const EDGE_TOO_HIGH: &str = "edge_too_high_phantom";
    pub const NO_CEX_DATA: &str = "no_cex_data";
    pub const CEX_DISAGREES: &str = "cex_disagrees";
    pub const NO_SIGMA: &str = "no_sigma";
    pub const NO_STRIKE: &str = "no_strike";
}
