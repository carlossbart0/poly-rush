//! Estado mutable del orderbook por asset_id. Espejo de `book_state.py`.
//!
//! Reglas:
//! - `book` (snapshot): reemplaza levels completo.
//! - `price_change` con `size == 0`: borra el nivel.
//! - `price_change` con `size > 0`: upserta el nivel.
//!
//! No thread-safe; updates desde un solo task tokio.

use rust_decimal::Decimal;
use std::collections::BTreeMap;

/// Un lado del book (bids o asks). BTreeMap mantiene orden por precio.
#[derive(Debug, Default, Clone)]
pub struct BookSide {
    pub levels: BTreeMap<Decimal, Decimal>,
}

impl BookSide {
    pub fn new() -> Self {
        Self::default()
    }

    /// Reemplaza completo desde un snapshot.
    pub fn replace_levels(&mut self, raw: &[(Decimal, Decimal)]) {
        let mut new_levels = BTreeMap::new();
        for (price, size) in raw {
            if *price <= Decimal::ZERO || *size <= Decimal::ZERO {
                continue;
            }
            new_levels.insert(*price, *size);
        }
        self.levels = new_levels;
    }

    /// Aplica un delta de price_change.
    pub fn apply_delta(&mut self, price: Decimal, size: Decimal) {
        if size <= Decimal::ZERO {
            self.levels.remove(&price);
        } else {
            self.levels.insert(price, size);
        }
    }

    /// Mejor precio: `top=true` para bids (max), `top=false` para asks (min).
    pub fn best_price(&self, top: bool) -> Option<Decimal> {
        if top {
            self.levels.keys().next_back().copied()
        } else {
            self.levels.keys().next().copied()
        }
    }

    pub fn size_at(&self, price: &Decimal) -> Decimal {
        self.levels.get(price).copied().unwrap_or(Decimal::ZERO)
    }
}

/// Snapshot del book de un asset.
#[derive(Debug, Clone)]
pub struct BookState {
    pub asset_id: String,
    pub market_id: String,
    pub bids: BookSide,
    pub asks: BookSide,
    pub last_update_ts_ms: i64,
}

impl BookState {
    pub fn new(asset_id: impl Into<String>, market_id: impl Into<String>) -> Self {
        Self {
            asset_id: asset_id.into(),
            market_id: market_id.into(),
            bids: BookSide::new(),
            asks: BookSide::new(),
            last_update_ts_ms: 0,
        }
    }

    pub fn best_bid(&self) -> Option<(Decimal, Decimal)> {
        let price = self.bids.best_price(true)?;
        Some((price, self.bids.size_at(&price)))
    }

    pub fn best_ask(&self) -> Option<(Decimal, Decimal)> {
        let price = self.asks.best_price(false)?;
        Some((price, self.asks.size_at(&price)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn replace_then_query() {
        let mut s = BookSide::new();
        s.replace_levels(&[(dec!(0.4), dec!(100)), (dec!(0.3), dec!(50))]);
        // Asks → best_price(top=false) = min = 0.3
        assert_eq!(s.best_price(false), Some(dec!(0.3)));
        // Bids → best_price(top=true) = max = 0.4
        assert_eq!(s.best_price(true), Some(dec!(0.4)));
        assert_eq!(s.size_at(&dec!(0.4)), dec!(100));
    }

    #[test]
    fn delta_removes_at_zero() {
        let mut s = BookSide::new();
        s.replace_levels(&[(dec!(0.4), dec!(100))]);
        s.apply_delta(dec!(0.4), Decimal::ZERO);
        assert_eq!(s.best_price(false), None);
    }

    #[test]
    fn delta_upserts() {
        let mut s = BookSide::new();
        s.apply_delta(dec!(0.5), dec!(200));
        assert_eq!(s.size_at(&dec!(0.5)), dec!(200));
        s.apply_delta(dec!(0.5), dec!(300));
        assert_eq!(s.size_at(&dec!(0.5)), dec!(300));
    }

    #[test]
    fn book_state_best_bid_ask() {
        let mut b = BookState::new("a1", "m1");
        b.bids.replace_levels(&[(dec!(0.40), dec!(10)), (dec!(0.41), dec!(5))]);
        b.asks.replace_levels(&[(dec!(0.55), dec!(20)), (dec!(0.56), dec!(8))]);
        assert_eq!(b.best_bid(), Some((dec!(0.41), dec!(5))));
        assert_eq!(b.best_ask(), Some((dec!(0.55), dec!(20))));
    }

    #[test]
    fn rejects_invalid_levels() {
        let mut s = BookSide::new();
        s.replace_levels(&[
            (dec!(0.4), dec!(100)),
            (dec!(0), dec!(50)),    // precio 0 invalido
            (dec!(0.5), dec!(0)),   // size 0 invalido
            (dec!(-0.1), dec!(10)), // precio negativo invalido
        ]);
        assert_eq!(s.levels.len(), 1);
        assert_eq!(s.size_at(&dec!(0.4)), dec!(100));
    }
}
