//! Tipos de dominio. Espejo del bot Python (`opportunity.py` + `markets.py`).

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Side {
    Buy,
    Sell,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Yes,
    No,
}

/// Mercado binario YES/NO. `condition_id` es la clave canonica (= market_id).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Market {
    pub condition_id: String,
    pub slug: String,
    pub question: String,
    pub yes_token_id: String,
    pub no_token_id: String,
    pub end_date: Option<DateTime<Utc>>,
    #[serde(default)]
    pub neg_risk: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArbLeg {
    pub asset_id: String,
    pub side: Side,
    #[serde(with = "rust_decimal::serde::str")]
    pub price: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub size_shares: Decimal,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArbOpportunity {
    pub strategy: String,
    pub market_id: String,
    pub detected_at: DateTime<Utc>,
    pub legs: Vec<ArbLeg>,
    #[serde(with = "rust_decimal::serde::str")]
    pub edge_per_unit: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub notional_usdc: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub expected_pnl_usdc: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub sum_ask: Decimal,
}
