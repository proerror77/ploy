use std::collections::{BTreeMap, HashMap};

use chrono::{DateTime, Duration, Utc};
use ploy_operator_contracts::Regime;

use crate::factors::{FactorObservation, ResearchPmBookSnapshot, pearson_ic, spearman_ic};

const DEFAULT_STAKE_USD: f64 = 15.0;
const DEFAULT_TOP_QUANTILE: f64 = 0.2;
const PM_BOOK_MAX_AGE_SECS: i64 = 30;
const EPS: f64 = 1e-9;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FactorFamily {
    Alpha,
    CexLob,
    CexAggTrade,
    PmLiquidity,
    PmDynamics,
    DeribitVol,
    Execution,
    Regime,
    Exit,
    PortfolioRisk,
}

impl FactorFamily {
    pub fn as_str(self) -> &'static str {
        match self {
            FactorFamily::Alpha => "alpha",
            FactorFamily::CexLob => "cex_lob",
            FactorFamily::CexAggTrade => "cex_agg_trade",
            FactorFamily::PmLiquidity => "pm_liquidity",
            FactorFamily::PmDynamics => "pm_dynamics",
            FactorFamily::DeribitVol => "deribit_vol",
            FactorFamily::Execution => "execution",
            FactorFamily::Regime => "regime",
            FactorFamily::Exit => "exit",
            FactorFamily::PortfolioRisk => "portfolio_risk",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreeLayerArchive {
    DirectionProbabilityEdge,
    CexMicrostructureConfirmation,
    PmExecutableLiquidityRiskGate,
}

impl ThreeLayerArchive {
    pub fn as_str(self) -> &'static str {
        match self {
            ThreeLayerArchive::DirectionProbabilityEdge => "layer1_direction_probability_edge",
            ThreeLayerArchive::CexMicrostructureConfirmation => {
                "layer2_cex_microstructure_confirmation"
            }
            ThreeLayerArchive::PmExecutableLiquidityRiskGate => {
                "layer3_pm_executable_liquidity_risk_gate"
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewSide {
    Up,
    Down,
}

impl ReviewSide {
    pub fn as_str(self) -> &'static str {
        match self {
            ReviewSide::Up => "up",
            ReviewSide::Down => "down",
        }
    }

    pub fn multiplier(self) -> f64 {
        match self {
            ReviewSide::Up => 1.0,
            ReviewSide::Down => -1.0,
        }
    }
}

#[derive(Debug, Clone)]
pub struct FactorReviewOptions {
    pub stake_usd: f64,
    pub min_observations: usize,
    pub top_quantile: f64,
}

impl Default for FactorReviewOptions {
    fn default() -> Self {
        Self {
            stake_usd: DEFAULT_STAKE_USD,
            min_observations: 20,
            top_quantile: DEFAULT_TOP_QUANTILE,
        }
    }
}

#[derive(Debug, Clone)]
pub struct DeribitFeatureSnapshot {
    pub symbol: String,
    pub ts: DateTime<Utc>,
    pub mark_iv: f64,
    pub bid_iv: f64,
    pub ask_iv: f64,
    pub underlying_price: f64,
    pub delta: f64,
    pub gamma: f64,
    pub vega: f64,
    pub theta: f64,
}

#[derive(Debug, Clone)]
pub struct FactorObservationV2 {
    pub event_id: String,
    pub symbol: String,
    pub tick_ts: DateTime<Utc>,
    pub time_remaining_secs: i64,
    pub regime: Regime,
    pub side: ReviewSide,

    pub side_model_prob: f64,
    pub side_fair_prob: f64,
    pub side_model_edge: f64,
    pub side_distance_over_sigma: f64,
    pub abs_distance_to_beat: f64,
    pub drift_10s: f64,
    pub drift_30s: f64,
    pub post_flip_drift: f64,
    pub sigma_horizon: f64,
    pub vol_gap: f64,
    pub obi_10: f64,
    pub depth_imbalance: f64,
    pub depth_acceleration: f64,
    pub microprice_offset_bps: f64,
    pub cex_spread_bps: f64,
    pub cum_mprice_drift_5m: f64,
    pub cum_trade_imbalance_5m: f64,
    pub obi_delta_10s_side: f64,
    pub obi_delta_30s_side: f64,
    pub obi_persistence_30s_side: f64,
    pub obi_flip_count_60s: f64,
    pub depth_imbalance_delta_30s_side: f64,
    pub microprice_momentum_30s_side: f64,
    pub trade_imbalance_delta_10s_side: f64,
    pub trade_imbalance_delta_30s_side: f64,
    pub cex_bar_return_30s: f64,
    pub cex_bar_return_60s: f64,
    pub cex_bar_volume_ratio_30s: f64,
    pub cex_bar_volume_trend_3: f64,
    pub cex_signed_volume_ratio_30s: f64,
    pub cex_consecutive_bar_side: f64,
    pub cex_breakout_volume_side: f64,
    pub cex_continuation_score_side: f64,
    pub cex_continuation_edge_gate: f64,
    pub cex_continuation_liquidity_gate: f64,

    pub entry_ask: f64,
    pub exit_bid: f64,
    pub entry_ask_size: f64,
    pub exit_bid_size: f64,
    pub opposite_ask: f64,
    pub opposite_bid: f64,
    pub up_down_ask_sum: f64,
    pub pm_spread_bps: f64,
    pub pm_lag_secs: f64,
    pub entry_ask_change_10s: f64,
    pub entry_ask_change_30s: f64,
    pub exit_bid_change_30s: f64,
    pub pm_spread_change_30s: f64,
    pub entry_size_change_30s: f64,
    pub up_down_ask_sum_change_30s: f64,
    pub pm_reprice_speed_30s: f64,
    pub pm_quote_stability_30s: f64,

    pub deribit_mark_iv: f64,
    pub deribit_bid_iv: f64,
    pub deribit_ask_iv: f64,
    pub deribit_iv_spread: f64,
    pub deribit_iv_lag_secs: f64,
    pub deribit_iv_horizon: f64,
    pub deribit_iv_gap_horizon: f64,
    pub deribit_iv_change_30s: f64,
    pub deribit_iv_change_60s: f64,
    pub deribit_underlying_basis_bps: f64,
    pub deribit_delta: f64,
    pub deribit_gamma: f64,
    pub deribit_vega: f64,
    pub deribit_theta: f64,

    pub stake_usd: f64,
    pub entry_shares: f64,
    pub entry_fee_usd: f64,
    pub entry_capacity_ratio: f64,
    pub exit_capacity_ratio: f64,
    pub entry_liquidity_usd: f64,
    pub exit_liquidity_usd: f64,
    pub liquidity_shortfall_usd: f64,
    pub slippage_to_fill_15u_bps: f64,
    pub entry_sweep_avg_price_15u: f64,
    pub exit_sweep_avg_price_15u: f64,
    pub entry_sweep_shares_15u: f64,
    pub exit_sweep_shares_15u: f64,
    pub entry_sweep_levels_15u: f64,
    pub exit_sweep_levels_15u: f64,
    pub entry_sweep_slippage_bps: f64,
    pub exit_sweep_slippage_bps: f64,
    pub roundtrip_cost_usd: f64,
    pub roundtrip_pnl_now_15u: Option<f64>,
    pub roundtrip_pnl_now_full_depth_15u: Option<f64>,

    pub portfolio_stake_usd: f64,
    pub portfolio_event_exposure_usd: f64,
    pub same_event_observation_count: f64,
    pub same_event_side_observation_count: f64,
    pub side_is_up: f64,

    pub label_settlement_win: Option<f64>,
    pub label_executable_pnl_15u: Option<f64>,
    pub label_full_depth_executable_pnl_15u: Option<f64>,
    pub label_executable_fillable: bool,
    pub label_exit_fillable: bool,
    pub label_full_depth_entry_fillable: bool,
    pub label_full_depth_exit_fillable: bool,
    pub label_future_exit_bid_change_30s: Option<f64>,
    pub label_future_exit_bid_change_60s: Option<f64>,
    pub label_future_exit_pnl_30s: Option<f64>,
    pub label_future_exit_fillable_30s: Option<f64>,
}

#[derive(Clone, Copy)]
pub struct FactorV2Descriptor {
    pub name: &'static str,
    pub family: FactorFamily,
    pub layer: ThreeLayerArchive,
    pub accessor: fn(&FactorObservationV2) -> f64,
}

#[derive(Debug, Clone)]
pub struct DataHealthReport {
    pub source_observations: usize,
    pub v2_rows: usize,
    pub settlement_label_rows: usize,
    pub entry_quote_rows: usize,
    pub exit_quote_rows: usize,
    pub entry_size_rows: usize,
    pub exit_size_rows: usize,
    pub entry_fillable_rows: usize,
    pub exit_fillable_rows: usize,
    pub entry_full_depth_fillable_rows: usize,
    pub exit_full_depth_fillable_rows: usize,
    pub executable_pnl_rows: usize,
    pub full_depth_executable_pnl_rows: usize,
    pub deribit_rows: usize,
    pub avg_pm_lag_secs: f64,
    pub avg_entry_capacity_ratio: f64,
    pub avg_exit_capacity_ratio: f64,
    pub avg_entry_sweep_slippage_bps: f64,
    pub avg_exit_sweep_slippage_bps: f64,
}

impl DataHealthReport {
    pub fn entry_fill_rate(&self) -> f64 {
        ratio(self.entry_fillable_rows, self.v2_rows)
    }

    pub fn exit_fill_rate(&self) -> f64 {
        ratio(self.exit_fillable_rows, self.v2_rows)
    }

    pub fn rejection_rate(&self) -> f64 {
        1.0 - self.entry_fill_rate()
    }

    pub fn full_depth_entry_fill_rate(&self) -> f64 {
        ratio(self.entry_full_depth_fillable_rows, self.v2_rows)
    }

    pub fn full_depth_exit_fill_rate(&self) -> f64 {
        ratio(self.exit_full_depth_fillable_rows, self.v2_rows)
    }
}

#[derive(Debug, Clone)]
pub struct SingleFactorReview {
    pub factor: String,
    pub family: FactorFamily,
    pub layer: ThreeLayerArchive,
    pub n: usize,
    pub coverage: f64,
    pub settlement_pearson_ic: f64,
    pub settlement_rank_ic: f64,
    pub executable_pnl_pearson_ic: f64,
    pub executable_pnl_rank_ic: f64,
    pub selected_n: usize,
    pub selected_rejection_rate: f64,
    pub selected_executable_fill_rate: f64,
    pub selected_avg_slippage_bps: f64,
    pub selected_total_pnl_after_cost: f64,
    pub selected_avg_pnl_after_cost: f64,
    pub selected_sharpe: f64,
    pub selected_max_drawdown: f64,
    pub by_symbol_positive_ratio: f64,
    pub by_time_bucket_positive_ratio: f64,
}

#[derive(Debug, Clone)]
pub struct FactorReviewV2Report {
    pub options: FactorReviewOptions,
    pub health: DataHealthReport,
    pub reviews: Vec<SingleFactorReview>,
}

#[derive(Debug, Clone)]
pub struct FactorWalkForwardOptions {
    pub review: FactorReviewOptions,
    pub train_window_days: i64,
    pub test_window_days: i64,
    pub step_days: i64,
    pub top_n: usize,
    pub factor_name_filter: Option<String>,
}

impl Default for FactorWalkForwardOptions {
    fn default() -> Self {
        Self {
            review: FactorReviewOptions::default(),
            train_window_days: 2,
            test_window_days: 1,
            step_days: 1,
            top_n: 20,
            factor_name_filter: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct FactorSelectionMetrics {
    pub n: usize,
    pub selected_n: usize,
    pub executable_fill_rate: f64,
    pub rejection_rate: f64,
    pub total_pnl_after_cost: f64,
    pub avg_pnl_after_cost: f64,
    pub sharpe: f64,
    pub max_drawdown: f64,
    pub by_symbol_positive_ratio: f64,
    pub by_time_bucket_positive_ratio: f64,
}

#[derive(Debug, Clone)]
pub struct FactorWalkForwardWindow {
    pub window_index: usize,
    pub train_start: DateTime<Utc>,
    pub train_end: DateTime<Utc>,
    pub test_start: DateTime<Utc>,
    pub test_end: DateTime<Utc>,
    pub factor: String,
    pub family: FactorFamily,
    pub layer: ThreeLayerArchive,
    pub direction: f64,
    pub threshold: f64,
    pub train_settlement_rank_ic: f64,
    pub train_executable_pnl_rank_ic: f64,
    pub train: FactorSelectionMetrics,
    pub test: FactorSelectionMetrics,
}

#[derive(Debug, Clone)]
pub struct FactorWalkForwardAggregate {
    pub factor: String,
    pub family: FactorFamily,
    pub layer: ThreeLayerArchive,
    pub windows: usize,
    pub positive_window_ratio: f64,
    pub total_test_pnl_after_cost: f64,
    pub avg_test_pnl_per_window: f64,
    pub min_test_pnl_after_cost: f64,
    pub avg_test_fill_rate: f64,
    pub avg_test_rejection_rate: f64,
}

#[derive(Debug, Clone)]
pub struct FactorWalkForwardReport {
    pub options: FactorWalkForwardOptions,
    pub health: DataHealthReport,
    pub windows: Vec<FactorWalkForwardWindow>,
    pub aggregates: Vec<FactorWalkForwardAggregate>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FactorStabilityDecision {
    Candidate,
    Watchlist,
    Reject,
}

impl FactorStabilityDecision {
    pub fn as_str(self) -> &'static str {
        match self {
            FactorStabilityDecision::Candidate => "candidate",
            FactorStabilityDecision::Watchlist => "watchlist",
            FactorStabilityDecision::Reject => "reject",
        }
    }
}

#[derive(Debug, Clone)]
pub struct FactorStabilityOptions {
    pub min_windows: usize,
    pub min_positive_window_ratio: f64,
    pub min_total_test_pnl_after_cost: f64,
    pub min_avg_fill_rate: f64,
    pub max_avg_rejection_rate: f64,
    pub min_by_symbol_positive_ratio: f64,
    pub min_by_time_bucket_positive_ratio: f64,
    pub min_abs_executable_pnl_icir: f64,
}

impl Default for FactorStabilityOptions {
    fn default() -> Self {
        Self {
            min_windows: 8,
            min_positive_window_ratio: 0.65,
            min_total_test_pnl_after_cost: 0.0,
            min_avg_fill_rate: 0.08,
            max_avg_rejection_rate: 0.92,
            min_by_symbol_positive_ratio: 0.5,
            min_by_time_bucket_positive_ratio: 0.5,
            min_abs_executable_pnl_icir: 0.25,
        }
    }
}

#[derive(Debug, Clone)]
pub struct FactorStabilityRow {
    pub factor: String,
    pub family: FactorFamily,
    pub layer: ThreeLayerArchive,
    pub windows: usize,
    pub settlement_rank_ic_mean: f64,
    pub settlement_rank_icir: f64,
    pub executable_pnl_rank_ic_mean: f64,
    pub executable_pnl_rank_icir: f64,
    pub positive_window_ratio: f64,
    pub total_test_pnl_after_cost: f64,
    pub avg_test_pnl_per_window: f64,
    pub min_test_pnl_after_cost: f64,
    pub avg_test_fill_rate: f64,
    pub avg_test_rejection_rate: f64,
    pub avg_by_symbol_positive_ratio: f64,
    pub avg_by_time_bucket_positive_ratio: f64,
    pub decision: FactorStabilityDecision,
    pub reason: String,
}

#[derive(Debug, Clone)]
pub struct FactorStabilityReport {
    pub options: FactorStabilityOptions,
    pub rows: Vec<FactorStabilityRow>,
}

#[derive(Debug, Clone)]
pub struct FactorComboV1Options {
    pub walk_forward: FactorWalkForwardOptions,
    pub max_factors_per_family: usize,
    pub max_total_factors: usize,
    pub min_abs_train_executable_pnl_rank_ic: f64,
}

impl Default for FactorComboV1Options {
    fn default() -> Self {
        Self {
            walk_forward: FactorWalkForwardOptions::default(),
            max_factors_per_family: 2,
            max_total_factors: 12,
            min_abs_train_executable_pnl_rank_ic: 0.03,
        }
    }
}

#[derive(Debug, Clone)]
pub struct FactorComboComponent {
    pub factor: String,
    pub family: FactorFamily,
    pub layer: ThreeLayerArchive,
    pub accessor: fn(&FactorObservationV2) -> f64,
    pub direction: f64,
    pub train_executable_pnl_rank_ic: f64,
    pub train_mean: f64,
    pub train_std: f64,
}

#[derive(Debug, Clone)]
pub struct FactorComboV1Window {
    pub window_index: usize,
    pub train_start: DateTime<Utc>,
    pub train_end: DateTime<Utc>,
    pub test_start: DateTime<Utc>,
    pub test_end: DateTime<Utc>,
    pub threshold: f64,
    pub components: Vec<FactorComboComponent>,
    pub train: FactorSelectionMetrics,
    pub test: FactorSelectionMetrics,
}

#[derive(Debug, Clone)]
pub struct FactorComboV1Aggregate {
    pub windows: usize,
    pub positive_window_ratio: f64,
    pub total_test_pnl_after_cost: f64,
    pub avg_test_pnl_per_window: f64,
    pub min_test_pnl_after_cost: f64,
    pub avg_test_fill_rate: f64,
    pub avg_test_rejection_rate: f64,
    pub avg_component_count: f64,
}

#[derive(Debug, Clone)]
pub struct FactorComboV1Report {
    pub options: FactorComboV1Options,
    pub health: DataHealthReport,
    pub windows: Vec<FactorComboV1Window>,
    pub aggregate: FactorComboV1Aggregate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FillabilityDecision {
    Candidate,
    Watchlist,
    Reject,
}

impl FillabilityDecision {
    pub fn as_str(self) -> &'static str {
        match self {
            FillabilityDecision::Candidate => "candidate",
            FillabilityDecision::Watchlist => "watchlist",
            FillabilityDecision::Reject => "reject",
        }
    }
}

#[derive(Debug, Clone)]
pub struct FillabilityReviewOptions {
    pub review: FactorReviewOptions,
    pub min_bucket_observations: usize,
    pub min_entry_fill_rate: f64,
    pub min_roundtrip_fill_rate: f64,
    pub max_rejection_rate: f64,
}

impl Default for FillabilityReviewOptions {
    fn default() -> Self {
        Self {
            review: FactorReviewOptions::default(),
            min_bucket_observations: 50,
            min_entry_fill_rate: 0.30,
            min_roundtrip_fill_rate: 0.20,
            max_rejection_rate: 0.70,
        }
    }
}

#[derive(Debug, Clone)]
pub struct FillabilityBucketRow {
    pub dimension: String,
    pub bucket: String,
    pub n: usize,
    pub coverage: f64,
    pub entry_fill_rate: f64,
    pub exit_fill_rate: f64,
    pub roundtrip_fill_rate: f64,
    pub rejection_rate: f64,
    pub avg_entry_capacity_ratio: f64,
    pub avg_exit_capacity_ratio: f64,
    pub avg_entry_liquidity_usd: f64,
    pub avg_exit_liquidity_usd: f64,
    pub avg_pm_spread_bps: f64,
    pub avg_pm_lag_secs: f64,
    pub avg_slippage_to_fill_bps: f64,
    pub avg_roundtrip_cost_usd: f64,
    pub total_executable_pnl_after_cost: f64,
    pub avg_executable_pnl_after_cost: f64,
    pub decision: FillabilityDecision,
    pub reason: String,
}

#[derive(Debug, Clone)]
pub struct FillabilityReviewReport {
    pub options: FillabilityReviewOptions,
    pub health: DataHealthReport,
    pub rows: Vec<FillabilityBucketRow>,
}

#[derive(Debug, Clone)]
pub struct LiquidityGateV1Options {
    pub review: FactorReviewOptions,
    pub min_entry_capacity_ratio: f64,
    pub min_exit_capacity_ratio: f64,
    pub max_pm_lag_secs: f64,
    pub max_pm_spread_bps: f64,
    pub min_time_remaining_secs: i64,
    pub min_entry_ask: f64,
    pub max_entry_ask: f64,
}

impl Default for LiquidityGateV1Options {
    fn default() -> Self {
        Self {
            review: FactorReviewOptions::default(),
            min_entry_capacity_ratio: 1.0,
            min_exit_capacity_ratio: 1.0,
            max_pm_lag_secs: 30.0,
            max_pm_spread_bps: 3_000.0,
            min_time_remaining_secs: 45,
            min_entry_ask: 0.02,
            max_entry_ask: 0.95,
        }
    }
}

#[derive(Debug, Clone)]
pub struct LiquidityGateV1Report {
    pub options: LiquidityGateV1Options,
    pub health: DataHealthReport,
    pub selected_n: usize,
    pub coverage: f64,
    pub entry_fill_rate: f64,
    pub exit_fill_rate: f64,
    pub roundtrip_fill_rate: f64,
    pub rejection_rate: f64,
    pub avg_entry_capacity_ratio: f64,
    pub avg_exit_capacity_ratio: f64,
    pub avg_pm_spread_bps: f64,
    pub avg_pm_lag_secs: f64,
    pub avg_roundtrip_cost_usd: f64,
    pub metrics: FactorSelectionMetrics,
}

#[derive(Debug, Clone)]
pub struct LiquidityGatedAlphaV1Options {
    pub gate: LiquidityGateV1Options,
    pub walk_forward: FactorWalkForwardOptions,
}

impl Default for LiquidityGatedAlphaV1Options {
    fn default() -> Self {
        Self {
            gate: LiquidityGateV1Options::default(),
            walk_forward: FactorWalkForwardOptions::default(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct LiquidityGatedAlphaV1Report {
    pub options: LiquidityGatedAlphaV1Options,
    pub baseline_health: DataHealthReport,
    pub gate: LiquidityGateV1Report,
    pub review: FactorReviewV2Report,
    pub walk_forward: FactorWalkForwardReport,
    pub stability: FactorStabilityReport,
}

#[derive(Debug, Clone)]
pub struct TradeFormationReviewOptions {
    pub review: FactorReviewOptions,
    pub gate: LiquidityGateV1Options,
    pub min_path_observations: usize,
    pub top_n: usize,
}

impl Default for TradeFormationReviewOptions {
    fn default() -> Self {
        Self {
            review: FactorReviewOptions::default(),
            gate: LiquidityGateV1Options::default(),
            min_path_observations: 20,
            top_n: 20,
        }
    }
}

#[derive(Debug, Clone)]
pub struct TradeFormationPathRow {
    pub path: String,
    pub n: usize,
    pub coverage: f64,
    pub executable_rows: usize,
    pub win_rate: f64,
    pub settlement_win_rate: f64,
    pub total_pnl_after_cost: f64,
    pub avg_pnl_after_cost: f64,
    pub sharpe: f64,
    pub avg_side_model_prob: f64,
    pub avg_side_distance_over_sigma: f64,
    pub avg_obi_10_side: f64,
    pub avg_obi_persistence_30s_side: f64,
    pub avg_cex_continuation_score_side: f64,
    pub avg_entry_capacity_ratio: f64,
    pub avg_exit_capacity_ratio: f64,
    pub avg_pm_spread_bps: f64,
    pub avg_future_exit_pnl_30s: f64,
    pub avg_future_exit_bid_change_30s: f64,
    pub avg_future_exit_bid_change_60s: f64,
}

#[derive(Debug, Clone)]
pub struct TradeFormationRuleRow {
    pub rule: String,
    pub n: usize,
    pub coverage: f64,
    pub win_rate: f64,
    pub total_pnl_after_cost: f64,
    pub avg_pnl_after_cost: f64,
    pub sharpe: f64,
    pub avg_future_exit_pnl_30s: f64,
}

#[derive(Debug, Clone)]
pub struct TradeFormationReviewReport {
    pub options: TradeFormationReviewOptions,
    pub health: DataHealthReport,
    pub gate: LiquidityGateV1Report,
    pub gated_rows: usize,
    pub profitable_gated_rows: usize,
    pub losing_gated_rows: usize,
    pub missed_winner_rows: usize,
    pub profitable_paths: Vec<TradeFormationPathRow>,
    pub losing_paths: Vec<TradeFormationPathRow>,
    pub missed_winner_paths: Vec<TradeFormationPathRow>,
    pub meta_label_rules: Vec<TradeFormationRuleRow>,
}

#[derive(Debug, Clone)]
pub struct MetaLabelWalkForwardOptions {
    pub review: FactorReviewOptions,
    pub gate: LiquidityGateV1Options,
    pub train_window_days: i64,
    pub test_window_days: i64,
    pub step_days: i64,
    pub min_rule_observations: usize,
    pub min_candidate_windows: usize,
    pub min_candidate_positive_window_ratio: f64,
    pub min_candidate_total_test_pnl_after_cost: f64,
    pub min_candidate_avg_test_selected: f64,
    pub min_candidate_avg_fill_rate: f64,
    pub max_candidate_avg_rejection_rate: f64,
    pub min_candidate_worst_window_pnl_after_cost: f64,
    pub top_n: usize,
}

impl Default for MetaLabelWalkForwardOptions {
    fn default() -> Self {
        Self {
            review: FactorReviewOptions::default(),
            gate: LiquidityGateV1Options::default(),
            train_window_days: 2,
            test_window_days: 1,
            step_days: 1,
            min_rule_observations: 20,
            min_candidate_windows: 8,
            min_candidate_positive_window_ratio: 0.65,
            min_candidate_total_test_pnl_after_cost: 0.0,
            min_candidate_avg_test_selected: 30.0,
            min_candidate_avg_fill_rate: 0.95,
            max_candidate_avg_rejection_rate: 0.05,
            min_candidate_worst_window_pnl_after_cost: -150.0,
            top_n: 20,
        }
    }
}

#[derive(Debug, Clone)]
pub struct MetaLabelWalkForwardWindow {
    pub window_index: usize,
    pub train_start: DateTime<Utc>,
    pub train_end: DateTime<Utc>,
    pub test_start: DateTime<Utc>,
    pub test_end: DateTime<Utc>,
    pub rule: String,
    pub train: FactorSelectionMetrics,
    pub test: FactorSelectionMetrics,
}

#[derive(Debug, Clone)]
pub struct MetaLabelWalkForwardAggregate {
    pub rule: String,
    pub windows: usize,
    pub positive_window_ratio: f64,
    pub total_test_pnl_after_cost: f64,
    pub avg_test_pnl_per_window: f64,
    pub min_test_pnl_after_cost: f64,
    pub avg_train_selected: f64,
    pub avg_test_selected: f64,
    pub avg_test_fill_rate: f64,
    pub avg_test_rejection_rate: f64,
    pub decision: FactorStabilityDecision,
    pub reason: String,
}

#[derive(Debug, Clone)]
pub struct MetaLabelWalkForwardReport {
    pub options: MetaLabelWalkForwardOptions,
    pub health: DataHealthReport,
    pub gate: LiquidityGateV1Report,
    pub windows: Vec<MetaLabelWalkForwardWindow>,
    pub aggregates: Vec<MetaLabelWalkForwardAggregate>,
}

pub fn build_factor_observations_v2(
    rows: &[FactorObservation],
    options: &FactorReviewOptions,
) -> Vec<FactorObservationV2> {
    build_factor_observations_v2_with_deribit(rows, &[], options)
}

pub fn build_factor_observations_v2_with_deribit(
    rows: &[FactorObservation],
    deribit: &[DeribitFeatureSnapshot],
    options: &FactorReviewOptions,
) -> Vec<FactorObservationV2> {
    build_factor_observations_v2_with_deribit_and_pm_books(rows, deribit, &[], options)
}

pub fn build_factor_observations_v2_with_deribit_and_pm_books(
    rows: &[FactorObservation],
    deribit: &[DeribitFeatureSnapshot],
    pm_books: &[ResearchPmBookSnapshot],
    options: &FactorReviewOptions,
) -> Vec<FactorObservationV2> {
    let stake_usd = options.stake_usd;
    let book_index = build_pm_book_index(pm_books);
    let mut out = Vec::with_capacity(rows.len() * 2);
    for row in rows {
        let up_book = latest_pm_book(&book_index, &row.event_id, ReviewSide::Up, row.tick_ts);
        let down_book = latest_pm_book(&book_index, &row.event_id, ReviewSide::Down, row.tick_ts);
        out.push(side_row(row, ReviewSide::Up, stake_usd, up_book));
        out.push(side_row(row, ReviewSide::Down, stake_usd, down_book));
    }
    enrich_rolling_features(&mut out, deribit);
    out
}

pub fn factor_v2_descriptors() -> Vec<FactorV2Descriptor> {
    vec![
        descriptor(
            "side_model_edge",
            FactorFamily::Alpha,
            ThreeLayerArchive::DirectionProbabilityEdge,
            |r| r.side_model_edge,
        ),
        descriptor(
            "side_model_prob",
            FactorFamily::Alpha,
            ThreeLayerArchive::DirectionProbabilityEdge,
            |r| r.side_model_prob,
        ),
        descriptor(
            "side_fair_prob",
            FactorFamily::Alpha,
            ThreeLayerArchive::DirectionProbabilityEdge,
            |r| r.side_fair_prob,
        ),
        descriptor(
            "side_distance_over_sigma",
            FactorFamily::Alpha,
            ThreeLayerArchive::DirectionProbabilityEdge,
            |r| r.side_distance_over_sigma,
        ),
        descriptor(
            "abs_distance_to_beat",
            FactorFamily::Alpha,
            ThreeLayerArchive::DirectionProbabilityEdge,
            |r| r.abs_distance_to_beat,
        ),
        descriptor(
            "drift_10s",
            FactorFamily::Alpha,
            ThreeLayerArchive::DirectionProbabilityEdge,
            |r| r.drift_10s * r.side.multiplier(),
        ),
        descriptor(
            "drift_30s",
            FactorFamily::Alpha,
            ThreeLayerArchive::DirectionProbabilityEdge,
            |r| r.drift_30s * r.side.multiplier(),
        ),
        descriptor(
            "post_flip_drift",
            FactorFamily::Alpha,
            ThreeLayerArchive::DirectionProbabilityEdge,
            |r| r.post_flip_drift,
        ),
        descriptor(
            "sigma_horizon",
            FactorFamily::Alpha,
            ThreeLayerArchive::DirectionProbabilityEdge,
            |r| r.sigma_horizon,
        ),
        descriptor(
            "vol_gap",
            FactorFamily::Alpha,
            ThreeLayerArchive::DirectionProbabilityEdge,
            |r| r.vol_gap,
        ),
        descriptor(
            "obi_10_side",
            FactorFamily::CexLob,
            ThreeLayerArchive::CexMicrostructureConfirmation,
            |r| r.obi_10 * r.side.multiplier(),
        ),
        descriptor(
            "depth_imbalance_side",
            FactorFamily::CexLob,
            ThreeLayerArchive::CexMicrostructureConfirmation,
            |r| r.depth_imbalance * r.side.multiplier(),
        ),
        descriptor(
            "depth_acceleration_side",
            FactorFamily::CexLob,
            ThreeLayerArchive::CexMicrostructureConfirmation,
            |r| r.depth_acceleration * r.side.multiplier(),
        ),
        descriptor(
            "microprice_offset_side_bps",
            FactorFamily::CexLob,
            ThreeLayerArchive::CexMicrostructureConfirmation,
            |r| r.microprice_offset_bps * r.side.multiplier(),
        ),
        descriptor(
            "cex_spread_bps",
            FactorFamily::CexLob,
            ThreeLayerArchive::CexMicrostructureConfirmation,
            |r| r.cex_spread_bps,
        ),
        descriptor(
            "cum_mprice_drift_5m_side",
            FactorFamily::CexLob,
            ThreeLayerArchive::CexMicrostructureConfirmation,
            |r| r.cum_mprice_drift_5m * r.side.multiplier(),
        ),
        descriptor(
            "cum_trade_imbalance_5m_side",
            FactorFamily::CexAggTrade,
            ThreeLayerArchive::CexMicrostructureConfirmation,
            |r| r.cum_trade_imbalance_5m * r.side.multiplier(),
        ),
        descriptor(
            "obi_delta_10s_side",
            FactorFamily::CexLob,
            ThreeLayerArchive::CexMicrostructureConfirmation,
            |r| r.obi_delta_10s_side,
        ),
        descriptor(
            "obi_delta_30s_side",
            FactorFamily::CexLob,
            ThreeLayerArchive::CexMicrostructureConfirmation,
            |r| r.obi_delta_30s_side,
        ),
        descriptor(
            "obi_persistence_30s_side",
            FactorFamily::CexLob,
            ThreeLayerArchive::CexMicrostructureConfirmation,
            |r| r.obi_persistence_30s_side,
        ),
        descriptor(
            "obi_flip_count_60s",
            FactorFamily::CexLob,
            ThreeLayerArchive::CexMicrostructureConfirmation,
            |r| r.obi_flip_count_60s,
        ),
        descriptor(
            "depth_imbalance_delta_30s_side",
            FactorFamily::CexLob,
            ThreeLayerArchive::CexMicrostructureConfirmation,
            |r| r.depth_imbalance_delta_30s_side,
        ),
        descriptor(
            "microprice_momentum_30s_side",
            FactorFamily::CexLob,
            ThreeLayerArchive::CexMicrostructureConfirmation,
            |r| r.microprice_momentum_30s_side,
        ),
        descriptor(
            "trade_imbalance_delta_10s_side",
            FactorFamily::CexAggTrade,
            ThreeLayerArchive::CexMicrostructureConfirmation,
            |r| r.trade_imbalance_delta_10s_side,
        ),
        descriptor(
            "trade_imbalance_delta_30s_side",
            FactorFamily::CexAggTrade,
            ThreeLayerArchive::CexMicrostructureConfirmation,
            |r| r.trade_imbalance_delta_30s_side,
        ),
        descriptor(
            "cex_bar_return_30s_side",
            FactorFamily::CexAggTrade,
            ThreeLayerArchive::CexMicrostructureConfirmation,
            |r| r.cex_bar_return_30s * r.side.multiplier(),
        ),
        descriptor(
            "cex_bar_return_60s_side",
            FactorFamily::CexAggTrade,
            ThreeLayerArchive::CexMicrostructureConfirmation,
            |r| r.cex_bar_return_60s * r.side.multiplier(),
        ),
        descriptor(
            "cex_bar_volume_ratio_30s",
            FactorFamily::CexAggTrade,
            ThreeLayerArchive::CexMicrostructureConfirmation,
            |r| r.cex_bar_volume_ratio_30s,
        ),
        descriptor(
            "cex_bar_volume_trend_3",
            FactorFamily::CexAggTrade,
            ThreeLayerArchive::CexMicrostructureConfirmation,
            |r| r.cex_bar_volume_trend_3,
        ),
        descriptor(
            "cex_signed_volume_ratio_30s_side",
            FactorFamily::CexAggTrade,
            ThreeLayerArchive::CexMicrostructureConfirmation,
            |r| r.cex_signed_volume_ratio_30s * r.side.multiplier(),
        ),
        descriptor(
            "cex_consecutive_bar_side",
            FactorFamily::CexAggTrade,
            ThreeLayerArchive::CexMicrostructureConfirmation,
            |r| r.cex_consecutive_bar_side,
        ),
        descriptor(
            "cex_breakout_volume_side",
            FactorFamily::CexAggTrade,
            ThreeLayerArchive::CexMicrostructureConfirmation,
            |r| r.cex_breakout_volume_side,
        ),
        descriptor(
            "cex_continuation_score_side",
            FactorFamily::CexAggTrade,
            ThreeLayerArchive::CexMicrostructureConfirmation,
            |r| r.cex_continuation_score_side,
        ),
        descriptor(
            "cex_continuation_edge_gate",
            FactorFamily::CexAggTrade,
            ThreeLayerArchive::CexMicrostructureConfirmation,
            |r| r.cex_continuation_edge_gate,
        ),
        descriptor(
            "cex_continuation_liquidity_gate",
            FactorFamily::CexAggTrade,
            ThreeLayerArchive::PmExecutableLiquidityRiskGate,
            |r| r.cex_continuation_liquidity_gate,
        ),
        descriptor(
            "entry_ask",
            FactorFamily::PmLiquidity,
            ThreeLayerArchive::PmExecutableLiquidityRiskGate,
            |r| r.entry_ask,
        ),
        descriptor(
            "exit_bid",
            FactorFamily::PmLiquidity,
            ThreeLayerArchive::PmExecutableLiquidityRiskGate,
            |r| r.exit_bid,
        ),
        descriptor(
            "pm_spread_bps",
            FactorFamily::PmLiquidity,
            ThreeLayerArchive::PmExecutableLiquidityRiskGate,
            |r| r.pm_spread_bps,
        ),
        descriptor(
            "entry_ask_size",
            FactorFamily::PmLiquidity,
            ThreeLayerArchive::PmExecutableLiquidityRiskGate,
            |r| r.entry_ask_size,
        ),
        descriptor(
            "exit_bid_size",
            FactorFamily::PmLiquidity,
            ThreeLayerArchive::PmExecutableLiquidityRiskGate,
            |r| r.exit_bid_size,
        ),
        descriptor(
            "entry_sweep_slippage_bps",
            FactorFamily::Execution,
            ThreeLayerArchive::PmExecutableLiquidityRiskGate,
            |r| r.entry_sweep_slippage_bps,
        ),
        descriptor(
            "exit_sweep_slippage_bps",
            FactorFamily::Execution,
            ThreeLayerArchive::PmExecutableLiquidityRiskGate,
            |r| r.exit_sweep_slippage_bps,
        ),
        descriptor(
            "entry_sweep_levels_15u",
            FactorFamily::PmLiquidity,
            ThreeLayerArchive::PmExecutableLiquidityRiskGate,
            |r| r.entry_sweep_levels_15u,
        ),
        descriptor(
            "exit_sweep_levels_15u",
            FactorFamily::PmLiquidity,
            ThreeLayerArchive::PmExecutableLiquidityRiskGate,
            |r| r.exit_sweep_levels_15u,
        ),
        descriptor(
            "entry_full_depth_fillable_15u",
            FactorFamily::Execution,
            ThreeLayerArchive::PmExecutableLiquidityRiskGate,
            |r| bool_num(r.label_full_depth_entry_fillable),
        ),
        descriptor(
            "exit_full_depth_fillable_15u",
            FactorFamily::Execution,
            ThreeLayerArchive::PmExecutableLiquidityRiskGate,
            |r| bool_num(r.label_full_depth_exit_fillable),
        ),
        descriptor(
            "up_down_ask_sum",
            FactorFamily::PmLiquidity,
            ThreeLayerArchive::PmExecutableLiquidityRiskGate,
            |r| r.up_down_ask_sum,
        ),
        descriptor(
            "pm_lag_secs",
            FactorFamily::PmLiquidity,
            ThreeLayerArchive::PmExecutableLiquidityRiskGate,
            |r| r.pm_lag_secs,
        ),
        descriptor(
            "entry_ask_change_10s",
            FactorFamily::PmDynamics,
            ThreeLayerArchive::PmExecutableLiquidityRiskGate,
            |r| r.entry_ask_change_10s,
        ),
        descriptor(
            "entry_ask_change_30s",
            FactorFamily::PmDynamics,
            ThreeLayerArchive::PmExecutableLiquidityRiskGate,
            |r| r.entry_ask_change_30s,
        ),
        descriptor(
            "exit_bid_change_30s",
            FactorFamily::PmDynamics,
            ThreeLayerArchive::PmExecutableLiquidityRiskGate,
            |r| r.exit_bid_change_30s,
        ),
        descriptor(
            "pm_spread_change_30s",
            FactorFamily::PmDynamics,
            ThreeLayerArchive::PmExecutableLiquidityRiskGate,
            |r| r.pm_spread_change_30s,
        ),
        descriptor(
            "entry_size_change_30s",
            FactorFamily::PmDynamics,
            ThreeLayerArchive::PmExecutableLiquidityRiskGate,
            |r| r.entry_size_change_30s,
        ),
        descriptor(
            "up_down_ask_sum_change_30s",
            FactorFamily::PmDynamics,
            ThreeLayerArchive::PmExecutableLiquidityRiskGate,
            |r| r.up_down_ask_sum_change_30s,
        ),
        descriptor(
            "pm_reprice_speed_30s",
            FactorFamily::PmDynamics,
            ThreeLayerArchive::PmExecutableLiquidityRiskGate,
            |r| r.pm_reprice_speed_30s,
        ),
        descriptor(
            "pm_quote_stability_30s",
            FactorFamily::PmDynamics,
            ThreeLayerArchive::PmExecutableLiquidityRiskGate,
            |r| r.pm_quote_stability_30s,
        ),
        descriptor(
            "deribit_mark_iv",
            FactorFamily::DeribitVol,
            ThreeLayerArchive::DirectionProbabilityEdge,
            |r| r.deribit_mark_iv,
        ),
        descriptor(
            "deribit_iv_spread",
            FactorFamily::DeribitVol,
            ThreeLayerArchive::DirectionProbabilityEdge,
            |r| r.deribit_iv_spread,
        ),
        descriptor(
            "deribit_iv_lag_secs",
            FactorFamily::DeribitVol,
            ThreeLayerArchive::DirectionProbabilityEdge,
            |r| r.deribit_iv_lag_secs,
        ),
        descriptor(
            "deribit_iv_horizon",
            FactorFamily::DeribitVol,
            ThreeLayerArchive::DirectionProbabilityEdge,
            |r| r.deribit_iv_horizon,
        ),
        descriptor(
            "deribit_iv_gap_horizon",
            FactorFamily::DeribitVol,
            ThreeLayerArchive::DirectionProbabilityEdge,
            |r| r.deribit_iv_gap_horizon,
        ),
        descriptor(
            "deribit_iv_change_30s",
            FactorFamily::DeribitVol,
            ThreeLayerArchive::DirectionProbabilityEdge,
            |r| r.deribit_iv_change_30s,
        ),
        descriptor(
            "deribit_iv_change_60s",
            FactorFamily::DeribitVol,
            ThreeLayerArchive::DirectionProbabilityEdge,
            |r| r.deribit_iv_change_60s,
        ),
        descriptor(
            "deribit_underlying_basis_bps",
            FactorFamily::DeribitVol,
            ThreeLayerArchive::DirectionProbabilityEdge,
            |r| r.deribit_underlying_basis_bps,
        ),
        descriptor(
            "deribit_delta_side",
            FactorFamily::DeribitVol,
            ThreeLayerArchive::DirectionProbabilityEdge,
            |r| r.deribit_delta * r.side.multiplier(),
        ),
        descriptor(
            "deribit_gamma",
            FactorFamily::DeribitVol,
            ThreeLayerArchive::DirectionProbabilityEdge,
            |r| r.deribit_gamma,
        ),
        descriptor(
            "deribit_vega",
            FactorFamily::DeribitVol,
            ThreeLayerArchive::DirectionProbabilityEdge,
            |r| r.deribit_vega,
        ),
        descriptor(
            "deribit_theta",
            FactorFamily::DeribitVol,
            ThreeLayerArchive::DirectionProbabilityEdge,
            |r| r.deribit_theta,
        ),
        descriptor(
            "entry_capacity_ratio",
            FactorFamily::Execution,
            ThreeLayerArchive::PmExecutableLiquidityRiskGate,
            |r| r.entry_capacity_ratio,
        ),
        descriptor(
            "exit_capacity_ratio",
            FactorFamily::Execution,
            ThreeLayerArchive::PmExecutableLiquidityRiskGate,
            |r| r.exit_capacity_ratio,
        ),
        descriptor(
            "entry_liquidity_usd",
            FactorFamily::Execution,
            ThreeLayerArchive::PmExecutableLiquidityRiskGate,
            |r| r.entry_liquidity_usd,
        ),
        descriptor(
            "exit_liquidity_usd",
            FactorFamily::Execution,
            ThreeLayerArchive::PmExecutableLiquidityRiskGate,
            |r| r.exit_liquidity_usd,
        ),
        descriptor(
            "liquidity_shortfall_usd",
            FactorFamily::Execution,
            ThreeLayerArchive::PmExecutableLiquidityRiskGate,
            |r| r.liquidity_shortfall_usd,
        ),
        descriptor(
            "slippage_to_fill_15u_bps",
            FactorFamily::Execution,
            ThreeLayerArchive::PmExecutableLiquidityRiskGate,
            |r| r.slippage_to_fill_15u_bps,
        ),
        descriptor(
            "roundtrip_cost_usd",
            FactorFamily::Execution,
            ThreeLayerArchive::PmExecutableLiquidityRiskGate,
            |r| r.roundtrip_cost_usd,
        ),
        descriptor(
            "time_remaining_secs",
            FactorFamily::Regime,
            ThreeLayerArchive::PmExecutableLiquidityRiskGate,
            |r| r.time_remaining_secs as f64,
        ),
        descriptor(
            "roundtrip_pnl_now_15u",
            FactorFamily::Exit,
            ThreeLayerArchive::PmExecutableLiquidityRiskGate,
            |r| r.roundtrip_pnl_now_15u.unwrap_or(f64::NAN),
        ),
        descriptor(
            "future_exit_bid_change_30s",
            FactorFamily::Exit,
            ThreeLayerArchive::PmExecutableLiquidityRiskGate,
            |r| r.label_future_exit_bid_change_30s.unwrap_or(f64::NAN),
        ),
        descriptor(
            "future_exit_bid_change_60s",
            FactorFamily::Exit,
            ThreeLayerArchive::PmExecutableLiquidityRiskGate,
            |r| r.label_future_exit_bid_change_60s.unwrap_or(f64::NAN),
        ),
        descriptor(
            "future_exit_pnl_30s",
            FactorFamily::Exit,
            ThreeLayerArchive::PmExecutableLiquidityRiskGate,
            |r| r.label_future_exit_pnl_30s.unwrap_or(f64::NAN),
        ),
        descriptor(
            "future_exit_fillable_30s",
            FactorFamily::Exit,
            ThreeLayerArchive::PmExecutableLiquidityRiskGate,
            |r| r.label_future_exit_fillable_30s.unwrap_or(f64::NAN),
        ),
        descriptor(
            "exit_fillable",
            FactorFamily::Exit,
            ThreeLayerArchive::PmExecutableLiquidityRiskGate,
            |r| bool_num(r.label_exit_fillable),
        ),
        descriptor(
            "portfolio_stake_usd",
            FactorFamily::PortfolioRisk,
            ThreeLayerArchive::PmExecutableLiquidityRiskGate,
            |r| r.portfolio_stake_usd,
        ),
        descriptor(
            "portfolio_event_exposure_usd",
            FactorFamily::PortfolioRisk,
            ThreeLayerArchive::PmExecutableLiquidityRiskGate,
            |r| r.portfolio_event_exposure_usd,
        ),
        descriptor(
            "same_event_observation_count",
            FactorFamily::PortfolioRisk,
            ThreeLayerArchive::PmExecutableLiquidityRiskGate,
            |r| r.same_event_observation_count,
        ),
        descriptor(
            "same_event_side_observation_count",
            FactorFamily::PortfolioRisk,
            ThreeLayerArchive::PmExecutableLiquidityRiskGate,
            |r| r.same_event_side_observation_count,
        ),
        descriptor(
            "side_is_up",
            FactorFamily::PortfolioRisk,
            ThreeLayerArchive::DirectionProbabilityEdge,
            |r| r.side_is_up,
        ),
    ]
}

pub fn build_data_health_report(
    source_rows: &[FactorObservation],
    v2_rows: &[FactorObservationV2],
) -> DataHealthReport {
    DataHealthReport {
        source_observations: source_rows.len(),
        v2_rows: v2_rows.len(),
        settlement_label_rows: v2_rows
            .iter()
            .filter(|row| row.label_settlement_win.is_some_and(f64::is_finite))
            .count(),
        entry_quote_rows: v2_rows
            .iter()
            .filter(|row| valid_price(row.entry_ask))
            .count(),
        exit_quote_rows: v2_rows
            .iter()
            .filter(|row| valid_price(row.exit_bid))
            .count(),
        entry_size_rows: v2_rows
            .iter()
            .filter(|row| row.entry_ask_size.is_finite() && row.entry_ask_size > 0.0)
            .count(),
        exit_size_rows: v2_rows
            .iter()
            .filter(|row| row.exit_bid_size.is_finite() && row.exit_bid_size > 0.0)
            .count(),
        entry_fillable_rows: v2_rows
            .iter()
            .filter(|row| row.label_executable_fillable)
            .count(),
        exit_fillable_rows: v2_rows.iter().filter(|row| row.label_exit_fillable).count(),
        entry_full_depth_fillable_rows: v2_rows
            .iter()
            .filter(|row| row.label_full_depth_entry_fillable)
            .count(),
        exit_full_depth_fillable_rows: v2_rows
            .iter()
            .filter(|row| row.label_full_depth_exit_fillable)
            .count(),
        executable_pnl_rows: v2_rows
            .iter()
            .filter(|row| row.label_executable_pnl_15u.is_some_and(f64::is_finite))
            .count(),
        full_depth_executable_pnl_rows: v2_rows
            .iter()
            .filter(|row| {
                row.label_full_depth_executable_pnl_15u
                    .is_some_and(f64::is_finite)
            })
            .count(),
        avg_pm_lag_secs: mean(v2_rows.iter().map(|row| row.pm_lag_secs)),
        avg_entry_capacity_ratio: mean(v2_rows.iter().map(|row| row.entry_capacity_ratio)),
        avg_exit_capacity_ratio: mean(v2_rows.iter().map(|row| row.exit_capacity_ratio)),
        avg_entry_sweep_slippage_bps: mean(v2_rows.iter().map(|row| row.entry_sweep_slippage_bps)),
        avg_exit_sweep_slippage_bps: mean(v2_rows.iter().map(|row| row.exit_sweep_slippage_bps)),
        deribit_rows: v2_rows
            .iter()
            .filter(|row| row.deribit_mark_iv.is_finite())
            .count(),
    }
}

pub fn review_factors_v2(
    source_rows: &[FactorObservation],
    options: FactorReviewOptions,
) -> FactorReviewV2Report {
    review_factors_v2_with_deribit(source_rows, &[], options)
}

pub fn review_factors_v2_with_deribit(
    source_rows: &[FactorObservation],
    deribit: &[DeribitFeatureSnapshot],
    options: FactorReviewOptions,
) -> FactorReviewV2Report {
    let v2_rows = build_factor_observations_v2_with_deribit(source_rows, deribit, &options);
    review_factor_rows(source_rows, &v2_rows, options)
}

pub fn review_factors_v2_with_deribit_and_pm_books(
    source_rows: &[FactorObservation],
    deribit: &[DeribitFeatureSnapshot],
    pm_books: &[ResearchPmBookSnapshot],
    options: FactorReviewOptions,
) -> FactorReviewV2Report {
    let v2_rows = build_factor_observations_v2_with_deribit_and_pm_books(
        source_rows,
        deribit,
        pm_books,
        &options,
    );
    review_factor_rows(source_rows, &v2_rows, options)
}

fn review_factor_rows(
    source_rows: &[FactorObservation],
    v2_rows: &[FactorObservationV2],
    options: FactorReviewOptions,
) -> FactorReviewV2Report {
    review_factor_rows_with_descriptor_filter(source_rows, v2_rows, options, |_| true)
}

fn review_candidate_factor_rows(
    source_rows: &[FactorObservation],
    v2_rows: &[FactorObservationV2],
    options: FactorReviewOptions,
) -> FactorReviewV2Report {
    review_factor_rows_with_descriptor_filter(
        source_rows,
        v2_rows,
        options,
        is_walk_forward_candidate_descriptor,
    )
}

fn review_factor_rows_with_descriptor_filter(
    source_rows: &[FactorObservation],
    v2_rows: &[FactorObservationV2],
    options: FactorReviewOptions,
    descriptor_filter: fn(&FactorV2Descriptor) -> bool,
) -> FactorReviewV2Report {
    let health = build_data_health_report(source_rows, v2_rows);
    let mut reviews: Vec<SingleFactorReview> = factor_v2_descriptors()
        .into_iter()
        .filter(descriptor_filter)
        .filter_map(|descriptor| review_one_factor(v2_rows, descriptor, &options))
        .collect();
    reviews.sort_by(|a, b| {
        b.selected_total_pnl_after_cost
            .partial_cmp(&a.selected_total_pnl_after_cost)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                b.executable_pnl_rank_ic
                    .abs()
                    .partial_cmp(&a.executable_pnl_rank_ic.abs())
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
    });
    FactorReviewV2Report {
        options,
        health,
        reviews,
    }
}

pub fn walk_forward_factors_v2_with_deribit(
    source_rows: &[FactorObservation],
    deribit: &[DeribitFeatureSnapshot],
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    options: FactorWalkForwardOptions,
) -> FactorWalkForwardReport {
    let mut v2_rows =
        build_factor_observations_v2_with_deribit(source_rows, deribit, &options.review);
    walk_forward_factor_rows(source_rows, &mut v2_rows, start, end, options)
}

pub fn walk_forward_factors_v2_with_deribit_and_pm_books(
    source_rows: &[FactorObservation],
    deribit: &[DeribitFeatureSnapshot],
    pm_books: &[ResearchPmBookSnapshot],
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    options: FactorWalkForwardOptions,
) -> FactorWalkForwardReport {
    let mut v2_rows = build_factor_observations_v2_with_deribit_and_pm_books(
        source_rows,
        deribit,
        pm_books,
        &options.review,
    );
    walk_forward_factor_rows(source_rows, &mut v2_rows, start, end, options)
}

fn walk_forward_factor_rows(
    source_rows: &[FactorObservation],
    v2_rows: &mut [FactorObservationV2],
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    options: FactorWalkForwardOptions,
) -> FactorWalkForwardReport {
    v2_rows.sort_by_key(|row| row.tick_ts);
    let health = build_data_health_report(source_rows, v2_rows);
    let train_duration = Duration::days(options.train_window_days.max(1));
    let test_duration = Duration::days(options.test_window_days.max(1));
    let step_duration = Duration::days(options.step_days.max(1));
    let descriptors: Vec<FactorV2Descriptor> = factor_v2_descriptors()
        .into_iter()
        .filter(is_walk_forward_candidate_descriptor)
        .filter(|descriptor| {
            factor_name_matches_filter(descriptor.name, &options.factor_name_filter)
        })
        .collect();

    let mut windows = Vec::new();
    let mut train_start = start;
    let mut window_index = 0usize;
    while train_start + train_duration + test_duration <= end + Duration::seconds(1) {
        let train_end = train_start + train_duration;
        let test_start = train_end;
        let test_end = test_start + test_duration;
        let train_rows: Vec<&FactorObservationV2> =
            walk_forward_time_slice(&v2_rows, train_start, train_end)
                .iter()
                .collect();
        let test_rows: Vec<&FactorObservationV2> =
            walk_forward_time_slice(&v2_rows, test_start, test_end)
                .iter()
                .collect();

        if train_rows.len() >= options.review.min_observations
            && test_rows.len() >= options.review.min_observations
        {
            let mut fitted: Vec<FactorWalkForwardWindow> = descriptors
                .iter()
                .filter_map(|descriptor| {
                    fit_walk_forward_factor(
                        &train_rows,
                        &test_rows,
                        *descriptor,
                        &options.review,
                        window_index,
                        train_start,
                        train_end,
                        test_start,
                        test_end,
                    )
                })
                .collect();
            fitted.sort_by(|a, b| {
                b.test
                    .total_pnl_after_cost
                    .partial_cmp(&a.test.total_pnl_after_cost)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| {
                        b.test
                            .sharpe
                            .partial_cmp(&a.test.sharpe)
                            .unwrap_or(std::cmp::Ordering::Equal)
                    })
            });
            windows.extend(fitted);
        }

        window_index += 1;
        train_start += step_duration;
    }

    let aggregates = aggregate_walk_forward_windows(&windows);
    FactorWalkForwardReport {
        options,
        health,
        windows,
        aggregates,
    }
}

pub fn build_factor_stability_report(
    report: &FactorWalkForwardReport,
    options: FactorStabilityOptions,
) -> FactorStabilityReport {
    let mut grouped: BTreeMap<&str, Vec<&FactorWalkForwardWindow>> = BTreeMap::new();
    for window in &report.windows {
        grouped.entry(&window.factor).or_default().push(window);
    }

    let mut rows = Vec::with_capacity(grouped.len());
    for (factor, windows) in grouped {
        let Some(first) = windows.first() else {
            continue;
        };
        let total_test_pnl_after_cost = windows
            .iter()
            .map(|window| window.test.total_pnl_after_cost)
            .sum::<f64>();
        let min_test_pnl_after_cost = windows
            .iter()
            .map(|window| window.test.total_pnl_after_cost)
            .fold(f64::INFINITY, f64::min);
        let positive_windows = windows
            .iter()
            .filter(|window| window.test.total_pnl_after_cost > 0.0)
            .count();
        let settlement_ics = windows
            .iter()
            .map(|window| window.train_settlement_rank_ic)
            .filter(|value| value.is_finite())
            .collect::<Vec<_>>();
        let executable_ics = windows
            .iter()
            .map(|window| window.train_executable_pnl_rank_ic)
            .filter(|value| value.is_finite())
            .collect::<Vec<_>>();
        let avg_by_symbol_positive_ratio = mean(
            windows
                .iter()
                .map(|window| window.test.by_symbol_positive_ratio),
        );
        let avg_by_time_bucket_positive_ratio = mean(
            windows
                .iter()
                .map(|window| window.test.by_time_bucket_positive_ratio),
        );
        let avg_test_fill_rate = mean(
            windows
                .iter()
                .map(|window| window.test.executable_fill_rate),
        );
        let avg_test_rejection_rate = mean(windows.iter().map(|window| window.test.rejection_rate));
        let positive_window_ratio = ratio(positive_windows, windows.len());
        let settlement_rank_ic_mean = mean(settlement_ics.iter().copied());
        let executable_pnl_rank_ic_mean = mean(executable_ics.iter().copied());
        let settlement_rank_icir = icir(&settlement_ics);
        let executable_pnl_rank_icir = icir(&executable_ics);
        let (decision, reason) = stability_decision(
            windows.len(),
            positive_window_ratio,
            total_test_pnl_after_cost,
            avg_test_fill_rate,
            avg_test_rejection_rate,
            avg_by_symbol_positive_ratio,
            avg_by_time_bucket_positive_ratio,
            executable_pnl_rank_icir,
            &options,
        );
        rows.push(FactorStabilityRow {
            factor: factor.to_string(),
            family: first.family,
            layer: first.layer,
            windows: windows.len(),
            settlement_rank_ic_mean,
            settlement_rank_icir,
            executable_pnl_rank_ic_mean,
            executable_pnl_rank_icir,
            positive_window_ratio,
            total_test_pnl_after_cost,
            avg_test_pnl_per_window: total_test_pnl_after_cost / windows.len() as f64,
            min_test_pnl_after_cost,
            avg_test_fill_rate,
            avg_test_rejection_rate,
            avg_by_symbol_positive_ratio,
            avg_by_time_bucket_positive_ratio,
            decision,
            reason,
        });
    }
    rows.sort_by(|a, b| {
        decision_rank(b.decision)
            .cmp(&decision_rank(a.decision))
            .then_with(|| {
                b.total_test_pnl_after_cost
                    .partial_cmp(&a.total_test_pnl_after_cost)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| {
                b.positive_window_ratio
                    .partial_cmp(&a.positive_window_ratio)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
    });
    FactorStabilityReport { options, rows }
}

pub fn walk_forward_factor_combo_v1_with_deribit(
    source_rows: &[FactorObservation],
    deribit: &[DeribitFeatureSnapshot],
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    options: FactorComboV1Options,
) -> FactorComboV1Report {
    let mut v2_rows = build_factor_observations_v2_with_deribit(
        source_rows,
        deribit,
        &options.walk_forward.review,
    );
    v2_rows.sort_by_key(|row| row.tick_ts);
    let descriptors: Vec<FactorV2Descriptor> = factor_v2_descriptors()
        .into_iter()
        .filter(is_walk_forward_candidate_descriptor)
        .filter(|descriptor| {
            factor_name_matches_filter(descriptor.name, &options.walk_forward.factor_name_filter)
        })
        .collect();

    walk_forward_factor_combo_from_v2_rows(source_rows, &v2_rows, &descriptors, start, end, options)
}

fn walk_forward_factor_combo_from_v2_rows(
    source_rows: &[FactorObservation],
    v2_rows: &[FactorObservationV2],
    descriptors: &[FactorV2Descriptor],
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    options: FactorComboV1Options,
) -> FactorComboV1Report {
    let health = build_data_health_report(source_rows, v2_rows);
    let mut windows = Vec::new();
    let train_duration = Duration::days(options.walk_forward.train_window_days.max(1));
    let test_duration = Duration::days(options.walk_forward.test_window_days.max(1));
    let step_duration = Duration::days(options.walk_forward.step_days.max(1));
    let mut train_start = start;
    let mut window_index = 0usize;
    while train_start + train_duration + test_duration <= end + Duration::seconds(1) {
        let train_end = train_start + train_duration;
        let test_start = train_end;
        let test_end = test_start + test_duration;
        let train_rows: Vec<&FactorObservationV2> =
            walk_forward_time_slice(&v2_rows, train_start, train_end)
                .iter()
                .collect();
        let test_rows: Vec<&FactorObservationV2> =
            walk_forward_time_slice(&v2_rows, test_start, test_end)
                .iter()
                .collect();

        if train_rows.len() >= options.walk_forward.review.min_observations
            && test_rows.len() >= options.walk_forward.review.min_observations
        {
            if let Some(window) = fit_combo_v1_window(
                &train_rows,
                &test_rows,
                &descriptors,
                &options,
                window_index,
                train_start,
                train_end,
                test_start,
                test_end,
            ) {
                windows.push(window);
            }
        }

        window_index += 1;
        train_start += step_duration;
    }
    let aggregate = aggregate_combo_v1_windows(&windows);
    FactorComboV1Report {
        options,
        health,
        windows,
        aggregate,
    }
}

pub fn review_fillability_v1_with_deribit(
    source_rows: &[FactorObservation],
    deribit: &[DeribitFeatureSnapshot],
    options: FillabilityReviewOptions,
) -> FillabilityReviewReport {
    let v2_rows = build_factor_observations_v2_with_deribit(source_rows, deribit, &options.review);
    let health = build_data_health_report(source_rows, &v2_rows);
    let mut rows = Vec::new();
    for spec in fillability_bucket_specs() {
        let mut buckets: BTreeMap<String, Vec<&FactorObservationV2>> = BTreeMap::new();
        for row in &v2_rows {
            if let Some(bucket) = (spec.bucket)(row) {
                buckets.entry(bucket).or_default().push(row);
            }
        }
        for (bucket, bucket_rows) in buckets {
            rows.push(build_fillability_bucket_row(
                spec.dimension,
                bucket,
                &bucket_rows,
                v2_rows.len(),
                &options,
            ));
        }
    }
    rows.sort_by(|a, b| {
        fillability_decision_rank(b.decision)
            .cmp(&fillability_decision_rank(a.decision))
            .then_with(|| {
                b.roundtrip_fill_rate
                    .partial_cmp(&a.roundtrip_fill_rate)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| {
                b.entry_fill_rate
                    .partial_cmp(&a.entry_fill_rate)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| {
                b.total_executable_pnl_after_cost
                    .partial_cmp(&a.total_executable_pnl_after_cost)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
    });
    FillabilityReviewReport {
        options,
        health,
        rows,
    }
}

pub fn liquidity_gate_v1_with_deribit(
    source_rows: &[FactorObservation],
    deribit: &[DeribitFeatureSnapshot],
    options: LiquidityGateV1Options,
) -> LiquidityGateV1Report {
    let v2_rows = build_factor_observations_v2_with_deribit(source_rows, deribit, &options.review);
    build_liquidity_gate_v1_report(source_rows, &v2_rows, options)
}

fn build_liquidity_gate_v1_report(
    source_rows: &[FactorObservation],
    v2_rows: &[FactorObservationV2],
    options: LiquidityGateV1Options,
) -> LiquidityGateV1Report {
    let health = build_data_health_report(source_rows, v2_rows);
    let selected = v2_rows
        .iter()
        .filter(|row| liquidity_gate_v1_accepts(row, &options))
        .collect::<Vec<_>>();
    let metrics = selection_metrics_for_rows(v2_rows.len(), &selected);
    LiquidityGateV1Report {
        options,
        health,
        selected_n: selected.len(),
        coverage: ratio(selected.len(), v2_rows.len()),
        entry_fill_rate: ratio(
            selected
                .iter()
                .filter(|row| row.label_executable_fillable)
                .count(),
            selected.len(),
        ),
        exit_fill_rate: ratio(
            selected
                .iter()
                .filter(|row| row.label_exit_fillable)
                .count(),
            selected.len(),
        ),
        roundtrip_fill_rate: ratio(
            selected
                .iter()
                .filter(|row| row.label_executable_fillable && row.label_exit_fillable)
                .count(),
            selected.len(),
        ),
        rejection_rate: ratio(
            selected
                .iter()
                .filter(|row| !row.label_executable_fillable)
                .count(),
            selected.len(),
        ),
        avg_entry_capacity_ratio: mean(selected.iter().map(|row| row.entry_capacity_ratio)),
        avg_exit_capacity_ratio: mean(selected.iter().map(|row| row.exit_capacity_ratio)),
        avg_pm_spread_bps: mean(selected.iter().map(|row| row.pm_spread_bps)),
        avg_pm_lag_secs: mean(selected.iter().map(|row| row.pm_lag_secs)),
        avg_roundtrip_cost_usd: mean(selected.iter().map(|row| row.roundtrip_cost_usd)),
        metrics,
    }
}

pub fn liquidity_gated_alpha_v1_with_deribit(
    source_rows: &[FactorObservation],
    deribit: &[DeribitFeatureSnapshot],
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    mut options: LiquidityGatedAlphaV1Options,
) -> LiquidityGatedAlphaV1Report {
    options.gate.review = options.walk_forward.review.clone();
    let v2_rows = build_factor_observations_v2_with_deribit(
        source_rows,
        deribit,
        &options.walk_forward.review,
    );
    let baseline_health = build_data_health_report(source_rows, &v2_rows);
    let gate = build_liquidity_gate_v1_report(source_rows, &v2_rows, options.gate.clone());
    let gated_rows = v2_rows
        .iter()
        .filter(|row| liquidity_gate_v1_accepts(row, &options.gate))
        .cloned()
        .collect::<Vec<_>>();
    let review = review_candidate_factor_rows(
        source_rows,
        &gated_rows,
        options.walk_forward.review.clone(),
    );
    let mut walk_rows = gated_rows.clone();
    let walk_forward = walk_forward_factor_rows(
        source_rows,
        &mut walk_rows,
        start,
        end,
        options.walk_forward.clone(),
    );
    let stability = build_factor_stability_report(&walk_forward, FactorStabilityOptions::default());
    LiquidityGatedAlphaV1Report {
        options,
        baseline_health,
        gate,
        review,
        walk_forward,
        stability,
    }
}

pub fn review_trade_formation_v1_with_deribit(
    source_rows: &[FactorObservation],
    deribit: &[DeribitFeatureSnapshot],
    mut options: TradeFormationReviewOptions,
) -> TradeFormationReviewReport {
    options.gate.review = options.review.clone();
    let mut v2_rows =
        build_factor_observations_v2_with_deribit(source_rows, deribit, &options.review);
    v2_rows.sort_by_key(|row| row.tick_ts);
    let health = build_data_health_report(source_rows, &v2_rows);
    let gate = build_liquidity_gate_v1_report(source_rows, &v2_rows, options.gate.clone());
    let gated = v2_rows
        .iter()
        .filter(|row| liquidity_gate_v1_accepts(row, &options.gate))
        .collect::<Vec<_>>();
    let rejected = v2_rows
        .iter()
        .filter(|row| !liquidity_gate_v1_accepts(row, &options.gate))
        .collect::<Vec<_>>();
    let profitable_gated = gated
        .iter()
        .copied()
        .filter(|row| executable_pnl(row).is_some_and(|pnl| pnl > 0.0))
        .collect::<Vec<_>>();
    let losing_gated = gated
        .iter()
        .copied()
        .filter(|row| executable_pnl(row).is_some_and(|pnl| pnl <= 0.0))
        .collect::<Vec<_>>();
    let missed_winners = rejected
        .iter()
        .copied()
        .filter(|row| executable_pnl(row).is_some_and(|pnl| pnl > 0.0))
        .collect::<Vec<_>>();
    let profitable_paths = build_trade_formation_path_rows(
        &profitable_gated,
        gated.len(),
        options.min_path_observations,
    );
    let losing_paths =
        build_trade_formation_path_rows(&losing_gated, gated.len(), options.min_path_observations);
    let missed_winner_paths = build_trade_formation_path_rows(
        &missed_winners,
        rejected.len(),
        options.min_path_observations,
    );
    let meta_label_rules = build_trade_formation_rule_rows(&gated, options.min_path_observations);

    TradeFormationReviewReport {
        options,
        health,
        gate,
        gated_rows: gated.len(),
        profitable_gated_rows: profitable_gated.len(),
        losing_gated_rows: losing_gated.len(),
        missed_winner_rows: missed_winners.len(),
        profitable_paths,
        losing_paths,
        missed_winner_paths,
        meta_label_rules,
    }
}

pub fn walk_forward_meta_label_v1_with_deribit(
    source_rows: &[FactorObservation],
    deribit: &[DeribitFeatureSnapshot],
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    mut options: MetaLabelWalkForwardOptions,
) -> MetaLabelWalkForwardReport {
    options.gate.review = options.review.clone();
    let mut v2_rows =
        build_factor_observations_v2_with_deribit(source_rows, deribit, &options.review);
    v2_rows.sort_by_key(|row| row.tick_ts);
    let health = build_data_health_report(source_rows, &v2_rows);
    let gate = build_liquidity_gate_v1_report(source_rows, &v2_rows, options.gate.clone());
    let mut windows = Vec::new();
    let train_duration = Duration::days(options.train_window_days.max(1));
    let test_duration = Duration::days(options.test_window_days.max(1));
    let step_duration = Duration::days(options.step_days.max(1));
    let mut train_start = start;
    let mut window_index = 0usize;

    while train_start + train_duration + test_duration <= end + Duration::seconds(1) {
        let train_end = train_start + train_duration;
        let test_start = train_end;
        let test_end = test_start + test_duration;
        let train_slice = walk_forward_time_slice(&v2_rows, train_start, train_end);
        let test_slice = walk_forward_time_slice(&v2_rows, test_start, test_end);
        let train_gated = train_slice
            .iter()
            .filter(|row| liquidity_gate_v1_accepts(row, &options.gate))
            .collect::<Vec<_>>();
        let test_gated = test_slice
            .iter()
            .filter(|row| liquidity_gate_v1_accepts(row, &options.gate))
            .collect::<Vec<_>>();

        if train_gated.len() >= options.review.min_observations
            && test_gated.len() >= options.review.min_observations
        {
            for spec in meta_label_rule_specs() {
                let train = evaluate_meta_label_rule(&train_gated, spec.predicate);
                if train.selected_n < options.min_rule_observations {
                    continue;
                }
                let test = evaluate_meta_label_rule(&test_gated, spec.predicate);
                windows.push(MetaLabelWalkForwardWindow {
                    window_index,
                    train_start,
                    train_end,
                    test_start,
                    test_end,
                    rule: spec.name.to_string(),
                    train,
                    test,
                });
            }
        }

        window_index += 1;
        train_start += step_duration;
    }

    let aggregates = aggregate_meta_label_windows(&windows, &options);
    MetaLabelWalkForwardReport {
        options,
        health,
        gate,
        windows,
        aggregates,
    }
}

pub fn format_factor_walk_forward_v2_report(report: &FactorWalkForwardReport) -> String {
    let mut out = String::new();
    out.push_str("=== Factor Walk-Forward V2 Data Health ===\n");
    out.push_str(&format!(
        "source_obs={} v2_rows={} settlement_labels={} executable_pnl_rows={} deribit_rows={}\n",
        report.health.source_observations,
        report.health.v2_rows,
        report.health.settlement_label_rows,
        report.health.executable_pnl_rows,
        report.health.deribit_rows,
    ));
    out.push_str(&format!(
        "entry_fill_rate={:.2}% rejection_rate={:.2}% exit_fill_rate={:.2}% avg_pm_lag_secs={:.2}\n",
        report.health.entry_fill_rate() * 100.0,
        report.health.rejection_rate() * 100.0,
        report.health.exit_fill_rate() * 100.0,
        report.health.avg_pm_lag_secs,
    ));
    out.push_str(&format!(
        "stake_usd={:.2} train_days={} test_days={} step_days={} top_quantile={:.2} factor_name_filter={}\n\n",
        report.options.review.stake_usd,
        report.options.train_window_days,
        report.options.test_window_days,
        report.options.step_days,
        report.options.review.top_quantile,
        report
            .options
            .factor_name_filter
            .as_deref()
            .unwrap_or("<none>"),
    ));

    out.push_str("=== Walk-Forward Aggregates By Test PnL ===\n");
    out.push_str("factor,family,layer,windows,pos_window_ratio,total_test_pnl,avg_window_pnl,min_window_pnl,avg_fill_rate,avg_reject_rate\n");
    for aggregate in report.aggregates.iter().take(report.options.top_n.max(1)) {
        out.push_str(&format!(
            "{},{},{},{},{:.4},{:.4},{:.4},{:.4},{:.4},{:.4}\n",
            aggregate.factor,
            aggregate.family.as_str(),
            aggregate.layer.as_str(),
            aggregate.windows,
            aggregate.positive_window_ratio,
            aggregate.total_test_pnl_after_cost,
            aggregate.avg_test_pnl_per_window,
            aggregate.min_test_pnl_after_cost,
            aggregate.avg_test_fill_rate,
            aggregate.avg_test_rejection_rate,
        ));
    }

    out.push_str("\n=== Walk-Forward Windows ===\n");
    out.push_str("window,train_start,train_end,test_start,test_end,factor,family,layer,direction,threshold,train_pnl_rank_ic,train_settle_rank_ic,train_selected,train_pnl,test_selected,test_fill,test_reject,test_pnl,test_avg_pnl,test_sharpe,test_max_dd,symbol_pos,time_bucket_pos\n");
    let mut displayed_by_window: BTreeMap<usize, usize> = BTreeMap::new();
    for window in &report.windows {
        let displayed = displayed_by_window.entry(window.window_index).or_insert(0);
        if *displayed >= report.options.top_n.max(1) {
            continue;
        }
        *displayed += 1;
        out.push_str(&format!(
            "{},{},{},{},{},{},{},{},{:.0},{:.8},{:.4},{:.4},{},{:.4},{},{:.4},{:.4},{:.4},{:.4},{:.4},{:.4},{:.4},{:.4}\n",
            window.window_index,
            window.train_start,
            window.train_end,
            window.test_start,
            window.test_end,
            window.factor,
            window.family.as_str(),
            window.layer.as_str(),
            window.direction,
            window.threshold,
            window.train_executable_pnl_rank_ic,
            window.train_settlement_rank_ic,
            window.train.selected_n,
            window.train.total_pnl_after_cost,
            window.test.selected_n,
            window.test.executable_fill_rate,
            window.test.rejection_rate,
            window.test.total_pnl_after_cost,
            window.test.avg_pnl_after_cost,
            window.test.sharpe,
            window.test.max_drawdown,
            window.test.by_symbol_positive_ratio,
            window.test.by_time_bucket_positive_ratio,
        ));
    }
    out
}

pub fn format_factor_stability_report(report: &FactorStabilityReport, top_n: usize) -> String {
    let mut out = String::new();
    out.push_str("=== Factor Stability Report ===\n");
    out.push_str(&format!(
        "min_windows={} min_pos_window_ratio={:.2} min_fill_rate={:.2} max_reject_rate={:.2} min_abs_pnl_icir={:.2}\n\n",
        report.options.min_windows,
        report.options.min_positive_window_ratio,
        report.options.min_avg_fill_rate,
        report.options.max_avg_rejection_rate,
        report.options.min_abs_executable_pnl_icir,
    ));
    out.push_str("decision,factor,family,layer,windows,settle_ic_mean,settle_icir,pnl_ic_mean,pnl_icir,pos_window_ratio,total_test_pnl,avg_window_pnl,min_window_pnl,avg_fill_rate,avg_reject_rate,symbol_pos,time_bucket_pos,reason\n");
    for row in report.rows.iter().take(top_n.max(1)) {
        out.push_str(&format!(
            "{},{},{},{},{},{:.4},{:.4},{:.4},{:.4},{:.4},{:.4},{:.4},{:.4},{:.4},{:.4},{:.4},{:.4},{}\n",
            row.decision.as_str(),
            row.factor,
            row.family.as_str(),
            row.layer.as_str(),
            row.windows,
            row.settlement_rank_ic_mean,
            row.settlement_rank_icir,
            row.executable_pnl_rank_ic_mean,
            row.executable_pnl_rank_icir,
            row.positive_window_ratio,
            row.total_test_pnl_after_cost,
            row.avg_test_pnl_per_window,
            row.min_test_pnl_after_cost,
            row.avg_test_fill_rate,
            row.avg_test_rejection_rate,
            row.avg_by_symbol_positive_ratio,
            row.avg_by_time_bucket_positive_ratio,
            row.reason,
        ));
    }
    out
}

pub fn format_factor_combo_v1_report(report: &FactorComboV1Report) -> String {
    let mut out = String::new();
    out.push_str("=== Factor Combo V1 Data Health ===\n");
    out.push_str(&format!(
        "source_obs={} v2_rows={} executable_pnl_rows={} deribit_rows={} entry_fill_rate={:.2}% rejection_rate={:.2}%\n",
        report.health.source_observations,
        report.health.v2_rows,
        report.health.executable_pnl_rows,
        report.health.deribit_rows,
        report.health.entry_fill_rate() * 100.0,
        report.health.rejection_rate() * 100.0,
    ));
    out.push_str(&format!(
        "train_days={} test_days={} step_days={} top_quantile={:.2} max_family={} max_total={} min_abs_train_pnl_ic={:.4} factor_name_filter={}\n\n",
        report.options.walk_forward.train_window_days,
        report.options.walk_forward.test_window_days,
        report.options.walk_forward.step_days,
        report.options.walk_forward.review.top_quantile,
        report.options.max_factors_per_family,
        report.options.max_total_factors,
        report.options.min_abs_train_executable_pnl_rank_ic,
        report
            .options
            .walk_forward
            .factor_name_filter
            .as_deref()
            .unwrap_or("<none>"),
    ));
    out.push_str("=== Combo V1 Aggregate ===\n");
    out.push_str("windows,pos_window_ratio,total_test_pnl,avg_window_pnl,min_window_pnl,avg_fill_rate,avg_reject_rate,avg_component_count\n");
    out.push_str(&format!(
        "{},{:.4},{:.4},{:.4},{:.4},{:.4},{:.4},{:.2}\n\n",
        report.aggregate.windows,
        report.aggregate.positive_window_ratio,
        report.aggregate.total_test_pnl_after_cost,
        report.aggregate.avg_test_pnl_per_window,
        report.aggregate.min_test_pnl_after_cost,
        report.aggregate.avg_test_fill_rate,
        report.aggregate.avg_test_rejection_rate,
        report.aggregate.avg_component_count,
    ));

    out.push_str("=== Combo V1 Windows ===\n");
    out.push_str("window,train_start,train_end,test_start,test_end,threshold,components,train_selected,train_pnl,test_selected,test_fill,test_reject,test_pnl,test_avg_pnl,test_sharpe,test_max_dd,symbol_pos,time_bucket_pos\n");
    for window in &report.windows {
        let components = window
            .components
            .iter()
            .map(|component| {
                format!(
                    "{}:{:.3}",
                    component.factor, component.train_executable_pnl_rank_ic
                )
            })
            .collect::<Vec<_>>()
            .join("+");
        out.push_str(&format!(
            "{},{},{},{},{},{:.8},{},{},{:.4},{},{:.4},{:.4},{:.4},{:.4},{:.4},{:.4},{:.4},{:.4}\n",
            window.window_index,
            window.train_start,
            window.train_end,
            window.test_start,
            window.test_end,
            window.threshold,
            components,
            window.train.selected_n,
            window.train.total_pnl_after_cost,
            window.test.selected_n,
            window.test.executable_fill_rate,
            window.test.rejection_rate,
            window.test.total_pnl_after_cost,
            window.test.avg_pnl_after_cost,
            window.test.sharpe,
            window.test.max_drawdown,
            window.test.by_symbol_positive_ratio,
            window.test.by_time_bucket_positive_ratio,
        ));
    }
    out
}

pub fn format_fillability_review_v1_report(
    report: &FillabilityReviewReport,
    top_n: usize,
) -> String {
    let mut out = String::new();
    out.push_str("=== Fillability Review V1 Data Health ===\n");
    out.push_str(&format!(
        "source_obs={} v2_rows={} executable_pnl_rows={} entry_fill_rate={:.2}% exit_fill_rate={:.2}% rejection_rate={:.2}%\n",
        report.health.source_observations,
        report.health.v2_rows,
        report.health.executable_pnl_rows,
        report.health.entry_fill_rate() * 100.0,
        report.health.exit_fill_rate() * 100.0,
        report.health.rejection_rate() * 100.0,
    ));
    out.push_str(&format!(
        "stake_usd={:.2} min_bucket_obs={} min_entry_fill={:.2} min_roundtrip_fill={:.2} max_reject={:.2}\n\n",
        report.options.review.stake_usd,
        report.options.min_bucket_observations,
        report.options.min_entry_fill_rate,
        report.options.min_roundtrip_fill_rate,
        report.options.max_rejection_rate,
    ));
    out.push_str("decision,dimension,bucket,n,coverage,entry_fill,exit_fill,roundtrip_fill,reject_rate,avg_entry_cap,avg_exit_cap,avg_entry_liq,avg_exit_liq,avg_pm_spread_bps,avg_pm_lag_secs,avg_slippage_bps,avg_roundtrip_cost,total_pnl,avg_pnl,reason\n");
    for row in report.rows.iter().take(top_n.max(1)) {
        out.push_str(&format!(
            "{},{},{},{},{:.4},{:.4},{:.4},{:.4},{:.4},{:.4},{:.4},{:.4},{:.4},{:.2},{:.2},{:.2},{:.4},{:.4},{:.4},{}\n",
            row.decision.as_str(),
            row.dimension,
            row.bucket,
            row.n,
            row.coverage,
            row.entry_fill_rate,
            row.exit_fill_rate,
            row.roundtrip_fill_rate,
            row.rejection_rate,
            row.avg_entry_capacity_ratio,
            row.avg_exit_capacity_ratio,
            row.avg_entry_liquidity_usd,
            row.avg_exit_liquidity_usd,
            row.avg_pm_spread_bps,
            row.avg_pm_lag_secs,
            row.avg_slippage_to_fill_bps,
            row.avg_roundtrip_cost_usd,
            row.total_executable_pnl_after_cost,
            row.avg_executable_pnl_after_cost,
            row.reason,
        ));
    }
    out
}

pub fn format_liquidity_gate_v1_report(report: &LiquidityGateV1Report) -> String {
    let mut out = String::new();
    out.push_str("=== Liquidity Gate V1 Report ===\n");
    out.push_str(&format!(
        "source_obs={} v2_rows={} selected={} coverage={:.4} stake_usd={:.2}\n",
        report.health.source_observations,
        report.health.v2_rows,
        report.selected_n,
        report.coverage,
        report.options.review.stake_usd,
    ));
    out.push_str(&format!(
        "min_entry_cap={:.2} min_exit_cap={:.2} max_pm_lag_secs={:.2} max_pm_spread_bps={:.2} min_time_remaining_secs={} entry_ask=[{:.2},{:.2}]\n",
        report.options.min_entry_capacity_ratio,
        report.options.min_exit_capacity_ratio,
        report.options.max_pm_lag_secs,
        report.options.max_pm_spread_bps,
        report.options.min_time_remaining_secs,
        report.options.min_entry_ask,
        report.options.max_entry_ask,
    ));
    out.push_str("entry_fill,exit_fill,roundtrip_fill,reject_rate,avg_entry_cap,avg_exit_cap,avg_pm_spread_bps,avg_pm_lag_secs,avg_roundtrip_cost,total_pnl,avg_pnl,sharpe,max_dd,symbol_pos,time_bucket_pos\n");
    out.push_str(&format!(
        "{:.4},{:.4},{:.4},{:.4},{:.4},{:.4},{:.2},{:.2},{:.4},{:.4},{:.4},{:.4},{:.4},{:.4},{:.4}\n",
        report.entry_fill_rate,
        report.exit_fill_rate,
        report.roundtrip_fill_rate,
        report.rejection_rate,
        report.avg_entry_capacity_ratio,
        report.avg_exit_capacity_ratio,
        report.avg_pm_spread_bps,
        report.avg_pm_lag_secs,
        report.avg_roundtrip_cost_usd,
        report.metrics.total_pnl_after_cost,
        report.metrics.avg_pnl_after_cost,
        report.metrics.sharpe,
        report.metrics.max_drawdown,
        report.metrics.by_symbol_positive_ratio,
        report.metrics.by_time_bucket_positive_ratio,
    ));
    out
}

pub fn format_liquidity_gated_alpha_v1_report(
    report: &LiquidityGatedAlphaV1Report,
    top_n: usize,
) -> String {
    let mut out = String::new();
    out.push_str("=== Liquidity-Gated Alpha V1 Data Health ===\n");
    out.push_str(&format!(
        "baseline_v2_rows={} baseline_entry_fill={:.2}% baseline_reject={:.2}% gated_rows={} gate_coverage={:.4} gate_entry_fill={:.2}% gate_roundtrip_fill={:.2}% gate_reject={:.2}%\n",
        report.baseline_health.v2_rows,
        report.baseline_health.entry_fill_rate() * 100.0,
        report.baseline_health.rejection_rate() * 100.0,
        report.gate.selected_n,
        report.gate.coverage,
        report.gate.entry_fill_rate * 100.0,
        report.gate.roundtrip_fill_rate * 100.0,
        report.gate.rejection_rate * 100.0,
    ));
    out.push_str(&format!(
        "stake_usd={:.2} train_days={} test_days={} step_days={} top_quantile={:.2} factor_name_filter={}\n\n",
        report.options.walk_forward.review.stake_usd,
        report.options.walk_forward.train_window_days,
        report.options.walk_forward.test_window_days,
        report.options.walk_forward.step_days,
        report.options.walk_forward.review.top_quantile,
        report
            .options
            .walk_forward
            .factor_name_filter
            .as_deref()
            .unwrap_or("<none>"),
    ));

    out.push_str("=== Liquidity-Gated Single-Factor Reviews ===\n");
    out.push_str("factor,family,layer,n,coverage,settle_rank_ic,pnl_rank_ic,selected_n,fill_rate,reject_rate,total_pnl,avg_pnl,sharpe,max_dd,symbol_pos,time_bucket_pos\n");
    for review in report.review.reviews.iter().take(top_n.max(1)) {
        out.push_str(&format!(
            "{},{},{},{},{:.4},{:.4},{:.4},{},{:.4},{:.4},{:.4},{:.4},{:.4},{:.4},{:.4},{:.4}\n",
            review.factor,
            review.family.as_str(),
            review.layer.as_str(),
            review.n,
            review.coverage,
            review.settlement_rank_ic,
            review.executable_pnl_rank_ic,
            review.selected_n,
            review.selected_executable_fill_rate,
            review.selected_rejection_rate,
            review.selected_total_pnl_after_cost,
            review.selected_avg_pnl_after_cost,
            review.selected_sharpe,
            review.selected_max_drawdown,
            review.by_symbol_positive_ratio,
            review.by_time_bucket_positive_ratio,
        ));
    }

    out.push_str("\n=== Liquidity-Gated Walk-Forward Aggregates ===\n");
    out.push_str("factor,family,layer,windows,pos_window_ratio,total_test_pnl,avg_window_pnl,min_window_pnl,avg_fill_rate,avg_reject_rate\n");
    for aggregate in report.walk_forward.aggregates.iter().take(top_n.max(1)) {
        out.push_str(&format!(
            "{},{},{},{},{:.4},{:.4},{:.4},{:.4},{:.4},{:.4}\n",
            aggregate.factor,
            aggregate.family.as_str(),
            aggregate.layer.as_str(),
            aggregate.windows,
            aggregate.positive_window_ratio,
            aggregate.total_test_pnl_after_cost,
            aggregate.avg_test_pnl_per_window,
            aggregate.min_test_pnl_after_cost,
            aggregate.avg_test_fill_rate,
            aggregate.avg_test_rejection_rate,
        ));
    }

    out.push_str("\n=== Liquidity-Gated Factor Stability ===\n");
    out.push_str("decision,factor,family,layer,windows,pnl_ic_mean,pnl_icir,pos_window_ratio,total_test_pnl,avg_fill_rate,avg_reject_rate,symbol_pos,time_bucket_pos,reason\n");
    for row in report.stability.rows.iter().take(top_n.max(1)) {
        out.push_str(&format!(
            "{},{},{},{},{},{:.4},{:.4},{:.4},{:.4},{:.4},{:.4},{:.4},{:.4},{}\n",
            row.decision.as_str(),
            row.factor,
            row.family.as_str(),
            row.layer.as_str(),
            row.windows,
            row.executable_pnl_rank_ic_mean,
            row.executable_pnl_rank_icir,
            row.positive_window_ratio,
            row.total_test_pnl_after_cost,
            row.avg_test_fill_rate,
            row.avg_test_rejection_rate,
            row.avg_by_symbol_positive_ratio,
            row.avg_by_time_bucket_positive_ratio,
            row.reason,
        ));
    }
    out
}

pub fn format_trade_formation_v1_report(report: &TradeFormationReviewReport) -> String {
    let mut out = String::new();
    out.push_str("=== Trade Formation Review V1 Data Health ===\n");
    out.push_str(&format!(
        "source_obs={} v2_rows={} executable_pnl_rows={} baseline_entry_fill={:.2}% baseline_reject={:.2}%\n",
        report.health.source_observations,
        report.health.v2_rows,
        report.health.executable_pnl_rows,
        report.health.entry_fill_rate() * 100.0,
        report.health.rejection_rate() * 100.0,
    ));
    out.push_str(&format!(
        "gated_rows={} gate_coverage={:.4} gate_entry_fill={:.2}% gate_roundtrip_fill={:.2}% gate_reject={:.2}% profitable_gated={} losing_gated={} missed_winners={}\n",
        report.gated_rows,
        report.gate.coverage,
        report.gate.entry_fill_rate * 100.0,
        report.gate.roundtrip_fill_rate * 100.0,
        report.gate.rejection_rate * 100.0,
        report.profitable_gated_rows,
        report.losing_gated_rows,
        report.missed_winner_rows,
    ));
    out.push_str(&format!(
        "stake_usd={:.2} min_path_obs={} top_n={}\n\n",
        report.options.review.stake_usd, report.options.min_path_observations, report.options.top_n,
    ));
    push_trade_formation_path_section(
        &mut out,
        "Profitable Paths",
        &report.profitable_paths,
        report.options.top_n,
    );
    push_trade_formation_path_section(
        &mut out,
        "Losing Paths",
        &report.losing_paths,
        report.options.top_n,
    );
    push_trade_formation_path_section(
        &mut out,
        "Missed Winner Paths",
        &report.missed_winner_paths,
        report.options.top_n,
    );

    out.push_str("\n=== Meta-Label Rule Candidates ===\n");
    out.push_str("rule,n,coverage,win_rate,total_pnl,avg_pnl,sharpe,avg_future_exit_pnl_30s\n");
    for row in report
        .meta_label_rules
        .iter()
        .take(report.options.top_n.max(1))
    {
        out.push_str(&format!(
            "{},{},{:.4},{:.4},{:.4},{:.4},{:.4},{:.4}\n",
            row.rule,
            row.n,
            row.coverage,
            row.win_rate,
            row.total_pnl_after_cost,
            row.avg_pnl_after_cost,
            row.sharpe,
            row.avg_future_exit_pnl_30s,
        ));
    }
    out
}

fn push_trade_formation_path_section(
    out: &mut String,
    title: &str,
    rows: &[TradeFormationPathRow],
    top_n: usize,
) {
    out.push_str(&format!("\n=== {title} ===\n"));
    out.push_str("path,n,coverage,executable_rows,win_rate,settle_win_rate,total_pnl,avg_pnl,sharpe,avg_model_prob,avg_distance_sigma,avg_obi10,avg_obi_persist,avg_continuation,avg_entry_cap,avg_exit_cap,avg_pm_spread,avg_future_exit_pnl_30s,avg_future_bid_chg_30s,avg_future_bid_chg_60s\n");
    for row in rows.iter().take(top_n.max(1)) {
        out.push_str(&format!(
            "{},{},{:.4},{},{:.4},{:.4},{:.4},{:.4},{:.4},{:.4},{:.4},{:.4},{:.4},{:.4},{:.4},{:.4},{:.2},{:.4},{:.4},{:.4}\n",
            row.path,
            row.n,
            row.coverage,
            row.executable_rows,
            row.win_rate,
            row.settlement_win_rate,
            row.total_pnl_after_cost,
            row.avg_pnl_after_cost,
            row.sharpe,
            row.avg_side_model_prob,
            row.avg_side_distance_over_sigma,
            row.avg_obi_10_side,
            row.avg_obi_persistence_30s_side,
            row.avg_cex_continuation_score_side,
            row.avg_entry_capacity_ratio,
            row.avg_exit_capacity_ratio,
            row.avg_pm_spread_bps,
            row.avg_future_exit_pnl_30s,
            row.avg_future_exit_bid_change_30s,
            row.avg_future_exit_bid_change_60s,
        ));
    }
}

pub fn format_meta_label_walk_forward_v1_report(report: &MetaLabelWalkForwardReport) -> String {
    let mut out = String::new();
    out.push_str("=== Meta-Label Walk-Forward V1 Data Health ===\n");
    out.push_str(&format!(
        "source_obs={} v2_rows={} executable_pnl_rows={} baseline_entry_fill={:.2}% baseline_reject={:.2}%\n",
        report.health.source_observations,
        report.health.v2_rows,
        report.health.executable_pnl_rows,
        report.health.entry_fill_rate() * 100.0,
        report.health.rejection_rate() * 100.0,
    ));
    out.push_str(&format!(
        "gate_selected={} gate_coverage={:.4} gate_entry_fill={:.2}% gate_roundtrip_fill={:.2}% gate_reject={:.2}%\n",
        report.gate.selected_n,
        report.gate.coverage,
        report.gate.entry_fill_rate * 100.0,
        report.gate.roundtrip_fill_rate * 100.0,
        report.gate.rejection_rate * 100.0,
    ));
    out.push_str(&format!(
        "train_days={} test_days={} step_days={} min_rule_obs={} min_candidate_windows={} min_candidate_pos_window_ratio={:.2} min_candidate_avg_selected={:.2} stake_usd={:.2}\n\n",
        report.options.train_window_days,
        report.options.test_window_days,
        report.options.step_days,
        report.options.min_rule_observations,
        report.options.min_candidate_windows,
        report.options.min_candidate_positive_window_ratio,
        report.options.min_candidate_avg_test_selected,
        report.options.review.stake_usd,
    ));

    out.push_str("=== Meta-Label Walk-Forward Aggregates ===\n");
    out.push_str("rule,decision,reason,windows,pos_window_ratio,total_test_pnl,avg_window_pnl,min_window_pnl,avg_train_selected,avg_test_selected,avg_test_fill,avg_test_reject\n");
    for aggregate in report.aggregates.iter().take(report.options.top_n.max(1)) {
        out.push_str(&format!(
            "{},{},{},{},{:.4},{:.4},{:.4},{:.4},{:.2},{:.2},{:.4},{:.4}\n",
            aggregate.rule,
            aggregate.decision.as_str(),
            aggregate.reason,
            aggregate.windows,
            aggregate.positive_window_ratio,
            aggregate.total_test_pnl_after_cost,
            aggregate.avg_test_pnl_per_window,
            aggregate.min_test_pnl_after_cost,
            aggregate.avg_train_selected,
            aggregate.avg_test_selected,
            aggregate.avg_test_fill_rate,
            aggregate.avg_test_rejection_rate,
        ));
    }

    out.push_str("\n=== Meta-Label Walk-Forward Windows ===\n");
    out.push_str("window,rule,train_start,train_end,test_start,test_end,train_selected,train_pnl,train_avg_pnl,train_sharpe,test_selected,test_fill,test_reject,test_pnl,test_avg_pnl,test_sharpe,test_max_dd,symbol_pos,time_bucket_pos\n");
    for window in &report.windows {
        out.push_str(&format!(
            "{},{},{},{},{},{},{},{:.4},{:.4},{:.4},{},{:.4},{:.4},{:.4},{:.4},{:.4},{:.4},{:.4},{:.4}\n",
            window.window_index,
            window.rule,
            window.train_start,
            window.train_end,
            window.test_start,
            window.test_end,
            window.train.selected_n,
            window.train.total_pnl_after_cost,
            window.train.avg_pnl_after_cost,
            window.train.sharpe,
            window.test.selected_n,
            window.test.executable_fill_rate,
            window.test.rejection_rate,
            window.test.total_pnl_after_cost,
            window.test.avg_pnl_after_cost,
            window.test.sharpe,
            window.test.max_drawdown,
            window.test.by_symbol_positive_ratio,
            window.test.by_time_bucket_positive_ratio,
        ));
    }
    out
}

pub fn format_factor_review_v2_report(report: &FactorReviewV2Report, top_n: usize) -> String {
    let mut out = String::new();
    out.push_str("=== Factor Review V2 Data Health ===\n");
    out.push_str(&format!(
        "source_obs={} v2_rows={} settlement_labels={} executable_pnl_rows={} deribit_rows={}\n",
        report.health.source_observations,
        report.health.v2_rows,
        report.health.settlement_label_rows,
        report.health.executable_pnl_rows,
        report.health.deribit_rows,
    ));
    out.push_str(&format!(
        "entry_quote_rows={} entry_size_rows={} entry_fill_rate={:.2}% rejection_rate={:.2}%\n",
        report.health.entry_quote_rows,
        report.health.entry_size_rows,
        report.health.entry_fill_rate() * 100.0,
        report.health.rejection_rate() * 100.0,
    ));
    out.push_str(&format!(
        "exit_quote_rows={} exit_size_rows={} exit_fill_rate={:.2}% avg_pm_lag_secs={:.2}\n",
        report.health.exit_quote_rows,
        report.health.exit_size_rows,
        report.health.exit_fill_rate() * 100.0,
        report.health.avg_pm_lag_secs,
    ));
    out.push_str(&format!(
        "full_depth_entry_fill_rate={:.2}% full_depth_exit_fill_rate={:.2}% full_depth_pnl_rows={} avg_entry_sweep_slip_bps={:.2} avg_exit_sweep_slip_bps={:.2}\n",
        report.health.full_depth_entry_fill_rate() * 100.0,
        report.health.full_depth_exit_fill_rate() * 100.0,
        report.health.full_depth_executable_pnl_rows,
        report.health.avg_entry_sweep_slippage_bps,
        report.health.avg_exit_sweep_slippage_bps,
    ));
    out.push_str(&format!(
        "stake_usd={:.2} top_quantile={:.2} min_observations={}\n\n",
        report.options.stake_usd, report.options.top_quantile, report.options.min_observations,
    ));

    out.push_str("=== Top Single-Factor Reviews By Executable PnL ===\n");
    out.push_str("factor,family,layer,n,coverage,settle_rank_ic,pnl_rank_ic,selected_n,fill_rate,reject_rate,total_pnl,avg_pnl,sharpe,max_dd,symbol_pos,time_bucket_pos\n");
    for review in report.reviews.iter().take(top_n) {
        out.push_str(&format!(
            "{},{},{},{},{:.4},{:.4},{:.4},{},{:.4},{:.4},{:.4},{:.4},{:.4},{:.4},{:.4},{:.4}\n",
            review.factor,
            review.family.as_str(),
            review.layer.as_str(),
            review.n,
            review.coverage,
            review.settlement_rank_ic,
            review.executable_pnl_rank_ic,
            review.selected_n,
            review.selected_executable_fill_rate,
            review.selected_rejection_rate,
            review.selected_total_pnl_after_cost,
            review.selected_avg_pnl_after_cost,
            review.selected_sharpe,
            review.selected_max_drawdown,
            review.by_symbol_positive_ratio,
            review.by_time_bucket_positive_ratio,
        ));
    }
    out
}

fn build_trade_formation_path_rows(
    rows: &[&FactorObservationV2],
    denominator: usize,
    min_observations: usize,
) -> Vec<TradeFormationPathRow> {
    let mut buckets: BTreeMap<String, Vec<&FactorObservationV2>> = BTreeMap::new();
    for row in rows {
        buckets
            .entry(trade_formation_path(row))
            .or_default()
            .push(row);
    }
    let mut out = buckets
        .into_iter()
        .filter_map(|(path, bucket_rows)| {
            (bucket_rows.len() >= min_observations)
                .then(|| build_trade_formation_path_row(path, &bucket_rows, denominator))
        })
        .collect::<Vec<_>>();
    out.sort_by(|a, b| {
        b.total_pnl_after_cost
            .partial_cmp(&a.total_pnl_after_cost)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| b.n.cmp(&a.n))
    });
    out
}

fn build_trade_formation_path_row(
    path: String,
    rows: &[&FactorObservationV2],
    denominator: usize,
) -> TradeFormationPathRow {
    let pnl_values = rows
        .iter()
        .filter_map(|row| executable_pnl(row))
        .collect::<Vec<_>>();
    let total_pnl = pnl_values.iter().sum::<f64>();
    let wins = pnl_values.iter().filter(|pnl| **pnl > 0.0).count();
    let settlement_wins = rows
        .iter()
        .filter(|row| row.label_settlement_win.is_some_and(|label| label >= 0.5))
        .count();
    TradeFormationPathRow {
        path,
        n: rows.len(),
        coverage: ratio(rows.len(), denominator),
        executable_rows: pnl_values.len(),
        win_rate: ratio(wins, pnl_values.len()),
        settlement_win_rate: ratio(settlement_wins, rows.len()),
        total_pnl_after_cost: total_pnl,
        avg_pnl_after_cost: if pnl_values.is_empty() {
            f64::NAN
        } else {
            total_pnl / pnl_values.len() as f64
        },
        sharpe: trade_sharpe(&pnl_values),
        avg_side_model_prob: mean(rows.iter().map(|row| row.side_model_prob)),
        avg_side_distance_over_sigma: mean(rows.iter().map(|row| row.side_distance_over_sigma)),
        avg_obi_10_side: mean(rows.iter().map(|row| row.obi_10 * row.side.multiplier())),
        avg_obi_persistence_30s_side: mean(rows.iter().map(|row| row.obi_persistence_30s_side)),
        avg_cex_continuation_score_side: mean(
            rows.iter().map(|row| row.cex_continuation_score_side),
        ),
        avg_entry_capacity_ratio: mean(rows.iter().map(|row| row.entry_capacity_ratio)),
        avg_exit_capacity_ratio: mean(rows.iter().map(|row| row.exit_capacity_ratio)),
        avg_pm_spread_bps: mean(rows.iter().map(|row| row.pm_spread_bps)),
        avg_future_exit_pnl_30s: mean(rows.iter().filter_map(|row| row.label_future_exit_pnl_30s)),
        avg_future_exit_bid_change_30s: mean(
            rows.iter()
                .filter_map(|row| row.label_future_exit_bid_change_30s),
        ),
        avg_future_exit_bid_change_60s: mean(
            rows.iter()
                .filter_map(|row| row.label_future_exit_bid_change_60s),
        ),
    }
}

fn build_trade_formation_rule_rows(
    rows: &[&FactorObservationV2],
    min_observations: usize,
) -> Vec<TradeFormationRuleRow> {
    let mut out = meta_label_rule_specs()
        .into_iter()
        .filter_map(|spec| {
            let selected = rows
                .iter()
                .copied()
                .filter(|row| (spec.predicate)(row))
                .collect::<Vec<_>>();
            (selected.len() >= min_observations).then(|| {
                build_trade_formation_rule_row(spec.name.to_string(), &selected, rows.len())
            })
        })
        .collect::<Vec<_>>();
    out.sort_by(|a, b| {
        b.total_pnl_after_cost
            .partial_cmp(&a.total_pnl_after_cost)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                b.win_rate
                    .partial_cmp(&a.win_rate)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
    });
    out
}

#[derive(Clone, Copy)]
struct MetaLabelRuleSpec {
    name: &'static str,
    predicate: fn(&FactorObservationV2) -> bool,
}

fn meta_label_rule_specs() -> Vec<MetaLabelRuleSpec> {
    vec![
        MetaLabelRuleSpec {
            name: "liquidity_gate_only",
            predicate: |_| true,
        },
        MetaLabelRuleSpec {
            name: "strong_direction",
            predicate: strong_direction_rule,
        },
        MetaLabelRuleSpec {
            name: "cex_obi_confirmation",
            predicate: cex_obi_confirmation_rule,
        },
        MetaLabelRuleSpec {
            name: "continuation_confirmation",
            predicate: continuation_confirmation_rule,
        },
        MetaLabelRuleSpec {
            name: "deribit_iv_confirmation",
            predicate: deribit_iv_confirmation_rule,
        },
        MetaLabelRuleSpec {
            name: "strong_direction_and_cex_obi",
            predicate: |row| strong_direction_rule(row) && cex_obi_confirmation_rule(row),
        },
        MetaLabelRuleSpec {
            name: "strong_direction_and_continuation",
            predicate: |row| strong_direction_rule(row) && continuation_confirmation_rule(row),
        },
        MetaLabelRuleSpec {
            name: "cex_obi_and_continuation",
            predicate: |row| cex_obi_confirmation_rule(row) && continuation_confirmation_rule(row),
        },
        MetaLabelRuleSpec {
            name: "strong_direction_cex_and_continuation",
            predicate: |row| {
                strong_direction_rule(row)
                    && cex_obi_confirmation_rule(row)
                    && continuation_confirmation_rule(row)
            },
        },
    ]
}

fn build_trade_formation_rule_row(
    rule: String,
    rows: &[&FactorObservationV2],
    denominator: usize,
) -> TradeFormationRuleRow {
    let pnl_values = rows
        .iter()
        .filter_map(|row| executable_pnl(row))
        .collect::<Vec<_>>();
    let total_pnl = pnl_values.iter().sum::<f64>();
    let wins = pnl_values.iter().filter(|pnl| **pnl > 0.0).count();
    TradeFormationRuleRow {
        rule,
        n: rows.len(),
        coverage: ratio(rows.len(), denominator),
        win_rate: ratio(wins, pnl_values.len()),
        total_pnl_after_cost: total_pnl,
        avg_pnl_after_cost: if pnl_values.is_empty() {
            f64::NAN
        } else {
            total_pnl / pnl_values.len() as f64
        },
        sharpe: trade_sharpe(&pnl_values),
        avg_future_exit_pnl_30s: mean(rows.iter().filter_map(|row| row.label_future_exit_pnl_30s)),
    }
}

fn trade_formation_path(row: &FactorObservationV2) -> String {
    format!(
        "direction={}|cex={}|continuation={}|pm={}|deribit={}|time={}",
        direction_bin(row),
        cex_obi_bin(row),
        continuation_bin(row),
        pm_execution_bin(row),
        deribit_bin(row),
        time_remaining_bin(row),
    )
}

fn executable_pnl(row: &FactorObservationV2) -> Option<f64> {
    row.label_full_depth_executable_pnl_15u
        .or(row.label_executable_pnl_15u)
        .filter(|pnl| pnl.is_finite())
}

fn entry_fillable(row: &FactorObservationV2) -> bool {
    row.label_full_depth_entry_fillable || row.label_executable_fillable
}

fn exit_fillable(row: &FactorObservationV2) -> bool {
    row.label_full_depth_exit_fillable || row.label_exit_fillable
}

fn roundtrip_fillable(row: &FactorObservationV2) -> bool {
    (row.label_full_depth_entry_fillable && row.label_full_depth_exit_fillable)
        || (row.label_executable_fillable && row.label_exit_fillable)
}

fn direction_bin(row: &FactorObservationV2) -> &'static str {
    let distance = row.side_distance_over_sigma;
    if row.side_model_prob >= 0.70 || distance >= 1.0 {
        "strong_model"
    } else if row.side_model_prob >= 0.60 || distance >= 0.50 {
        "medium_model"
    } else {
        "weak_model"
    }
}

fn cex_obi_bin(row: &FactorObservationV2) -> &'static str {
    let obi_side = row.obi_10 * row.side.multiplier();
    if row.obi_persistence_30s_side >= 0.75 && obi_side > 0.0 {
        "persistent_obi"
    } else if row.depth_imbalance * row.side.multiplier() >= 0.25 {
        "depth_confirmed"
    } else if row.obi_flip_count_60s >= 2.0 {
        "obi_unstable"
    } else {
        "obi_neutral"
    }
}

fn continuation_bin(row: &FactorObservationV2) -> &'static str {
    if row.cex_continuation_score_side >= 0.75
        || (row.cex_bar_return_30s * row.side.multiplier() > 0.0
            && row.cex_signed_volume_ratio_30s * row.side.multiplier() > 0.0)
    {
        "continuation_confirmed"
    } else if row.cex_continuation_score_side <= -0.75 {
        "continuation_against"
    } else {
        "continuation_neutral"
    }
}

fn pm_execution_bin(row: &FactorObservationV2) -> &'static str {
    let min_cap = row.entry_capacity_ratio.min(row.exit_capacity_ratio);
    if min_cap >= 5.0 && row.pm_spread_bps <= 300.0 {
        "deep_tight"
    } else if min_cap >= 1.0 && row.pm_spread_bps <= 500.0 {
        "fillable_tight"
    } else if min_cap >= 1.0 {
        "fillable_wide"
    } else {
        "thin"
    }
}

fn deribit_bin(row: &FactorObservationV2) -> &'static str {
    if !row.deribit_iv_change_60s.is_finite() {
        "deribit_missing"
    } else if row.deribit_iv_change_60s >= 0.02 {
        "iv_up"
    } else if row.deribit_iv_change_60s <= -0.02 {
        "iv_down"
    } else {
        "iv_flat"
    }
}

fn time_remaining_bin(row: &FactorObservationV2) -> &'static str {
    if row.time_remaining_secs >= 240 {
        "early"
    } else if row.time_remaining_secs >= 120 {
        "middle"
    } else {
        "late"
    }
}

fn strong_direction_rule(row: &FactorObservationV2) -> bool {
    matches!(direction_bin(row), "strong_model")
}

fn cex_obi_confirmation_rule(row: &FactorObservationV2) -> bool {
    matches!(cex_obi_bin(row), "persistent_obi" | "depth_confirmed")
}

fn continuation_confirmation_rule(row: &FactorObservationV2) -> bool {
    matches!(continuation_bin(row), "continuation_confirmed")
}

fn deribit_iv_confirmation_rule(row: &FactorObservationV2) -> bool {
    matches!(deribit_bin(row), "iv_up" | "iv_down")
}

struct FillabilityBucketSpec {
    dimension: &'static str,
    bucket: fn(&FactorObservationV2) -> Option<String>,
}

fn fillability_bucket_specs() -> Vec<FillabilityBucketSpec> {
    vec![
        FillabilityBucketSpec {
            dimension: "symbol",
            bucket: |row| Some(row.symbol.clone()),
        },
        FillabilityBucketSpec {
            dimension: "side",
            bucket: |row| Some(row.side.as_str().to_string()),
        },
        FillabilityBucketSpec {
            dimension: "regime",
            bucket: |row| Some(row.regime.as_str().to_string()),
        },
        FillabilityBucketSpec {
            dimension: "time_remaining_secs",
            bucket: |row| {
                bucket_value(row.time_remaining_secs as f64, &[60.0, 120.0, 180.0, 240.0])
            },
        },
        FillabilityBucketSpec {
            dimension: "entry_ask",
            bucket: |row| bucket_value(row.entry_ask, &[0.05, 0.10, 0.15, 0.25, 0.40, 0.60, 0.80]),
        },
        FillabilityBucketSpec {
            dimension: "pm_spread_bps",
            bucket: |row| {
                bucket_value(
                    row.pm_spread_bps,
                    &[250.0, 500.0, 1_000.0, 2_000.0, 4_000.0],
                )
            },
        },
        FillabilityBucketSpec {
            dimension: "pm_lag_secs",
            bucket: |row| bucket_value(row.pm_lag_secs, &[1.0, 3.0, 10.0, 30.0, 60.0]),
        },
        FillabilityBucketSpec {
            dimension: "entry_capacity_ratio",
            bucket: |row| bucket_value(row.entry_capacity_ratio, &[0.25, 0.50, 1.0, 2.0, 5.0]),
        },
        FillabilityBucketSpec {
            dimension: "exit_capacity_ratio",
            bucket: |row| bucket_value(row.exit_capacity_ratio, &[0.25, 0.50, 1.0, 2.0, 5.0]),
        },
        FillabilityBucketSpec {
            dimension: "min_entry_exit_capacity_ratio",
            bucket: |row| {
                bucket_value(
                    row.entry_capacity_ratio.min(row.exit_capacity_ratio),
                    &[0.25, 0.50, 1.0, 2.0, 5.0],
                )
            },
        },
        FillabilityBucketSpec {
            dimension: "entry_liquidity_usd",
            bucket: |row| {
                bucket_value(
                    row.entry_liquidity_usd,
                    &[5.0, 10.0, 15.0, 30.0, 75.0, 150.0],
                )
            },
        },
        FillabilityBucketSpec {
            dimension: "exit_liquidity_usd",
            bucket: |row| {
                bucket_value(
                    row.exit_liquidity_usd,
                    &[5.0, 10.0, 15.0, 30.0, 75.0, 150.0],
                )
            },
        },
        FillabilityBucketSpec {
            dimension: "cex_spread_bps",
            bucket: |row| bucket_value(row.cex_spread_bps, &[1.0, 3.0, 5.0, 10.0, 20.0]),
        },
        FillabilityBucketSpec {
            dimension: "obi_10_side",
            bucket: |row| {
                bucket_value(
                    row.obi_10 * row.side.multiplier(),
                    &[-0.5, -0.1, 0.0, 0.1, 0.5],
                )
            },
        },
        FillabilityBucketSpec {
            dimension: "depth_imbalance_side",
            bucket: |row| {
                bucket_value(
                    row.depth_imbalance * row.side.multiplier(),
                    &[-0.5, -0.1, 0.0, 0.1, 0.5],
                )
            },
        },
        FillabilityBucketSpec {
            dimension: "cex_continuation_score_side",
            bucket: |row| {
                bucket_value(
                    row.cex_continuation_score_side,
                    &[-1.0, -0.25, 0.0, 0.25, 1.0, 2.0],
                )
            },
        },
        FillabilityBucketSpec {
            dimension: "cex_bar_volume_ratio_30s",
            bucket: |row| bucket_value(row.cex_bar_volume_ratio_30s, &[0.5, 1.0, 1.5, 2.5, 5.0]),
        },
        FillabilityBucketSpec {
            dimension: "deribit_mark_iv",
            bucket: |row| bucket_value(row.deribit_mark_iv, &[0.30, 0.50, 0.75, 1.00, 1.50]),
        },
        FillabilityBucketSpec {
            dimension: "deribit_iv_change_30s",
            bucket: |row| bucket_value(row.deribit_iv_change_30s, &[-0.05, -0.01, 0.0, 0.01, 0.05]),
        },
    ]
}

fn build_fillability_bucket_row(
    dimension: &str,
    bucket: String,
    rows: &[&FactorObservationV2],
    total_rows: usize,
    options: &FillabilityReviewOptions,
) -> FillabilityBucketRow {
    let entry_filled = rows.iter().filter(|row| entry_fillable(row)).count();
    let exit_filled = rows.iter().filter(|row| exit_fillable(row)).count();
    let roundtrip_filled = rows.iter().filter(|row| roundtrip_fillable(row)).count();
    let executable_pnls = rows
        .iter()
        .filter_map(|row| executable_pnl(row))
        .collect::<Vec<_>>();
    let total_executable_pnl_after_cost = executable_pnls.iter().sum::<f64>();
    let entry_fill_rate = ratio(entry_filled, rows.len());
    let roundtrip_fill_rate = ratio(roundtrip_filled, rows.len());
    let rejection_rate = 1.0 - entry_fill_rate;
    let (decision, reason) = fillability_decision(
        rows.len(),
        entry_fill_rate,
        roundtrip_fill_rate,
        rejection_rate,
        options,
    );
    FillabilityBucketRow {
        dimension: dimension.to_string(),
        bucket,
        n: rows.len(),
        coverage: ratio(rows.len(), total_rows),
        entry_fill_rate,
        exit_fill_rate: ratio(exit_filled, rows.len()),
        roundtrip_fill_rate,
        rejection_rate,
        avg_entry_capacity_ratio: mean(rows.iter().map(|row| row.entry_capacity_ratio)),
        avg_exit_capacity_ratio: mean(rows.iter().map(|row| row.exit_capacity_ratio)),
        avg_entry_liquidity_usd: mean(rows.iter().map(|row| row.entry_liquidity_usd)),
        avg_exit_liquidity_usd: mean(rows.iter().map(|row| row.exit_liquidity_usd)),
        avg_pm_spread_bps: mean(rows.iter().map(|row| row.pm_spread_bps)),
        avg_pm_lag_secs: mean(rows.iter().map(|row| row.pm_lag_secs)),
        avg_slippage_to_fill_bps: mean(rows.iter().map(|row| row.slippage_to_fill_15u_bps)),
        avg_roundtrip_cost_usd: mean(rows.iter().map(|row| row.roundtrip_cost_usd)),
        total_executable_pnl_after_cost,
        avg_executable_pnl_after_cost: if executable_pnls.is_empty() {
            f64::NAN
        } else {
            total_executable_pnl_after_cost / executable_pnls.len() as f64
        },
        decision,
        reason,
    }
}

fn fillability_decision(
    n: usize,
    entry_fill_rate: f64,
    roundtrip_fill_rate: f64,
    rejection_rate: f64,
    options: &FillabilityReviewOptions,
) -> (FillabilityDecision, String) {
    if n < options.min_bucket_observations {
        return (
            FillabilityDecision::Reject,
            "too_few_observations".to_string(),
        );
    }
    if entry_fill_rate < options.min_entry_fill_rate {
        return (
            FillabilityDecision::Reject,
            "low_entry_fill_rate".to_string(),
        );
    }
    if rejection_rate > options.max_rejection_rate {
        return (
            FillabilityDecision::Reject,
            "high_rejection_rate".to_string(),
        );
    }
    if roundtrip_fill_rate < options.min_roundtrip_fill_rate {
        return (
            FillabilityDecision::Watchlist,
            "entry_fill_ok_but_roundtrip_weak".to_string(),
        );
    }
    (FillabilityDecision::Candidate, "passed".to_string())
}

fn fillability_decision_rank(decision: FillabilityDecision) -> usize {
    match decision {
        FillabilityDecision::Candidate => 3,
        FillabilityDecision::Watchlist => 2,
        FillabilityDecision::Reject => 1,
    }
}

fn liquidity_gate_v1_accepts(row: &FactorObservationV2, options: &LiquidityGateV1Options) -> bool {
    valid_price(row.entry_ask)
        && row.entry_ask >= options.min_entry_ask
        && row.entry_ask <= options.max_entry_ask
        && row.time_remaining_secs >= options.min_time_remaining_secs
        && row.pm_lag_secs.is_finite()
        && row.pm_lag_secs <= options.max_pm_lag_secs
        && row.pm_spread_bps.is_finite()
        && row.pm_spread_bps <= options.max_pm_spread_bps
        && row.entry_capacity_ratio.is_finite()
        && row.entry_capacity_ratio >= options.min_entry_capacity_ratio
        && row.exit_capacity_ratio.is_finite()
        && row.exit_capacity_ratio >= options.min_exit_capacity_ratio
}

fn selection_metrics_for_rows(
    total_rows: usize,
    selected: &[&FactorObservationV2],
) -> FactorSelectionMetrics {
    let filled = selected
        .iter()
        .copied()
        .filter(|row| executable_pnl(row).is_some())
        .collect::<Vec<_>>();
    let pnls = filled
        .iter()
        .filter_map(|row| executable_pnl(row).map(|pnl| (row.tick_ts, pnl)))
        .collect::<Vec<_>>();
    let pnl_values = pnls.iter().map(|(_, pnl)| *pnl).collect::<Vec<_>>();
    let total_pnl = pnl_values.iter().sum::<f64>();
    FactorSelectionMetrics {
        n: total_rows,
        selected_n: selected.len(),
        executable_fill_rate: ratio(filled.len(), selected.len()),
        rejection_rate: ratio(
            selected.iter().filter(|row| !entry_fillable(row)).count(),
            selected.len(),
        ),
        total_pnl_after_cost: total_pnl,
        avg_pnl_after_cost: if pnl_values.is_empty() {
            f64::NAN
        } else {
            total_pnl / pnl_values.len() as f64
        },
        sharpe: trade_sharpe(&pnl_values),
        max_drawdown: max_drawdown(&pnls),
        by_symbol_positive_ratio: positive_group_ratio(&filled, |row| row.symbol.clone()),
        by_time_bucket_positive_ratio: positive_group_ratio(&filled, |row| {
            row.regime.as_str().to_string()
        }),
    }
}

fn bucket_value(value: f64, edges: &[f64]) -> Option<String> {
    if !value.is_finite() {
        return None;
    }
    let first = *edges.first()?;
    if value < first {
        return Some(format!("lt_{}", bucket_num(first)));
    }
    for pair in edges.windows(2) {
        let lo = pair[0];
        let hi = pair[1];
        if value >= lo && value < hi {
            return Some(format!("{}_{}", bucket_num(lo), bucket_num(hi)));
        }
    }
    edges
        .last()
        .map(|last| format!("gte_{}", bucket_num(*last)))
}

fn bucket_num(value: f64) -> String {
    let raw = if value.abs() >= 100.0 {
        format!("{value:.0}")
    } else if value.abs() >= 10.0 {
        format!("{value:.1}")
    } else {
        format!("{value:.2}")
    };
    raw.replace('-', "m").replace('.', "p")
}

fn stability_decision(
    windows: usize,
    positive_window_ratio: f64,
    total_test_pnl_after_cost: f64,
    avg_test_fill_rate: f64,
    avg_test_rejection_rate: f64,
    avg_by_symbol_positive_ratio: f64,
    avg_by_time_bucket_positive_ratio: f64,
    executable_pnl_rank_icir: f64,
    options: &FactorStabilityOptions,
) -> (FactorStabilityDecision, String) {
    if windows < options.min_windows {
        return if total_test_pnl_after_cost > options.min_total_test_pnl_after_cost {
            (
                FactorStabilityDecision::Watchlist,
                "too_few_windows_positive_pnl".to_string(),
            )
        } else {
            (
                FactorStabilityDecision::Reject,
                "too_few_windows_nonpositive_pnl".to_string(),
            )
        };
    }
    if total_test_pnl_after_cost <= options.min_total_test_pnl_after_cost {
        return (
            FactorStabilityDecision::Reject,
            "nonpositive_executable_pnl".to_string(),
        );
    }
    if positive_window_ratio < options.min_positive_window_ratio {
        return (
            FactorStabilityDecision::Watchlist,
            "positive_pnl_but_unstable_windows".to_string(),
        );
    }
    if avg_test_fill_rate < options.min_avg_fill_rate {
        return (
            FactorStabilityDecision::Watchlist,
            "positive_pnl_but_low_fill_rate".to_string(),
        );
    }
    if avg_test_rejection_rate > options.max_avg_rejection_rate {
        return (
            FactorStabilityDecision::Watchlist,
            "positive_pnl_but_high_rejection".to_string(),
        );
    }
    if avg_by_symbol_positive_ratio < options.min_by_symbol_positive_ratio {
        return (
            FactorStabilityDecision::Watchlist,
            "positive_pnl_but_symbol_unstable".to_string(),
        );
    }
    if avg_by_time_bucket_positive_ratio < options.min_by_time_bucket_positive_ratio {
        return (
            FactorStabilityDecision::Watchlist,
            "positive_pnl_but_regime_unstable".to_string(),
        );
    }
    if !executable_pnl_rank_icir.is_finite()
        || executable_pnl_rank_icir.abs() < options.min_abs_executable_pnl_icir
    {
        return (
            FactorStabilityDecision::Watchlist,
            "positive_pnl_but_low_executable_icir".to_string(),
        );
    }
    (FactorStabilityDecision::Candidate, "passed".to_string())
}

fn decision_rank(decision: FactorStabilityDecision) -> usize {
    match decision {
        FactorStabilityDecision::Candidate => 3,
        FactorStabilityDecision::Watchlist => 2,
        FactorStabilityDecision::Reject => 1,
    }
}

fn fit_combo_v1_window(
    train_rows: &[&FactorObservationV2],
    test_rows: &[&FactorObservationV2],
    descriptors: &[FactorV2Descriptor],
    options: &FactorComboV1Options,
    window_index: usize,
    train_start: DateTime<Utc>,
    train_end: DateTime<Utc>,
    test_start: DateTime<Utc>,
    test_end: DateTime<Utc>,
) -> Option<FactorComboV1Window> {
    let mut by_family: BTreeMap<&'static str, Vec<FactorComboComponent>> = BTreeMap::new();
    for descriptor in descriptors {
        if let Some(component) = fit_combo_component(
            train_rows,
            *descriptor,
            options.walk_forward.review.min_observations,
            options.min_abs_train_executable_pnl_rank_ic,
        ) {
            by_family
                .entry(component.family.as_str())
                .or_default()
                .push(component);
        }
    }

    let mut components = Vec::new();
    for family_components in by_family.values_mut() {
        family_components.sort_by(|a, b| {
            b.train_executable_pnl_rank_ic
                .abs()
                .partial_cmp(&a.train_executable_pnl_rank_ic.abs())
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        components.extend(
            family_components
                .iter()
                .take(options.max_factors_per_family.max(1))
                .cloned(),
        );
    }
    components.sort_by(|a, b| {
        b.train_executable_pnl_rank_ic
            .abs()
            .partial_cmp(&a.train_executable_pnl_rank_ic.abs())
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    components.truncate(options.max_total_factors.max(1));
    if components.is_empty() {
        return None;
    }

    let mut train_scores = train_rows
        .iter()
        .filter_map(|row| combo_v1_score(row, &components))
        .filter(|score| score.is_finite())
        .collect::<Vec<_>>();
    if train_scores.len() < options.walk_forward.review.min_observations {
        return None;
    }
    train_scores.sort_by(|a, b| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));
    let selected_n = ((train_scores.len() as f64)
        * options.walk_forward.review.top_quantile.clamp(0.01, 1.0))
    .ceil()
    .max(1.0) as usize;
    let threshold = train_scores[selected_n.min(train_scores.len()) - 1];
    let train = evaluate_combo_v1_threshold(train_rows, &components, threshold);
    let test = evaluate_combo_v1_threshold(test_rows, &components, threshold);
    Some(FactorComboV1Window {
        window_index,
        train_start,
        train_end,
        test_start,
        test_end,
        threshold,
        components,
        train,
        test,
    })
}

fn fit_combo_component(
    rows: &[&FactorObservationV2],
    descriptor: FactorV2Descriptor,
    min_observations: usize,
    min_abs_train_ic: f64,
) -> Option<FactorComboComponent> {
    let scored: Vec<(&FactorObservationV2, f64)> = rows
        .iter()
        .filter_map(|row| {
            let value = (descriptor.accessor)(row);
            value.is_finite().then_some((*row, value))
        })
        .collect();
    if scored.len() < min_observations {
        return None;
    }
    let executable_pairs: Vec<(f64, f64)> = scored
        .iter()
        .filter_map(|(row, score)| executable_pnl(row).map(|label| (*score, label)))
        .filter(|(score, label)| score.is_finite() && label.is_finite())
        .collect();
    if executable_pairs.len() < min_observations {
        return None;
    }
    let train_executable_pnl_rank_ic = pair_spearman(&executable_pairs);
    if !train_executable_pnl_rank_ic.is_finite()
        || train_executable_pnl_rank_ic.abs() < min_abs_train_ic
    {
        return None;
    }
    let values = scored.iter().map(|(_, value)| *value).collect::<Vec<_>>();
    let train_mean = mean(values.iter().copied());
    let train_std = stddev(&values);
    if !train_std.is_finite() || train_std <= EPS {
        return None;
    }
    Some(FactorComboComponent {
        factor: descriptor.name.to_string(),
        family: descriptor.family,
        layer: descriptor.layer,
        accessor: descriptor.accessor,
        direction: train_executable_pnl_rank_ic.signum(),
        train_executable_pnl_rank_ic,
        train_mean,
        train_std,
    })
}

fn evaluate_combo_v1_threshold(
    rows: &[&FactorObservationV2],
    components: &[FactorComboComponent],
    threshold: f64,
) -> FactorSelectionMetrics {
    let scored_n = rows
        .iter()
        .filter(|row| combo_v1_score(row, components).is_some())
        .count();
    let selected: Vec<&FactorObservationV2> = rows
        .iter()
        .copied()
        .filter(|row| {
            combo_v1_score(row, components)
                .map(|score| score >= threshold)
                .unwrap_or(false)
        })
        .collect();
    let filled: Vec<&FactorObservationV2> = selected
        .iter()
        .copied()
        .filter(|row| executable_pnl(row).is_some())
        .collect();
    let pnls: Vec<(DateTime<Utc>, f64)> = filled
        .iter()
        .filter_map(|row| executable_pnl(row).map(|pnl| (row.tick_ts, pnl)))
        .collect();
    let pnl_values: Vec<f64> = pnls.iter().map(|(_, pnl)| *pnl).collect();
    let total_pnl = pnl_values.iter().sum::<f64>();
    FactorSelectionMetrics {
        n: scored_n,
        selected_n: selected.len(),
        executable_fill_rate: ratio(filled.len(), selected.len()),
        rejection_rate: ratio(
            selected.iter().filter(|row| !entry_fillable(row)).count(),
            selected.len(),
        ),
        total_pnl_after_cost: total_pnl,
        avg_pnl_after_cost: if pnl_values.is_empty() {
            f64::NAN
        } else {
            total_pnl / pnl_values.len() as f64
        },
        sharpe: trade_sharpe(&pnl_values),
        max_drawdown: max_drawdown(&pnls),
        by_symbol_positive_ratio: positive_group_ratio(&filled, |row| row.symbol.clone()),
        by_time_bucket_positive_ratio: positive_group_ratio(&filled, |row| {
            row.regime.as_str().to_string()
        }),
    }
}

fn combo_v1_score(row: &FactorObservationV2, components: &[FactorComboComponent]) -> Option<f64> {
    let mut by_family: BTreeMap<&'static str, Vec<f64>> = BTreeMap::new();
    for component in components {
        let value = (component.accessor)(row);
        if !value.is_finite() || !component.train_std.is_finite() || component.train_std <= EPS {
            continue;
        }
        let z = ((value - component.train_mean) / component.train_std).clamp(-5.0, 5.0);
        by_family
            .entry(component.family.as_str())
            .or_default()
            .push(z * component.direction);
    }
    if by_family.is_empty() {
        return None;
    }
    let family_scores = by_family
        .values()
        .map(|values| mean(values.iter().copied()))
        .filter(|value| value.is_finite())
        .collect::<Vec<_>>();
    if family_scores.is_empty() {
        None
    } else {
        Some(mean(family_scores.iter().copied()))
    }
}

fn aggregate_combo_v1_windows(windows: &[FactorComboV1Window]) -> FactorComboV1Aggregate {
    if windows.is_empty() {
        return FactorComboV1Aggregate {
            windows: 0,
            positive_window_ratio: f64::NAN,
            total_test_pnl_after_cost: 0.0,
            avg_test_pnl_per_window: f64::NAN,
            min_test_pnl_after_cost: f64::NAN,
            avg_test_fill_rate: f64::NAN,
            avg_test_rejection_rate: f64::NAN,
            avg_component_count: f64::NAN,
        };
    }
    let total_test_pnl_after_cost = windows
        .iter()
        .map(|window| window.test.total_pnl_after_cost)
        .sum::<f64>();
    let min_test_pnl_after_cost = windows
        .iter()
        .map(|window| window.test.total_pnl_after_cost)
        .fold(f64::INFINITY, f64::min);
    let positive_windows = windows
        .iter()
        .filter(|window| window.test.total_pnl_after_cost > 0.0)
        .count();
    FactorComboV1Aggregate {
        windows: windows.len(),
        positive_window_ratio: ratio(positive_windows, windows.len()),
        total_test_pnl_after_cost,
        avg_test_pnl_per_window: total_test_pnl_after_cost / windows.len() as f64,
        min_test_pnl_after_cost,
        avg_test_fill_rate: mean(
            windows
                .iter()
                .map(|window| window.test.executable_fill_rate),
        ),
        avg_test_rejection_rate: mean(windows.iter().map(|window| window.test.rejection_rate)),
        avg_component_count: mean(windows.iter().map(|window| window.components.len() as f64)),
    }
}

fn evaluate_meta_label_rule(
    rows: &[&FactorObservationV2],
    predicate: fn(&FactorObservationV2) -> bool,
) -> FactorSelectionMetrics {
    let selected = rows
        .iter()
        .copied()
        .filter(|row| predicate(row))
        .collect::<Vec<_>>();
    let filled = selected
        .iter()
        .copied()
        .filter(|row| executable_pnl(row).is_some())
        .collect::<Vec<_>>();
    let pnls = filled
        .iter()
        .filter_map(|row| executable_pnl(row).map(|pnl| (row.tick_ts, pnl)))
        .collect::<Vec<_>>();
    let pnl_values = pnls.iter().map(|(_, pnl)| *pnl).collect::<Vec<_>>();
    let total_pnl = pnl_values.iter().sum::<f64>();

    FactorSelectionMetrics {
        n: rows.len(),
        selected_n: selected.len(),
        executable_fill_rate: ratio(filled.len(), selected.len()),
        rejection_rate: ratio(
            selected.iter().filter(|row| !entry_fillable(row)).count(),
            selected.len(),
        ),
        total_pnl_after_cost: total_pnl,
        avg_pnl_after_cost: if pnl_values.is_empty() {
            f64::NAN
        } else {
            total_pnl / pnl_values.len() as f64
        },
        sharpe: trade_sharpe(&pnl_values),
        max_drawdown: max_drawdown(&pnls),
        by_symbol_positive_ratio: positive_group_ratio(&filled, |row| row.symbol.clone()),
        by_time_bucket_positive_ratio: positive_group_ratio(&filled, |row| {
            row.regime.as_str().to_string()
        }),
    }
}

fn aggregate_meta_label_windows(
    windows: &[MetaLabelWalkForwardWindow],
    options: &MetaLabelWalkForwardOptions,
) -> Vec<MetaLabelWalkForwardAggregate> {
    let mut grouped: BTreeMap<&str, Vec<&MetaLabelWalkForwardWindow>> = BTreeMap::new();
    for window in windows {
        grouped
            .entry(window.rule.as_str())
            .or_default()
            .push(window);
    }

    let mut out = grouped
        .into_iter()
        .map(|(rule, rule_windows)| {
            let total_test_pnl_after_cost = rule_windows
                .iter()
                .map(|window| window.test.total_pnl_after_cost)
                .sum::<f64>();
            let min_test_pnl_after_cost = rule_windows
                .iter()
                .map(|window| window.test.total_pnl_after_cost)
                .fold(f64::INFINITY, f64::min);
            let positive_windows = rule_windows
                .iter()
                .filter(|window| window.test.total_pnl_after_cost > 0.0)
                .count();
            let avg_test_selected = mean(
                rule_windows
                    .iter()
                    .map(|window| window.test.selected_n as f64),
            );
            let avg_test_fill_rate = mean(
                rule_windows
                    .iter()
                    .map(|window| window.test.executable_fill_rate),
            );
            let avg_test_rejection_rate =
                mean(rule_windows.iter().map(|window| window.test.rejection_rate));
            let (decision, reason) = meta_label_readiness_decision(
                rule_windows.len(),
                positive_windows,
                total_test_pnl_after_cost,
                min_test_pnl_after_cost,
                avg_test_selected,
                avg_test_fill_rate,
                avg_test_rejection_rate,
                options,
            );
            MetaLabelWalkForwardAggregate {
                rule: rule.to_string(),
                windows: rule_windows.len(),
                positive_window_ratio: ratio(positive_windows, rule_windows.len()),
                total_test_pnl_after_cost,
                avg_test_pnl_per_window: total_test_pnl_after_cost / rule_windows.len() as f64,
                min_test_pnl_after_cost,
                avg_train_selected: mean(
                    rule_windows
                        .iter()
                        .map(|window| window.train.selected_n as f64),
                ),
                avg_test_selected,
                avg_test_fill_rate,
                avg_test_rejection_rate,
                decision,
                reason,
            }
        })
        .collect::<Vec<_>>();
    out.sort_by(|a, b| {
        decision_rank(b.decision)
            .cmp(&decision_rank(a.decision))
            .then_with(|| {
                b.total_test_pnl_after_cost
                    .partial_cmp(&a.total_test_pnl_after_cost)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| {
                b.positive_window_ratio
                    .partial_cmp(&a.positive_window_ratio)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
    });
    out
}

fn meta_label_readiness_decision(
    windows: usize,
    positive_windows: usize,
    total_test_pnl_after_cost: f64,
    min_test_pnl_after_cost: f64,
    avg_test_selected: f64,
    avg_test_fill_rate: f64,
    avg_test_rejection_rate: f64,
    options: &MetaLabelWalkForwardOptions,
) -> (FactorStabilityDecision, String) {
    if windows < options.min_candidate_windows {
        return if total_test_pnl_after_cost > options.min_candidate_total_test_pnl_after_cost {
            (
                FactorStabilityDecision::Watchlist,
                "too_few_oos_windows_positive_pnl".to_string(),
            )
        } else {
            (
                FactorStabilityDecision::Reject,
                "too_few_oos_windows_nonpositive_pnl".to_string(),
            )
        };
    }
    if total_test_pnl_after_cost <= options.min_candidate_total_test_pnl_after_cost {
        return (
            FactorStabilityDecision::Reject,
            "nonpositive_oos_executable_pnl".to_string(),
        );
    }
    if ratio(positive_windows, windows) < options.min_candidate_positive_window_ratio {
        return (
            FactorStabilityDecision::Watchlist,
            "positive_pnl_but_unstable_oos_windows".to_string(),
        );
    }
    if avg_test_selected < options.min_candidate_avg_test_selected {
        return (
            FactorStabilityDecision::Watchlist,
            "positive_pnl_but_low_oos_sample".to_string(),
        );
    }
    if avg_test_fill_rate < options.min_candidate_avg_fill_rate {
        return (
            FactorStabilityDecision::Watchlist,
            "positive_pnl_but_low_fill_rate".to_string(),
        );
    }
    if avg_test_rejection_rate > options.max_candidate_avg_rejection_rate {
        return (
            FactorStabilityDecision::Watchlist,
            "positive_pnl_but_high_rejection".to_string(),
        );
    }
    if min_test_pnl_after_cost < options.min_candidate_worst_window_pnl_after_cost {
        return (
            FactorStabilityDecision::Watchlist,
            "positive_pnl_but_large_worst_window_loss".to_string(),
        );
    }
    (FactorStabilityDecision::Candidate, "passed".to_string())
}

fn fit_walk_forward_factor(
    train_rows: &[&FactorObservationV2],
    test_rows: &[&FactorObservationV2],
    descriptor: FactorV2Descriptor,
    options: &FactorReviewOptions,
    window_index: usize,
    train_start: DateTime<Utc>,
    train_end: DateTime<Utc>,
    test_start: DateTime<Utc>,
    test_end: DateTime<Utc>,
) -> Option<FactorWalkForwardWindow> {
    let scored: Vec<(&FactorObservationV2, f64)> = train_rows
        .iter()
        .filter_map(|row| {
            let value = (descriptor.accessor)(row);
            value.is_finite().then_some((*row, value))
        })
        .collect();
    if scored.len() < options.min_observations {
        return None;
    }

    let settlement_pairs: Vec<(f64, f64)> = scored
        .iter()
        .filter_map(|(row, score)| row.label_settlement_win.map(|label| (*score, label)))
        .filter(|(score, label)| score.is_finite() && label.is_finite())
        .collect();
    let executable_pairs: Vec<(f64, f64)> = scored
        .iter()
        .filter_map(|(row, score)| executable_pnl(row).map(|label| (*score, label)))
        .filter(|(score, label)| score.is_finite() && label.is_finite())
        .collect();
    let train_settlement_rank_ic = pair_spearman(&settlement_pairs);
    let train_executable_pnl_rank_ic = pair_spearman(&executable_pairs);
    let direction = if train_executable_pnl_rank_ic.is_finite() {
        train_executable_pnl_rank_ic.signum()
    } else if train_settlement_rank_ic.is_finite() {
        train_settlement_rank_ic.signum()
    } else {
        1.0
    };
    let direction = if direction.abs() <= EPS {
        1.0
    } else {
        direction
    };

    let mut directed_scores: Vec<f64> = scored
        .iter()
        .map(|(_, score)| *score * direction)
        .filter(|score| score.is_finite())
        .collect();
    if directed_scores.is_empty() {
        return None;
    }
    directed_scores.sort_by(|a, b| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));
    let selected_n = ((directed_scores.len() as f64) * options.top_quantile.clamp(0.01, 1.0))
        .ceil()
        .max(1.0) as usize;
    let threshold = directed_scores[selected_n.min(directed_scores.len()) - 1];

    let train = evaluate_factor_threshold(train_rows, descriptor, direction, threshold);
    let test = evaluate_factor_threshold(test_rows, descriptor, direction, threshold);
    Some(FactorWalkForwardWindow {
        window_index,
        train_start,
        train_end,
        test_start,
        test_end,
        factor: descriptor.name.to_string(),
        family: descriptor.family,
        layer: descriptor.layer,
        direction,
        threshold,
        train_settlement_rank_ic,
        train_executable_pnl_rank_ic,
        train,
        test,
    })
}

fn is_walk_forward_candidate_descriptor(descriptor: &FactorV2Descriptor) -> bool {
    // `future_exit_*` descriptors are diagnostic labels for exit feasibility.
    // They are intentionally reviewed in the single-window report, but using
    // them as train-time selection factors would leak future PM quote movement.
    !descriptor.name.starts_with("future_exit_")
}

fn factor_name_matches_filter(name: &str, filter: &Option<String>) -> bool {
    let Some(filter) = filter.as_deref() else {
        return true;
    };
    filter
        .split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .any(|part| name.contains(part))
}

fn walk_forward_time_slice(
    rows: &[FactorObservationV2],
    start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> &[FactorObservationV2] {
    let lo = rows.partition_point(|row| row.tick_ts < start);
    let hi = rows.partition_point(|row| row.tick_ts < end);
    &rows[lo..hi]
}

fn evaluate_factor_threshold(
    rows: &[&FactorObservationV2],
    descriptor: FactorV2Descriptor,
    direction: f64,
    threshold: f64,
) -> FactorSelectionMetrics {
    let scored_n = rows
        .iter()
        .filter(|row| (descriptor.accessor)(row).is_finite())
        .count();
    let selected: Vec<&FactorObservationV2> = rows
        .iter()
        .copied()
        .filter(|row| {
            let value = (descriptor.accessor)(row);
            value.is_finite() && value * direction >= threshold
        })
        .collect();
    let filled: Vec<&FactorObservationV2> = selected
        .iter()
        .copied()
        .filter(|row| executable_pnl(row).is_some())
        .collect();
    let pnls: Vec<(DateTime<Utc>, f64)> = filled
        .iter()
        .filter_map(|row| executable_pnl(row).map(|pnl| (row.tick_ts, pnl)))
        .collect();
    let pnl_values: Vec<f64> = pnls.iter().map(|(_, pnl)| *pnl).collect();
    let total_pnl = pnl_values.iter().sum::<f64>();
    FactorSelectionMetrics {
        n: scored_n,
        selected_n: selected.len(),
        executable_fill_rate: ratio(filled.len(), selected.len()),
        rejection_rate: ratio(
            selected.iter().filter(|row| !entry_fillable(row)).count(),
            selected.len(),
        ),
        total_pnl_after_cost: total_pnl,
        avg_pnl_after_cost: if pnl_values.is_empty() {
            f64::NAN
        } else {
            total_pnl / pnl_values.len() as f64
        },
        sharpe: trade_sharpe(&pnl_values),
        max_drawdown: max_drawdown(&pnls),
        by_symbol_positive_ratio: positive_group_ratio(&filled, |row| row.symbol.clone()),
        by_time_bucket_positive_ratio: positive_group_ratio(&filled, |row| {
            row.regime.as_str().to_string()
        }),
    }
}

fn aggregate_walk_forward_windows(
    windows: &[FactorWalkForwardWindow],
) -> Vec<FactorWalkForwardAggregate> {
    let mut grouped: BTreeMap<String, Vec<&FactorWalkForwardWindow>> = BTreeMap::new();
    for window in windows {
        grouped
            .entry(window.factor.clone())
            .or_default()
            .push(window);
    }
    let mut aggregates = Vec::with_capacity(grouped.len());
    for (factor, rows) in grouped {
        let Some(first) = rows.first() else {
            continue;
        };
        let total_test_pnl_after_cost = rows
            .iter()
            .map(|row| row.test.total_pnl_after_cost)
            .sum::<f64>();
        let min_test_pnl_after_cost = rows
            .iter()
            .map(|row| row.test.total_pnl_after_cost)
            .fold(f64::INFINITY, f64::min);
        let positive_windows = rows
            .iter()
            .filter(|row| row.test.total_pnl_after_cost > 0.0)
            .count();
        aggregates.push(FactorWalkForwardAggregate {
            factor,
            family: first.family,
            layer: first.layer,
            windows: rows.len(),
            positive_window_ratio: ratio(positive_windows, rows.len()),
            total_test_pnl_after_cost,
            avg_test_pnl_per_window: total_test_pnl_after_cost / rows.len() as f64,
            min_test_pnl_after_cost,
            avg_test_fill_rate: mean(rows.iter().map(|row| row.test.executable_fill_rate)),
            avg_test_rejection_rate: mean(rows.iter().map(|row| row.test.rejection_rate)),
        });
    }
    aggregates.sort_by(|a, b| {
        b.total_test_pnl_after_cost
            .partial_cmp(&a.total_test_pnl_after_cost)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                b.positive_window_ratio
                    .partial_cmp(&a.positive_window_ratio)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
    });
    aggregates
}

fn review_one_factor(
    rows: &[FactorObservationV2],
    descriptor: FactorV2Descriptor,
    options: &FactorReviewOptions,
) -> Option<SingleFactorReview> {
    let scored: Vec<(usize, f64)> = rows
        .iter()
        .enumerate()
        .filter_map(|(idx, row)| {
            let value = (descriptor.accessor)(row);
            value.is_finite().then_some((idx, value))
        })
        .collect();
    if scored.len() < options.min_observations {
        return None;
    }

    let settlement_pairs: Vec<(f64, f64)> = scored
        .iter()
        .filter_map(|(idx, score)| rows[*idx].label_settlement_win.map(|label| (*score, label)))
        .filter(|(score, label)| score.is_finite() && label.is_finite())
        .collect();
    let executable_pairs: Vec<(f64, f64)> = scored
        .iter()
        .filter_map(|(idx, score)| executable_pnl(&rows[*idx]).map(|label| (*score, label)))
        .filter(|(score, label)| score.is_finite() && label.is_finite())
        .collect();

    let settlement_pearson_ic = pair_pearson(&settlement_pairs);
    let settlement_rank_ic = pair_spearman(&settlement_pairs);
    let executable_pnl_pearson_ic = pair_pearson(&executable_pairs);
    let executable_pnl_rank_ic = pair_spearman(&executable_pairs);
    let direction = if executable_pnl_rank_ic.is_finite() {
        executable_pnl_rank_ic.signum()
    } else if settlement_rank_ic.is_finite() {
        settlement_rank_ic.signum()
    } else {
        1.0
    };
    let direction = if direction.abs() <= EPS {
        1.0
    } else {
        direction
    };

    let mut directed = scored;
    directed.sort_by(|a, b| {
        (b.1 * direction)
            .partial_cmp(&(a.1 * direction))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let selected_n = ((directed.len() as f64) * options.top_quantile.clamp(0.01, 1.0))
        .ceil()
        .max(1.0) as usize;
    let selected: Vec<&FactorObservationV2> = directed
        .iter()
        .take(selected_n)
        .map(|(idx, _)| &rows[*idx])
        .collect();
    let filled: Vec<&FactorObservationV2> = selected
        .iter()
        .copied()
        .filter(|row| executable_pnl(row).is_some())
        .collect();
    let pnls: Vec<(DateTime<Utc>, f64)> = filled
        .iter()
        .filter_map(|row| executable_pnl(row).map(|pnl| (row.tick_ts, pnl)))
        .collect();
    let pnl_values: Vec<f64> = pnls.iter().map(|(_, pnl)| *pnl).collect();
    let total_pnl = pnl_values.iter().sum::<f64>();

    Some(SingleFactorReview {
        factor: descriptor.name.to_string(),
        family: descriptor.family,
        layer: descriptor.layer,
        n: directed.len(),
        coverage: directed.len() as f64 / rows.len().max(1) as f64,
        settlement_pearson_ic,
        settlement_rank_ic,
        executable_pnl_pearson_ic,
        executable_pnl_rank_ic,
        selected_n: selected.len(),
        selected_rejection_rate: ratio(
            selected.iter().filter(|row| !entry_fillable(row)).count(),
            selected.len(),
        ),
        selected_executable_fill_rate: ratio(filled.len(), selected.len()),
        selected_avg_slippage_bps: mean(selected.iter().map(|row| row.slippage_to_fill_15u_bps)),
        selected_total_pnl_after_cost: total_pnl,
        selected_avg_pnl_after_cost: if pnl_values.is_empty() {
            f64::NAN
        } else {
            total_pnl / pnl_values.len() as f64
        },
        selected_sharpe: trade_sharpe(&pnl_values),
        selected_max_drawdown: max_drawdown(&pnls),
        by_symbol_positive_ratio: positive_group_ratio(&filled, |row| row.symbol.clone()),
        by_time_bucket_positive_ratio: positive_group_ratio(&filled, |row| {
            row.regime.as_str().to_string()
        }),
    })
}

type PmBookIndex<'a> = HashMap<(String, String), Vec<&'a ResearchPmBookSnapshot>>;

#[derive(Debug, Clone, Copy)]
struct SweepFill {
    fillable: bool,
    avg_price: f64,
    shares: f64,
    levels_used: f64,
    slippage_bps: f64,
}

impl Default for SweepFill {
    fn default() -> Self {
        Self {
            fillable: false,
            avg_price: f64::NAN,
            shares: f64::NAN,
            levels_used: f64::NAN,
            slippage_bps: f64::NAN,
        }
    }
}

fn build_pm_book_index(pm_books: &[ResearchPmBookSnapshot]) -> PmBookIndex<'_> {
    let mut index: PmBookIndex<'_> = HashMap::new();
    for book in pm_books {
        index
            .entry((book.event_id.clone(), book.side.to_ascii_uppercase()))
            .or_default()
            .push(book);
    }
    for books in index.values_mut() {
        books.sort_by_key(|book| book.ts);
    }
    index
}

fn latest_pm_book<'a>(
    index: &'a PmBookIndex<'a>,
    event_id: &str,
    side: ReviewSide,
    tick_ts: DateTime<Utc>,
) -> Option<&'a ResearchPmBookSnapshot> {
    let key = (event_id.to_string(), book_side_key(side).to_string());
    let books = index.get(&key)?;
    let pos = books.partition_point(|book| book.ts <= tick_ts);
    let book = *books.get(pos.checked_sub(1)?)?;
    let age_secs = (tick_ts - book.ts).num_seconds();
    (age_secs >= 0 && age_secs <= PM_BOOK_MAX_AGE_SECS).then_some(book)
}

fn book_side_key(side: ReviewSide) -> &'static str {
    match side {
        ReviewSide::Up => "UP",
        ReviewSide::Down => "DOWN",
    }
}

fn sweep_buy_to_stake(
    levels: &[crate::factors::ResearchPmBookLevel],
    reference_price: f64,
    stake_usd: f64,
) -> SweepFill {
    if !valid_price(reference_price) || !stake_usd.is_finite() || stake_usd <= 0.0 {
        return SweepFill::default();
    }
    let mut remaining = stake_usd;
    let mut spent = 0.0;
    let mut shares = 0.0;
    let mut levels_used = 0.0;
    for level in levels
        .iter()
        .filter(|level| valid_price(level.price) && level.size.is_finite() && level.size > 0.0)
    {
        if remaining <= EPS {
            break;
        }
        let level_notional = level.price * level.size;
        let take_notional = remaining.min(level_notional);
        if take_notional <= EPS {
            continue;
        }
        spent += take_notional;
        shares += take_notional / level.price;
        remaining -= take_notional;
        levels_used += 1.0;
    }
    if remaining > EPS || shares <= EPS {
        return SweepFill::default();
    }
    let avg_price = spent / shares;
    SweepFill {
        fillable: true,
        avg_price,
        shares,
        levels_used,
        slippage_bps: ((avg_price - reference_price).max(0.0) / reference_price) * 10_000.0,
    }
}

fn sweep_sell_shares(
    levels: &[crate::factors::ResearchPmBookLevel],
    reference_price: f64,
    shares_to_sell: f64,
) -> SweepFill {
    if !valid_price(reference_price) || !shares_to_sell.is_finite() || shares_to_sell <= 0.0 {
        return SweepFill::default();
    }
    let mut remaining = shares_to_sell;
    let mut proceeds = 0.0;
    let mut sold = 0.0;
    let mut levels_used = 0.0;
    for level in levels
        .iter()
        .filter(|level| valid_price(level.price) && level.size.is_finite() && level.size > 0.0)
    {
        if remaining <= EPS {
            break;
        }
        let take_shares = remaining.min(level.size);
        proceeds += take_shares * level.price;
        sold += take_shares;
        remaining -= take_shares;
        levels_used += 1.0;
    }
    if remaining > EPS || sold <= EPS {
        return SweepFill::default();
    }
    let avg_price = proceeds / sold;
    SweepFill {
        fillable: true,
        avg_price,
        shares: sold,
        levels_used,
        slippage_bps: ((reference_price - avg_price).max(0.0) / reference_price) * 10_000.0,
    }
}

fn side_row(
    row: &FactorObservation,
    side: ReviewSide,
    stake_usd: f64,
    pm_book: Option<&ResearchPmBookSnapshot>,
) -> FactorObservationV2 {
    let (
        side_model_prob,
        side_fair_prob,
        side_model_edge,
        side_distance_over_sigma,
        entry_ask,
        exit_bid,
        entry_ask_size,
        exit_bid_size,
        opposite_ask,
        opposite_bid,
        settlement_win,
    ) = match side {
        ReviewSide::Up => (
            row.model_prob_up,
            row.fair_prob_up_clean,
            row.model_edge_up,
            row.distance_over_sigma,
            row.pm_up_ask,
            row.pm_up_bid,
            row.pm_up_ask_size,
            row.pm_up_bid_size,
            row.pm_down_ask,
            row.pm_down_bid,
            row.settlement_up,
        ),
        ReviewSide::Down => {
            let model_prob_down = if row.model_prob_up.is_finite() {
                1.0 - row.model_prob_up
            } else {
                f64::NAN
            };
            let fair_prob_down = if row.fair_prob_up_clean.is_finite() {
                1.0 - row.fair_prob_up_clean
            } else {
                f64::NAN
            };
            let edge_down = if valid_price(row.pm_down_ask) {
                model_prob_down - row.pm_down_ask - crypto_fee_cost(row.pm_down_ask)
            } else {
                f64::NAN
            };
            (
                model_prob_down,
                fair_prob_down,
                edge_down,
                -row.distance_over_sigma,
                row.pm_down_ask,
                row.pm_down_bid,
                row.pm_down_ask_size,
                row.pm_down_bid_size,
                row.pm_up_ask,
                row.pm_up_bid,
                1.0 - row.settlement_up,
            )
        }
    };

    let fee_per_share = if valid_price(entry_ask) {
        crypto_fee_cost(entry_ask)
    } else {
        f64::NAN
    };
    let entry_shares = if valid_price(entry_ask) {
        stake_usd / entry_ask
    } else {
        f64::NAN
    };
    let entry_fee_usd = entry_shares * fee_per_share;
    let entry_fillable = entry_shares.is_finite()
        && entry_ask_size.is_finite()
        && entry_ask_size + EPS >= entry_shares;
    let exit_fillable = entry_shares.is_finite()
        && exit_bid_size.is_finite()
        && exit_bid_size + EPS >= entry_shares;
    let entry_capacity_ratio = if entry_shares.is_finite() && entry_shares > 0.0 {
        entry_ask_size / entry_shares
    } else {
        f64::NAN
    };
    let exit_capacity_ratio = if entry_shares.is_finite() && entry_shares > 0.0 {
        exit_bid_size / entry_shares
    } else {
        f64::NAN
    };
    let entry_liquidity_usd = if entry_ask_size.is_finite() && valid_price(entry_ask) {
        entry_ask_size * entry_ask
    } else {
        f64::NAN
    };
    let exit_liquidity_usd = if exit_bid_size.is_finite() && valid_price(exit_bid) {
        exit_bid_size * exit_bid
    } else {
        f64::NAN
    };
    let liquidity_shortfall_usd = if entry_shares.is_finite() && entry_ask_size.is_finite() {
        ((entry_shares - entry_ask_size).max(0.0)) * entry_ask
    } else {
        f64::NAN
    };
    let entry_sweep = pm_book
        .map(|book| sweep_buy_to_stake(&book.asks, entry_ask, stake_usd))
        .unwrap_or_default();
    let exit_sweep = if entry_sweep.fillable {
        pm_book
            .map(|book| sweep_sell_shares(&book.bids, exit_bid, entry_sweep.shares))
            .unwrap_or_default()
    } else {
        SweepFill::default()
    };
    let slippage_to_fill_15u_bps = if entry_sweep.fillable {
        entry_sweep.slippage_bps
    } else if entry_fillable {
        0.0
    } else {
        f64::NAN
    };
    let roundtrip_pnl_now_15u = if entry_fillable && exit_fillable && valid_price(exit_bid) {
        Some(entry_shares * exit_bid - stake_usd - entry_fee_usd)
    } else {
        None
    };
    let full_depth_entry_fee_usd = if entry_sweep.fillable && valid_price(entry_sweep.avg_price) {
        entry_sweep.shares * crypto_fee_cost(entry_sweep.avg_price)
    } else {
        f64::NAN
    };
    let roundtrip_pnl_now_full_depth_15u =
        if entry_sweep.fillable && exit_sweep.fillable && full_depth_entry_fee_usd.is_finite() {
            Some(entry_sweep.shares * exit_sweep.avg_price - stake_usd - full_depth_entry_fee_usd)
        } else {
            None
        };
    let roundtrip_cost_usd = if let Some(pnl) = roundtrip_pnl_now_15u {
        -pnl
    } else if valid_price(entry_ask) && valid_price(exit_bid) && entry_shares.is_finite() {
        (stake_usd - entry_shares * exit_bid + entry_fee_usd).max(0.0)
    } else {
        f64::NAN
    };
    let executable_pnl = if entry_fillable && settlement_win.is_finite() {
        Some(if settlement_win >= 0.5 {
            stake_usd * (1.0 / entry_ask - 1.0) - stake_usd * fee_per_share / entry_ask
        } else {
            -stake_usd - stake_usd * fee_per_share / entry_ask
        })
    } else {
        None
    };
    let full_depth_executable_pnl = if entry_sweep.fillable
        && settlement_win.is_finite()
        && full_depth_entry_fee_usd.is_finite()
    {
        Some(if settlement_win >= 0.5 {
            entry_sweep.shares - stake_usd - full_depth_entry_fee_usd
        } else {
            -stake_usd - full_depth_entry_fee_usd
        })
    } else {
        None
    };
    let up_down_ask_sum = if valid_price(row.pm_up_ask) && valid_price(row.pm_down_ask) {
        row.pm_up_ask + row.pm_down_ask
    } else {
        f64::NAN
    };
    let pm_spread_bps = if valid_price(entry_ask) && valid_price(exit_bid) {
        ((entry_ask - exit_bid).max(0.0) / entry_ask) * 10_000.0
    } else {
        f64::NAN
    };
    let side_mult = side.multiplier();
    let cex_consecutive_bar_side = match side {
        ReviewSide::Up => row.cex_consecutive_up_bars,
        ReviewSide::Down => row.cex_consecutive_down_bars,
    };
    let cex_breakout_volume_side = row.cex_breakout_volume_score * side_mult;
    let cex_continuation_score_side = continuation_score(
        row.cex_bar_return_30s * side_mult,
        row.cex_bar_return_60s * side_mult,
        row.cex_signed_volume_ratio_30s * side_mult,
        row.cex_bar_volume_ratio_30s,
        row.cex_bar_volume_trend_3,
        cex_consecutive_bar_side,
    );
    let cex_continuation_edge_gate =
        if side_model_edge.is_finite() && cex_continuation_score_side.is_finite() {
            side_model_edge * cex_continuation_score_side.max(0.0)
        } else {
            f64::NAN
        };
    let executable_capacity_gate = entry_capacity_ratio.min(exit_capacity_ratio).min(2.0);
    let cex_continuation_liquidity_gate =
        if cex_continuation_score_side.is_finite() && executable_capacity_gate.is_finite() {
            cex_continuation_score_side * executable_capacity_gate.max(0.0)
        } else {
            f64::NAN
        };

    FactorObservationV2 {
        event_id: row.event_id.clone(),
        symbol: row.symbol.clone(),
        tick_ts: row.tick_ts,
        time_remaining_secs: row.time_remaining_secs,
        regime: Regime::from_secs(row.time_remaining_secs),
        side,
        side_model_prob,
        side_fair_prob,
        side_model_edge,
        side_distance_over_sigma,
        abs_distance_to_beat: row.abs_distance_to_beat,
        drift_10s: row.drift_10s,
        drift_30s: row.drift_30s,
        post_flip_drift: row.post_flip_drift,
        sigma_horizon: row.sigma_horizon,
        vol_gap: row.vol_gap,
        obi_10: row.obi_10,
        depth_imbalance: row.depth_imbalance,
        depth_acceleration: row.depth_acceleration,
        microprice_offset_bps: row.microprice_offset_bps,
        cex_spread_bps: row.spread_bps,
        cum_mprice_drift_5m: row.cum_mprice_drift_5m,
        cum_trade_imbalance_5m: row.cum_trade_imbalance_5m,
        obi_delta_10s_side: f64::NAN,
        obi_delta_30s_side: f64::NAN,
        obi_persistence_30s_side: f64::NAN,
        obi_flip_count_60s: f64::NAN,
        depth_imbalance_delta_30s_side: f64::NAN,
        microprice_momentum_30s_side: f64::NAN,
        trade_imbalance_delta_10s_side: f64::NAN,
        trade_imbalance_delta_30s_side: f64::NAN,
        cex_bar_return_30s: row.cex_bar_return_30s,
        cex_bar_return_60s: row.cex_bar_return_60s,
        cex_bar_volume_ratio_30s: row.cex_bar_volume_ratio_30s,
        cex_bar_volume_trend_3: row.cex_bar_volume_trend_3,
        cex_signed_volume_ratio_30s: row.cex_signed_volume_ratio_30s,
        cex_consecutive_bar_side,
        cex_breakout_volume_side,
        cex_continuation_score_side,
        cex_continuation_edge_gate,
        cex_continuation_liquidity_gate,
        entry_ask,
        exit_bid,
        entry_ask_size,
        exit_bid_size,
        opposite_ask,
        opposite_bid,
        up_down_ask_sum,
        pm_spread_bps,
        pm_lag_secs: row.pm_lag_secs,
        entry_ask_change_10s: f64::NAN,
        entry_ask_change_30s: f64::NAN,
        exit_bid_change_30s: f64::NAN,
        pm_spread_change_30s: f64::NAN,
        entry_size_change_30s: f64::NAN,
        up_down_ask_sum_change_30s: f64::NAN,
        pm_reprice_speed_30s: f64::NAN,
        pm_quote_stability_30s: f64::NAN,
        deribit_mark_iv: f64::NAN,
        deribit_bid_iv: f64::NAN,
        deribit_ask_iv: f64::NAN,
        deribit_iv_spread: f64::NAN,
        deribit_iv_lag_secs: f64::NAN,
        deribit_iv_horizon: f64::NAN,
        deribit_iv_gap_horizon: f64::NAN,
        deribit_iv_change_30s: f64::NAN,
        deribit_iv_change_60s: f64::NAN,
        deribit_underlying_basis_bps: f64::NAN,
        deribit_delta: f64::NAN,
        deribit_gamma: f64::NAN,
        deribit_vega: f64::NAN,
        deribit_theta: f64::NAN,
        stake_usd,
        entry_shares,
        entry_fee_usd,
        entry_capacity_ratio,
        exit_capacity_ratio,
        entry_liquidity_usd,
        exit_liquidity_usd,
        liquidity_shortfall_usd,
        slippage_to_fill_15u_bps,
        entry_sweep_avg_price_15u: entry_sweep.avg_price,
        exit_sweep_avg_price_15u: exit_sweep.avg_price,
        entry_sweep_shares_15u: entry_sweep.shares,
        exit_sweep_shares_15u: exit_sweep.shares,
        entry_sweep_levels_15u: entry_sweep.levels_used,
        exit_sweep_levels_15u: exit_sweep.levels_used,
        entry_sweep_slippage_bps: entry_sweep.slippage_bps,
        exit_sweep_slippage_bps: exit_sweep.slippage_bps,
        roundtrip_cost_usd,
        roundtrip_pnl_now_15u,
        roundtrip_pnl_now_full_depth_15u,
        portfolio_stake_usd: stake_usd,
        portfolio_event_exposure_usd: stake_usd,
        same_event_observation_count: f64::NAN,
        same_event_side_observation_count: f64::NAN,
        side_is_up: bool_num(side == ReviewSide::Up),
        label_settlement_win: settlement_win.is_finite().then_some(settlement_win),
        label_executable_pnl_15u: executable_pnl,
        label_full_depth_executable_pnl_15u: full_depth_executable_pnl,
        label_executable_fillable: entry_fillable,
        label_exit_fillable: exit_fillable,
        label_full_depth_entry_fillable: entry_sweep.fillable,
        label_full_depth_exit_fillable: exit_sweep.fillable,
        label_future_exit_bid_change_30s: None,
        label_future_exit_bid_change_60s: None,
        label_future_exit_pnl_30s: None,
        label_future_exit_fillable_30s: None,
    }
}

fn enrich_rolling_features(rows: &mut [FactorObservationV2], deribit: &[DeribitFeatureSnapshot]) {
    rows.sort_by_key(|row| {
        (
            row.event_id.clone(),
            row.side.as_str().to_string(),
            row.tick_ts,
        )
    });

    let mut deribit_by_symbol: HashMap<String, Vec<&DeribitFeatureSnapshot>> = HashMap::new();
    for snapshot in deribit {
        deribit_by_symbol
            .entry(normalize_symbol(&snapshot.symbol))
            .or_default()
            .push(snapshot);
    }
    for snapshots in deribit_by_symbol.values_mut() {
        snapshots.sort_by_key(|snapshot| snapshot.ts);
    }

    let mut event_counts: HashMap<String, usize> = HashMap::new();
    let mut event_side_counts: HashMap<(String, &'static str), usize> = HashMap::new();
    let mut groups: BTreeMap<(String, &'static str), Vec<usize>> = BTreeMap::new();
    for (idx, row) in rows.iter().enumerate() {
        groups
            .entry((row.event_id.clone(), row.side.as_str()))
            .or_default()
            .push(idx);
    }

    for indexes in groups.values_mut() {
        indexes.sort_by_key(|idx| rows[*idx].tick_ts);
        for pos in 0..indexes.len() {
            let idx = indexes[pos];
            let ts = rows[idx].tick_ts;
            let event_count = event_counts
                .entry(rows[idx].event_id.clone())
                .and_modify(|count| *count += 1)
                .or_insert(1);
            rows[idx].same_event_observation_count = *event_count as f64;

            let event_side_count = event_side_counts
                .entry((rows[idx].event_id.clone(), rows[idx].side.as_str()))
                .and_modify(|count| *count += 1)
                .or_insert(1);
            rows[idx].same_event_side_observation_count = *event_side_count as f64;

            if let Some(prev_idx) =
                previous_idx_at_or_before(rows, indexes, pos, ts - chrono::Duration::seconds(10))
            {
                let prev = rows[prev_idx].clone();
                rows[idx].entry_ask_change_10s = diff(rows[idx].entry_ask, prev.entry_ask);
                rows[idx].obi_delta_10s_side =
                    diff(rows[idx].obi_10, prev.obi_10) * rows[idx].side.multiplier();
                rows[idx].trade_imbalance_delta_10s_side = diff(
                    rows[idx].cum_trade_imbalance_5m,
                    prev.cum_trade_imbalance_5m,
                ) * rows[idx].side.multiplier();
            }
            if let Some(prev_idx) =
                previous_idx_at_or_before(rows, indexes, pos, ts - chrono::Duration::seconds(30))
            {
                let prev = rows[prev_idx].clone();
                rows[idx].entry_ask_change_30s = diff(rows[idx].entry_ask, prev.entry_ask);
                rows[idx].exit_bid_change_30s = diff(rows[idx].exit_bid, prev.exit_bid);
                rows[idx].pm_spread_change_30s = diff(rows[idx].pm_spread_bps, prev.pm_spread_bps);
                rows[idx].entry_size_change_30s =
                    diff(rows[idx].entry_ask_size, prev.entry_ask_size);
                rows[idx].up_down_ask_sum_change_30s =
                    diff(rows[idx].up_down_ask_sum, prev.up_down_ask_sum);
                rows[idx].pm_reprice_speed_30s = rows[idx].entry_ask_change_30s / 30.0;
                rows[idx].obi_delta_30s_side =
                    diff(rows[idx].obi_10, prev.obi_10) * rows[idx].side.multiplier();
                rows[idx].depth_imbalance_delta_30s_side =
                    diff(rows[idx].depth_imbalance, prev.depth_imbalance)
                        * rows[idx].side.multiplier();
                rows[idx].microprice_momentum_30s_side =
                    diff(rows[idx].microprice_offset_bps, prev.microprice_offset_bps)
                        * rows[idx].side.multiplier();
                rows[idx].trade_imbalance_delta_30s_side = diff(
                    rows[idx].cum_trade_imbalance_5m,
                    prev.cum_trade_imbalance_5m,
                ) * rows[idx].side.multiplier();
            }
            if let Some(future_idx) =
                future_idx_at_or_after(rows, indexes, pos, ts + chrono::Duration::seconds(30))
            {
                let future = rows[future_idx].clone();
                rows[idx].label_future_exit_bid_change_30s =
                    finite_diff(future.exit_bid, rows[idx].exit_bid);
                rows[idx].label_future_exit_fillable_30s =
                    Some(bool_num(future.label_exit_fillable));
                if rows[idx].entry_shares.is_finite() && valid_price(future.exit_bid) {
                    rows[idx].label_future_exit_pnl_30s = Some(
                        rows[idx].entry_shares * future.exit_bid
                            - rows[idx].stake_usd
                            - rows[idx].entry_fee_usd,
                    );
                }
            }
            if let Some(future_idx) =
                future_idx_at_or_after(rows, indexes, pos, ts + chrono::Duration::seconds(60))
            {
                let future = rows[future_idx].clone();
                rows[idx].label_future_exit_bid_change_60s =
                    finite_diff(future.exit_bid, rows[idx].exit_bid);
            }

            rows[idx].pm_quote_stability_30s = quote_stability(rows, indexes, pos, 30);
            rows[idx].obi_persistence_30s_side = side_persistence(rows, indexes, pos, 30, |row| {
                row.obi_10 * row.side.multiplier()
            });
            rows[idx].obi_flip_count_60s = flip_count(rows, indexes, pos, 60, |row| row.obi_10);

            if let Some(snapshot) =
                latest_deribit_snapshot(&deribit_by_symbol, &rows[idx].symbol, rows[idx].tick_ts)
            {
                apply_deribit_snapshot(&mut rows[idx], snapshot);
                if let Some(prev) = latest_deribit_before(
                    &deribit_by_symbol,
                    &rows[idx].symbol,
                    rows[idx].tick_ts - chrono::Duration::seconds(30),
                ) {
                    rows[idx].deribit_iv_change_30s =
                        diff(rows[idx].deribit_mark_iv, normalized_iv(prev.mark_iv));
                }
                if let Some(prev) = latest_deribit_before(
                    &deribit_by_symbol,
                    &rows[idx].symbol,
                    rows[idx].tick_ts - chrono::Duration::seconds(60),
                ) {
                    rows[idx].deribit_iv_change_60s =
                        diff(rows[idx].deribit_mark_iv, normalized_iv(prev.mark_iv));
                }
            }
        }
    }
}

fn previous_idx_at_or_before(
    rows: &[FactorObservationV2],
    indexes: &[usize],
    pos: usize,
    target: DateTime<Utc>,
) -> Option<usize> {
    indexes[..pos]
        .iter()
        .rev()
        .copied()
        .find(|idx| rows[*idx].tick_ts <= target)
}

fn future_idx_at_or_after(
    rows: &[FactorObservationV2],
    indexes: &[usize],
    pos: usize,
    target: DateTime<Utc>,
) -> Option<usize> {
    indexes[pos + 1..]
        .iter()
        .copied()
        .find(|idx| rows[*idx].tick_ts >= target)
}

fn quote_stability(
    rows: &[FactorObservationV2],
    indexes: &[usize],
    pos: usize,
    window_secs: i64,
) -> f64 {
    let current = rows[indexes[pos]].entry_ask;
    if !valid_price(current) {
        return f64::NAN;
    }
    let cutoff = rows[indexes[pos]].tick_ts - chrono::Duration::seconds(window_secs);
    let mut total = 0usize;
    let mut stable = 0usize;
    for idx in indexes[..=pos].iter().rev() {
        let row = &rows[*idx];
        if row.tick_ts < cutoff {
            break;
        }
        if valid_price(row.entry_ask) {
            total += 1;
            if (row.entry_ask - current).abs() <= 0.01 {
                stable += 1;
            }
        }
    }
    ratio(stable, total)
}

fn side_persistence<F>(
    rows: &[FactorObservationV2],
    indexes: &[usize],
    pos: usize,
    window_secs: i64,
    value_fn: F,
) -> f64
where
    F: Fn(&FactorObservationV2) -> f64,
{
    let cutoff = rows[indexes[pos]].tick_ts - chrono::Duration::seconds(window_secs);
    let mut total = 0usize;
    let mut favorable = 0usize;
    for idx in indexes[..=pos].iter().rev() {
        let row = &rows[*idx];
        if row.tick_ts < cutoff {
            break;
        }
        let value = value_fn(row);
        if value.is_finite() {
            total += 1;
            if value > 0.0 {
                favorable += 1;
            }
        }
    }
    ratio(favorable, total)
}

fn flip_count<F>(
    rows: &[FactorObservationV2],
    indexes: &[usize],
    pos: usize,
    window_secs: i64,
    value_fn: F,
) -> f64
where
    F: Fn(&FactorObservationV2) -> f64,
{
    let cutoff = rows[indexes[pos]].tick_ts - chrono::Duration::seconds(window_secs);
    let mut values: Vec<f64> = indexes[..=pos]
        .iter()
        .rev()
        .map(|idx| &rows[*idx])
        .take_while(|row| row.tick_ts >= cutoff)
        .filter_map(|row| {
            let value = signum(value_fn(row));
            (value != 0.0).then_some(value)
        })
        .collect();
    values.reverse();
    values
        .windows(2)
        .filter(|window| window[0] != window[1])
        .count() as f64
}

fn latest_deribit_snapshot<'a>(
    by_symbol: &'a HashMap<String, Vec<&'a DeribitFeatureSnapshot>>,
    symbol: &str,
    ts: DateTime<Utc>,
) -> Option<&'a DeribitFeatureSnapshot> {
    latest_deribit_before(by_symbol, symbol, ts)
}

fn latest_deribit_before<'a>(
    by_symbol: &'a HashMap<String, Vec<&'a DeribitFeatureSnapshot>>,
    symbol: &str,
    ts: DateTime<Utc>,
) -> Option<&'a DeribitFeatureSnapshot> {
    by_symbol
        .get(&normalize_symbol(symbol))?
        .iter()
        .rev()
        .copied()
        .find(|snapshot| snapshot.ts <= ts)
}

fn apply_deribit_snapshot(row: &mut FactorObservationV2, snapshot: &DeribitFeatureSnapshot) {
    let mark_iv = normalized_iv(snapshot.mark_iv);
    let bid_iv = normalized_iv(snapshot.bid_iv);
    let ask_iv = normalized_iv(snapshot.ask_iv);
    row.deribit_mark_iv = mark_iv;
    row.deribit_bid_iv = bid_iv;
    row.deribit_ask_iv = ask_iv;
    row.deribit_iv_spread = finite_diff(ask_iv, bid_iv).unwrap_or(f64::NAN);
    row.deribit_iv_lag_secs = (row.tick_ts - snapshot.ts).num_milliseconds() as f64 / 1000.0;
    row.deribit_iv_horizon = if mark_iv.is_finite() && row.time_remaining_secs > 0 {
        mark_iv * (row.time_remaining_secs as f64 / 31_536_000.0).sqrt()
    } else {
        f64::NAN
    };
    row.deribit_iv_gap_horizon = diff(row.deribit_iv_horizon, row.sigma_horizon);
    row.deribit_underlying_basis_bps = if snapshot.underlying_price.is_finite()
        && snapshot.underlying_price > 0.0
        && row.abs_distance_to_beat.is_finite()
    {
        // FactorObservation no longer carries spot level, so this is intentionally
        // left as a placeholder unless the Deribit snapshot carries a comparable
        // reference in a future schema.
        f64::NAN
    } else {
        f64::NAN
    };
    row.deribit_delta = snapshot.delta;
    row.deribit_gamma = snapshot.gamma;
    row.deribit_vega = snapshot.vega;
    row.deribit_theta = snapshot.theta;
}

fn normalize_symbol(symbol: &str) -> String {
    let upper = symbol.trim().to_ascii_uppercase();
    match upper.as_str() {
        "BTC" | "BTC-PERPETUAL" => "BTCUSDT".to_string(),
        "ETH" | "ETH-PERPETUAL" => "ETHUSDT".to_string(),
        "SOL" | "SOL-PERPETUAL" => "SOLUSDT".to_string(),
        other => other.to_string(),
    }
}

fn normalized_iv(value: f64) -> f64 {
    if !value.is_finite() {
        return f64::NAN;
    }
    if value > 2.0 { value / 100.0 } else { value }
}

fn finite_diff(now: f64, before: f64) -> Option<f64> {
    (now.is_finite() && before.is_finite()).then_some(now - before)
}

fn diff(now: f64, before: f64) -> f64 {
    finite_diff(now, before).unwrap_or(f64::NAN)
}

fn continuation_score(
    return_30s_side: f64,
    return_60s_side: f64,
    signed_volume_side: f64,
    volume_ratio_30s: f64,
    volume_trend_3: f64,
    consecutive_bar_side: f64,
) -> f64 {
    let mut score = 0.0;
    let mut weight = 0.0;
    if return_30s_side.is_finite() {
        score += 3_000.0 * return_30s_side;
        weight += 1.0;
    }
    if return_60s_side.is_finite() {
        score += 1_500.0 * return_60s_side;
        weight += 1.0;
    }
    if signed_volume_side.is_finite() {
        score += signed_volume_side;
        weight += 1.0;
    }
    if volume_ratio_30s.is_finite() {
        score += (volume_ratio_30s - 1.0).clamp(-2.0, 4.0) * 0.5;
        weight += 1.0;
    }
    if volume_trend_3.is_finite() {
        score += volume_trend_3 * 0.5;
        weight += 1.0;
    }
    if consecutive_bar_side.is_finite() {
        score += consecutive_bar_side.min(4.0) * 0.25;
        weight += 1.0;
    }
    if weight > 0.0 {
        score / weight
    } else {
        f64::NAN
    }
}

fn descriptor(
    name: &'static str,
    family: FactorFamily,
    layer: ThreeLayerArchive,
    accessor: fn(&FactorObservationV2) -> f64,
) -> FactorV2Descriptor {
    FactorV2Descriptor {
        name,
        family,
        layer,
        accessor,
    }
}

fn pair_pearson(pairs: &[(f64, f64)]) -> f64 {
    if pairs.len() < 2 {
        return f64::NAN;
    }
    let xs: Vec<f64> = pairs.iter().map(|(x, _)| *x).collect();
    let ys: Vec<f64> = pairs.iter().map(|(_, y)| *y).collect();
    pearson_ic(&xs, &ys)
}

fn pair_spearman(pairs: &[(f64, f64)]) -> f64 {
    if pairs.len() < 2 {
        return f64::NAN;
    }
    let xs: Vec<f64> = pairs.iter().map(|(x, _)| *x).collect();
    let ys: Vec<f64> = pairs.iter().map(|(_, y)| *y).collect();
    spearman_ic(&xs, &ys)
}

fn positive_group_ratio<F>(rows: &[&FactorObservationV2], key_fn: F) -> f64
where
    F: Fn(&FactorObservationV2) -> String,
{
    let mut groups: BTreeMap<String, f64> = BTreeMap::new();
    for row in rows {
        if let Some(pnl) = executable_pnl(row) {
            *groups.entry(key_fn(row)).or_default() += pnl;
        }
    }
    if groups.is_empty() {
        return f64::NAN;
    }
    let positive = groups.values().filter(|pnl| **pnl > 0.0).count();
    positive as f64 / groups.len() as f64
}

fn trade_sharpe(pnls: &[f64]) -> f64 {
    if pnls.len() < 2 {
        return f64::NAN;
    }
    let avg = pnls.iter().sum::<f64>() / pnls.len() as f64;
    let var = pnls.iter().map(|pnl| (pnl - avg).powi(2)).sum::<f64>() / pnls.len() as f64;
    let std = var.sqrt();
    if std <= EPS {
        f64::NAN
    } else {
        avg / std * (pnls.len() as f64).sqrt()
    }
}

fn max_drawdown(pnls: &[(DateTime<Utc>, f64)]) -> f64 {
    let mut ordered = pnls.to_vec();
    ordered.sort_by_key(|(ts, _)| *ts);
    let mut equity = 0.0;
    let mut peak = 0.0;
    let mut max_dd = 0.0;
    for (_, pnl) in ordered {
        equity += pnl;
        if equity > peak {
            peak = equity;
        }
        let dd = peak - equity;
        if dd > max_dd {
            max_dd = dd;
        }
    }
    max_dd
}

fn mean(values: impl Iterator<Item = f64>) -> f64 {
    let vals: Vec<f64> = values.filter(|value| value.is_finite()).collect();
    if vals.is_empty() {
        f64::NAN
    } else {
        vals.iter().sum::<f64>() / vals.len() as f64
    }
}

fn stddev(values: &[f64]) -> f64 {
    let vals = values
        .iter()
        .copied()
        .filter(|value| value.is_finite())
        .collect::<Vec<_>>();
    if vals.len() < 2 {
        return f64::NAN;
    }
    let avg = vals.iter().sum::<f64>() / vals.len() as f64;
    let var = vals.iter().map(|value| (value - avg).powi(2)).sum::<f64>() / vals.len() as f64;
    var.sqrt()
}

fn icir(values: &[f64]) -> f64 {
    let avg = mean(values.iter().copied());
    let std = stddev(values);
    if avg.is_finite() && std.is_finite() && std > EPS {
        avg / std
    } else {
        f64::NAN
    }
}

fn ratio(num: usize, denom: usize) -> f64 {
    if denom == 0 {
        f64::NAN
    } else {
        num as f64 / denom as f64
    }
}

fn valid_price(value: f64) -> bool {
    value.is_finite() && value > 0.0 && value < 1.0
}

fn signum(value: f64) -> f64 {
    if value > 1e-9 {
        1.0
    } else if value < -1e-9 {
        -1.0
    } else {
        0.0
    }
}

fn bool_num(value: bool) -> f64 {
    if value { 1.0 } else { 0.0 }
}

fn crypto_fee_cost(entry_price: f64) -> f64 {
    0.02 * entry_price * (1.0 - entry_price)
}

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use super::*;

    fn base_obs() -> FactorObservation {
        FactorObservation {
            event_id: "evt".into(),
            symbol: "BTCUSDT".into(),
            tick_ts: Utc::now(),
            time_remaining_secs: 220,
            signed_distance_to_beat: 0.01,
            abs_distance_to_beat: 0.01,
            drift_10s: 0.001,
            drift_30s: 0.002,
            flip_age_secs: 10.0,
            post_flip_drift: 0.002,
            sigma_horizon: 0.01,
            fair_prob_up: 0.6,
            fair_prob_up_clean: 0.62,
            prob_disagreement: 0.0,
            implied_sigma_horizon: 0.01,
            vol_gap: 0.0,
            distance_over_sigma: 1.0,
            model_prob_up: 0.7,
            model_edge_up: 0.2,
            reward_risk_up: 1.0,
            reward_risk_down: 1.0,
            obi: 0.1,
            spread_bps: 2.0,
            microprice_offset_bps: 0.5,
            bid_depth_near: 10.0,
            ask_depth_near: 8.0,
            depth_ratio: 1.25,
            depth_imbalance: 0.1,
            depth_far_ratio: 1.1,
            depth_acceleration: 0.05,
            obi_10: 0.2,
            pm_up_bid: 0.48,
            pm_up_ask: 0.50,
            pm_up_bid_size: 40.0,
            pm_up_ask_size: 40.0,
            pm_down_bid: 0.48,
            pm_down_ask: 0.50,
            pm_down_bid_size: 40.0,
            pm_down_ask_size: 40.0,
            pm_lag_secs: 1.0,
            settlement_up: 1.0,
            future_up_ask_change_30s: Some(0.05),
            future_up_ask_change_60s: Some(0.08),
            cum_obi_delta_5m: 0.1,
            cum_depth_delta_5m: 0.1,
            cum_mprice_drift_5m: 0.2,
            cum_trade_imbalance_5m: 1.0,
            cex_bar_return_30s: 0.002,
            cex_bar_return_60s: 0.003,
            cex_bar_volume_ratio_30s: 2.0,
            cex_bar_volume_trend_3: 1.0,
            cex_signed_volume_ratio_30s: 0.6,
            cex_consecutive_up_bars: 2.0,
            cex_consecutive_down_bars: 0.0,
            cex_breakout_volume_score: 1.2,
        }
    }

    #[test]
    fn v2_rows_are_side_aware_and_use_fixed_stake_liquidity() {
        let options = FactorReviewOptions::default();
        let rows = build_factor_observations_v2(&[base_obs()], &options);
        assert_eq!(rows.len(), 2);

        let up = rows.iter().find(|row| row.side == ReviewSide::Up).unwrap();
        assert!(up.label_executable_fillable);
        assert!(up.label_exit_fillable);
        assert_eq!(up.label_settlement_win, Some(1.0));
        assert!(up.label_executable_pnl_15u.unwrap() > 0.0);

        let down = rows
            .iter()
            .find(|row| row.side == ReviewSide::Down)
            .unwrap();
        assert_eq!(down.label_settlement_win, Some(0.0));
        assert!(down.label_executable_pnl_15u.unwrap() < 0.0);
    }

    #[test]
    fn health_report_counts_missing_size_as_rejected() {
        let mut obs = base_obs();
        obs.pm_up_ask_size = 1.0;
        obs.pm_down_ask_size = 1.0;
        let options = FactorReviewOptions::default();
        let v2 = build_factor_observations_v2(&[obs.clone()], &options);
        let health = build_data_health_report(&[obs], &v2);
        assert_eq!(health.v2_rows, 2);
        assert_eq!(health.entry_fillable_rows, 0);
        assert_eq!(health.executable_pnl_rows, 0);
        assert!((health.rejection_rate() - 1.0).abs() < EPS);
    }

    #[test]
    fn full_depth_sweep_labels_cross_multiple_pm_levels_without_changing_top_book_label() {
        let mut obs = base_obs();
        obs.pm_up_ask_size = 10.0;
        obs.pm_down_ask_size = 10.0;
        let ts = obs.tick_ts;
        let books = vec![ResearchPmBookSnapshot {
            event_id: "evt".into(),
            token_id: "up-token".into(),
            side: "UP".into(),
            ts,
            bids: vec![crate::factors::ResearchPmBookLevel {
                price: 0.49,
                size: 40.0,
            }],
            asks: vec![
                crate::factors::ResearchPmBookLevel {
                    price: 0.50,
                    size: 10.0,
                },
                crate::factors::ResearchPmBookLevel {
                    price: 0.51,
                    size: 25.0,
                },
            ],
        }];

        let rows = build_factor_observations_v2_with_deribit_and_pm_books(
            &[obs],
            &[],
            &books,
            &FactorReviewOptions::default(),
        );
        let up = rows.iter().find(|row| row.side == ReviewSide::Up).unwrap();

        assert!(!up.label_executable_fillable);
        assert!(up.label_full_depth_entry_fillable);
        assert!(up.label_full_depth_exit_fillable);
        assert!(up.label_executable_pnl_15u.is_none());
        assert!(up.label_full_depth_executable_pnl_15u.unwrap() > 0.0);
        assert_eq!(up.entry_sweep_levels_15u, 2.0);
        assert!(up.entry_sweep_slippage_bps > 0.0);
    }

    #[test]
    fn fillability_review_marks_capacity_bucket_candidate() {
        let observations = (0..40).map(|_| base_obs()).collect::<Vec<_>>();
        let report = review_fillability_v1_with_deribit(
            &observations,
            &[],
            FillabilityReviewOptions {
                min_bucket_observations: 10,
                min_entry_fill_rate: 0.30,
                min_roundtrip_fill_rate: 0.20,
                max_rejection_rate: 0.70,
                ..Default::default()
            },
        );

        let capacity = report
            .rows
            .iter()
            .find(|row| {
                row.dimension == "min_entry_exit_capacity_ratio" && row.bucket == "1p00_2p00"
            })
            .expect("capacity bucket");
        assert_eq!(capacity.decision, FillabilityDecision::Candidate);
        assert!((capacity.entry_fill_rate - 1.0).abs() < EPS);
        assert!((capacity.roundtrip_fill_rate - 1.0).abs() < EPS);
    }

    #[test]
    fn liquidity_gate_v1_selects_only_point_in_time_capacity() {
        let observations = (0..20)
            .map(|idx| {
                let mut obs = base_obs();
                if idx % 2 == 1 {
                    obs.pm_up_ask_size = 1.0;
                    obs.pm_up_bid_size = 1.0;
                    obs.pm_down_ask_size = 1.0;
                    obs.pm_down_bid_size = 1.0;
                }
                obs
            })
            .collect::<Vec<_>>();
        let report = liquidity_gate_v1_with_deribit(
            &observations,
            &[],
            LiquidityGateV1Options {
                review: FactorReviewOptions {
                    stake_usd: 15.0,
                    ..Default::default()
                },
                ..Default::default()
            },
        );

        assert_eq!(report.health.v2_rows, 40);
        assert_eq!(report.selected_n, 20);
        assert!((report.coverage - 0.5).abs() < EPS);
        assert!((report.entry_fill_rate - 1.0).abs() < EPS);
        assert!((report.roundtrip_fill_rate - 1.0).abs() < EPS);
    }

    #[test]
    fn liquidity_gated_alpha_v1_reviews_only_tradeable_rows() {
        let base = Utc::now();
        let observations = (0..96)
            .map(|idx| {
                let mut obs = base_obs();
                obs.event_id = format!("event-{idx}");
                obs.tick_ts = base + chrono::Duration::hours(idx);
                obs.model_prob_up = if idx % 2 == 0 { 0.80 } else { 0.20 };
                obs.model_edge_up =
                    obs.model_prob_up - obs.pm_up_ask - crypto_fee_cost(obs.pm_up_ask);
                obs.settlement_up = if idx % 2 == 0 { 1.0 } else { 0.0 };
                if idx % 3 == 0 {
                    obs.pm_up_ask_size = 1.0;
                    obs.pm_up_bid_size = 1.0;
                    obs.pm_down_ask_size = 1.0;
                    obs.pm_down_bid_size = 1.0;
                }
                obs
            })
            .collect::<Vec<_>>();

        let report = liquidity_gated_alpha_v1_with_deribit(
            &observations,
            &[],
            base,
            base + chrono::Duration::days(4) - chrono::Duration::seconds(1),
            LiquidityGatedAlphaV1Options {
                gate: LiquidityGateV1Options::default(),
                walk_forward: FactorWalkForwardOptions {
                    review: FactorReviewOptions {
                        stake_usd: 15.0,
                        min_observations: 10,
                        top_quantile: 0.2,
                    },
                    train_window_days: 2,
                    test_window_days: 1,
                    step_days: 1,
                    top_n: 10,
                    factor_name_filter: Some("side_model_prob".to_string()),
                },
            },
        );

        assert!(report.gate.selected_n < report.baseline_health.v2_rows);
        assert!((report.gate.entry_fill_rate - 1.0).abs() < EPS);
        assert!(!report.review.reviews.is_empty());
        assert!(
            report
                .review
                .reviews
                .iter()
                .all(|review| !review.factor.starts_with("future_exit_"))
        );
        assert!(!report.walk_forward.windows.is_empty());
        assert!(
            report
                .stability
                .rows
                .iter()
                .any(|row| row.factor == "side_model_prob")
        );
    }

    #[test]
    fn trade_formation_review_discovers_profitable_paths_and_meta_rules() {
        let base = Utc::now();
        let observations = (0..96)
            .map(|idx| {
                let mut obs = base_obs();
                obs.event_id = format!("event-{idx}");
                obs.tick_ts = base + chrono::Duration::seconds(idx * 30);
                let up_wins = idx % 2 == 0;
                obs.model_prob_up = if up_wins { 0.80 } else { 0.20 };
                obs.model_edge_up =
                    obs.model_prob_up - obs.pm_up_ask - crypto_fee_cost(obs.pm_up_ask);
                obs.distance_over_sigma = if up_wins { 1.10 } else { -1.10 };
                obs.obi_10 = if up_wins { 0.40 } else { -0.40 };
                obs.depth_imbalance = if up_wins { 0.40 } else { -0.40 };
                obs.cex_bar_return_30s = if up_wins { 0.004 } else { -0.004 };
                obs.cex_bar_return_60s = if up_wins { 0.006 } else { -0.006 };
                obs.cex_signed_volume_ratio_30s = if up_wins { 0.70 } else { -0.70 };
                obs.cex_consecutive_up_bars = if up_wins { 3.0 } else { 0.0 };
                obs.cex_consecutive_down_bars = if up_wins { 0.0 } else { 3.0 };
                obs.cex_breakout_volume_score = if up_wins { 1.5 } else { -1.5 };
                obs.settlement_up = if up_wins { 1.0 } else { 0.0 };
                obs
            })
            .collect::<Vec<_>>();

        let report = review_trade_formation_v1_with_deribit(
            &observations,
            &[],
            TradeFormationReviewOptions {
                review: FactorReviewOptions {
                    stake_usd: 15.0,
                    min_observations: 10,
                    top_quantile: 0.2,
                },
                min_path_observations: 10,
                top_n: 10,
                ..Default::default()
            },
        );

        assert_eq!(report.gated_rows, 192);
        assert_eq!(report.profitable_gated_rows, 96);
        assert_eq!(report.losing_gated_rows, 96);
        assert!(report.missed_winner_paths.is_empty());
        assert!(report.profitable_paths.iter().any(|row| {
            row.path.contains("direction=strong_model")
                && (row.path.contains("cex=depth_confirmed")
                    || row.path.contains("cex=persistent_obi"))
                && row.path.contains("continuation=continuation_confirmed")
                && row.total_pnl_after_cost > 0.0
        }));
        assert!(report.losing_paths.iter().any(|row| {
            row.path.contains("direction=weak_model")
                && row.path.contains("continuation=continuation_against")
                && row.total_pnl_after_cost < 0.0
        }));
        assert!(report.meta_label_rules.iter().any(|row| {
            row.rule == "strong_direction_cex_and_continuation" && row.win_rate > 0.99
        }));

        let formatted = format_trade_formation_v1_report(&report);
        assert!(formatted.contains("Trade Formation Review V1"));
        assert!(formatted.contains("Meta-Label Rule Candidates"));
    }

    #[test]
    fn meta_label_walk_forward_tests_fixed_rules_out_of_sample() {
        let base = Utc::now();
        let observations = (0..96)
            .map(|idx| {
                let mut obs = base_obs();
                obs.event_id = format!("event-{idx}");
                obs.tick_ts = base + chrono::Duration::hours(idx);
                let up_wins = idx % 2 == 0;
                obs.model_prob_up = if up_wins { 0.80 } else { 0.20 };
                obs.model_edge_up =
                    obs.model_prob_up - obs.pm_up_ask - crypto_fee_cost(obs.pm_up_ask);
                obs.distance_over_sigma = if up_wins { 1.10 } else { -1.10 };
                obs.obi_10 = if up_wins { 0.40 } else { -0.40 };
                obs.depth_imbalance = if up_wins { 0.40 } else { -0.40 };
                obs.cex_bar_return_30s = if up_wins { 0.004 } else { -0.004 };
                obs.cex_bar_return_60s = if up_wins { 0.006 } else { -0.006 };
                obs.cex_signed_volume_ratio_30s = if up_wins { 0.70 } else { -0.70 };
                obs.cex_consecutive_up_bars = if up_wins { 3.0 } else { 0.0 };
                obs.cex_consecutive_down_bars = if up_wins { 0.0 } else { 3.0 };
                obs.cex_breakout_volume_score = if up_wins { 1.5 } else { -1.5 };
                obs.settlement_up = if up_wins { 1.0 } else { 0.0 };
                obs
            })
            .collect::<Vec<_>>();

        let report = walk_forward_meta_label_v1_with_deribit(
            &observations,
            &[],
            base,
            base + chrono::Duration::days(4) - chrono::Duration::seconds(1),
            MetaLabelWalkForwardOptions {
                review: FactorReviewOptions {
                    stake_usd: 15.0,
                    min_observations: 10,
                    top_quantile: 0.2,
                },
                train_window_days: 2,
                test_window_days: 1,
                step_days: 1,
                min_rule_observations: 10,
                top_n: 10,
                ..Default::default()
            },
        );

        let continuation = report
            .aggregates
            .iter()
            .find(|row| row.rule == "continuation_confirmation")
            .expect("continuation aggregate");
        assert_eq!(continuation.windows, 2);
        assert!((continuation.positive_window_ratio - 1.0).abs() < EPS);
        assert!(continuation.total_test_pnl_after_cost > 0.0);
        assert_eq!(continuation.decision, FactorStabilityDecision::Watchlist);
        assert_eq!(continuation.reason, "too_few_oos_windows_positive_pnl");
        assert!(report.windows.iter().any(|window| {
            window.rule == "continuation_confirmation"
                && window.test.selected_n > 0
                && window.test.total_pnl_after_cost > 0.0
        }));

        let formatted = format_meta_label_walk_forward_v1_report(&report);
        assert!(formatted.contains("Meta-Label Walk-Forward V1"));
        assert!(formatted.contains("continuation_confirmation"));
        assert!(formatted.contains("decision"));
        assert!(formatted.contains("too_few_oos_windows_positive_pnl"));

        let candidate_report = walk_forward_meta_label_v1_with_deribit(
            &observations,
            &[],
            base,
            base + chrono::Duration::days(4) - chrono::Duration::seconds(1),
            MetaLabelWalkForwardOptions {
                review: FactorReviewOptions {
                    stake_usd: 15.0,
                    min_observations: 10,
                    top_quantile: 0.2,
                },
                train_window_days: 2,
                test_window_days: 1,
                step_days: 1,
                min_rule_observations: 10,
                min_candidate_windows: 2,
                min_candidate_avg_test_selected: 10.0,
                top_n: 10,
                ..Default::default()
            },
        );
        let candidate_continuation = candidate_report
            .aggregates
            .iter()
            .find(|row| row.rule == "continuation_confirmation")
            .expect("candidate continuation aggregate");
        assert_eq!(
            candidate_continuation.decision,
            FactorStabilityDecision::Candidate
        );
        assert_eq!(candidate_continuation.reason, "passed");
    }

    #[test]
    fn review_reports_single_factor_execution_metrics() {
        let mut observations = Vec::new();
        for i in 0..40 {
            let mut obs = base_obs();
            obs.event_id = format!("event-{i}");
            obs.model_prob_up = if i % 2 == 0 { 0.75 } else { 0.25 };
            obs.model_edge_up = obs.model_prob_up - obs.pm_up_ask - crypto_fee_cost(obs.pm_up_ask);
            obs.settlement_up = if i % 2 == 0 { 1.0 } else { 0.0 };
            observations.push(obs);
        }

        let report = review_factors_v2(&observations, FactorReviewOptions::default());
        assert!(report.health.entry_fill_rate() > 0.99);
        assert!(
            report
                .reviews
                .iter()
                .any(|review| review.factor == "side_model_edge")
        );
    }

    #[test]
    fn walk_forward_uses_train_threshold_on_future_window() {
        let base = Utc::now();
        let mut observations = Vec::new();
        for i in 0..72 {
            let mut obs = base_obs();
            obs.event_id = format!("event-{i}");
            obs.tick_ts = base + chrono::Duration::hours(i);
            obs.model_prob_up = if i % 2 == 0 { 0.78 } else { 0.22 };
            obs.model_edge_up = obs.model_prob_up - obs.pm_up_ask - crypto_fee_cost(obs.pm_up_ask);
            obs.settlement_up = if i % 2 == 0 { 1.0 } else { 0.0 };
            observations.push(obs);
        }

        let report = walk_forward_factors_v2_with_deribit(
            &observations,
            &[],
            base,
            base + chrono::Duration::days(3) - chrono::Duration::seconds(1),
            FactorWalkForwardOptions {
                review: FactorReviewOptions {
                    stake_usd: 15.0,
                    min_observations: 10,
                    top_quantile: 0.2,
                },
                train_window_days: 2,
                test_window_days: 1,
                step_days: 1,
                top_n: 10,
                factor_name_filter: None,
            },
        );

        assert!(!report.windows.is_empty());
        assert!(report.windows.len() > report.options.top_n);
        let side_model = report
            .windows
            .iter()
            .find(|window| window.factor == "side_model_prob")
            .expect("side_model_prob window");
        assert_eq!(side_model.window_index, 0);
        assert!(side_model.threshold.is_finite());
        assert!(side_model.test.selected_n > 0);
        assert!(side_model.test.total_pnl_after_cost > 0.0);
        assert!(
            report
                .aggregates
                .iter()
                .any(|aggregate| aggregate.factor == "side_model_prob")
        );
        assert!(
            report
                .windows
                .iter()
                .all(|window| !window.factor.starts_with("future_exit_"))
        );
    }

    #[test]
    fn stability_report_rejects_too_short_negative_factors() {
        let base = Utc::now();
        let report = FactorWalkForwardReport {
            options: FactorWalkForwardOptions::default(),
            health: DataHealthReport {
                source_observations: 0,
                v2_rows: 0,
                settlement_label_rows: 0,
                entry_quote_rows: 0,
                exit_quote_rows: 0,
                entry_size_rows: 0,
                exit_size_rows: 0,
                entry_fillable_rows: 0,
                exit_fillable_rows: 0,
                entry_full_depth_fillable_rows: 0,
                exit_full_depth_fillable_rows: 0,
                executable_pnl_rows: 0,
                full_depth_executable_pnl_rows: 0,
                deribit_rows: 0,
                avg_pm_lag_secs: f64::NAN,
                avg_entry_capacity_ratio: f64::NAN,
                avg_exit_capacity_ratio: f64::NAN,
                avg_entry_sweep_slippage_bps: f64::NAN,
                avg_exit_sweep_slippage_bps: f64::NAN,
            },
            windows: vec![FactorWalkForwardWindow {
                window_index: 0,
                train_start: base,
                train_end: base + chrono::Duration::days(1),
                test_start: base + chrono::Duration::days(1),
                test_end: base + chrono::Duration::days(2),
                factor: "bad_factor".to_string(),
                family: FactorFamily::Alpha,
                layer: ThreeLayerArchive::DirectionProbabilityEdge,
                direction: 1.0,
                threshold: 0.0,
                train_settlement_rank_ic: -0.1,
                train_executable_pnl_rank_ic: -0.2,
                train: FactorSelectionMetrics {
                    n: 10,
                    selected_n: 2,
                    executable_fill_rate: 1.0,
                    rejection_rate: 0.0,
                    total_pnl_after_cost: 1.0,
                    avg_pnl_after_cost: 0.5,
                    sharpe: 1.0,
                    max_drawdown: 0.0,
                    by_symbol_positive_ratio: 1.0,
                    by_time_bucket_positive_ratio: 1.0,
                },
                test: FactorSelectionMetrics {
                    n: 10,
                    selected_n: 2,
                    executable_fill_rate: 1.0,
                    rejection_rate: 0.0,
                    total_pnl_after_cost: -1.0,
                    avg_pnl_after_cost: -0.5,
                    sharpe: -1.0,
                    max_drawdown: 1.0,
                    by_symbol_positive_ratio: 0.0,
                    by_time_bucket_positive_ratio: 0.0,
                },
            }],
            aggregates: Vec::new(),
        };

        let stability = build_factor_stability_report(&report, FactorStabilityOptions::default());
        assert_eq!(stability.rows.len(), 1);
        assert_eq!(stability.rows[0].decision, FactorStabilityDecision::Reject);
        assert_eq!(stability.rows[0].reason, "too_few_windows_nonpositive_pnl");
    }

    #[test]
    fn stability_report_watchlists_unstable_icir() {
        let base = Utc::now();
        let metrics = || FactorSelectionMetrics {
            n: 20,
            selected_n: 5,
            executable_fill_rate: 1.0,
            rejection_rate: 0.0,
            total_pnl_after_cost: 2.0,
            avg_pnl_after_cost: 0.4,
            sharpe: 1.0,
            max_drawdown: 0.0,
            by_symbol_positive_ratio: 1.0,
            by_time_bucket_positive_ratio: 1.0,
        };
        let windows = (0..8)
            .map(|idx| FactorWalkForwardWindow {
                window_index: idx,
                train_start: base + chrono::Duration::days(idx as i64),
                train_end: base + chrono::Duration::days(idx as i64 + 1),
                test_start: base + chrono::Duration::days(idx as i64 + 1),
                test_end: base + chrono::Duration::days(idx as i64 + 2),
                factor: "flat_ic_factor".to_string(),
                family: FactorFamily::Alpha,
                layer: ThreeLayerArchive::DirectionProbabilityEdge,
                direction: 1.0,
                threshold: 0.0,
                train_settlement_rank_ic: 0.2,
                train_executable_pnl_rank_ic: 0.2,
                train: metrics(),
                test: metrics(),
            })
            .collect();
        let report = FactorWalkForwardReport {
            options: FactorWalkForwardOptions::default(),
            health: DataHealthReport {
                source_observations: 0,
                v2_rows: 0,
                settlement_label_rows: 0,
                entry_quote_rows: 0,
                exit_quote_rows: 0,
                entry_size_rows: 0,
                exit_size_rows: 0,
                entry_fillable_rows: 0,
                exit_fillable_rows: 0,
                entry_full_depth_fillable_rows: 0,
                exit_full_depth_fillable_rows: 0,
                executable_pnl_rows: 0,
                full_depth_executable_pnl_rows: 0,
                deribit_rows: 0,
                avg_pm_lag_secs: f64::NAN,
                avg_entry_capacity_ratio: f64::NAN,
                avg_exit_capacity_ratio: f64::NAN,
                avg_entry_sweep_slippage_bps: f64::NAN,
                avg_exit_sweep_slippage_bps: f64::NAN,
            },
            windows,
            aggregates: Vec::new(),
        };

        let stability = build_factor_stability_report(&report, FactorStabilityOptions::default());
        assert_eq!(stability.rows.len(), 1);
        assert_eq!(
            stability.rows[0].decision,
            FactorStabilityDecision::Watchlist
        );
        assert_eq!(
            stability.rows[0].reason,
            "positive_pnl_but_low_executable_icir"
        );
    }

    #[test]
    fn combo_v1_uses_train_normalized_factor_scores() {
        let base = Utc::now();
        let mut observations = Vec::new();
        for i in 0..96 {
            let mut obs = base_obs();
            obs.event_id = format!("event-{i}");
            obs.tick_ts = base + chrono::Duration::hours(i);
            obs.model_prob_up = if i % 2 == 0 { 0.80 } else { 0.20 };
            obs.model_edge_up = obs.model_prob_up - obs.pm_up_ask - crypto_fee_cost(obs.pm_up_ask);
            obs.settlement_up = if i % 2 == 0 { 1.0 } else { 0.0 };
            observations.push(obs);
        }

        let report = walk_forward_factor_combo_v1_with_deribit(
            &observations,
            &[],
            base,
            base + chrono::Duration::days(4) - chrono::Duration::seconds(1),
            FactorComboV1Options {
                walk_forward: FactorWalkForwardOptions {
                    review: FactorReviewOptions {
                        stake_usd: 15.0,
                        min_observations: 10,
                        top_quantile: 0.2,
                    },
                    train_window_days: 2,
                    test_window_days: 1,
                    step_days: 1,
                    top_n: 10,
                    factor_name_filter: Some("side_model_prob".to_string()),
                },
                max_factors_per_family: 1,
                max_total_factors: 2,
                min_abs_train_executable_pnl_rank_ic: 0.01,
            },
        );

        assert!(!report.windows.is_empty());
        assert!(report.aggregate.total_test_pnl_after_cost > 0.0);
        assert!(
            report
                .windows
                .iter()
                .all(|window| !window.components.is_empty())
        );
    }
}
