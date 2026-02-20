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

class ApiService {
  private async fetch<T>(endpoint: string, options?: RequestInit): Promise<T> {
    const response = await fetch(`${API_BASE}${endpoint}`, {
      credentials: 'same-origin',
      headers: {
        'Content-Type': 'application/json',
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

  async getAuthSession(): Promise<{ authenticated: boolean; auth_required: boolean }> {
    return this.fetch<{ authenticated: boolean; auth_required: boolean }>('/auth/session');
  }

  async login(adminToken: string): Promise<{ success: boolean }> {
    return this.fetch<{ success: boolean }>('/auth/login', {
      method: 'POST',
      body: JSON.stringify({
        admin_token: adminToken,
      }),
    });
  }

  async logout(): Promise<{ success: boolean }> {
    return this.fetch<{ success: boolean }>('/auth/logout', {
      method: 'POST',
    });
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
