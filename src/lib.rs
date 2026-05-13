//! Bot de arbitraje bilateral en Polymarket — port Rust.
//!
//! Objetivo: detectar `ask(YES) + ask(NO) + fees < 1` en mercados crypto cortos
//! (5m/15m/hourly) y persistir cada oportunidad. Modo DRY_RUN puro: no firma
//! ni envia ordenes. Validacion paralela contra el bot Python.

pub mod bilateral;
pub mod book_state;
pub mod cex_feed;
pub mod config;
pub mod fees;
pub mod gamma;
pub mod live;
pub mod live_runner;
pub mod poly_ws;
pub mod pricing_model;
pub mod recorder;
pub mod runner;
pub mod strategies;
pub mod tp_manager;
pub mod trinity_runner;
pub mod types;
