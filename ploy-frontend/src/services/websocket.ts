import type { LogEntry, Trade, Position, MarketData } from '@/types';

export type WebSocketEvent =
  | { type: 'log'; data: LogEntry }
  | { type: 'trade'; data: Trade }
  | { type: 'position'; data: Position }
  | { type: 'market'; data: MarketData }
  | { type: 'status'; data: { status: 'running' | 'stopped' | 'error' } }
  | { type: 'nba_update'; data: NBAUpdateData };

export interface NBAUpdateData {
  state: string;
  game: {
    gameId: string;
    homeTeam: string;
    awayTeam: string;
    homeScore: number;
    awayScore: number;
    quarter: number;
    timeRemaining: number;
    possession: string;
  } | null;
  prediction: {
    winProb: number;
    confidence: number;
  } | null;
  marketPrice: number | null;
}

type ConnectionCallback = (connected: boolean) => void;

function defaultWsUrl(): string {
  const proto = window.location.protocol === 'https:' ? 'wss' : 'ws';
  return `${proto}://${window.location.host}/ws`;
}

export class WebSocketService {
  private ws: WebSocket | null = null;
  private listeners: Map<string, Set<(event: WebSocketEvent) => void>> = new Map();
  private connectionListeners: Set<ConnectionCallback> = new Set();
  private reconnectAttempts = 0;
  private maxReconnectAttempts = 10;
  private reconnectTimer: number | null = null;
  private manualDisconnect = false;

  connect(url: string = defaultWsUrl()) {
    if (
      this.ws &&
      (this.ws.readyState === WebSocket.OPEN ||
        this.ws.readyState === WebSocket.CONNECTING)
    ) {
      return;
    }

    this.manualDisconnect = false;
    this.ws = new WebSocket(url);

    this.ws.onopen = () => {
      console.log('[WebSocket] Connected');
      this.reconnectAttempts = 0;
      this.notifyConnectionChange(true);
    };

    this.ws.onclose = (ev) => {
      console.log('[WebSocket] Disconnected:', ev.code, ev.reason);
      this.notifyConnectionChange(false);
      this.ws = null;

      if (!this.manualDisconnect) {
        this.scheduleReconnect(url);
      }
    };

    this.ws.onerror = (err) => {
      console.error('[WebSocket] Error:', err);
    };

    this.ws.onmessage = (ev) => {
      if (typeof ev.data !== 'string') return;

      let parsed: any;
      try {
        parsed = JSON.parse(ev.data);
      } catch {
        return;
      }

      const t = parsed?.type;
      const data = parsed?.data;
      if (typeof t !== 'string') return;

      if (
        t === 'log' ||
        t === 'trade' ||
        t === 'position' ||
        t === 'market' ||
        t === 'status' ||
        t === 'nba_update'
      ) {
        this.emit({ type: t, data } as WebSocketEvent);
      }
    };
  }

  disconnect() {
    this.manualDisconnect = true;
    if (this.reconnectTimer) {
      window.clearTimeout(this.reconnectTimer);
      this.reconnectTimer = null;
    }
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
    return this.ws?.readyState === WebSocket.OPEN;
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

  private scheduleReconnect(url: string) {
    if (this.reconnectAttempts >= this.maxReconnectAttempts) {
      console.error('[WebSocket] Max reconnect attempts reached');
      return;
    }

    const backoffMs = Math.min(1000 * 2 ** this.reconnectAttempts, 10_000);
    this.reconnectAttempts++;

    this.reconnectTimer = window.setTimeout(() => {
      this.connect(url);
    }, backoffMs);
  }
}

export const ws = new WebSocketService();

