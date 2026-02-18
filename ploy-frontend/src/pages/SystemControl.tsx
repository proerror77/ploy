import { useState, useEffect } from 'react';
import { useQuery, useMutation, useQueryClient } from '@tanstack/react-query';
import { api } from '@/services/api';
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/Card';
import { Button } from '@/components/ui/Button';
import { Badge } from '@/components/ui/Badge';
import { ws } from '@/services/websocket';
import type { WebSocketEvent } from '@/services/websocket';
import { formatDuration } from '@/lib/utils';
import { Play, Square, RotateCw, Activity, Database, Wifi, WifiOff } from 'lucide-react';

export function SystemControl() {
  const queryClient = useQueryClient();
  const [wsConnected, setWsConnected] = useState(ws.isConnected());
  const [realtimeStatus, setRealtimeStatus] = useState<'running' | 'stopped' | 'error' | null>(null);

  // Track WebSocket connection
  useEffect(() => {
    const unsub = ws.onConnectionChange(setWsConnected);
    return unsub;
  }, []);

  // Subscribe to real-time status events
  useEffect(() => {
    const unsub = ws.subscribe('status', (event: WebSocketEvent) => {
      if (event.type === 'status') {
        setRealtimeStatus(event.data.status);
      }
    });
    return unsub;
  }, []);

  // Fallback polling at 30s (in case WebSocket disconnects)
  const { data: status, isLoading } = useQuery({
    queryKey: ['system', 'status'],
    queryFn: () => api.getSystemStatus(),
    refetchInterval: 30000,
  });

  // Merge real-time status with polled data
  const effectiveStatus = realtimeStatus ?? status?.status;

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
    if (!effectiveStatus) return null;
    const variants = {
      running: 'success' as const,
      stopped: 'secondary' as const,
      error: 'destructive' as const,
    };
    return <Badge variant={variants[effectiveStatus]}>{effectiveStatus}</Badge>;
  };

  if (isLoading) {
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
          <h1 className="text-3xl font-bold">System Control</h1>
          <p className="text-muted-foreground">Start, stop, and monitor the trading system</p>
        </div>
        <Badge
          variant={wsConnected ? 'success' : 'destructive'}
          className="flex items-center gap-1.5"
        >
          {wsConnected ? <Wifi className="h-3 w-3" /> : <WifiOff className="h-3 w-3" />}
          {wsConnected ? 'Live' : 'Polling'}
        </Badge>
      </div>

      <div className="grid grid-cols-1 gap-8 lg:grid-cols-3">
        <div className="lg:col-span-2 space-y-8">
          {/* System Status */}
          <Card>
            <CardHeader>
              <CardTitle>System Status</CardTitle>
            </CardHeader>
            <CardContent>
              <div className="space-y-4">
                <div className="flex items-center justify-between">
                  <span className="text-muted-foreground">Status</span>
                  {getStatusBadge()}
                </div>
                <div className="flex items-center justify-between">
                  <span className="text-muted-foreground">Uptime</span>
                  <span className="font-medium">
                    {status ? formatDuration(status.uptime_seconds) : '-'}
                  </span>
                </div>
                <div className="flex items-center justify-between">
                  <span className="text-muted-foreground">Version</span>
                  <span className="font-medium">{status?.version ?? '-'}</span>
                </div>
                <div className="flex items-center justify-between">
                  <span className="text-muted-foreground">Strategy</span>
                  <span className="font-medium">{status?.strategy ?? '-'}</span>
                </div>
                <div className="flex items-center justify-between">
                  <span className="text-muted-foreground">Last Trade</span>
                  <span className="font-medium">
                    {status?.last_trade_time
                      ? new Date(status.last_trade_time).toLocaleString('en-US')
                      : 'None'}
                  </span>
                </div>
              </div>
            </CardContent>
          </Card>

          {/* Control Panel */}
          <Card>
            <CardHeader>
              <CardTitle>Control Panel</CardTitle>
            </CardHeader>
            <CardContent>
              <div className="space-y-4">
                <div className="grid grid-cols-3 gap-4">
                  <Button
                    onClick={() => startMutation.mutate()}
                    disabled={
                      effectiveStatus === 'running' || startMutation.isPending
                    }
                    className="w-full"
                  >
                    <Play className="mr-2 h-4 w-4" />
                    Start
                  </Button>
                  <Button
                    variant="destructive"
                    onClick={() => stopMutation.mutate()}
                    disabled={effectiveStatus === 'stopped' || stopMutation.isPending}
                    className="w-full"
                  >
                    <Square className="mr-2 h-4 w-4" />
                    Stop
                  </Button>
                  <Button
                    variant="outline"
                    onClick={() => restartMutation.mutate()}
                    disabled={restartMutation.isPending}
                    className="w-full"
                  >
                    <RotateCw className="mr-2 h-4 w-4" />
                    Restart
                  </Button>
                </div>
                <div className="rounded-lg bg-muted p-4 text-sm text-muted-foreground">
                  <p>Warning: Stopping the system will close all active positions</p>
                  <p>Warning: Restart may take 30-60 seconds</p>
                </div>
              </div>
            </CardContent>
          </Card>
        </div>

        {/* Connection Status */}
        <div>
          <Card>
            <CardHeader>
              <CardTitle>Connections</CardTitle>
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
                      wsConnected ? 'bg-success' : 'bg-destructive'
                    }`}
                  />
                </div>
                <div className="flex items-center justify-between rounded-lg border p-3">
                  <div className="flex items-center gap-2">
                    <Database className="h-4 w-4" />
                    <span className="text-sm">Database</span>
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
                    <span className="text-sm">Errors (1h)</span>
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
