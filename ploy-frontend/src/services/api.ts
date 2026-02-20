import type {
  TodayStats,
  Trade,
  Position,
  SystemStatus,
  StrategyConfig,
  SecurityEvent,
  PnLDataPoint,
  RunningStrategy,
  RiskData,
} from '@/types';

const API_BASE = '/api';

type RuntimeConfig = {
  PLOY_API_ADMIN_TOKEN?: string;
  PLOY_SIDECAR_AUTH_TOKEN?: string;
};

declare global {
  interface Window {
    __PLOY_RUNTIME_CONFIG__?: RuntimeConfig;
  }
}

const viteEnv = import.meta.env as Record<string, string | undefined>;

function runtimeConfig(): RuntimeConfig {
  if (typeof window === 'undefined') {
    return {};
  }
  return window.__PLOY_RUNTIME_CONFIG__ ?? {};
}

function readStorage(key: string): string | undefined {
  if (typeof window === 'undefined') {
    return undefined;
  }
  const value = window.localStorage.getItem(key)?.trim();
  return value ? value : undefined;
}

function resolveAdminToken(): string | undefined {
  return (
    runtimeConfig().PLOY_API_ADMIN_TOKEN?.trim() ||
    viteEnv.VITE_PLOY_API_ADMIN_TOKEN?.trim() ||
    readStorage('PLOY_API_ADMIN_TOKEN')
  );
}

function resolveSidecarToken(): string | undefined {
  return (
    runtimeConfig().PLOY_SIDECAR_AUTH_TOKEN?.trim() ||
    viteEnv.VITE_PLOY_SIDECAR_AUTH_TOKEN?.trim() ||
    readStorage('PLOY_SIDECAR_AUTH_TOKEN')
  );
}

class ApiService {
  private async fetch<T>(endpoint: string, options?: RequestInit): Promise<T> {
    const adminToken = resolveAdminToken();
    const sidecarToken = resolveSidecarToken();
    const response = await fetch(`${API_BASE}${endpoint}`, {
      headers: {
        'Content-Type': 'application/json',
        ...(adminToken ? { 'x-ploy-admin-token': adminToken } : {}),
        ...(sidecarToken ? { 'x-ploy-sidecar-token': sidecarToken } : {}),
        ...options?.headers,
      },
      ...options,
    });

    if (!response.ok) {
      const error = await response.text();
      throw new Error(`API Error: ${response.status} - ${error}`);
    }

    return response.json();
  }

  // Stats endpoints
  async getTodayStats(): Promise<TodayStats> {
    return this.fetch<TodayStats>('/stats/today');
  }

  async getPnLHistory(hours: number = 24): Promise<PnLDataPoint[]> {
    return this.fetch<PnLDataPoint[]>(`/stats/pnl?hours=${hours}`);
  }

  // Trades endpoints
  async getTrades(params?: {
    limit?: number;
    offset?: number;
    status?: string;
    start_time?: string;
    end_time?: string;
  }): Promise<{ trades: Trade[]; total: number }> {
    const queryParams = new URLSearchParams();
    if (params?.limit) queryParams.append('limit', params.limit.toString());
    if (params?.offset) queryParams.append('offset', params.offset.toString());
    if (params?.status) queryParams.append('status', params.status);
    if (params?.start_time) queryParams.append('start_time', params.start_time);
    if (params?.end_time) queryParams.append('end_time', params.end_time);

    const query = queryParams.toString();
    return this.fetch<{ trades: Trade[]; total: number }>(
      `/trades${query ? `?${query}` : ''}`
    );
  }

  async getTradeById(id: string): Promise<Trade> {
    return this.fetch<Trade>(`/trades/${id}`);
  }

  // Positions endpoints
  async getPositions(): Promise<Position[]> {
    return this.fetch<Position[]>('/positions');
  }

  // System endpoints
  async getSystemStatus(): Promise<SystemStatus> {
    return this.fetch<SystemStatus>('/system/status');
  }

  async startSystem(): Promise<{ success: boolean; message: string }> {
    return this.fetch<{ success: boolean; message: string }>('/system/start', {
      method: 'POST',
    });
  }

  async stopSystem(): Promise<{ success: boolean; message: string }> {
    return this.fetch<{ success: boolean; message: string }>('/system/stop', {
      method: 'POST',
    });
  }

  async restartSystem(): Promise<{ success: boolean; message: string }> {
    return this.fetch<{ success: boolean; message: string }>('/system/restart', {
      method: 'POST',
    });
  }

  // Config endpoints
  async getConfig(): Promise<StrategyConfig> {
    return this.fetch<StrategyConfig>('/config');
  }

  async updateConfig(config: Partial<StrategyConfig>): Promise<{ success: boolean }> {
    return this.fetch<{ success: boolean }>('/config', {
      method: 'PUT',
      body: JSON.stringify(config),
    });
  }

  // Strategy endpoints
  async getRunningStrategies(): Promise<RunningStrategy[]> {
    return this.fetch<RunningStrategy[]>('/strategies/running');
  }

  async pauseSystem(domain?: string): Promise<{ success: boolean; message: string }> {
    return this.fetch<{ success: boolean; message: string }>('/system/pause', {
      method: 'POST',
      body: domain ? JSON.stringify({ domain }) : undefined,
    });
  }

  async resumeSystem(domain?: string): Promise<{ success: boolean; message: string }> {
    return this.fetch<{ success: boolean; message: string }>('/system/resume', {
      method: 'POST',
      body: domain ? JSON.stringify({ domain }) : undefined,
    });
  }

  async haltSystem(domain?: string): Promise<{ success: boolean; message: string }> {
    return this.fetch<{ success: boolean; message: string }>('/system/halt', {
      method: 'POST',
      body: domain ? JSON.stringify({ domain }) : undefined,
    });
  }

  // Risk endpoints
  async getRiskData(): Promise<RiskData> {
    return this.fetch<RiskData>('/sidecar/risk');
  }

  // Security endpoints
  async getSecurityEvents(params?: {
    limit?: number;
    severity?: string;
    start_time?: string;
  }): Promise<SecurityEvent[]> {
    const queryParams = new URLSearchParams();
    if (params?.limit) queryParams.append('limit', params.limit.toString());
    if (params?.severity) queryParams.append('severity', params.severity);
    if (params?.start_time) queryParams.append('start_time', params.start_time);

    const query = queryParams.toString();
    return this.fetch<SecurityEvent[]>(`/security/events${query ? `?${query}` : ''}`);
  }
}

export const api = new ApiService();
