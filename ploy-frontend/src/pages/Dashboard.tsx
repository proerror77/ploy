import { useQuery } from '@tanstack/react-query';
import { api } from '@/services/api';
import { StatCard } from '@/components/StatCard';
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/Card';
import { Badge } from '@/components/ui/Badge';
import { useStore } from '@/store';
import {
  TrendingUp,
  DollarSign,
  Target,
  AlertCircle,
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

  const { data: stats, isLoading: statsLoading } = useQuery({
    queryKey: ['stats', 'today'],
    queryFn: () => api.getTodayStats(),
    refetchInterval: 5000,
  });

  const { data: pnlHistory } = useQuery({
    queryKey: ['pnl', 'history'],
    queryFn: () => api.getPnLHistory(24),
    refetchInterval: 30000,
  });

  if (statsLoading) {
    return (
      <div className="flex h-full items-center justify-center">
        <div className="text-muted-foreground">加载中...</div>
      </div>
    );
  }

  return (
    <div className="p-8">
      <div className="mb-8">
        <h1 className="text-3xl font-bold">交易仪表盘</h1>
        <p className="text-muted-foreground">实时监控您的交易表现</p>
      </div>

      {/* Stats Grid */}
      <div className="mb-8 grid grid-cols-1 gap-4 md:grid-cols-2 lg:grid-cols-4">
        <StatCard
          title="今日盈亏"
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
          title="总交易次数"
          value={formatNumber(stats?.total_trades ?? 0)}
          subtitle={`成功 ${stats?.successful_trades ?? 0} / 失败 ${stats?.failed_trades ?? 0}`}
          icon={<Target className="h-4 w-4" />}
        />
        <StatCard
          title="胜率"
          value={formatPercentage(stats?.win_rate ?? 0)}
          subtitle={`共 ${stats?.total_trades ?? 0} 笔交易`}
          icon={<TrendingUp className="h-4 w-4" />}
          trend={stats && stats.win_rate > 0.5 ? 'up' : 'down'}
        />
        <StatCard
          title="活跃仓位"
          value={formatNumber(stats?.active_positions ?? 0)}
          subtitle={`当前持有`}
          icon={<AlertCircle className="h-4 w-4" />}
        />
      </div>

      {/* PnL Chart */}
      <Card className="mb-8">
        <CardHeader>
          <CardTitle>24小时盈亏曲线</CardTitle>
        </CardHeader>
        <CardContent>
          <ResponsiveContainer width="100%" height={300}>
            <LineChart data={pnlHistory ?? []}>
              <CartesianGrid strokeDasharray="3 3" />
              <XAxis
                dataKey="timestamp"
                tickFormatter={(value) =>
                  new Date(value).toLocaleTimeString('zh-CN', {
                    hour: '2-digit',
                    minute: '2-digit',
                  })
                }
              />
              <YAxis />
              <Tooltip
                formatter={(value: number) => formatCurrency(value)}
                labelFormatter={(label) => new Date(label).toLocaleString('zh-CN')}
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
            <CardTitle>活跃仓位</CardTitle>
          </CardHeader>
          <CardContent>
            <div className="space-y-4">
              {positions.length === 0 ? (
                <p className="text-center text-muted-foreground">暂无活跃仓位</p>
              ) : (
                positions.map((position) => (
                  <div
                    key={`${position.token_id}-${position.side}`}
                    className="flex items-center justify-between rounded-lg border p-4"
                  >
                    <div>
                      <div className="font-medium">{position.token_name}</div>
                      <div className="text-sm text-muted-foreground">
                        {position.shares} 股 @ {formatCurrency(position.entry_price)}
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
            <CardTitle>市场监控</CardTitle>
          </CardHeader>
          <CardContent>
            <div className="space-y-4">
              {Array.from(marketData.values()).length === 0 ? (
                <p className="text-center text-muted-foreground">暂无市场数据</p>
              ) : (
                Array.from(marketData.values()).map((market) => (
                  <div key={market.token_id} className="rounded-lg border p-4">
                    <div className="mb-2 flex items-center justify-between">
                      <div className="font-medium">{market.token_name}</div>
                      <div className="text-sm text-muted-foreground">
                        价差 {formatPercentage(market.spread)}
                      </div>
                    </div>
                    <div className="flex justify-between text-sm">
                      <div>
                        <span className="text-muted-foreground">买:</span>{' '}
                        {formatCurrency(market.best_bid)}
                      </div>
                      <div>
                        <span className="text-muted-foreground">卖:</span>{' '}
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
