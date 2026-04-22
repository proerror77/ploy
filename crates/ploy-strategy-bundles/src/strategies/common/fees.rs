#[must_use]
pub fn crypto_fee_cost(entry_price: f64) -> f64 {
    0.02 * entry_price * (1.0 - entry_price)
}
