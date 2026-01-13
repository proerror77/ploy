import { io, Socket } from 'socket.io-client';
import type { LogEntry, Trade, Position, MarketData } from '@/types';

export type WebSocketEvent =
  | { type: 'log'; data: LogEntry }
  | { type: 'trade'; data: Trade }
  | { type: 'position'; data: Position }
  | { type: 'market'; data: MarketData }
  | { type: 'status'; data: { status: 'running' | 'stopped' | 'error' } };

export class WebSocketService {
  private socket: Socket | null = null;
  private listeners: Map<string, Set<(event: WebSocketEvent) => void>> = new Map();
  private reconnectAttempts = 0;
  private maxReconnectAttempts = 10;

  connect() {
    if (this.socket?.connected) {
      return;
    }

    this.socket = io(window.location.origin, {
      path: '/ws',
      transports: ['websocket'],
      reconnection: true,
      reconnectionDelay: 1000,
      reconnectionDelayMax: 5000,
    });

    this.socket.on('connect', () => {
      console.log('[WebSocket] Connected');
      this.reconnectAttempts = 0;
    });

    this.socket.on('disconnect', (reason) => {
      console.log('[WebSocket] Disconnected:', reason);
    });

    this.socket.on('reconnect_attempt', () => {
      this.reconnectAttempts++;
      if (this.reconnectAttempts > this.maxReconnectAttempts) {
        console.error('[WebSocket] Max reconnect attempts reached');
        this.socket?.disconnect();
      }
    });

    // Subscribe to all event types
    this.socket.on('log', (data: LogEntry) => {
      this.emit({ type: 'log', data });
    });

    this.socket.on('trade', (data: Trade) => {
      this.emit({ type: 'trade', data });
    });

    this.socket.on('position', (data: Position) => {
      this.emit({ type: 'position', data });
    });

    this.socket.on('market', (data: MarketData) => {
      this.emit({ type: 'market', data });
    });

    this.socket.on('status', (data: { status: 'running' | 'stopped' | 'error' }) => {
      this.emit({ type: 'status', data });
    });
  }

  disconnect() {
    if (this.socket) {
      this.socket.disconnect();
      this.socket = null;
    }
  }

  subscribe(eventType: string, callback: (event: WebSocketEvent) => void) {
    if (!this.listeners.has(eventType)) {
      this.listeners.set(eventType, new Set());
    }
    this.listeners.get(eventType)!.add(callback);

    // Return unsubscribe function
    return () => {
      const listeners = this.listeners.get(eventType);
      if (listeners) {
        listeners.delete(callback);
      }
    };
  }

  private emit(event: WebSocketEvent) {
    const listeners = this.listeners.get(event.type);
    if (listeners) {
      listeners.forEach((callback) => callback(event));
    }

    // Also emit to wildcard listeners
    const wildcardListeners = this.listeners.get('*');
    if (wildcardListeners) {
      wildcardListeners.forEach((callback) => callback(event));
    }
  }

  isConnected(): boolean {
    return this.socket?.connected ?? false;
  }
}

export const ws = new WebSocketService();
