import type {
  AlertRecord,
  DeploymentSummary,
  LogEntry,
  MarketData,
  Position,
  SystemMetrics,
  SystemStatus,
  Trade,
  TradingStateSnapshot,
} from '@/types';

export type WebSocketEvent =
  | { type: 'log'; data: LogEntry }
  | { type: 'trade'; data: Trade }
  | { type: 'position'; data: Position }
  | { type: 'market'; data: MarketData }
  | { type: 'status'; data: { status: 'running' | 'stopped' | 'error' } }
  | { type: 'system_snapshot'; data: { system: SystemStatus } }
  | { type: 'deployment_snapshot'; data: { deployments: DeploymentSummary[] } }
  | { type: 'trading_snapshot'; data: { trading: TradingStateSnapshot[] } }
  | { type: 'alert_snapshot'; data: { alerts: AlertRecord[] } }
  | { type: 'metrics_snapshot'; data: { metrics: SystemMetrics } };

type ConnectionCallback = (connected: boolean) => void;

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null;
}

function defaultWsUrl(): string {
  return `${window.location.origin}/api/events/stream`;
}

export class WebSocketService {
  private ws: EventSource | null = null;
  private listeners: Map<string, Set<(event: WebSocketEvent) => void>> = new Map();
  private connectionListeners: Set<ConnectionCallback> = new Set();
  private manualDisconnect = false;

  connect(url: string = defaultWsUrl()) {
    if (
      this.ws &&
      (this.ws.readyState === EventSource.OPEN ||
        this.ws.readyState === EventSource.CONNECTING)
    ) {
      return;
    }

    this.manualDisconnect = false;
    this.ws = new EventSource(url, { withCredentials: true });

    this.ws.onopen = () => {
      console.log('[EventStream] Connected');
      this.notifyConnectionChange(true);
    };

    this.ws.onerror = (err) => {
      console.error('[EventStream] Error:', err);
      this.notifyConnectionChange(false);
      if (this.manualDisconnect) {
        this.ws?.close();
        this.ws = null;
      }
    };

    this.ws.onmessage = (ev) => {
      if (typeof ev.data !== 'string') return;

      let parsed: unknown;
      try {
        parsed = JSON.parse(ev.data);
      } catch {
        return;
      }

      if (!isRecord(parsed)) return;

      const t = parsed?.type;
      const data = parsed?.data;
      if (typeof t !== 'string') return;

      if (
        t === 'log' ||
        t === 'trade' ||
        t === 'position' ||
        t === 'market' ||
        t === 'status' ||
        t === 'system_snapshot' ||
        t === 'deployment_snapshot' ||
        t === 'trading_snapshot' ||
        t === 'alert_snapshot' ||
        t === 'metrics_snapshot'
      ) {
        this.emit({ type: t, data } as WebSocketEvent);
      }
    };
  }

  disconnect() {
    this.manualDisconnect = true;
    if (this.ws) {
      this.ws.close();
      this.ws = null;
    }
    this.notifyConnectionChange(false);
  }

  subscribe(eventType: string, callback: (event: WebSocketEvent) => void) {
    if (!this.listeners.has(eventType)) {
      this.listeners.set(eventType, new Set());
    }
    this.listeners.get(eventType)!.add(callback);

    return () => {
      const listeners = this.listeners.get(eventType);
      if (listeners) {
        listeners.delete(callback);
      }
    };
  }

  onConnectionChange(callback: ConnectionCallback): () => void {
    this.connectionListeners.add(callback);
    callback(this.isConnected());
    return () => {
      this.connectionListeners.delete(callback);
    };
  }

  isConnected(): boolean {
    return this.ws?.readyState === EventSource.OPEN;
  }

  private notifyConnectionChange(connected: boolean) {
    this.connectionListeners.forEach((cb) => cb(connected));
  }

  private emit(event: WebSocketEvent) {
    const listeners = this.listeners.get(event.type);
    if (listeners) {
      listeners.forEach((callback) => callback(event));
    }

    const wildcardListeners = this.listeners.get('*');
    if (wildcardListeners) {
      wildcardListeners.forEach((callback) => callback(event));
    }
  }
}

export const ws = new WebSocketService();
