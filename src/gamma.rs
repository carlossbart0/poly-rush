//! Cliente Gamma API + discovery de mercados crypto cortos.
//! Port de `data/gamma.py` + `strategy/arbitrage/markets.py`.
//!
//! Shape Gamma `/markets`:
//! - `conditionId`: str (canonical id)
//! - `slug`, `question`: str
//! - `clobTokenIds`: JSON-encoded array of 2 strings (token_id YES, NO)
//! - `outcomes`: JSON-encoded array of 2 strings ("Yes", "No")
//! - `endDate`: ISO timestamp string
//! - `closed`: bool, `active`: bool

use crate::types::Market;
use anyhow::{Context, Result};
use chrono::{DateTime, Duration, Utc};
use regex::Regex;
use reqwest::Client;
use serde::Deserialize;
use std::sync::OnceLock;
use tracing::{debug, info, warn};

static CRYPTO_RE: OnceLock<Regex> = OnceLock::new();
static HOURLY_RE: OnceLock<Regex> = OnceLock::new();

fn crypto_re() -> &'static Regex {
    CRYPTO_RE.get_or_init(|| {
        Regex::new(r"(?i)(bitcoin|btc|ethereum|eth|solana|sol|xrp|dogecoin|doge|bnb|hyperliquid|hype)")
            .expect("crypto regex compiles")
    })
}

fn hourly_re() -> &'static Regex {
    HOURLY_RE.get_or_init(|| {
        // [-\s]* permite "up or down", "up-or-down", "updown".
        Regex::new(r"(?i)(up[-\s]*or[-\s]*down|updown|above|below|reach|hourly|\b5m\b|\b15m\b|\b1h\b|15-min|5-min)")
            .expect("hourly regex compiles")
    })
}

#[derive(Debug, Deserialize)]
struct GammaMarketRaw {
    #[serde(rename = "conditionId")]
    condition_id: Option<String>,
    slug: Option<String>,
    question: Option<String>,
    #[serde(rename = "clobTokenIds")]
    clob_token_ids: Option<String>,
    #[allow(dead_code)]
    outcomes: Option<String>,
    #[serde(rename = "endDate")]
    end_date: Option<String>,
    #[serde(default)]
    closed: bool,
    #[serde(rename = "negRisk", default)]
    neg_risk: bool,
}

pub struct GammaClient {
    http: Client,
    base_url: String,
}

impl GammaClient {
    pub fn new(base_url: impl Into<String>) -> Result<Self> {
        let http = Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .user_agent("bot-polymarket-rust/0.1")
            .build()
            .context("failed to build reqwest client")?;
        Ok(Self {
            http,
            base_url: base_url.into(),
        })
    }

    /// Discovery: mercados crypto activos con cierre dentro del horizonte.
    pub async fn discover_hourly_crypto(
        &self,
        max_horizon_seconds: u64,
        limit: u32,
    ) -> Result<Vec<Market>> {
        let now = Utc::now();
        let end_max = now + Duration::seconds(max_horizon_seconds as i64);

        let url = format!("{}/markets", self.base_url);
        let params = [
            ("active", "true".to_string()),
            ("closed", "false".to_string()),
            ("limit", limit.to_string()),
            ("order", "endDate".to_string()),
            ("ascending", "true".to_string()),
            ("archived", "false".to_string()),
            ("end_date_min", now.to_rfc3339()),
            ("end_date_max", end_max.to_rfc3339()),
            ("liquidity_num_min", "50".to_string()),
        ];

        let resp = self
            .http
            .get(&url)
            .query(&params)
            .send()
            .await
            .context("gamma request failed")?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("gamma /markets HTTP {}: {}", status, &body[..body.len().min(500)]);
        }

        let raw: Vec<GammaMarketRaw> = resp.json().await.context("gamma response not JSON list")?;
        let candidates = raw.len();

        let mut matched = Vec::new();
        for entry in raw {
            if entry.closed {
                continue;
            }
            let Some(cid) = entry.condition_id else { continue };
            let Some(slug) = entry.slug.clone() else { continue };
            let Some(question) = entry.question.clone() else { continue };
            let Some(token_ids_str) = entry.clob_token_ids.as_ref() else { continue };

            // Parsear clobTokenIds (JSON-encoded array of 2 strings).
            let token_ids: Vec<String> = match serde_json::from_str(token_ids_str) {
                Ok(v) => v,
                Err(_) => continue,
            };
            if token_ids.len() != 2 {
                continue;
            }

            // end_date dentro del horizonte
            let end_date_dt: Option<DateTime<Utc>> = entry
                .end_date
                .as_deref()
                .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                .map(|d| d.with_timezone(&Utc));
            let Some(end_dt) = end_date_dt else { continue };
            let delta = (end_dt - now).num_seconds();
            if delta < 0 || delta as u64 > max_horizon_seconds {
                continue;
            }

            // Regex filters
            let text = format!("{} {}", slug.to_lowercase(), question.to_lowercase());
            if !crypto_re().is_match(&text) {
                continue;
            }
            if !hourly_re().is_match(&text) {
                continue;
            }

            matched.push(Market {
                condition_id: cid,
                slug,
                question,
                yes_token_id: token_ids[0].clone(),
                no_token_id: token_ids[1].clone(),
                end_date: Some(end_dt),
                neg_risk: entry.neg_risk,
            });
        }

        info!(
            candidates,
            matched = matched.len(),
            horizon_s = max_horizon_seconds,
            "arb_markets_discovered"
        );
        debug!(
            sample = ?matched.iter().take(3).map(|m| &m.slug).collect::<Vec<_>>(),
            "discovered sample"
        );
        Ok(matched)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crypto_re_matches() {
        assert!(crypto_re().is_match("bitcoin-up-or-down-on-may-12"));
        assert!(crypto_re().is_match("ethereum"));
        assert!(crypto_re().is_match("sol"));
        assert!(!crypto_re().is_match("trump-election"));
    }

    #[test]
    fn hourly_re_matches() {
        assert!(hourly_re().is_match("up-or-down"));
        assert!(hourly_re().is_match("above-79000"));
        assert!(hourly_re().is_match("updown"));
        assert!(hourly_re().is_match("hourly-bitcoin"));
        assert!(!hourly_re().is_match("trump-win-2026"));
    }
}
