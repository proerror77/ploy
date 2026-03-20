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
  status: 'running' | 'stopped' | 'error';
  uptime_seconds: number;
  version: string;
  strategy: string;
  last_trade_time: string | null;
  websocket_connected: boolean;
  database_connected: boolean;
  error_count_1h: number;
}

export interface SystemControlResponse {
  success: boolean;
  message: string;
}

export interface StrategyControlEntry {
  deployment_id: string;
  strategy: string;
  strategy_version: string;
  domain: string;
  enabled: boolean;
  timeframe: string;
  lifecycle_stage: string;
  product_type: string;
  market_selector_mode: string;
  allocator_profile: string;
  risk_profile: string;
  priority: number;
  cooldown_secs: number;
  last_evaluated_at: string | null;
  last_evaluation_score: number | null;
  latest_evaluation_id: string | null;
  latest_evaluation_stage: string | null;
  latest_evaluation_dataset_hash: string | null;
  latest_evaluation_model_hash: string | null;
  latest_evaluation_sample_size: number | null;
  domain_ingress_mode: string;
  running_agents: string[];
}

export interface StrategiesControlResponse {
  account_id: string | null;
  ingress_mode: string | null;
  items: StrategyControlEntry[];
  updated_at: string;
}

export interface UpdateStrategyControlRequest {
  enabled?: boolean;
  priority?: number;
  cooldown_secs?: number;
  allocator_profile?: string;
  risk_profile?: string;
  strategy_version?: string;
  lifecycle_stage?: string;
  product_type?: string;
  last_evaluation_score?: number;
}

export interface StrategyControlMutationResponse {
  success: boolean;
  deployment_id: string;
  strategy_version: string;
  enabled: boolean;
  priority: number;
  cooldown_secs: number;
  lifecycle_stage: string;
  product_type: string;
  last_evaluated_at: string | null;
  last_evaluation_score: number | null;
  latest_evaluation_id: string | null;
  latest_evaluation_stage: string | null;
  latest_evaluation_dataset_hash: string | null;
  latest_evaluation_model_hash: string | null;
  latest_evaluation_sample_size: number | null;
  allocator_profile: string;
  risk_profile: string;
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
  | { type: 'status'; data: StatusUpdate };

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
