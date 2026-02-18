import { useState, useEffect } from 'react';
import { useQuery } from '@tanstack/react-query';
import { api } from '@/services/api';
import { StatCard } from '@/components/StatCard';
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/Card';
import { Badge } from '@/components/ui/Badge';
import { useStore } from '@/store';
import { ws } from '@/services/websocket';
import type { WebSocketEvent } from '@/services/websocket';
import type { Position } from '@/types';
import {
  TrendingUp,
  DollarSign,
  Target,
  AlertCircle,
  Wifi,
  WifiOff,
} from 'lucide-react';
import { formatCurrency, formatPercentage, formatNumber } from '@/lib/utils';
import {
  LineChart,
  Line,
  XAxis,
  YAxis,
  CartesianGrid,
  Tooltip,
  ResponsiveContainer,
} from 'recharts';

export function Dashboard() {
  const { positions, marketData } = useStore();
  const [wsConnected, setWsConnected] = useState(ws.isConnected());
  const [realtimePositions, setRealtimePositions] = useState<Position[]>([]);
  const [recentTradeCount, setRecentTradeCount] = useState(0);

  // WebSocket connection tracking
  useEffect(() => {
    const unsub = ws.onConnectionChange(setWsConnected);
    return unsub;
  }, []);

  // Subscribe to position events via WebSocket
  useEffect(() => {
    const unsubPosition = ws.subscribe('position', (event: WebSocketEvent) => {
      if (event.type === 'position') {
        setRealtimePositions((prev) => {
          const idx = prev.findIndex(
            (p) => p.token_id === event.data.token_id && p.side === event.data.side
          );
          if (idx >= 0) {
            const updated = [...prev];
            updated[idx] = event.data;
            return updated;
          }
          return [...prev, event.data];
        });
      }
    });

    const unsubTrade = ws.subscribe('trade', (_event: WebSocketEvent) => {
      setRecentTradeCount((c) => c + 1);
    });

    return () => {
      unsubPosition();
      unsubTrade();
    };
  }, []);

  // Use WebSocket positions if available, otherwise fall back to store
  const { data: polledPositions } = useQuery({
    queryKey: ['positions'],
    queryFn: () => api.getPositions(),
    refetchInterval: wsConnected ? 30000 : 10000,
  });

  const displayPositions =
    realtimePositions.length > 0
      ? realtimePositions
      : polledPositions && polledPositions.length > 0
        ? polledPositions
        : positions;

  // PnL aggregate: 30s polling (not worth WebSocket)
  const { data: stats, isLoading: statsLoading } = useQuery({
    queryKey: ['stats', 'today'],
    queryFn: () => api.getTodayStats(),
    refetchInterval: 30000,
  });

  const { data: pnlHistory } = useQuery({
    queryKey: ['pnl', 'history'],
    queryFn: () => api.getPnLHistory(24),
    refetchInterval: 30000,
  });

  if (statsLoading) {
    return (
      <div className="flex h-full items-center justify-center">
        <div className="text-muted-foreground">Loading...</div>
      </div>
    );
  }

  return (
    <div className="p-8">
      <div className="mb-8 flex items-center justify-between">
        <div>
          <h1 className="text-3xl font-bold">Trading Dashboard</h1>
          <p className="text-muted-foreground">Real-time trading performance monitoring</p>
        </div>
        <Badge
          variant={wsConnected ? 'success' : 'destructive'}
          className="flex items-center gap-1.5"
        >
          {wsConnected ? (
            <Wifi className="h-3 w-3" />
          ) : (
            <WifiOff className="h-3 w-3" />
          )}
          {wsConnected ? 'Connected' : 'Disconnected'}
        </Badge>
      </div>

      {/* Stats Grid */}
      <div className="mb-8 grid grid-cols-1 gap-4 md:grid-cols-2 lg:grid-cols-4">
        <StatCard
          title="Today P&L"
          value={formatCurrency(stats?.pnl ?? 0)}
          subtitle={
            stats && stats.pnl > 0
              ? `+${formatPercentage(stats.pnl / (stats.total_volume || 1))}`
              : undefined
          }
          icon={<DollarSign className="h-4 w-4" />}
          trend={stats && stats.pnl > 0 ? 'up' : stats && stats.pnl < 0 ? 'down' : 'neutral'}
        />
        <StatCard
          title="Total Trades"
          value={formatNumber((stats?.total_trades ?? 0) + recentTradeCount)}
          subtitle={`Win ${stats?.successful_trades ?? 0} / Fail ${stats?.failed_trades ?? 0}`}
          icon={<Target className="h-4 w-4" />}
        />
        <StatCard
          title="Win Rate"
          value={formatPercentage(stats?.win_rate ?? 0)}
          subtitle={`${(stats?.total_trades ?? 0) + recentTradeCount} total trades`}
          icon={<TrendingUp className="h-4 w-4" />}
          trend={stats && stats.win_rate > 0.5 ? 'up' : 'down'}
        />
        <StatCard
          title="Active Positions"
          value={formatNumber(displayPositions.length || stats?.active_positions || 0)}
          subtitle="Currently held"
          icon={<AlertCircle className="h-4 w-4" />}
        />
      </div>

      {/* PnL Chart */}
      <Card className="mb-8">
        <CardHeader>
          <CardTitle>24h P&L Curve</CardTitle>
        </CardHeader>
        <CardContent>
          <ResponsiveContainer width="100%" height={300}>
            <LineChart data={pnlHistory ?? []}>
              <CartesianGrid strokeDasharray="3 3" />
              <XAxis
                dataKey="timestamp"
                tickFormatter={(value) =>
                  new Date(value).toLocaleTimeString('en-US', {
                    hour: '2-digit',
                    minute: '2-digit',
                  })
                }
              />
              <YAxis />
              <Tooltip
                formatter={(value: number) => formatCurrency(value)}
                labelFormatter={(label) => new Date(label).toLocaleString('en-US')}
              />
              <Line
                type="monotone"
                dataKey="cumulative_pnl"
                stroke="#3b82f6"
                strokeWidth={2}
                dot={false}
              />
            </LineChart>
          </ResponsiveContainer>
        </CardContent>
      </Card>

      <div className="grid grid-cols-1 gap-8 lg:grid-cols-2">
        {/* Active Positions */}
        <Card>
          <CardHeader>
            <CardTitle>Active Positions</CardTitle>
          </CardHeader>
          <CardContent>
            <div className="space-y-4">
              {displayPositions.length === 0 ? (
                <p className="text-center text-muted-foreground">No active positions</p>
              ) : (
                displayPositions.map((position) => (
                  <div
                    key={`${position.token_id}-${position.side}`}
                    className="flex items-center justify-between rounded-lg border p-4"
                  >
                    <div>
                      <div className="font-medium">{position.token_name}</div>
                      <div className="text-sm text-muted-foreground">
                        {position.shares} shares @ {formatCurrency(position.entry_price)}
                      </div>
                    </div>
                    <div className="text-right">
                      <Badge variant={position.side === 'UP' ? 'success' : 'destructive'}>
                        {position.side}
                      </Badge>
                      <div
                        className={`mt-1 text-sm font-medium ${
                          position.unrealized_pnl >= 0 ? 'text-success' : 'text-destructive'
                        }`}
                      >
                        {formatCurrency(position.unrealized_pnl)}
                      </div>
                    </div>
                  </div>
                ))
              )}
            </div>
          </CardContent>
        </Card>

        {/* Market Monitor */}
        <Card>
          <CardHeader>
            <CardTitle>Market Monitor</CardTitle>
          </CardHeader>
          <CardContent>
            <div className="space-y-4">
              {Array.from(marketData.values()).length === 0 ? (
                <p className="text-center text-muted-foreground">No market data</p>
              ) : (
                Array.from(marketData.values()).map((market) => (
                  <div key={market.token_id} className="rounded-lg border p-4">
                    <div className="mb-2 flex items-center justify-between">
                      <div className="font-medium">{market.token_name}</div>
                      <div className="text-sm text-muted-foreground">
                        Spread {formatPercentage(market.spread)}
                      </div>
                    </div>
                    <div className="flex justify-between text-sm">
                      <div>
                        <span className="text-muted-foreground">Bid:</span>{' '}
                        {formatCurrency(market.best_bid)}
                      </div>
                      <div>
                        <span className="text-muted-foreground">Ask:</span>{' '}
                        {formatCurrency(market.best_ask)}
                      </div>
                    </div>
                  </div>
                ))
              )}
            </div>
          </CardContent>
        </Card>
      </div>
    </div>
  );
}
