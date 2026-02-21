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
  private readonly ADMIN_TOKEN_KEY = 'ploy_admin_token';
  private readonly SIDECAR_TOKEN_KEY = 'ploy_sidecar_token';

  private getStoredToken(key: string): string | null {
    if (typeof window === 'undefined') return null;
    try {
      return sessionStorage.getItem(key);
    } catch {
      return null;
    }
  }

  private setStoredToken(key: string, value: string | null) {
    if (typeof window === 'undefined') return;
    try {
      if (value) {
        sessionStorage.setItem(key, value);
      } else {
        sessionStorage.removeItem(key);
      }
    } catch {
      // storage not available in this environment
    }
  }

  setAdminToken(token: string) {
    const trimmed = token.trim();
    if (trimmed) {
      this.setStoredToken(this.ADMIN_TOKEN_KEY, trimmed);
    } else {
      this.setStoredToken(this.ADMIN_TOKEN_KEY, null);
    }
  }

  setSidecarToken(token: string) {
    const trimmed = token.trim();
    if (trimmed) {
      this.setStoredToken(this.SIDECAR_TOKEN_KEY, trimmed);
    } else {
      this.setStoredToken(this.SIDECAR_TOKEN_KEY, null);
    }
  }

  clearAdminToken() {
    this.setStoredToken(this.ADMIN_TOKEN_KEY, null);
  }

  clearSidecarToken() {
    this.setStoredToken(this.SIDECAR_TOKEN_KEY, null);
  }

  private async fetch<T>(endpoint: string, options?: RequestInit): Promise<T> {
    const headers = new Headers(options?.headers ? options.headers : {});
    headers.set('Content-Type', 'application/json');

    const adminToken = this.getStoredToken(this.ADMIN_TOKEN_KEY);
    const sidecarToken = this.getStoredToken(this.SIDECAR_TOKEN_KEY);
    if (adminToken && !headers.has('x-ploy-admin-token')) {
      headers.set('x-ploy-admin-token', adminToken);
    }
    if (adminToken && !headers.has('Authorization')) {
      headers.set('Authorization', `Bearer ${adminToken}`);
    }
    if (sidecarToken && !headers.has('x-ploy-sidecar-token')) {
      headers.set('x-ploy-sidecar-token', sidecarToken);
    }

    const response = await fetch(`${API_BASE}${endpoint}`, {
      credentials: 'same-origin',
      headers,
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
    const result = await this.fetch<{ success: boolean }>('/auth/login', {
      method: 'POST',
      body: JSON.stringify({
        admin_token: adminToken,
      }),
    });
    this.setAdminToken(adminToken);
    return result;
  }

  async logout(): Promise<{ success: boolean }> {
    this.clearAdminToken();
    this.clearSidecarToken();
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
