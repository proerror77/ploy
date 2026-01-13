import { useState } from 'react';
import { useQuery } from '@tanstack/react-query';
import { api } from '@/services/api';
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/Card';
import { Badge } from '@/components/ui/Badge';
import { formatCurrency, formatTimestamp } from '@/lib/utils';
import type { Trade } from '@/types';

export function TradeHistory() {
  const [statusFilter, setStatusFilter] = useState<string>('all');
  const [page, setPage] = useState(0);
  const pageSize = 20;

  const { data, isLoading } = useQuery({
    queryKey: ['trades', statusFilter, page],
    queryFn: () =>
      api.getTrades({
        limit: pageSize,
        offset: page * pageSize,
        status: statusFilter === 'all' ? undefined : statusFilter,
      }),
    refetchInterval: 10000,
  });

  const getStatusBadge = (status: Trade['status']) => {
    const variants: Record<Trade['status'], 'success' | 'warning' | 'destructive' | 'secondary'> = {
      COMPLETED: 'success',
      LEG1_FILLED: 'warning',
      LEG2_FILLED: 'warning',
      PENDING: 'secondary',
      FAILED: 'destructive',
    };
    return <Badge variant={variants[status]}>{status}</Badge>;
  };

  return (
    <div className="p-8">
      <div className="mb-8">
        <h1 className="text-3xl font-bold">交易历史</h1>
        <p className="text-muted-foreground">查看所有历史交易记录</p>
      </div>

      <Card>
        <CardHeader>
          <div className="flex items-center justify-between">
            <CardTitle>交易列表</CardTitle>
            <div className="flex gap-2">
              {['all', 'COMPLETED', 'PENDING', 'FAILED'].map((status) => (
                <button
                  key={status}
                  onClick={() => setStatusFilter(status)}
                  className={`rounded-md px-3 py-1 text-sm ${
                    statusFilter === status
                      ? 'bg-primary text-primary-foreground'
                      : 'bg-secondary text-secondary-foreground hover:bg-secondary/80'
                  }`}
                >
                  {status === 'all' ? '全部' : status}
                </button>
              ))}
            </div>
          </div>
        </CardHeader>
        <CardContent>
          {isLoading ? (
            <div className="py-8 text-center text-muted-foreground">加载中...</div>
          ) : !data?.trades || data.trades.length === 0 ? (
            <div className="py-8 text-center text-muted-foreground">暂无交易记录</div>
          ) : (
            <>
              <div className="overflow-x-auto">
                <table className="w-full">
                  <thead>
                    <tr className="border-b text-left text-sm font-medium text-muted-foreground">
                      <th className="pb-3">时间</th>
                      <th className="pb-3">标的</th>
                      <th className="pb-3">方向</th>
                      <th className="pb-3">数量</th>
                      <th className="pb-3">入场价</th>
                      <th className="pb-3">出场价</th>
                      <th className="pb-3">盈亏</th>
                      <th className="pb-3">状态</th>
                    </tr>
                  </thead>
                  <tbody>
                    {data.trades.map((trade) => (
                      <tr key={trade.id} className="border-b">
                        <td className="py-4 text-sm">
                          {formatTimestamp(trade.timestamp)}
                        </td>
                        <td className="py-4">
                          <div className="font-medium">{trade.token_name}</div>
                          <div className="text-xs text-muted-foreground">
                            {trade.token_id.slice(0, 8)}...
                          </div>
                        </td>
                        <td className="py-4">
                          <Badge variant={trade.side === 'UP' ? 'success' : 'destructive'}>
                            {trade.side}
                          </Badge>
                        </td>
                        <td className="py-4">{trade.shares}</td>
                        <td className="py-4">{formatCurrency(trade.entry_price)}</td>
                        <td className="py-4">
                          {trade.exit_price ? formatCurrency(trade.exit_price) : '-'}
                        </td>
                        <td className="py-4">
                          {trade.pnl !== null ? (
                            <span
                              className={
                                trade.pnl >= 0 ? 'text-success' : 'text-destructive'
                              }
                            >
                              {formatCurrency(trade.pnl)}
                            </span>
                          ) : (
                            '-'
                          )}
                        </td>
                        <td className="py-4">{getStatusBadge(trade.status)}</td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>

              {/* Pagination */}
              <div className="mt-4 flex items-center justify-between">
                <div className="text-sm text-muted-foreground">
                  共 {data.total} 笔交易
                </div>
                <div className="flex gap-2">
                  <button
                    onClick={() => setPage((p) => Math.max(0, p - 1))}
                    disabled={page === 0}
                    className="rounded-md bg-secondary px-3 py-1 text-sm disabled:opacity-50"
                  >
                    上一页
                  </button>
                  <button
                    onClick={() => setPage((p) => p + 1)}
                    disabled={(page + 1) * pageSize >= data.total}
                    className="rounded-md bg-secondary px-3 py-1 text-sm disabled:opacity-50"
                  >
                    下一页
                  </button>
                </div>
              </div>
            </>
          )}
        </CardContent>
      </Card>
    </div>
  );
}
