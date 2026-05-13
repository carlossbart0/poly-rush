//! Settings desde environment. Port simplificado de `config/settings.py`.

use anyhow::{Context, Result};
use rust_decimal::Decimal;
use std::path::PathBuf;
use std::str::FromStr;

#[derive(Debug, Clone)]
pub struct Settings {
    pub poly_ws_url: String,
    pub gamma_url: String,
    pub arb_min_edge_per_unit: Decimal,
    pub arb_max_size_usdc: Decimal,
    pub arb_fee_rate_bps: u32,
    pub arb_slippage_buffer_bps: u32,
    pub arb_markets_refresh_seconds: u64,
    pub arb_horizon_seconds: u64,
    pub arb_gamma_limit: u32,
    pub db_path: PathBuf,
    pub stop_file_path: PathBuf,
}

impl Settings {
    pub fn from_env() -> Result<Self> {
        // Cargar .env del cwd (no requerimos que exista).
        let _ = dotenvy::dotenv();

        Ok(Self {
            poly_ws_url: getenv_string(
                "POLY_WS_URL",
                "wss://ws-subscriptions-clob.polymarket.com/ws/market",
            ),
            gamma_url: getenv_string("GAMMA_URL", "https://gamma-api.polymarket.com"),
            arb_min_edge_per_unit: getenv_decimal("ARB_MIN_EDGE_PER_UNIT", "0.002")?,
            arb_max_size_usdc: getenv_decimal("ARB_MAX_SIZE_USDC", "20")?,
            arb_fee_rate_bps: getenv_u32("ARB_FEE_RATE_BPS", 180)?,
            arb_slippage_buffer_bps: getenv_u32("ARB_SLIPPAGE_BUFFER_BPS", 20)?,
            arb_markets_refresh_seconds: getenv_u64("ARB_MARKETS_REFRESH_SECONDS", 120)?,
            arb_horizon_seconds: getenv_u64("ARB_HORIZON_SECONDS", 7200)?,
            arb_gamma_limit: getenv_u32("ARB_GAMMA_LIMIT", 500)?,
            db_path: PathBuf::from(getenv_string("DB_PATH", "state/rust_bot.jsonl")),
            stop_file_path: PathBuf::from(getenv_string("STOP_FILE_PATH", "state/STOP")),
        })
    }
}

fn getenv_string(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

fn getenv_decimal(key: &str, default: &str) -> Result<Decimal> {
    let raw = std::env::var(key).unwrap_or_else(|_| default.to_string());
    Decimal::from_str(&raw).with_context(|| format!("{key} not a decimal: {raw}"))
}

fn getenv_u32(key: &str, default: u32) -> Result<u32> {
    match std::env::var(key) {
        Ok(v) => v.parse().with_context(|| format!("{key} not a u32: {v}")),
        Err(_) => Ok(default),
    }
}

fn getenv_u64(key: &str, default: u64) -> Result<u64> {
    match std::env::var(key) {
        Ok(v) => v.parse().with_context(|| format!("{key} not a u64: {v}")),
        Err(_) => Ok(default),
    }
}
