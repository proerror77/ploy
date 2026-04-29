use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DryRunSummary {
    pub total_trades: usize,
    pub closed_trades: usize,
    pub wins: usize,
    pub losses: usize,
    pub win_rate_pct: f64,
    pub realized_pnl: f64,
    pub total_fees: f64,
    pub open_positions: usize,
    pub open_exposure: f64,
    pub latest_opened_at: Option<String>,
    pub latest_closed_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum NumberOrText {
    Number(f64),
    Text(String),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DryRunMetrics {
    pub sharpe: Option<f64>,
    pub profit_factor: Option<NumberOrText>,
    pub max_drawdown: f64,
    pub avg_trade: Option<f64>,
    pub gross_profit: f64,
    pub gross_loss: f64,
    pub equity_points: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DryRunEquityPoint {
    pub index: usize,
    pub label: String,
    pub timestamp: Option<String>,
    pub symbol: Option<String>,
    pub pnl: f64,
    pub cumulative: f64,
    pub drawdown: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DryRunWindowRow {
    pub window_secs: Option<i64>,
    pub window_label: String,
    pub total_trades: usize,
    pub closed_trades: usize,
    pub wins: usize,
    pub losses: usize,
    pub win_rate_pct: f64,
    pub realized_pnl: f64,
    pub avg_pnl: Option<f64>,
    pub avg_entry: Option<f64>,
    pub min_entry_ttr_secs: Option<i64>,
    pub max_entry_ttr_secs: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DryRunDailyRow {
    pub trading_day_cst: Option<String>,
    pub trade_count: usize,
    pub closed_trade_count: usize,
    pub wins: usize,
    pub losses: usize,
    pub confirmed_trade_count: usize,
    pub net_pnl: f64,
    pub confirmed_pnl: f64,
    pub fees: f64,
    pub open_quantity: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DryRunDailyWindowRow {
    pub trading_day_cst: String,
    pub window_secs: Option<i64>,
    pub window_label: String,
    pub trade_count: usize,
    pub closed_trade_count: usize,
    pub wins: usize,
    pub losses: usize,
    pub net_pnl: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DryRunSymbolRow {
    pub symbol: String,
    pub trades: usize,
    pub wins: usize,
    pub losses: usize,
    pub net_pnl: f64,
    pub avg_entry: Option<f64>,
    pub window_secs: Option<i64>,
    pub window_label: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DryRunClosedTradeRow {
    pub runtime_mode: Option<String>,
    pub strategy_id: Option<String>,
    pub deployment_id: Option<String>,
    pub trade_key: Option<String>,
    pub event_id: Option<String>,
    pub symbol: Option<String>,
    pub window_secs: Option<i64>,
    pub window_label: String,
    pub market_side: Option<String>,
    pub entry_price: Option<f64>,
    pub exit_price: Option<f64>,
    pub exit_type: String,
    pub quantity: f64,
    pub notional: f64,
    pub net_pnl: f64,
    pub entry_time_remaining_secs: Option<i64>,
    pub opened_at: Option<String>,
    pub closed_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DryRunOpenPositionRow {
    pub runtime_mode: Option<String>,
    pub strategy_id: Option<String>,
    pub deployment_id: Option<String>,
    pub trade_key: Option<String>,
    pub event_id: Option<String>,
    pub symbol: Option<String>,
    pub window_secs: Option<i64>,
    pub window_label: String,
    pub market_side: Option<String>,
    pub entry_price: Option<f64>,
    pub quantity: f64,
    pub notional: f64,
    pub entry_time_remaining_secs: Option<i64>,
    pub opened_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DryRunPairingReport {
    pub pair_key: String,
    pub mixed_event_groups: usize,
    pub fills_in_mixed_event_groups: usize,
    pub current_view_rows: usize,
    pub side_aware_rows: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DryRunStrategyReport {
    pub runtime_mode: String,
    pub strategy_id: String,
    pub deployment_id: String,
    pub label: String,
    pub summary: DryRunSummary,
    pub metrics: DryRunMetrics,
    pub equity_curve: Vec<DryRunEquityPoint>,
    pub by_window: Vec<DryRunWindowRow>,
    pub daily: Vec<DryRunDailyRow>,
    pub daily_by_window: Vec<DryRunDailyWindowRow>,
    pub symbols: Vec<DryRunSymbolRow>,
    pub symbols_by_window: Vec<DryRunSymbolRow>,
    pub closed_trades: Vec<DryRunClosedTradeRow>,
    pub recent_closed: Vec<DryRunClosedTradeRow>,
    pub open_positions: Vec<DryRunOpenPositionRow>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DryRunPerformanceReport {
    pub generated_at: String,
    pub summary: DryRunSummary,
    pub metrics: DryRunMetrics,
    pub equity_curve: Vec<DryRunEquityPoint>,
    pub by_window: Vec<DryRunWindowRow>,
    pub daily: Vec<DryRunDailyRow>,
    pub daily_by_window: Vec<DryRunDailyWindowRow>,
    pub symbols: Vec<DryRunSymbolRow>,
    pub symbols_by_window: Vec<DryRunSymbolRow>,
    pub closed_trades: Vec<DryRunClosedTradeRow>,
    pub recent_closed: Vec<DryRunClosedTradeRow>,
    pub open_positions: Vec<DryRunOpenPositionRow>,
    pub strategies: Vec<DryRunStrategyReport>,
    pub pairing: DryRunPairingReport,
}
