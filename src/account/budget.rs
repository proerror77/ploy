use rust_decimal::Decimal;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountBudgetSnapshot {
    pub available_notional_usd: Decimal,
    pub reserved_notional_usd: Decimal,
}

impl Default for AccountBudgetSnapshot {
    fn default() -> Self {
        Self {
            available_notional_usd: Decimal::ZERO,
            reserved_notional_usd: Decimal::ZERO,
        }
    }
}
