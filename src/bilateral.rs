//! Estrategia bilateral: arbitraje intra-Polymarket en mercados binarios.
//! Port de `strategies/bilateral.py`.
//!
//! Si `best_ask(YES) + best_ask(NO) + fees < 1` → comprar 1 de cada
//! garantiza $1 de payout (exactamente 1 token resuelve a $1).

use crate::book_state::BookState;
use crate::fees::effective_fee_per_share;
use crate::types::{ArbLeg, ArbOpportunity, Market, Side};
use chrono::Utc;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::collections::HashMap;

pub struct BilateralConfig {
    pub fee_rate_bps: u32,
    pub min_edge_per_unit: Decimal,
    pub slippage_buffer_bps: u32,
    pub max_shares: Decimal,
}

pub fn detect_bilateral(
    market: &Market,
    book_a: &BookState,
    book_b: &BookState,
    cfg: &BilateralConfig,
) -> Option<ArbOpportunity> {
    let (price_a, size_a) = book_a.best_ask()?;
    let (price_b, size_b) = book_b.best_ask()?;
    if price_a <= Decimal::ZERO
        || price_b <= Decimal::ZERO
        || price_a >= Decimal::ONE
        || price_b >= Decimal::ONE
    {
        return None;
    }
    let fee_a = effective_fee_per_share(price_a, cfg.fee_rate_bps);
    let fee_b = effective_fee_per_share(price_b, cfg.fee_rate_bps);
    let cost_per_unit = price_a + price_b + fee_a + fee_b;
    let edge_per_unit = Decimal::ONE - cost_per_unit;
    let slippage = Decimal::from(cfg.slippage_buffer_bps) / dec!(10000);
    let edge_after_buffer = edge_per_unit - slippage;
    if edge_after_buffer < cfg.min_edge_per_unit {
        return None;
    }
    let shares = size_a.min(size_b).min(cfg.max_shares);
    if shares <= Decimal::ZERO {
        return None;
    }
    let notional = (price_a + price_b) * shares;
    let expected_pnl = edge_after_buffer * shares;
    Some(ArbOpportunity {
        strategy: "bilateral".to_string(),
        market_id: market.condition_id.clone(),
        detected_at: Utc::now(),
        legs: vec![
            ArbLeg {
                asset_id: book_a.asset_id.clone(),
                side: Side::Buy,
                price: price_a,
                size_shares: shares,
            },
            ArbLeg {
                asset_id: book_b.asset_id.clone(),
                side: Side::Buy,
                price: price_b,
                size_shares: shares,
            },
        ],
        edge_per_unit: edge_after_buffer,
        notional_usdc: notional,
        expected_pnl_usdc: expected_pnl,
        sum_ask: price_a + price_b,
    })
}

/// Wrapper stateful: indexa markets y resuelve un asset_id → market_id.
pub struct BilateralArbStrategy {
    cfg: BilateralConfig,
    asset_to_market: HashMap<String, String>,
    markets: HashMap<String, Market>,
    market_pairs: HashMap<String, (String, String)>,
}

impl BilateralArbStrategy {
    pub fn new(cfg: BilateralConfig) -> Self {
        Self {
            cfg,
            asset_to_market: HashMap::new(),
            markets: HashMap::new(),
            market_pairs: HashMap::new(),
        }
    }

    pub fn register_market(&mut self, market: Market) {
        let a = market.yes_token_id.clone();
        let b = market.no_token_id.clone();
        let cid = market.condition_id.clone();
        self.asset_to_market.insert(a.clone(), cid.clone());
        self.asset_to_market.insert(b.clone(), cid.clone());
        self.market_pairs.insert(cid.clone(), (a, b));
        self.markets.insert(cid, market);
    }

    pub fn unregister_market(&mut self, condition_id: &str) {
        if let Some((a, b)) = self.market_pairs.remove(condition_id) {
            self.asset_to_market.remove(&a);
            self.asset_to_market.remove(&b);
        }
        self.markets.remove(condition_id);
    }

    pub fn market_id_for_asset(&self, asset_id: &str) -> Option<&str> {
        self.asset_to_market.get(asset_id).map(String::as_str)
    }

    pub fn evaluate_market(
        &self,
        market_id: &str,
        books: &HashMap<String, BookState>,
    ) -> Option<ArbOpportunity> {
        let market = self.markets.get(market_id)?;
        let (a, b) = self.market_pairs.get(market_id)?;
        let book_a = books.get(a)?;
        let book_b = books.get(b)?;
        detect_bilateral(market, book_a, book_b, &self.cfg)
    }

    pub fn tracked_assets(&self) -> HashMap<String, String> {
        self.asset_to_market.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn mk_market() -> Market {
        Market {
            condition_id: "0xMKT".to_string(),
            slug: "btc-up-or-down-5m".to_string(),
            question: "BTC up or down".to_string(),
            yes_token_id: "TOKEN_A".to_string(),
            no_token_id: "TOKEN_B".to_string(),
            end_date: Some(Utc::now()),
            neg_risk: false,
        }
    }

    fn book(id: &str, mid: &str, ask_price: Decimal, ask_size: Decimal) -> BookState {
        let mut b = BookState::new(id, mid);
        b.asks.replace_levels(&[(ask_price, ask_size)]);
        b
    }

    #[test]
    fn no_edge_when_sum_exceeds_one() {
        let m = mk_market();
        let ba = book("TOKEN_A", "0xMKT", dec!(0.55), dec!(100));
        let bb = book("TOKEN_B", "0xMKT", dec!(0.50), dec!(100));
        let cfg = BilateralConfig {
            fee_rate_bps: 180,
            min_edge_per_unit: dec!(0.002),
            slippage_buffer_bps: 20,
            max_shares: dec!(1000),
        };
        assert!(detect_bilateral(&m, &ba, &bb, &cfg).is_none());
    }

    #[test]
    fn detects_clear_edge() {
        let m = mk_market();
        // 0.40 + 0.50 = 0.90. fees @0.4 = 0.4*0.018*0.4*0.6 = 0.001728
        //                       fees @0.5 = 0.5*0.018*0.5*0.5 = 0.00225
        // cost = 0.90 + 0.001728 + 0.00225 = 0.903978
        // edge_gross = 0.096022; slippage 20 bps = 0.002; edge_net ≈ 0.094
        let ba = book("TOKEN_A", "0xMKT", dec!(0.40), dec!(100));
        let bb = book("TOKEN_B", "0xMKT", dec!(0.50), dec!(50));
        let cfg = BilateralConfig {
            fee_rate_bps: 180,
            min_edge_per_unit: dec!(0.002),
            slippage_buffer_bps: 20,
            max_shares: dec!(1000),
        };
        let opp = detect_bilateral(&m, &ba, &bb, &cfg).expect("expected opp");
        assert_eq!(opp.legs.len(), 2);
        // shares = min(100, 50, 1000) = 50
        assert_eq!(opp.legs[0].size_shares, dec!(50));
        assert_eq!(opp.legs[1].size_shares, dec!(50));
        assert!(opp.edge_per_unit > dec!(0.09));
        assert!(opp.expected_pnl_usdc > dec!(4.5)); // 0.09 * 50 = 4.5
    }

    #[test]
    fn no_opp_when_size_zero_after_cap() {
        let m = mk_market();
        let ba = book("TOKEN_A", "0xMKT", dec!(0.40), dec!(0));
        let bb = book("TOKEN_B", "0xMKT", dec!(0.50), dec!(50));
        let cfg = BilateralConfig {
            fee_rate_bps: 180,
            min_edge_per_unit: dec!(0.002),
            slippage_buffer_bps: 20,
            max_shares: dec!(1000),
        };
        // size_a vacio → BookState.best_ask() retorna None tras replace
        assert!(detect_bilateral(&m, &ba, &bb, &cfg).is_none());
    }

    #[test]
    fn shares_capped_by_max_shares() {
        let m = mk_market();
        let ba = book("TOKEN_A", "0xMKT", dec!(0.40), dec!(1000));
        let bb = book("TOKEN_B", "0xMKT", dec!(0.50), dec!(1000));
        let cfg = BilateralConfig {
            fee_rate_bps: 180,
            min_edge_per_unit: dec!(0.002),
            slippage_buffer_bps: 20,
            max_shares: dec!(40), // cap
        };
        let opp = detect_bilateral(&m, &ba, &bb, &cfg).expect("expected opp");
        assert_eq!(opp.legs[0].size_shares, dec!(40));
    }

    #[test]
    fn strategy_indexing_and_evaluation() {
        let mut s = BilateralArbStrategy::new(BilateralConfig {
            fee_rate_bps: 180,
            min_edge_per_unit: dec!(0.002),
            slippage_buffer_bps: 20,
            max_shares: dec!(1000),
        });
        s.register_market(mk_market());
        assert_eq!(s.market_id_for_asset("TOKEN_A"), Some("0xMKT"));
        assert_eq!(s.market_id_for_asset("TOKEN_B"), Some("0xMKT"));
        assert_eq!(s.market_id_for_asset("OTHER"), None);
        let mut books = HashMap::new();
        books.insert("TOKEN_A".to_string(), book("TOKEN_A", "0xMKT", dec!(0.40), dec!(100)));
        books.insert("TOKEN_B".to_string(), book("TOKEN_B", "0xMKT", dec!(0.50), dec!(50)));
        let opp = s.evaluate_market("0xMKT", &books).expect("opp");
        assert_eq!(opp.market_id, "0xMKT");
    }
}
