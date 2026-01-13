import { useQuery, useMutation, useQueryClient } from '@tanstack/react-query';
import { api } from '@/services/api';
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/Card';
import { Button } from '@/components/ui/Button';
import { Badge } from '@/components/ui/Badge';
import { formatDuration } from '@/lib/utils';
import { Play, Square, RotateCw, Activity, Database, Wifi } from 'lucide-react';

export function SystemControl() {
  const queryClient = useQueryClient();

  const { data: status, isLoading } = useQuery({
    queryKey: ['system', 'status'],
    queryFn: () => api.getSystemStatus(),
    refetchInterval: 5000,
  });

  const startMutation = useMutation({
    mutationFn: () => api.startSystem(),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['system', 'status'] });
    },
  });

  const stopMutation = useMutation({
    mutationFn: () => api.stopSystem(),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['system', 'status'] });
    },
  });

  const restartMutation = useMutation({
    mutationFn: () => api.restartSystem(),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['system', 'status'] });
    },
  });

  const getStatusBadge = () => {
    if (!status) return null;
    const variants = {
      running: 'success' as const,
      stopped: 'secondary' as const,
      error: 'destructive' as const,
    };
    return <Badge variant={variants[status.status]}>{status.status}</Badge>;
  };

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
        <h1 className="text-3xl font-bold">系统控制</h1>
        <p className="text-muted-foreground">启动、停止和监控交易系统</p>
      </div>

      <div className="grid grid-cols-1 gap-8 lg:grid-cols-3">
        <div className="lg:col-span-2 space-y-8">
          {/* System Status */}
          <Card>
            <CardHeader>
              <CardTitle>系统状态</CardTitle>
            </CardHeader>
            <CardContent>
              <div className="space-y-4">
                <div className="flex items-center justify-between">
                  <span className="text-muted-foreground">运行状态</span>
                  {getStatusBadge()}
                </div>
                <div className="flex items-center justify-between">
                  <span className="text-muted-foreground">运行时间</span>
                  <span className="font-medium">
                    {status ? formatDuration(status.uptime_seconds) : '-'}
                  </span>
                </div>
                <div className="flex items-center justify-between">
                  <span className="text-muted-foreground">版本</span>
                  <span className="font-medium">{status?.version ?? '-'}</span>
                </div>
                <div className="flex items-center justify-between">
                  <span className="text-muted-foreground">当前策略</span>
                  <span className="font-medium">{status?.strategy ?? '-'}</span>
                </div>
                <div className="flex items-center justify-between">
                  <span className="text-muted-foreground">最后交易时间</span>
                  <span className="font-medium">
                    {status?.last_trade_time
                      ? new Date(status.last_trade_time).toLocaleString('zh-CN')
                      : '无'}
                  </span>
                </div>
              </div>
            </CardContent>
          </Card>

          {/* Control Panel */}
          <Card>
            <CardHeader>
              <CardTitle>控制面板</CardTitle>
            </CardHeader>
            <CardContent>
              <div className="space-y-4">
                <div className="grid grid-cols-3 gap-4">
                  <Button
                    onClick={() => startMutation.mutate()}
                    disabled={
                      status?.status === 'running' || startMutation.isPending
                    }
                    className="w-full"
                  >
                    <Play className="mr-2 h-4 w-4" />
                    启动
                  </Button>
                  <Button
                    variant="destructive"
                    onClick={() => stopMutation.mutate()}
                    disabled={status?.status === 'stopped' || stopMutation.isPending}
                    className="w-full"
                  >
                    <Square className="mr-2 h-4 w-4" />
                    停止
                  </Button>
                  <Button
                    variant="outline"
                    onClick={() => restartMutation.mutate()}
                    disabled={restartMutation.isPending}
                    className="w-full"
                  >
                    <RotateCw className="mr-2 h-4 w-4" />
                    重启
                  </Button>
                </div>
                <div className="rounded-lg bg-muted p-4 text-sm text-muted-foreground">
                  <p>⚠️ 停止系统将关闭所有活跃仓位</p>
                  <p>⚠️ 重启系统可能需要 30-60 秒</p>
                </div>
              </div>
            </CardContent>
          </Card>
        </div>

        {/* Connection Status */}
        <div>
          <Card>
            <CardHeader>
              <CardTitle>连接状态</CardTitle>
            </CardHeader>
            <CardContent>
              <div className="space-y-4">
                <div className="flex items-center justify-between rounded-lg border p-3">
                  <div className="flex items-center gap-2">
                    <Wifi className="h-4 w-4" />
                    <span className="text-sm">WebSocket</span>
                  </div>
                  <div
                    className={`h-2 w-2 rounded-full ${
                      status?.websocket_connected ? 'bg-success' : 'bg-destructive'
                    }`}
                  />
                </div>
                <div className="flex items-center justify-between rounded-lg border p-3">
                  <div className="flex items-center gap-2">
                    <Database className="h-4 w-4" />
                    <span className="text-sm">数据库</span>
                  </div>
                  <div
                    className={`h-2 w-2 rounded-full ${
                      status?.database_connected ? 'bg-success' : 'bg-destructive'
                    }`}
                  />
                </div>
                <div className="flex items-center justify-between rounded-lg border p-3">
                  <div className="flex items-center gap-2">
                    <Activity className="h-4 w-4" />
                    <span className="text-sm">1小时错误数</span>
                  </div>
                  <span className="text-sm font-medium">
                    {status?.error_count_1h ?? 0}
                  </span>
                </div>
              </div>
            </CardContent>
          </Card>
        </div>
      </div>
    </div>
  );
}
