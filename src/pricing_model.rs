//! Log-normal Black-Scholes para P(S_T > K).
//!
//! Para markets `{crypto}-updown-5m-{unix_close}`:
//!   K = precio CEX al inicio del bucket (T_open = T_close - 300s)
//!   S_now = precio CEX actual
//!   dt = (T_close - now) / seconds_per_year
//!   sigma = realized vol annualized
//!
//! P(S_T > K) = Phi(d2)
//!   donde d2 = (log(S_now/K) - 0.5*sigma^2*dt) / (sigma*sqrt(dt))

use chrono::{DateTime, Utc};

const SECONDS_PER_YEAR: f64 = 365.25 * 24.0 * 3600.0;
const SIGMA_FALLBACK: f64 = 0.60; // 60% annualized si insuficiente data
const MIN_DT_SECONDS: f64 = 1.0;
const MAX_DT_SECONDS: f64 = 3600.0;

/// CDF Normal estándar via approximation de Abramowitz-Stegun (precision ~7e-8).
fn standard_normal_cdf(x: f64) -> f64 {
    // Constants for Abramowitz-Stegun
    let a1 = 0.254829592;
    let a2 = -0.284496736;
    let a3 = 1.421413741;
    let a4 = -1.453152027;
    let a5 = 1.061405429;
    let p = 0.3275911;

    let sign = if x < 0.0 { -1.0 } else { 1.0 };
    let abs_x = x.abs() / std::f64::consts::SQRT_2;
    let t = 1.0 / (1.0 + p * abs_x);
    let y = 1.0
        - (((((a5 * t + a4) * t) + a3) * t + a2) * t + a1) * t * (-abs_x * abs_x).exp();
    0.5 * (1.0 + sign * y)
}

/// Calcula P(S_T > K) usando log-normal Black-Scholes.
/// Devuelve None si inputs no son utilizables.
pub fn prob_above_threshold(
    s_now: f64,
    k: f64,
    dt_seconds: f64,
    sigma_annual: f64,
) -> Option<f64> {
    if s_now <= 0.0 || k <= 0.0 || sigma_annual <= 0.0 {
        return None;
    }
    if dt_seconds < MIN_DT_SECONDS || dt_seconds > MAX_DT_SECONDS {
        return None;
    }
    let dt = dt_seconds / SECONDS_PER_YEAR;
    let sigma_sqrt_dt = sigma_annual * dt.sqrt();
    if sigma_sqrt_dt <= 0.0 {
        return None;
    }
    let d2 = ((s_now / k).ln() - 0.5 * sigma_annual.powi(2) * dt) / sigma_sqrt_dt;
    Some(standard_normal_cdf(d2))
}

/// Strike inferido para markets `{crypto}-updown-5m-{unix_close}`.
///
/// Hipótesis: K = precio del crypto al timestamp T_open = T_close - 300s.
/// Retorna T_open en epoch seconds.
pub fn strike_time_for_5m_market(unix_close: i64) -> i64 {
    unix_close - 300
}

/// Helper: parsear el unix_close del slug.
/// Ej "btc-updown-5m-1778621400" → 1778621400
pub fn parse_unix_close_from_slug(slug: &str) -> Option<i64> {
    slug.rsplit('-').next().and_then(|s| s.parse::<i64>().ok())
}

/// Helper: detectar crypto del slug.
/// Ej "btc-updown-5m-..." → "BTC-USD"
pub fn product_id_from_slug(slug: &str) -> Option<&'static str> {
    let s = slug.to_lowercase();
    if s.starts_with("btc-") || s.contains("bitcoin") {
        Some("BTC-USD")
    } else if s.starts_with("eth-") || s.contains("ethereum") {
        Some("ETH-USD")
    } else if s.starts_with("sol-") || s.contains("solana") {
        Some("SOL-USD")
    } else if s.starts_with("xrp-") || s.contains("xrp") {
        Some("XRP-USD")
    } else {
        None
    }
}

/// Vol con fallback: usa realized si disponible, sino const.
pub fn sigma_or_fallback(realized: Option<f64>) -> f64 {
    realized.unwrap_or(SIGMA_FALLBACK).max(0.10).min(3.0)
}

/// Calcula edge direccional para market updown.
///
/// `price_yes_market`: mid implícito YES en Polymarket (0..1)
/// Returns: (P_model_yes, edge = P_model - price_yes)
pub fn directional_edge(
    s_now: f64,
    k: f64,
    time_to_close: DateTime<Utc>,
    now: DateTime<Utc>,
    sigma_annual: f64,
    price_yes_market: f64,
) -> Option<(f64, f64)> {
    let dt_s = (time_to_close - now).num_milliseconds() as f64 / 1000.0;
    let p_model = prob_above_threshold(s_now, k, dt_s, sigma_annual)?;
    Some((p_model, p_model - price_yes_market))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normal_cdf_known_values() {
        // Phi(0) = 0.5
        let v = standard_normal_cdf(0.0);
        assert!((v - 0.5).abs() < 1e-6);
        // Phi(1.96) ~ 0.975
        let v = standard_normal_cdf(1.96);
        assert!((v - 0.975).abs() < 0.001);
        // Phi(-1.96) ~ 0.025
        let v = standard_normal_cdf(-1.96);
        assert!((v - 0.025).abs() < 0.001);
    }

    #[test]
    fn prob_above_at_strike_is_near_half() {
        // S=K, debería dar ~0.5 (con leve drift negativo por el -0.5*sigma^2)
        let p = prob_above_threshold(63000.0, 63000.0, 60.0, 0.50).unwrap();
        assert!((p - 0.5).abs() < 0.01);
    }

    #[test]
    fn prob_above_far_above_strike_high() {
        // S >> K → P alto
        let p = prob_above_threshold(63500.0, 63000.0, 60.0, 0.50).unwrap();
        assert!(p > 0.7);
    }

    #[test]
    fn prob_above_far_below_strike_low() {
        // S << K → P bajo
        let p = prob_above_threshold(62500.0, 63000.0, 60.0, 0.50).unwrap();
        assert!(p < 0.3);
    }

    #[test]
    fn invalid_inputs_return_none() {
        assert!(prob_above_threshold(0.0, 63000.0, 60.0, 0.5).is_none());
        assert!(prob_above_threshold(63000.0, 0.0, 60.0, 0.5).is_none());
        assert!(prob_above_threshold(63000.0, 63000.0, 0.0, 0.5).is_none());
        assert!(prob_above_threshold(63000.0, 63000.0, 60.0, 0.0).is_none());
    }

    #[test]
    fn slug_parsing() {
        assert_eq!(parse_unix_close_from_slug("btc-updown-5m-1778621400"), Some(1778621400));
        assert_eq!(product_id_from_slug("btc-updown-5m-..."), Some("BTC-USD"));
        assert_eq!(product_id_from_slug("eth-updown-5m-..."), Some("ETH-USD"));
        assert_eq!(product_id_from_slug("trump-2028"), None);
    }
}
