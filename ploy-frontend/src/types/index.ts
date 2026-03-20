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
  deployment_state: DeploymentState;
  desired_state: DesiredState;
  observed_state: ObservedState;
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
  | { type: 'trading_snapshot'; data: { trading: TradingStateSnapshot[] } };

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
