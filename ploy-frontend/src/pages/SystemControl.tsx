import { useState, useEffect } from 'react';
import { useQuery, useMutation, useQueryClient } from '@tanstack/react-query';
import { api } from '@/services/api';
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/Card';
import { Button } from '@/components/ui/Button';
import { Badge } from '@/components/ui/Badge';
import { ws } from '@/services/websocket';
import type { WebSocketEvent } from '@/services/websocket';
import type { ActiveAlert, PlatformMetrics } from '@/types';
import { formatDuration } from '@/lib/utils';
import {
  Play,
  Square,
  RotateCw,
  Activity,
  Database,
  Wifi,
  WifiOff,
  AlertTriangle,
  Clock,
} from 'lucide-react';

export function SystemControl() {
  const queryClient = useQueryClient();
  const [wsConnected, setWsConnected] = useState(ws.isConnected());
  const [realtimeStatus, setRealtimeStatus] = useState<string | null>(null);
  const [realtimeSnapshot, setRealtimeSnapshot] = useState<Awaited<
    ReturnType<typeof api.getSystemStatus>
  > | null>(null);
  const [realtimeMetrics, setRealtimeMetrics] = useState<PlatformMetrics | null>(null);
  const [realtimeAlerts, setRealtimeAlerts] = useState<ActiveAlert[] | null>(null);

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

  useEffect(() => {
    const unsub = ws.subscribe('system_snapshot', (event: WebSocketEvent) => {
      if (event.type === 'system_snapshot') {
        setRealtimeSnapshot(event.data.system);
        setRealtimeStatus(event.data.system.status);
        queryClient.setQueryData(['system', 'status'], event.data.system);
      }
    });
    return unsub;
  }, [queryClient]);

  useEffect(() => {
    const unsub = ws.subscribe('metrics_snapshot', (event: WebSocketEvent) => {
      if (event.type === 'metrics_snapshot') {
        setRealtimeMetrics(event.data.metrics);
        queryClient.setQueryData(['system', 'metrics'], event.data.metrics);
      }
    });
    return unsub;
  }, [queryClient]);

  useEffect(() => {
    const unsub = ws.subscribe('alert_snapshot', (event: WebSocketEvent) => {
      if (event.type === 'alert_snapshot') {
        setRealtimeAlerts(event.data.alerts);
        queryClient.setQueryData(['system', 'alerts'], event.data.alerts);
      }
    });
    return unsub;
  }, [queryClient]);

  // Fallback polling at 30s (in case WebSocket disconnects)
  const { data: status, isLoading: statusLoading } = useQuery({
    queryKey: ['system', 'status'],
    queryFn: () => api.getSystemStatus(),
    refetchInterval: 30000,
  });

  const { data: metrics, isLoading: metricsLoading } = useQuery({
    queryKey: ['system', 'metrics'],
    queryFn: () => api.getSystemMetrics(),
    refetchInterval: 30000,
  });

  const { data: alerts, isLoading: alertsLoading } = useQuery({
    queryKey: ['system', 'alerts'],
    queryFn: () => api.getSystemAlerts(),
    refetchInterval: 30000,
  });

  // Merge real-time status with polled data
  const effectiveStatus = realtimeStatus ?? realtimeSnapshot?.status ?? status?.status;
  const effectiveSnapshot = realtimeSnapshot ?? status;
  const effectiveMetrics = realtimeMetrics ?? metrics;
  const effectiveAlerts = realtimeAlerts ?? alerts ?? [];
  const heartbeats = effectiveMetrics?.heartbeats ?? [];
  const isLoading = statusLoading || metricsLoading || alertsLoading;

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
    const variant =
      effectiveStatus === 'running'
        ? 'success'
        : effectiveStatus === 'stopped'
          ? 'secondary'
          : effectiveStatus === 'starting' || effectiveStatus === 'recovering'
            ? 'outline'
            : 'destructive';
    return <Badge variant={variant}>{effectiveStatus}</Badge>;
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
                    {effectiveSnapshot ? formatDuration(effectiveSnapshot.uptime_seconds) : '-'}
                  </span>
                </div>
                <div className="flex items-center justify-between">
                  <span className="text-muted-foreground">Version</span>
                  <span className="font-medium">{effectiveSnapshot?.version ?? '-'}</span>
                </div>
                <div className="flex items-center justify-between">
                  <span className="text-muted-foreground">Strategy</span>
                  <span className="font-medium">{effectiveSnapshot?.strategy ?? '-'}</span>
                </div>
                <div className="flex items-center justify-between">
                  <span className="text-muted-foreground">Last Trade</span>
                  <span className="font-medium">
                    {effectiveSnapshot?.last_trade_time
                      ? new Date(effectiveSnapshot.last_trade_time).toLocaleString('en-US')
                      : 'None'}
                  </span>
                </div>
                <div className="flex items-center justify-between">
                  <span className="text-muted-foreground">Last Reconcile</span>
                  <span className="font-medium">
                    {effectiveSnapshot?.last_live_reconcile_success_at
                      ? new Date(effectiveSnapshot.last_live_reconcile_success_at).toLocaleString(
                          'en-US'
                        )
                      : 'None'}
                  </span>
                </div>
                <div className="flex items-center justify-between">
                  <span className="text-muted-foreground">Active Alerts</span>
                  <span className="font-medium">{effectiveSnapshot?.active_alert_count ?? 0}</span>
                </div>
                <div className="flex items-center justify-between">
                  <span className="text-muted-foreground">Stale Sources</span>
                  <span className="font-medium">{effectiveSnapshot?.stale_source_count ?? 0}</span>
                </div>
              </div>
            </CardContent>
          </Card>

          <Card>
            <CardHeader>
              <CardTitle>Runtime Metrics</CardTitle>
            </CardHeader>
            <CardContent>
              <div className="space-y-4">
                <div className="grid grid-cols-2 gap-4">
                  <div className="rounded-lg border p-4">
                    <div className="text-sm text-muted-foreground">Deployments</div>
                    <div className="mt-1 text-lg font-semibold">
                      {effectiveMetrics?.total_deployments ?? 0}
                    </div>
                  </div>
                  <div className="rounded-lg border p-4">
                    <div className="text-sm text-muted-foreground">Live / Degraded</div>
                    <div className="mt-1 text-lg font-semibold">
                      {effectiveMetrics?.live_deployments ?? 0} /{' '}
                      {effectiveMetrics?.degraded_deployments ?? 0}
                    </div>
                  </div>
                  <div className="rounded-lg border p-4">
                    <div className="text-sm text-muted-foreground">Active Alerts</div>
                    <div className="mt-1 text-lg font-semibold">
                      {effectiveMetrics?.active_alerts ?? effectiveSnapshot?.active_alert_count ?? 0}
                    </div>
                  </div>
                  <div className="rounded-lg border p-4">
                    <div className="text-sm text-muted-foreground">Stale Sources</div>
                    <div className="mt-1 text-lg font-semibold">
                      {effectiveMetrics?.stale_sources ?? effectiveSnapshot?.stale_source_count ?? 0}
                    </div>
                  </div>
                </div>

                <div className="flex items-center justify-between rounded-lg border p-3">
                  <div className="flex items-center gap-2">
                    <Clock className="h-4 w-4" />
                    <span className="text-sm">Last Successful Reconcile</span>
                  </div>
                  <span className="text-sm font-medium">
                    {effectiveMetrics?.last_live_reconcile_success_at ??
                      effectiveSnapshot?.last_live_reconcile_success_at ??
                      '-'}
                  </span>
                </div>

                <div className="space-y-2">
                  <div className="text-sm font-medium">Heartbeats</div>
                  {heartbeats.length === 0 ? (
                    <div className="rounded-lg border p-3 text-sm text-muted-foreground">
                      No heartbeat sources reported yet.
                    </div>
                  ) : (
                    <div className="space-y-2">
                      {heartbeats.map((heartbeat) => (
                        <div
                          key={heartbeat.source_id}
                          className="flex items-center justify-between rounded-lg border p-3"
                        >
                          <div>
                            <div className="text-sm font-medium">{heartbeat.source_id}</div>
                            <div className="text-xs text-muted-foreground">
                              {heartbeat.source_kind} • stale after {heartbeat.stale_after_seconds}s
                            </div>
                            {heartbeat.message ? (
                              <div className="mt-1 text-xs text-muted-foreground">
                                {heartbeat.message}
                              </div>
                            ) : null}
                          </div>
                          <Badge
                            variant={heartbeat.state === 'healthy' ? 'success' : 'destructive'}
                          >
                            {heartbeat.state}
                          </Badge>
                        </div>
                      ))}
                    </div>
                  )}
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
                      effectiveStatus === 'running' ||
                      effectiveStatus === 'recovering' ||
                      startMutation.isPending
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
                  <p>Stop pauses coordinator-managed activity. It does not force-close positions.</p>
                  <p>Use emergency halt from the domain-specific control surface when positions must be unwound immediately.</p>
                  <p>Restart performs a pause/resume cycle and may take 30-60 seconds.</p>
                </div>
              </div>
            </CardContent>
          </Card>

          <Card>
            <CardHeader>
              <CardTitle>Active Alerts</CardTitle>
            </CardHeader>
            <CardContent>
              <div className="space-y-3">
                {effectiveAlerts.length === 0 ? (
                  <div className="rounded-lg border p-4 text-sm text-muted-foreground">
                    No active alerts.
                  </div>
                ) : (
                  effectiveAlerts.map((alert) => (
                    <div key={alert.alert_id} className="rounded-lg border p-4">
                      <div className="flex items-start justify-between gap-3">
                        <div className="flex items-start gap-2">
                          <AlertTriangle className="mt-0.5 h-4 w-4 text-warning" />
                          <div>
                            <div className="text-sm font-medium">{alert.message}</div>
                            <div className="text-xs text-muted-foreground">
                              {alert.source_id} • {alert.kind}
                            </div>
                            <div className="mt-1 text-xs text-muted-foreground">
                              Triggered {new Date(alert.triggered_at).toLocaleString('en-US')}
                            </div>
                          </div>
                        </div>
                        <Badge
                          variant={alert.severity === 'critical' ? 'destructive' : 'outline'}
                        >
                          {alert.severity}
                        </Badge>
                      </div>
                    </div>
                  ))
                )}
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
                    <span className="text-sm">Event Stream</span>
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
                      effectiveSnapshot?.database_connected ? 'bg-success' : 'bg-destructive'
                    }`}
                  />
                </div>
                <div className="flex items-center justify-between rounded-lg border p-3">
                  <div className="flex items-center gap-2">
                    <Activity className="h-4 w-4" />
                    <span className="text-sm">Errors (1h)</span>
                  </div>
                  <span className="text-sm font-medium">
                    {effectiveSnapshot?.error_count_1h ?? 0}
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
