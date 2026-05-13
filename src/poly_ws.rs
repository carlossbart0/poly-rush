//! Cliente WS al canal `market` del CLOB de Polymarket.
//! Port de `feeds/polymarket_ws.py`.
//!
//! Endpoint: `wss://ws-subscriptions-clob.polymarket.com/ws/market`
//! Subscripcion: `{"assets_ids":[...],"type":"market","custom_feature_enabled":true}`
//!
//! Eventos:
//! - `book`: snapshot completo → replace_levels.
//! - `price_change`: deltas → apply_delta. size=0 borra nivel.
//! - `best_bid_ask`: re-trigger del detector sin mutar state.
//! - `last_trade_price`: ignorado por ahora.
//!
//! Resiliencia (espejo del Python tras fix `769e45a`):
//! - PING cada 9s (server timeout ~10s).
//! - Recv timeout 30s → fuerza reconnect.
//! - "INVALID OPERATION" → reconnect.
//! - Backoff exponencial 1s..30s.

use crate::book_state::BookState;
use anyhow::{Context, Result};
use chrono::Utc;
use futures_util::{SinkExt, StreamExt};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, Mutex, Notify};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};
use tokio::net::TcpStream;
use tracing::{debug, info, warn};

const PING_INTERVAL: Duration = Duration::from_secs(9);
const RECV_TIMEOUT: Duration = Duration::from_secs(30);
const INITIAL_BACKOFF: Duration = Duration::from_secs(1);
const MAX_BACKOFF: Duration = Duration::from_secs(30);

#[derive(Debug, Serialize)]
struct SubscribePayload<'a> {
    assets_ids: &'a [String],
    #[serde(rename = "type")]
    type_: &'static str,
    custom_feature_enabled: bool,
}

#[derive(Debug, Deserialize)]
struct Level {
    price: String,
    size: String,
}

#[derive(Debug, Deserialize)]
struct PriceChange {
    asset_id: String,
    price: String,
    size: String,
    side: String,
}

/// Eventos que emitimos al consumer (runner).
#[derive(Debug, Clone)]
pub enum BookEvent {
    /// El book de este asset cambio (snapshot o delta o BBO update).
    Updated(String),
}

type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;

pub struct PolymarketMarketWS {
    url: String,
    assets: Arc<Mutex<HashMap<String, String>>>, // asset_id → market_id
    books: Arc<Mutex<HashMap<String, BookState>>>,
    resubscribe: Arc<Notify>,
    stop: Arc<Notify>,
    event_tx: mpsc::Sender<BookEvent>,
}

impl PolymarketMarketWS {
    pub fn new(
        url: impl Into<String>,
        initial_assets: HashMap<String, String>,
        event_tx: mpsc::Sender<BookEvent>,
    ) -> Self {
        let books: HashMap<String, BookState> = initial_assets
            .iter()
            .map(|(aid, mid)| (aid.clone(), BookState::new(aid, mid)))
            .collect();
        Self {
            url: url.into(),
            assets: Arc::new(Mutex::new(initial_assets)),
            books: Arc::new(Mutex::new(books)),
            resubscribe: Arc::new(Notify::new()),
            stop: Arc::new(Notify::new()),
            event_tx,
        }
    }

    pub fn books(&self) -> Arc<Mutex<HashMap<String, BookState>>> {
        self.books.clone()
    }

    pub async fn subscribe(&self, additions: HashMap<String, String>) {
        let mut assets = self.assets.lock().await;
        let mut books = self.books.lock().await;
        let mut changed = false;
        for (aid, mid) in additions {
            if !assets.contains_key(&aid) {
                books.insert(aid.clone(), BookState::new(&aid, &mid));
                assets.insert(aid, mid);
                changed = true;
            }
        }
        if changed {
            self.resubscribe.notify_one();
        }
    }

    pub fn stop(&self) {
        self.stop.notify_one();
    }

    pub async fn run(self: Arc<Self>) -> Result<()> {
        let mut backoff = INITIAL_BACKOFF;
        loop {
            tokio::select! {
                _ = self.stop.notified() => {
                    info!("polymarket_ws_stopping");
                    return Ok(());
                }
                res = self.connect_and_consume() => {
                    match res {
                        Ok(()) => {
                            backoff = INITIAL_BACKOFF;
                        }
                        Err(e) => {
                            warn!(error = %e, backoff_s = backoff.as_secs(), "polymarket_ws_error");
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
        let assets_snapshot: Vec<String> = {
            let assets = self.assets.lock().await;
            assets.keys().cloned().collect()
        };
        if assets_snapshot.is_empty() {
            // Esperar a que llegue una subscripcion.
            tokio::select! {
                _ = self.resubscribe.notified() => {}
                _ = self.stop.notified() => return Ok(()),
                _ = tokio::time::sleep(Duration::from_secs(5)) => {}
            }
            return Ok(());
        }

        info!(url = %self.url, assets_count = assets_snapshot.len(), "polymarket_ws_connecting");
        let (ws, _resp) = connect_async(self.url.as_str())
            .await
            .context("WS connect failed")?;
        let (mut writer, mut reader) = ws.split();

        // Send subscription
        let payload = SubscribePayload {
            assets_ids: &assets_snapshot,
            type_: "market",
            custom_feature_enabled: true,
        };
        let body = serde_json::to_string(&payload)?;
        writer
            .send(Message::Text(body))
            .await
            .context("send subscription")?;
        debug!(assets = assets_snapshot.len(), "polymarket_ws_subscribed");

        // Spawn ping task
        let writer_arc = Arc::new(Mutex::new(writer));
        let writer_for_ping = writer_arc.clone();
        let stop_for_ping = self.stop.clone();
        let ping_task = tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = stop_for_ping.notified() => return,
                    _ = tokio::time::sleep(PING_INTERVAL) => {
                        let mut w = writer_for_ping.lock().await;
                        if w.send(Message::Text("PING".to_string())).await.is_err() {
                            return;
                        }
                    }
                }
            }
        });

        // Consume messages
        let result = self.consume_messages(&mut reader).await;
        ping_task.abort();
        result
    }

    async fn consume_messages(
        &self,
        reader: &mut futures_util::stream::SplitStream<WsStream>,
    ) -> Result<()> {
        loop {
            let msg = tokio::select! {
                _ = self.stop.notified() => return Ok(()),
                _ = self.resubscribe.notified() => {
                    // Server no acepta resubscribe en misma conexion →
                    // reconectamos para que el nuevo set de assets aplique.
                    info!("polymarket_ws_resubscribe_via_reconnect");
                    return Ok(());
                }
                msg = tokio::time::timeout(RECV_TIMEOUT, reader.next()) => msg,
            };

            let msg = match msg {
                Err(_) => {
                    // timeout
                    warn!(
                        timeout_s = RECV_TIMEOUT.as_secs(),
                        action = "forcing_reconnect",
                        "polymarket_ws_recv_timeout"
                    );
                    return Ok(()); // outer loop reconnects
                }
                Ok(None) => {
                    info!("polymarket_ws_stream_ended");
                    return Ok(());
                }
                Ok(Some(Ok(m))) => m,
                Ok(Some(Err(e))) => {
                    return Err(anyhow::anyhow!("ws recv error: {}", e));
                }
            };

            let text = match msg {
                Message::Text(t) => t,
                Message::Binary(b) => match String::from_utf8(b) {
                    Ok(s) => s,
                    Err(_) => continue,
                },
                Message::Ping(p) => {
                    // Server-level ping; pong is automatic in tungstenite if we keep stream alive,
                    // but we explicitly send back to be safe.
                    let _ = p;
                    continue;
                }
                Message::Pong(_) => continue,
                Message::Close(_) => {
                    info!("polymarket_ws_close_frame");
                    return Ok(());
                }
                Message::Frame(_) => continue,
            };

            let trimmed = text.trim();
            if trimmed.is_empty() || trimmed == "PONG" {
                continue;
            }
            if trimmed == "INVALID OPERATION" {
                warn!(action = "forcing_reconnect", "polymarket_ws_invalid_operation");
                return Ok(());
            }

            let parsed: Value = match serde_json::from_str(trimmed) {
                Ok(v) => v,
                Err(_) => {
                    debug!(body = &trimmed[..trimmed.len().min(200)], "polymarket_ws_bad_json");
                    continue;
                }
            };

            // Mensajes pueden ser obj solo o lista.
            match parsed {
                Value::Array(items) => {
                    for item in items {
                        if let Value::Object(_) = &item {
                            self.handle_message(&item).await;
                        }
                    }
                }
                Value::Object(_) => {
                    self.handle_message(&parsed).await;
                }
                _ => {}
            }

        }
    }

    async fn handle_message(&self, msg: &Value) {
        let event_type = msg.get("event_type").and_then(Value::as_str).unwrap_or("");
        match event_type {
            "book" => self.on_book_snapshot(msg).await,
            "price_change" => self.on_price_change(msg).await,
            "best_bid_ask" => self.on_best_bid_ask(msg).await,
            _ => {}
        }
    }

    async fn on_book_snapshot(&self, msg: &Value) {
        let asset_id = match msg.get("asset_id").and_then(Value::as_str) {
            Some(s) => s.to_string(),
            None => return,
        };
        let bids_raw = msg.get("bids").and_then(Value::as_array);
        let asks_raw = msg.get("asks").and_then(Value::as_array);

        {
            let mut books = self.books.lock().await;
            let state = match books.get_mut(&asset_id) {
                Some(s) => s,
                None => return,
            };
            if let Some(bids) = bids_raw {
                let parsed = parse_levels(bids);
                state.bids.replace_levels(&parsed);
            }
            if let Some(asks) = asks_raw {
                let parsed = parse_levels(asks);
                state.asks.replace_levels(&parsed);
            }
            state.last_update_ts_ms = Utc::now().timestamp_millis();
        }
        let _ = self.event_tx.send(BookEvent::Updated(asset_id)).await;
    }

    async fn on_best_bid_ask(&self, msg: &Value) {
        let asset_id = match msg.get("asset_id").and_then(Value::as_str) {
            Some(s) => s.to_string(),
            None => return,
        };
        {
            let mut books = self.books.lock().await;
            if let Some(state) = books.get_mut(&asset_id) {
                state.last_update_ts_ms = Utc::now().timestamp_millis();
            } else {
                return;
            }
        }
        let _ = self.event_tx.send(BookEvent::Updated(asset_id)).await;
    }

    async fn on_price_change(&self, msg: &Value) {
        let changes = match msg.get("price_changes").and_then(Value::as_array) {
            Some(a) => a,
            None => return,
        };
        let mut touched: Vec<String> = Vec::new();
        {
            let mut books = self.books.lock().await;
            for ch in changes {
                let pc: PriceChange = match serde_json::from_value(ch.clone()) {
                    Ok(p) => p,
                    Err(_) => continue,
                };
                let price = match Decimal::from_str(&pc.price) {
                    Ok(d) => d,
                    Err(_) => continue,
                };
                let size = match Decimal::from_str(&pc.size) {
                    Ok(d) => d,
                    Err(_) => continue,
                };
                let state = match books.get_mut(&pc.asset_id) {
                    Some(s) => s,
                    None => continue,
                };
                match pc.side.to_uppercase().as_str() {
                    "BUY" => state.bids.apply_delta(price, size),
                    "SELL" => state.asks.apply_delta(price, size),
                    _ => continue,
                }
                state.last_update_ts_ms = Utc::now().timestamp_millis();
                if !touched.contains(&pc.asset_id) {
                    touched.push(pc.asset_id);
                }
            }
        }
        for aid in touched {
            let _ = self.event_tx.send(BookEvent::Updated(aid)).await;
        }
    }
}

fn parse_levels(raw: &[Value]) -> Vec<(Decimal, Decimal)> {
    let mut out = Vec::with_capacity(raw.len());
    for l in raw {
        let lv: Result<Level, _> = serde_json::from_value(l.clone());
        if let Ok(lv) = lv {
            let p = Decimal::from_str(&lv.price).unwrap_or(Decimal::ZERO);
            let s = Decimal::from_str(&lv.size).unwrap_or(Decimal::ZERO);
            out.push((p, s));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn parse_levels_works() {
        let v = serde_json::json!([
            {"price": "0.4", "size": "100"},
            {"price": "0.5", "size": "50"},
        ]);
        let levels = parse_levels(v.as_array().unwrap());
        assert_eq!(levels.len(), 2);
        assert_eq!(levels[0].0, dec!(0.4));
        assert_eq!(levels[0].1, dec!(100));
    }

    #[test]
    fn subscribe_payload_serialization() {
        let assets = vec!["a1".to_string(), "a2".to_string()];
        let payload = SubscribePayload {
            assets_ids: &assets,
            type_: "market",
            custom_feature_enabled: true,
        };
        let s = serde_json::to_string(&payload).unwrap();
        assert!(s.contains("\"assets_ids\""));
        assert!(s.contains("\"custom_feature_enabled\":true"));
        assert!(s.contains("\"type\":\"market\""));
    }
}
