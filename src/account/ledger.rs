use rust_decimal::Decimal;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountLedgerSnapshot {
    pub realized_notional_usd: Decimal,
    pub pending_claim_notional_usd: Decimal,
}

impl Default for AccountLedgerSnapshot {
    fn default() -> Self {
        Self {
            realized_notional_usd: Decimal::ZERO,
            pending_claim_notional_usd: Decimal::ZERO,
        }
    }
}
