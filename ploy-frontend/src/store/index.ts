import { create } from 'zustand';
import type { LogEntry, Trade, Position, MarketData } from '@/types';

interface AppState {
  // WebSocket connection state
  wsConnected: boolean;
  setWsConnected: (connected: boolean) => void;

  // Real-time logs
  logs: LogEntry[];
  addLog: (log: LogEntry) => void;
  clearLogs: () => void;

  // Real-time trades
  recentTrades: Trade[];
  addTrade: (trade: Trade) => void;

  // Real-time positions
  positions: Position[];
  updatePositions: (positions: Position[]) => void;

  // Real-time market data
  marketData: Map<string, MarketData>;
  updateMarketData: (data: MarketData) => void;

  // System status
  systemStatus: 'running' | 'stopped' | 'error';
  setSystemStatus: (status: 'running' | 'stopped' | 'error') => void;
}

const MAX_LOGS = 1000;
const MAX_RECENT_TRADES = 50;

export const useStore = create<AppState>((set) => ({
  // WebSocket state
  wsConnected: false,
  setWsConnected: (connected) => set({ wsConnected: connected }),

  // Logs
  logs: [],
  addLog: (log) =>
    set((state) => ({
      logs: [...state.logs.slice(-(MAX_LOGS - 1)), log],
    })),
  clearLogs: () => set({ logs: [] }),

  // Trades
  recentTrades: [],
  addTrade: (trade) =>
    set((state) => ({
      recentTrades: [trade, ...state.recentTrades.slice(0, MAX_RECENT_TRADES - 1)],
    })),

  // Positions
  positions: [],
  updatePositions: (positions) => set({ positions }),

  // Market data
  marketData: new Map(),
  updateMarketData: (data) =>
    set((state) => {
      const newMarketData = new Map(state.marketData);
      newMarketData.set(data.token_id, data);
      return { marketData: newMarketData };
    }),

  // System status
  systemStatus: 'stopped',
  setSystemStatus: (status) => set({ systemStatus: status }),
}));
