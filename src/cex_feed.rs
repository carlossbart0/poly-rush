//! Cliente WS al feed publico de Coinbase Exchange.
//!
//! Endpoint: wss://ws-feed.exchange.coinbase.com
//! Subscribe: {"type":"subscribe","product_ids":["BTC-USD","ETH-USD"],"channels":["ticker"]}
//! Recibe: {"type":"ticker","product_id":"BTC-USD","price":"...","time":"..."}
//!
//! Track last_price + rolling vol window por product. Sin auth.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::connect_async;
use tracing::{debug, info, warn};

const CEX_WS_URL: &str = "wss://ws-feed.exchange.coinbase.com";
const VOL_LOOKBACK_TICKS: usize = 60; // ~60 ticks de price changes
const RECV_TIMEOUT: Duration = Duration::from_secs(30);
const INITIAL_BACKOFF: Duration = Duration::from_secs(1);
const MAX_BACKOFF: Duration = Duration::from_secs(30);

#[derive(Debug, Serialize)]
struct SubscribePayload<'a> {
    #[serde(rename = "type")]
    type_: &'static str,
    product_ids: &'a [String],
    channels: [&'static str; 1],
}

#[derive(Debug, Deserialize)]
struct TickerMessage {
    #[serde(rename = "type")]
    type_: String,
    product_id: Option<String>,
    price: Option<String>,
    time: Option<String>,
}

#[derive(Debug, Clone)]
pub struct CexPrice {
    pub product_id: String,
    pub price: f64,
    pub time: DateTime<Utc>,
}

/// Snapshot por product: precio actual + log returns rolling para vol calc.
#[derive(Debug, Clone)]
pub struct CexState {
    pub product_id: String,
    pub last_price: Option<f64>,
    pub last_time: Option<DateTime<Utc>>,
    pub log_returns: VecDeque<f64>, // rolling window
    pub tick_dt_seconds: VecDeque<f64>, // delta-t entre ticks
}

impl CexState {
    pub fn new(product_id: impl Into<String>) -> Self {
        Self {
            product_id: product_id.into(),
            last_price: None,
            last_time: None,
            log_returns: VecDeque::with_capacity(VOL_LOOKBACK_TICKS + 1),
            tick_dt_seconds: VecDeque::with_capacity(VOL_LOOKBACK_TICKS + 1),
        }
    }

    /// Actualiza con nuevo tick. Calcula log return si hay tick previo.
    pub fn update(&mut self, price: f64, time: DateTime<Utc>) {
        if let (Some(last_p), Some(last_t)) = (self.last_price, self.last_time) {
            if last_p > 0.0 && price > 0.0 {
                let log_ret = (price / last_p).ln();
                let dt = (time - last_t).num_milliseconds() as f64 / 1000.0;
                if dt > 0.0 && dt < 300.0 {
                    self.log_returns.push_back(log_ret);
                    self.tick_dt_seconds.push_back(dt);
                    while self.log_returns.len() > VOL_LOOKBACK_TICKS {
                        self.log_returns.pop_front();
                        self.tick_dt_seconds.pop_front();
                    }
                }
            }
        }
        self.last_price = Some(price);
        self.last_time = Some(time);
    }

    /// Realized volatility anualizada (sqrt-time scaling).
    /// Si pocos ticks, retorna None (caller usa fallback).
    pub fn realized_vol_annual(&self) -> Option<f64> {
        if self.log_returns.len() < 10 || self.tick_dt_seconds.is_empty() {
            return None;
        }
        let n = self.log_returns.len() as f64;
        let mean: f64 = self.log_returns.iter().sum::<f64>() / n;
        let var: f64 = self
            .log_returns
            .iter()
            .map(|x| (x - mean).powi(2))
            .sum::<f64>()
            / (n - 1.0).max(1.0);
        let avg_dt: f64 = self.tick_dt_seconds.iter().sum::<f64>() / n;
        if avg_dt <= 0.0 {
            return None;
        }
        // sigma per tick → anualizar via sqrt(seconds_per_year / avg_dt)
        let seconds_per_year = 365.25 * 24.0 * 3600.0;
        let sigma_per_tick = var.sqrt();
        Some(sigma_per_tick * (seconds_per_year / avg_dt).sqrt())
    }
}

pub struct CexFeed {
    state: Arc<Mutex<std::collections::HashMap<String, CexState>>>,
    stop: Arc<tokio::sync::Notify>,
    products: Vec<String>,
}

impl CexFeed {
    pub fn new(products: Vec<String>) -> Self {
        let mut map = std::collections::HashMap::new();
        for p in &products {
            map.insert(p.clone(), CexState::new(p));
        }
        Self {
            state: Arc::new(Mutex::new(map)),
            stop: Arc::new(tokio::sync::Notify::new()),
            products,
        }
    }

    pub fn snapshot_arc(&self) -> Arc<Mutex<std::collections::HashMap<String, CexState>>> {
        self.state.clone()
    }

    pub fn stop(&self) {
        self.stop.notify_one();
    }

    pub async fn run(self: Arc<Self>) -> Result<()> {
        let mut backoff = INITIAL_BACKOFF;
        loop {
            tokio::select! {
                _ = self.stop.notified() => {
                    info!("cex_feed_stopping");
                    return Ok(());
                }
                res = self.connect_and_consume() => {
                    match res {
                        Ok(()) => {
                            backoff = INITIAL_BACKOFF;
                        }
                        Err(e) => {
                            warn!(error = %e, backoff_s = backoff.as_secs(), "cex_feed_error");
                            tokio::select! {
                                _ = self.stop.notified() => return Ok(()),
                                _ = tokio::time::sleep(backoff) => {}
                            }
                            backoff = (backoff * 2).min(MAX_BACKOFF);
                        }
                    }
                }
            }
        }
    }

    async fn connect_and_consume(&self) -> Result<()> {
        info!(url = CEX_WS_URL, products = ?self.products, "cex_feed_connecting");
        let (ws, _resp) = connect_async(CEX_WS_URL).await.context("cex ws connect")?;
        let (mut writer, mut reader) = ws.split();

        let payload = SubscribePayload {
            type_: "subscribe",
            product_ids: &self.products,
            channels: ["ticker"],
        };
        let body = serde_json::to_string(&payload)?;
        writer.send(Message::Text(body)).await.context("send sub")?;
        debug!("cex_feed_subscribed");

        loop {
            let msg = tokio::select! {
                _ = self.stop.notified() => return Ok(()),
                m = tokio::time::timeout(RECV_TIMEOUT, reader.next()) => m,
            };
            let msg = match msg {
                Err(_) => {
                    warn!("cex_feed_recv_timeout");
                    return Ok(());
                }
                Ok(None) => return Ok(()),
                Ok(Some(Ok(m))) => m,
                Ok(Some(Err(e))) => return Err(anyhow::anyhow!("cex ws err: {}", e)),
            };
            let text = match msg {
                Message::Text(t) => t,
                Message::Close(_) => return Ok(()),
                _ => continue,
            };
            let tm: TickerMessage = match serde_json::from_str(&text) {
                Ok(v) => v,
                Err(_) => continue,
            };
            if tm.type_ != "ticker" {
                continue;
            }
            let (Some(pid), Some(price_s), Some(time_s)) = (tm.product_id, tm.price, tm.time)
            else {
                continue;
            };
            let price: f64 = match price_s.parse() {
                Ok(v) => v,
                Err(_) => continue,
            };
            let time = match DateTime::parse_from_rfc3339(&time_s) {
                Ok(d) => d.with_timezone(&Utc),
                Err(_) => continue,
            };
            let mut state = self.state.lock().await;
            if let Some(st) = state.get_mut(&pid) {
                st.update(price, time);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn vol_calc_with_constant_price_is_zero() {
        let mut s = CexState::new("BTC-USD");
        let base = Utc.with_ymd_and_hms(2026, 5, 12, 0, 0, 0).unwrap();
        for i in 0..30 {
            s.update(63000.0, base + chrono::Duration::seconds(i));
        }
        let v = s.realized_vol_annual();
        if let Some(v) = v {
            assert!(v.abs() < 1e-9);
        }
    }

    #[test]
    fn vol_calc_with_synthetic_walk() {
        let mut s = CexState::new("BTC-USD");
        let base = Utc.with_ymd_and_hms(2026, 5, 12, 0, 0, 0).unwrap();
        // simular caminata con vol ~50% annual
        let prices = [
            63000.0, 63050.0, 63010.0, 63080.0, 63040.0, 63100.0, 63070.0, 63130.0, 63090.0,
            63150.0, 63110.0, 63170.0, 63130.0, 63190.0, 63150.0, 63210.0, 63170.0, 63230.0,
            63190.0, 63250.0,
        ];
        for (i, p) in prices.iter().enumerate() {
            s.update(*p, base + chrono::Duration::seconds(i as i64));
        }
        let v = s.realized_vol_annual().expect("should compute vol");
        assert!(v > 0.0 && v < 5.0); // sanity bounds
    }
}
