import { useQuery } from '@tanstack/react-query';
import { api } from '@/services/api';
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/Card';
import { Badge } from '@/components/ui/Badge';
import { formatTimestamp, formatCurrency } from '@/lib/utils';
import { Activity, TrendingUp, Target, DollarSign } from 'lucide-react';

interface RunningStrategy {
  id: string;
  name: string;
  status: 'running' | 'stopped' | 'error';
  start_time: string;
  symbols: string[];
  total_trades: number;
  successful_trades: number;
  current_pnl: number;
  active_positions: number;
  last_trade_time: string | null;
  config: {
    min_move: number;
    max_entry: number;
    shares: number;
    predictive: boolean;
  };
}

// Removed unused StrategyPosition interface

export function StrategyMonitor() {
  const { data: strategies, isLoading } = useQuery({
    queryKey: ['strategies', 'running'],
    queryFn: async () => {
      // TODO: 实现获取运行中策略的 API
      // 暂时返回模拟数据
      return [
        {
          id: 'momentum-1',
          name: 'Momentum Strategy',
          status: 'running' as const,
          start_time: new Date(Date.now() - 3600000).toISOString(),
          symbols: ['BTCUSDT', 'ETHUSDT', 'SOLUSDT'],
          total_trades: 42,
          successful_trades: 38,
          current_pnl: 1250.50,
          active_positions: 3,
          last_trade_time: new Date(Date.now() - 300000).toISOString(),
          config: {
            min_move: 0.15,
            max_entry: 45,
            shares: 100,
            predictive: true,
          },
        },
      ] as RunningStrategy[];
    },
    refetchInterval: 5000,
  });

  const { data: positions } = useQuery({
    queryKey: ['positions'],
    queryFn: () => api.getPositions(),
    refetchInterval: 5000,
  });

  if (isLoading) {
    return (
      <div className="flex h-full items-center justify-center">
        <div className="text-muted-foreground">加载中...</div>
      </div>
    );
  }

  return (
    <div className="p-8">
      <div className="mb-8">
        <h1 className="text-3xl font-bold">策略监控</h1>
        <p className="text-muted-foreground">监控运行中的交易策略和实时状态</p>
      </div>

      {/* 运行中的策略列表 */}
      <div className="mb-8 space-y-4">
        {!strategies || strategies.length === 0 ? (
          <Card>
            <CardContent className="py-12 text-center">
              <p className="text-muted-foreground">暂无运行中的策略</p>
            </CardContent>
          </Card>
        ) : (
          strategies.map((strategy) => (
            <Card key={strategy.id} className="border-l-4 border-l-primary">
              <CardHeader>
                <div className="flex items-center justify-between">
                  <div className="flex items-center gap-3">
                    <Activity className="h-6 w-6 text-primary" />
                    <div>
                      <CardTitle>{strategy.name}</CardTitle>
                      <p className="text-sm text-muted-foreground">
                        运行时间: {formatTimestamp(strategy.start_time)}
                      </p>
                    </div>
                  </div>
                  <Badge
                    variant={
                      strategy.status === 'running'
                        ? 'success'
                        : strategy.status === 'error'
                        ? 'destructive'
                        : 'secondary'
                    }
                  >
                    {strategy.status === 'running' && '运行中'}
                    {strategy.status === 'stopped' && '已停止'}
                    {strategy.status === 'error' && '错误'}
                  </Badge>
                </div>
              </CardHeader>
              <CardContent>
                <div className="grid grid-cols-1 gap-6 lg:grid-cols-2">
                  {/* 左侧：监控目标 */}
                  <div>
                    <h3 className="mb-3 font-semibold">监控目标</h3>
                    <div className="space-y-2">
                      <div className="flex items-center justify-between rounded-lg bg-muted p-3">
                        <span className="text-sm text-muted-foreground">交易标的</span>
                        <div className="flex gap-1">
                          {strategy.symbols.map((symbol) => (
                            <Badge key={symbol} variant="secondary">
                              {symbol}
                            </Badge>
                          ))}
                        </div>
                      </div>
                      <div className="flex items-center justify-between rounded-lg bg-muted p-3">
                        <span className="text-sm text-muted-foreground">最小移动</span>
                        <span className="font-medium">{strategy.config.min_move}%</span>
                      </div>
                      <div className="flex items-center justify-between rounded-lg bg-muted p-3">
                        <span className="text-sm text-muted-foreground">最大入场</span>
                        <span className="font-medium">{strategy.config.max_entry}%</span>
                      </div>
                      <div className="flex items-center justify-between rounded-lg bg-muted p-3">
                        <span className="text-sm text-muted-foreground">每笔股数</span>
                        <span className="font-medium">{strategy.config.shares}</span>
                      </div>
                      <div className="flex items-center justify-between rounded-lg bg-muted p-3">
                        <span className="text-sm text-muted-foreground">预测模式</span>
                        <Badge variant={strategy.config.predictive ? 'success' : 'secondary'}>
                          {strategy.config.predictive ? '开启' : '关闭'}
                        </Badge>
                      </div>
                    </div>
                  </div>

                  {/* 右侧：运行统计 */}
                  <div>
                    <h3 className="mb-3 font-semibold">运行统计</h3>
                    <div className="grid grid-cols-2 gap-3">
                      <div className="rounded-lg border bg-card p-4">
                        <div className="flex items-center gap-2 text-muted-foreground">
                          <Target className="h-4 w-4" />
                          <span className="text-sm">总交易</span>
                        </div>
                        <div className="mt-2 text-2xl font-bold">
                          {strategy.total_trades}
                        </div>
                        <div className="mt-1 text-xs text-success">
                          成功 {strategy.successful_trades}
                        </div>
                      </div>

                      <div className="rounded-lg border bg-card p-4">
                        <div className="flex items-center gap-2 text-muted-foreground">
                          <DollarSign className="h-4 w-4" />
                          <span className="text-sm">当前盈亏</span>
                        </div>
                        <div
                          className={`mt-2 text-2xl font-bold ${
                            strategy.current_pnl >= 0 ? 'text-success' : 'text-destructive'
                          }`}
                        >
                          {formatCurrency(strategy.current_pnl)}
                        </div>
                        <div className="mt-1 text-xs text-muted-foreground">
                          胜率{' '}
                          {((strategy.successful_trades / strategy.total_trades) * 100).toFixed(
                            1
                          )}
                          %
                        </div>
                      </div>

                      <div className="rounded-lg border bg-card p-4">
                        <div className="flex items-center gap-2 text-muted-foreground">
                          <Activity className="h-4 w-4" />
                          <span className="text-sm">活跃仓位</span>
                        </div>
                        <div className="mt-2 text-2xl font-bold">
                          {strategy.active_positions}
                        </div>
                        <div className="mt-1 text-xs text-muted-foreground">持仓中</div>
                      </div>

                      <div className="rounded-lg border bg-card p-4">
                        <div className="flex items-center gap-2 text-muted-foreground">
                          <TrendingUp className="h-4 w-4" />
                          <span className="text-sm">最后交易</span>
                        </div>
                        <div className="mt-2 text-sm font-medium">
                          {strategy.last_trade_time
                            ? new Date(strategy.last_trade_time).toLocaleTimeString('zh-CN')
                            : '无'}
                        </div>
                        <div className="mt-1 text-xs text-muted-foreground">
                          {strategy.last_trade_time
                            ? `${Math.floor(
                                (Date.now() - new Date(strategy.last_trade_time).getTime()) /
                                  60000
                              )} 分钟前`
                            : '-'}
                        </div>
                      </div>
                    </div>
                  </div>
                </div>

                {/* 当前持仓详情 */}
                {positions && positions.length > 0 && (
                  <div className="mt-6">
                    <h3 className="mb-3 font-semibold">当前持仓</h3>
                    <div className="space-y-2">
                      {positions.map((position) => (
                        <div
                          key={`${position.token_id}-${position.side}`}
                          className="flex items-center justify-between rounded-lg border p-4"
                        >
                          <div className="flex items-center gap-4">
                            <Badge variant={position.side === 'UP' ? 'success' : 'destructive'}>
                              {position.side}
                            </Badge>
                            <div>
                              <div className="font-medium">{position.token_name}</div>
                              <div className="text-sm text-muted-foreground">
                                {position.shares} 股 @ {formatCurrency(position.entry_price)}
                              </div>
                            </div>
                          </div>
                          <div className="text-right">
                            <div className="font-medium">
                              当前: {formatCurrency(position.current_price)}
                            </div>
                            <div
                              className={`text-sm font-medium ${
                                position.unrealized_pnl >= 0 ? 'text-success' : 'text-destructive'
                              }`}
                            >
                              {position.unrealized_pnl >= 0 ? '+' : ''}
                              {formatCurrency(position.unrealized_pnl)}
                            </div>
                          </div>
                        </div>
                      ))}
                    </div>
                  </div>
                )}
              </CardContent>
            </Card>
          ))
        )}
      </div>
    </div>
  );
}
