export interface TodayStats {
  total_trades: number;
  successful_trades: number;
  failed_trades: number;
  total_volume: number;
  pnl: number;
  win_rate: number;
  avg_trade_time_ms: number;
  active_positions: number;
}

export type DeploymentState = 'enabled' | 'draining' | 'disabled' | 'archived';

export type IntentPurpose = 'entry' | 'exit' | 'reduce' | 'hedge' | 'cancel';

export interface TradeResponse {
  id: string;
  timestamp: string;
  token_id: string;
  token_name: string;
  side: 'UP' | 'DOWN';
  shares: number;
  entry_price: number;
  exit_price: number | null;
  pnl: number | null;
  status: 'PENDING' | 'LEG1_FILLED' | 'LEG2_FILLED' | 'COMPLETED' | 'FAILED';
  error_message?: string;
}

export type Trade = TradeResponse;

export interface PositionResponse {
  token_id: string;
  token_name: string;
  side: 'UP' | 'DOWN';
  shares: number;
  entry_price: number;
  current_price: number;
  unrealized_pnl: number;
  entry_time: string;
  duration_seconds: number;
}

export type Position = PositionResponse;

export interface SystemStatus {
  status: string;
  uptime_seconds: number;
  version: string;
  strategy: string;
  last_trade_time: string | null;
  websocket_connected: boolean;
  database_connected: boolean;
  error_count_1h: number;
  live_reconcile_failures: number;
  next_live_reconcile_at: string | null;
  last_live_reconcile_error: string | null;
  active_alert_count: number;
  stale_source_count: number;
  last_live_reconcile_success_at: string | null;
}

export type HeartbeatState = 'healthy' | 'stale';

export interface HeartbeatStatus {
  source_id: string;
  source_kind: string;
  state: HeartbeatState;
  last_seen_at: string | null;
  stale_after_seconds: number;
  message: string | null;
}

export type AlertSeverity = 'warning' | 'critical';
export type AlertKind = 'source_stale';

export interface ActiveAlert {
  alert_id: string;
  kind: AlertKind;
  severity: AlertSeverity;
  source_id: string;
  message: string;
  triggered_at: string;
}

export interface PlatformMetrics {
  total_deployments: number;
  live_deployments: number;
  degraded_deployments: number;
  active_alerts: number;
  stale_sources: number;
  live_reconcile_failures: number;
  last_trade_time: string | null;
  last_live_reconcile_success_at: string | null;
  heartbeats: HeartbeatStatus[];
}

export interface SystemControlResponse {
  success: boolean;
  message: string;
}

export type DesiredState = 'running' | 'paused' | 'stopped';

export type ObservedState =
  | 'starting'
  | 'running'
  | 'degraded'
  | 'paused'
  | 'stopped'
  | 'failed';

export interface DeploymentSummary {
  deployment_id: string;
  bundle_id: string;
  runtime_mode: string;
  account_id?: string;
  max_gross_exposure?: string | null;
  deployment_state: DeploymentState;
  desired_state: DesiredState;
  observed_state: ObservedState;
}

export interface OversightSignal {
  severity: 'info' | 'warning' | 'critical';
  kind: string;
  deployment_id?: string;
  message: string;
  recommended_action: string;
  evidence: string[];
}

export interface OversightAction {
  kind: string;
  target: string;
  rationale: string;
  operator_command: string;
  config_hint?: string | null;
  evidence: string[];
}

export interface OversightReport {
  timestamp: string;
  platform_status: string;
  deployments_reviewed: number;
  signal_count: number;
  signals: OversightSignal[];
  recommended_actions: OversightAction[];
}

export interface DiagnosticsEvidence {
  source: string;
  label: string;
  detail: string;
  observed_at?: string | null;
}

export interface DiagnosticsFinding {
  severity: string;
  kind: string;
  message: string;
  first_observed_at?: string | null;
  likely_causes?: string[];
  operator_command?: string | null;
  evidence?: DiagnosticsEvidence[];
}

export interface PlatformDiagnosticsReport {
  generated_at: string;
  platform_status: string;
  first_diverged_metric?: string | null;
  findings?: DiagnosticsFinding[];
  recent_evidence?: DiagnosticsEvidence[];
}

export interface DeploymentDiagnosticsMetrics {
  pending_intents: number;
  active_orders: number;
  open_positions: number;
  gross_exposure: string;
  net_pnl: string;
}

export interface DeploymentDiagnosticsReport {
  generated_at: string;
  deployment_id: string;
  bundle_id: string;
  runtime_mode: string;
  account_id: string;
  desired_state: string;
  observed_state: string;
  max_gross_exposure?: string | null;
  metrics: DeploymentDiagnosticsMetrics;
  primary_diagnosis: string;
  first_diverged_metric?: string | null;
  findings?: DiagnosticsFinding[];
  recent_evidence?: DiagnosticsEvidence[];
}

export type ProposalActionKind =
  | 'pause_deployment'
  | 'drain_deployment'
  | 'reduce_max_exposure';

export type ProposalStatus = 'pending' | 'approved' | 'rejected' | 'failed';

export interface SafetyProposal {
  proposal_id: string;
  action_kind: ProposalActionKind;
  target_deployment_id: string;
  status: ProposalStatus;
  rationale: string;
  evidence: string[];
  source_run_id?: string | null;
  proposed_max_gross_exposure?: string | null;
  created_at: string;
  decided_at?: string | null;
  decision_note?: string | null;
}

export type AgentRunStatus = 'started' | 'succeeded' | 'failed';

export interface AgentToolCallRecord {
  name: string;
  status: string;
}

export interface AgentRunEvaluation {
  usefulness: string;
  research_reports: number;
  oversight_alerts: number;
  operator_recommendations: number;
}

export interface AgentRuntimeContextSummary {
  deployment_sample: string[];
  oversight_signal_summary: string[];
  oversight_playbook_summary: string[];
  diagnostic_candidates: string[];
}

export interface AgentRunOutputSummary {
  research_report_summaries: string[];
  oversight_alert_summaries: string[];
  operator_recommendation_summaries: string[];
}

export interface AgentRunRecord {
  run_id: string;
  cycle_kind: string;
  status: AgentRunStatus;
  started_at: string;
  finished_at?: string | null;
  session_id?: string | null;
  model: string;
  platform_status?: string | null;
  deployment_count: number;
  oversight_signal_count: number;
  oversight_playbook_count: number;
  total_cost_usd?: number | null;
  tool_calls: AgentToolCallRecord[];
  research_reports: number;
  oversight_alerts: number;
  operator_recommendations: number;
  failure_reason?: string | null;
  runtime_context?: AgentRuntimeContextSummary | null;
  output_summary?: AgentRunOutputSummary | null;
  evaluation?: AgentRunEvaluation | null;
}

export interface AuditLogEntry {
  timestamp: string;
  method: string;
  path: string;
  client_addr?: string | null;
  auth_level: string;
  required_access: string;
  status_code: number;
  outcome: string;
  message?: string | null;
}

export interface TradingPnlSnapshot {
  realized_pnl: string;
  unrealized_pnl: string;
  total_fees: string;
  net_pnl: string;
}

export interface TradingRiskSnapshot {
  pending_intents: number;
  active_orders: number;
  open_positions: number;
  gross_exposure: string;
}

export interface TradingStateSnapshot {
  deployment_id: string;
  runtime_mode: string;
  intents: unknown[];
  orders: unknown[];
  fills: unknown[];
  positions: unknown[];
  pnl: TradingPnlSnapshot;
  risk: TradingRiskSnapshot;
}

export interface UpdateDeploymentStateRequest {
  desired_state?: DesiredState;
  deployment_state?: DeploymentState;
}

export interface StrategyConfig {
  symbols: string[];
  min_move: number;
  max_entry: number;
  shares: number;
  predictive: boolean;
  exit_edge_floor?: number | null;
  exit_price_band?: number | null;
  time_decay_exit_secs?: number | null;
  liquidity_exit_spread_bps?: number | null;
}

export interface LogEntry {
  timestamp: string;
  level: 'INFO' | 'WARN' | 'ERROR' | 'DEBUG';
  component: string;
  message: string;
  metadata?: Record<string, JsonValue>;
}

export interface SecurityEvent {
  id: string;
  timestamp: string;
  event_type: 'DUPLICATE_ORDER' | 'VERSION_CONFLICT' | 'STALE_QUOTE' | 'NONCE_RECOVERY';
  severity: 'LOW' | 'MEDIUM' | 'HIGH' | 'CRITICAL';
  details: string;
  metadata?: Record<string, JsonValue>;
}

export interface MarketData {
  token_id: string;
  token_name: string;
  best_bid: number;
  best_ask: number;
  spread: number;
  last_price: number;
  volume_24h: number;
  timestamp: string;
}

export interface StatusUpdate {
  status: 'running' | 'stopped' | 'error';
}

export interface DeploymentStateSummary {
  enabled: number;
  draining: number;
  disabled: number;
  archived: number;
}

export type WsMessage =
  | { type: 'log'; data: LogEntry }
  | { type: 'trade'; data: TradeResponse }
  | { type: 'position'; data: PositionResponse }
  | { type: 'market'; data: MarketData }
  | { type: 'status'; data: StatusUpdate }
  | { type: 'system_snapshot'; data: { system: SystemStatus } }
  | { type: 'deployment_snapshot'; data: { deployments: DeploymentSummary[] } }
  | { type: 'trading_snapshot'; data: { trading: TradingStateSnapshot[] } }
  | { type: 'metrics_snapshot'; data: { metrics: PlatformMetrics } }
  | { type: 'alert_snapshot'; data: { alerts: ActiveAlert[] } }
  | { type: 'oversight_snapshot'; data: { oversight: OversightReport } }
  | { type: 'proposal_snapshot'; data: { proposals: SafetyProposal[] } };

export interface PnLDataPoint {
  timestamp: string;
  cumulative_pnl: number;
  trade_count: number;
}

export interface RunningStrategy {
  name: string;
  status: 'running' | 'paused' | 'error';
  pnl_usd: number;
  order_count: number;
  domain: 'crypto' | 'sports' | 'politics';
}

export interface RiskData {
  risk_state: 'Normal' | 'Elevated' | 'Halted';
  daily_pnl_usd: number;
  daily_loss_limit_usd: number;
  queue_depth: number;
  positions: Array<{
    market: string;
    side: 'Yes' | 'No';
    size: number;
    pnl_usd: number;
  }>;
  circuit_breaker_events: Array<{
    timestamp: string;
    reason: string;
    state: string;
  }>;
}
export type JsonValue =
  | string
  | number
  | boolean
  | null
  | JsonValue[]
  | { [key: string]: JsonValue };
