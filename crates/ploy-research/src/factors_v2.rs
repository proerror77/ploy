use std::collections::{BTreeMap, HashMap};

use chrono::{DateTime, Duration, Utc};
use ploy_operator_contracts::Regime;

use crate::factors::{FactorObservation, pearson_ic, spearman_ic};

const DEFAULT_STAKE_USD: f64 = 15.0;
const DEFAULT_TOP_QUANTILE: f64 = 0.2;
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
    pub roundtrip_cost_usd: f64,
    pub roundtrip_pnl_now_15u: Option<f64>,

    pub portfolio_stake_usd: f64,
    pub portfolio_event_exposure_usd: f64,
    pub same_event_observation_count: f64,
    pub same_event_side_observation_count: f64,
    pub side_is_up: f64,

    pub label_settlement_win: Option<f64>,
    pub label_executable_pnl_15u: Option<f64>,
    pub label_executable_fillable: bool,
    pub label_exit_fillable: bool,
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
    pub executable_pnl_rows: usize,
    pub deribit_rows: usize,
    pub avg_pm_lag_secs: f64,
    pub avg_entry_capacity_ratio: f64,
    pub avg_exit_capacity_ratio: f64,
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
    let stake_usd = options.stake_usd;
    let mut out = Vec::with_capacity(rows.len() * 2);
    for row in rows {
        out.push(side_row(row, ReviewSide::Up, stake_usd));
        out.push(side_row(row, ReviewSide::Down, stake_usd));
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
        executable_pnl_rows: v2_rows
            .iter()
            .filter(|row| row.label_executable_pnl_15u.is_some_and(f64::is_finite))
            .count(),
        avg_pm_lag_secs: mean(v2_rows.iter().map(|row| row.pm_lag_secs)),
        avg_entry_capacity_ratio: mean(v2_rows.iter().map(|row| row.entry_capacity_ratio)),
        avg_exit_capacity_ratio: mean(v2_rows.iter().map(|row| row.exit_capacity_ratio)),
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
    let health = build_data_health_report(source_rows, &v2_rows);
    let mut reviews: Vec<SingleFactorReview> = factor_v2_descriptors()
        .into_iter()
        .filter_map(|descriptor| review_one_factor(&v2_rows, descriptor, &options))
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
    v2_rows.sort_by_key(|row| row.tick_ts);
    let health = build_data_health_report(source_rows, &v2_rows);
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
        .filter_map(|(row, score)| row.label_executable_pnl_15u.map(|label| (*score, label)))
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
        .filter(|row| row.label_executable_pnl_15u.is_some())
        .collect();
    let pnls: Vec<(DateTime<Utc>, f64)> = filled
        .iter()
        .filter_map(|row| row.label_executable_pnl_15u.map(|pnl| (row.tick_ts, pnl)))
        .filter(|(_, pnl)| pnl.is_finite())
        .collect();
    let pnl_values: Vec<f64> = pnls.iter().map(|(_, pnl)| *pnl).collect();
    let total_pnl = pnl_values.iter().sum::<f64>();
    FactorSelectionMetrics {
        n: scored_n,
        selected_n: selected.len(),
        executable_fill_rate: ratio(filled.len(), selected.len()),
        rejection_rate: ratio(
            selected
                .iter()
                .filter(|row| !row.label_executable_fillable)
                .count(),
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
        .filter_map(|(idx, score)| {
            rows[*idx]
                .label_executable_pnl_15u
                .map(|label| (*score, label))
        })
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
        .filter(|row| row.label_executable_pnl_15u.is_some())
        .collect();
    let pnls: Vec<(DateTime<Utc>, f64)> = filled
        .iter()
        .filter_map(|row| row.label_executable_pnl_15u.map(|pnl| (row.tick_ts, pnl)))
        .filter(|(_, pnl)| pnl.is_finite())
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
            selected
                .iter()
                .filter(|row| !row.label_executable_fillable)
                .count(),
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

fn side_row(row: &FactorObservation, side: ReviewSide, stake_usd: f64) -> FactorObservationV2 {
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
    let slippage_to_fill_15u_bps = if entry_fillable { 0.0 } else { f64::NAN };
    let roundtrip_pnl_now_15u = if entry_fillable && exit_fillable && valid_price(exit_bid) {
        Some(entry_shares * exit_bid - stake_usd - entry_fee_usd)
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
        roundtrip_cost_usd,
        roundtrip_pnl_now_15u,
        portfolio_stake_usd: stake_usd,
        portfolio_event_exposure_usd: stake_usd,
        same_event_observation_count: f64::NAN,
        same_event_side_observation_count: f64::NAN,
        side_is_up: bool_num(side == ReviewSide::Up),
        label_settlement_win: settlement_win.is_finite().then_some(settlement_win),
        label_executable_pnl_15u: executable_pnl,
        label_executable_fillable: entry_fillable,
        label_exit_fillable: exit_fillable,
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
        if let Some(pnl) = row.label_executable_pnl_15u {
            if pnl.is_finite() {
                *groups.entry(key_fn(row)).or_default() += pnl;
            }
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
}
