use rust_decimal::Decimal;

mod alerts;
mod collector_targets;
mod settlements;
mod trades;

pub(crate) use alerts::ensure_clob_trade_alerts_table;
pub(crate) use collector_targets::spawn_polymarket_trade_persistence_from_collector_targets;
pub(crate) use settlements::spawn_pm_token_settlement_persistence;
pub(crate) use trades::spawn_polymarket_trade_persistence;

fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn env_i64(name: &str, default: i64) -> i64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn env_decimal(name: &str, default: Decimal) -> Decimal {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}
