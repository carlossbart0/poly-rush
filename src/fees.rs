//! Fee math de Polymarket. Espejo exacto de `fees.py` del bot Python.
//!
//! Formula taker: `fee = C * (base_rate_bps/10000) * p * (1-p)`
//! Maximo en p=0.5. Crypto base_rate = 180 bps (1.80%).

use rust_decimal::Decimal;
use rust_decimal_macros::dec;

/// Fee rate efectivo como fraccion del notional al precio dado.
pub fn fee_rate_for_price(price: Decimal, base_rate_bps: u32) -> Decimal {
    if price <= Decimal::ZERO || price >= Decimal::ONE {
        return Decimal::ZERO;
    }
    let base = Decimal::from(base_rate_bps) / dec!(10000);
    base * price * (Decimal::ONE - price)
}

/// Fee en USDC por share (taker BUY) al precio dado.
pub fn effective_fee_per_share(price: Decimal, base_rate_bps: u32) -> Decimal {
    if price <= Decimal::ZERO || price >= Decimal::ONE {
        return Decimal::ZERO;
    }
    let rate = fee_rate_for_price(price, base_rate_bps);
    price * rate
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fee_rate_zero_at_extremes() {
        assert_eq!(fee_rate_for_price(Decimal::ZERO, 180), Decimal::ZERO);
        assert_eq!(fee_rate_for_price(Decimal::ONE, 180), Decimal::ZERO);
        assert_eq!(fee_rate_for_price(dec!(-0.1), 180), Decimal::ZERO);
        assert_eq!(fee_rate_for_price(dec!(1.5), 180), Decimal::ZERO);
    }

    #[test]
    fn fee_rate_max_at_half() {
        // base 180 bps, p=0.5 → 0.018 * 0.25 = 0.0045
        let r = fee_rate_for_price(dec!(0.5), 180);
        assert_eq!(r, dec!(0.00450));
    }

    #[test]
    fn fee_per_share_at_05() {
        // 0.5 * 0.0045 = 0.00225
        let f = effective_fee_per_share(dec!(0.5), 180);
        assert_eq!(f, dec!(0.002250));
    }

    #[test]
    fn fee_per_share_symmetry() {
        // p y 1-p deben dar mismo rate (fee/share difiere por el factor price)
        let a = fee_rate_for_price(dec!(0.3), 180);
        let b = fee_rate_for_price(dec!(0.7), 180);
        assert_eq!(a, b);
    }
}
