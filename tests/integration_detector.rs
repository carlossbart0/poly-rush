//! Integration test: alimenta secuencia de eventos book/price_change al
//! detector bilateral y verifica que se detecta exactamente cuando debe.
//!
//! Esto valida el flow end-to-end (BookState mutation + detector evaluation +
//! recorder insert) SIN necesidad de internet ni Polymarket WS real.

use bot_polymarket_rust::{
    bilateral::{BilateralArbStrategy, BilateralConfig},
    book_state::BookState,
    recorder::Recorder,
    types::Market,
};
use chrono::Utc;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::collections::HashMap;

fn mk_market(cid: &str, a: &str, b: &str) -> Market {
    Market {
        condition_id: cid.to_string(),
        slug: "btc-up-or-down-5m".to_string(),
        question: "BTC up or down".to_string(),
        yes_token_id: a.to_string(),
        no_token_id: b.to_string(),
        end_date: Some(Utc::now()),
        neg_risk: false,
    }
}

#[test]
fn detector_finds_opp_after_book_update() {
    let mut strat = BilateralArbStrategy::new(BilateralConfig {
        fee_rate_bps: 180,
        min_edge_per_unit: dec!(0.002),
        slippage_buffer_bps: 20,
        max_shares: dec!(1000),
    });
    strat.register_market(mk_market("0xMKT", "A", "B"));

    let mut books = HashMap::new();
    let mut ba = BookState::new("A", "0xMKT");
    let mut bb = BookState::new("B", "0xMKT");

    // Sin asks → no opp
    books.insert("A".to_string(), ba.clone());
    books.insert("B".to_string(), bb.clone());
    assert!(strat.evaluate_market("0xMKT", &books).is_none());

    // Asks suman > 1 → no opp
    ba.asks.replace_levels(&[(dec!(0.55), dec!(100))]);
    bb.asks.replace_levels(&[(dec!(0.50), dec!(100))]);
    books.insert("A".to_string(), ba.clone());
    books.insert("B".to_string(), bb.clone());
    assert!(strat.evaluate_market("0xMKT", &books).is_none());

    // price_change: SELL side asks bajan → arb opp
    ba.asks.apply_delta(dec!(0.55), Decimal::ZERO); // remove
    ba.asks.apply_delta(dec!(0.40), dec!(50));
    bb.asks.apply_delta(dec!(0.50), Decimal::ZERO);
    bb.asks.apply_delta(dec!(0.45), dec!(80));
    books.insert("A".to_string(), ba);
    books.insert("B".to_string(), bb);
    let opp = strat
        .evaluate_market("0xMKT", &books)
        .expect("expected arb opp after asks dropped");
    assert!(opp.edge_per_unit > dec!(0.10));
    assert_eq!(opp.legs.len(), 2);
    assert_eq!(opp.legs[0].size_shares, dec!(50)); // min(50,80,1000)
}

#[test]
fn recorder_accumulates_pnl_across_events() {
    let path = std::env::temp_dir().join(format!(
        "rust_bot_e2e_{}.jsonl",
        Utc::now().timestamp_micros()
    ));
    let _ = std::fs::remove_file(&path);

    let rec = Recorder::open(&path).unwrap();

    let mut strat = BilateralArbStrategy::new(BilateralConfig {
        fee_rate_bps: 180,
        min_edge_per_unit: dec!(0.002),
        slippage_buffer_bps: 20,
        max_shares: dec!(100),
    });
    strat.register_market(mk_market("0xMKT", "A", "B"));

    // Simular 20 eventos sucesivos donde el book entra/sale del estado arb.
    let mut books = HashMap::new();
    let mut total_inserts = 0;
    for i in 0..20 {
        let mut ba = BookState::new("A", "0xMKT");
        let mut bb = BookState::new("B", "0xMKT");
        // Alternar entre arb y no arb.
        if i % 2 == 0 {
            ba.asks.replace_levels(&[(dec!(0.40), dec!(50))]);
            bb.asks.replace_levels(&[(dec!(0.50), dec!(50))]);
        } else {
            ba.asks.replace_levels(&[(dec!(0.55), dec!(100))]);
            bb.asks.replace_levels(&[(dec!(0.50), dec!(100))]);
        }
        books.insert("A".to_string(), ba);
        books.insert("B".to_string(), bb);
        if let Some(opp) = strat.evaluate_market("0xMKT", &books) {
            rec.insert(&opp).unwrap();
            total_inserts += 1;
        }
    }
    assert_eq!(total_inserts, 10);
    let stats = rec.stats().unwrap();
    assert_eq!(stats.count, 10);
    // 10 inserts × 50 shares × ~0.094 edge = ~$47
    assert!(stats.total_pnl > 40.0 && stats.total_pnl < 55.0);
    let _ = std::fs::remove_file(&path);
}
