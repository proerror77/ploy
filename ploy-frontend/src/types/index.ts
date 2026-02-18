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

export interface Trade {
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

export interface Position {
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

export interface StrategyConfig {
  symbols: string[];
  min_move: number;
  max_entry: number;
  shares: number;
  predictive: boolean;
  take_profit?: number;
  stop_loss?: number;
}

export interface LogEntry {
  timestamp: string;
  level: 'INFO' | 'WARN' | 'ERROR' | 'DEBUG';
  component: string;
  message: string;
  metadata?: Record<string, any>;
}

export interface SecurityEvent {
  id: string;
  timestamp: string;
  event_type: 'DUPLICATE_ORDER' | 'VERSION_CONFLICT' | 'STALE_QUOTE' | 'NONCE_RECOVERY';
  severity: 'LOW' | 'MEDIUM' | 'HIGH' | 'CRITICAL';
  details: string;
  metadata?: Record<string, any>;
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
