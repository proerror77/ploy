import { useEffect, useState } from 'react';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import {
  Activity,
  AlertTriangle,
  Bell,
  Database,
  Gauge,
  Play,
  RotateCw,
  Square,
  Wifi,
  WifiOff,
} from 'lucide-react';

import { Badge } from '@/components/ui/Badge';
import { Button } from '@/components/ui/Button';
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/Card';
import { api } from '@/services/api';
import { ws } from '@/services/websocket';
import type { AlertRecord, SystemMetrics, SystemStatus } from '@/types';
import { formatDuration } from '@/lib/utils';
import type { WebSocketEvent } from '@/services/websocket';

function statusBadgeVariant(status?: string | null) {
  if (!status) {
    return 'secondary' as const;
  }

  if (status === 'running') {
    return 'success' as const;
  }

  if (status === 'starting' || status === 'recovering') {
    return 'outline' as const;
  }

  if (status === 'degraded') {
    return 'warning' as const;
  }

  if (status === 'stopped') {
    return 'secondary' as const;
  }

  return 'destructive' as const;
}

function alertBadgeVariant(severity: AlertRecord['severity']) {
  switch (severity) {
    case 'critical':
      return 'destructive' as const;
    case 'warning':
      return 'warning' as const;
    case 'info':
    default:
      return 'outline' as const;
  }
}

function formatTimestamp(value: string | null | undefined) {
  if (!value) {
    return '-';
  }

  return new Date(value).toLocaleString('en-US');
}

function MetricTile({
  label,
  value,
  hint,
}: {
  label: string;
  value: string | number;
  hint: string;
}) {
  return (
    <div className="rounded-lg border bg-card p-4">
      <div className="text-sm text-muted-foreground">{label}</div>
      <div className="mt-2 text-2xl font-bold">{value}</div>
      <div className="mt-1 text-xs text-muted-foreground">{hint}</div>
    </div>
  );
}

export function SystemControl() {
  const queryClient = useQueryClient();
  const [wsConnected, setWsConnected] = useState(ws.isConnected());
  const [realtimeStatus, setRealtimeStatus] = useState<string | null>(null);
  const [realtimeSnapshot, setRealtimeSnapshot] = useState<SystemStatus | null>(null);
  const [realtimeMetrics, setRealtimeMetrics] = useState<SystemMetrics | null>(null);
  const [realtimeAlerts, setRealtimeAlerts] = useState<AlertRecord[] | null>(null);

  useEffect(() => {
    const unsub = ws.onConnectionChange(setWsConnected);
    return unsub;
  }, []);

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

  const { data: status, isLoading: statusLoading } = useQuery({
    queryKey: ['system', 'status'],
    queryFn: () => api.getSystemStatus(),
    refetchInterval: 30000,
  });

  const { data: metrics, isLoading: metricsLoading, error: metricsError } = useQuery({
    queryKey: ['system', 'metrics'],
    queryFn: () => api.getSystemMetrics(),
    refetchInterval: 30000,
  });

  const {
    data: alerts = [],
    isLoading: alertsLoading,
    error: alertsError,
  } = useQuery({
    queryKey: ['system', 'alerts'],
    queryFn: () => api.getSystemAlerts(),
    refetchInterval: 30000,
  });

  const effectiveStatus = realtimeStatus ?? realtimeSnapshot?.status ?? status?.status;
  const effectiveSnapshot = realtimeSnapshot ?? status;
  const effectiveMetrics = realtimeMetrics ?? metrics;
  const effectiveAlerts = realtimeAlerts ?? alerts;

  const invalidateSystemViews = () => {
    queryClient.invalidateQueries({ queryKey: ['system', 'status'] });
    queryClient.invalidateQueries({ queryKey: ['system', 'metrics'] });
    queryClient.invalidateQueries({ queryKey: ['system', 'alerts'] });
  };

  const startMutation = useMutation({
    mutationFn: () => api.startSystem(),
    onSuccess: invalidateSystemViews,
  });

  const stopMutation = useMutation({
    mutationFn: () => api.stopSystem(),
    onSuccess: invalidateSystemViews,
  });

  const restartMutation = useMutation({
    mutationFn: () => api.restartSystem(),
    onSuccess: invalidateSystemViews,
  });

  const getStatusBadge = () => {
    if (!effectiveStatus) return null;
    return <Badge variant={statusBadgeVariant(effectiveStatus)}>{effectiveStatus}</Badge>;
  };

  if (statusLoading && !effectiveSnapshot) {
    return (
      <div className="flex h-full items-center justify-center">
        <div className="text-muted-foreground">Loading...</div>
      </div>
    );
  }

  const activeAlertCount = effectiveMetrics?.active_alert_count ?? effectiveAlerts.length;
  const warningAlertCount = effectiveMetrics?.warning_alert_count ?? 0;
  const criticalAlertCount = effectiveMetrics?.critical_alert_count ?? 0;

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

      <div className="grid grid-cols-1 gap-8 xl:grid-cols-2">
        <div className="space-y-8">
          <Card>
            <CardHeader>
              <div className="flex items-center gap-2">
                <Activity className="h-5 w-5 text-primary" />
                <CardTitle>System Status</CardTitle>
              </div>
              <CardDescription>Daemon health, uptime, and current execution posture.</CardDescription>
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
                  <span className="text-muted-foreground">Last Claim</span>
                  <span className="font-medium">
                    {effectiveSnapshot?.last_claim_time
                      ? new Date(effectiveSnapshot.last_claim_time).toLocaleString('en-US')
                      : 'None'}
                  </span>
                </div>
                <div className="flex items-center justify-between">
                  <span className="text-muted-foreground">Active Alerts</span>
                  <span className="font-medium">
                    {activeAlertCount} total / {criticalAlertCount} critical
                  </span>
                </div>
              </div>
            </CardContent>
          </Card>

          <Card>
            <CardHeader>
              <div className="flex items-center gap-2">
                <Gauge className="h-5 w-5 text-primary" />
                <CardTitle>Runtime Metrics</CardTitle>
              </div>
              <CardDescription>
                Aggregated deployment, trading, exposure, claim, and alert counts from `ployd`.
              </CardDescription>
            </CardHeader>
            <CardContent>
              {metricsError ? (
                <div className="rounded-lg border border-dashed p-4 text-sm text-muted-foreground">
                  Metrics are unavailable right now. {String(metricsError)}
                </div>
              ) : (
                <div className="space-y-4">
                  <div className="grid grid-cols-2 gap-4 md:grid-cols-4">
                    <MetricTile
                      label="Deployments"
                      value={effectiveMetrics?.deployments_total ?? 0}
                      hint="Total registered deployments"
                    />
                    <MetricTile
                      label="Running"
                      value={effectiveMetrics?.deployments_running ?? 0}
                      hint="Deployments observed as running"
                    />
                    <MetricTile
                      label="Degraded"
                      value={effectiveMetrics?.deployments_degraded ?? 0}
                      hint="Deployments needing attention"
                    />
                    <MetricTile
                      label="Failed"
                      value={effectiveMetrics?.deployments_failed ?? 0}
                      hint="Deployments observed as failed"
                    />
                  </div>

                  <div className="grid grid-cols-1 gap-4 md:grid-cols-2">
                    <MetricTile
                      label="Trading Load"
                      value={`${effectiveMetrics?.pending_intents ?? 0} pending / ${effectiveMetrics?.active_orders ?? 0} orders`}
                      hint={`${effectiveMetrics?.open_positions ?? 0} open positions`}
                    />
                    <MetricTile
                      label="Exposure"
                      value={effectiveMetrics?.total_gross_exposure ?? '0'}
                      hint={`gross ${effectiveMetrics?.gross_exposure ?? '0'} + reserved ${effectiveMetrics?.reserved_order_exposure ?? '0'}`}
                    />
                    <MetricTile
                      label="Runtime Mode Mix"
                      value={`${effectiveMetrics?.live_deployments ?? 0} live / ${effectiveMetrics?.paper_deployments ?? 0} paper`}
                      hint="Deployment mix by runtime mode"
                    />
                    <MetricTile
                      label="Claim Accounts"
                      value={`${effectiveMetrics?.claim_accounts_total ?? 0} total / ${effectiveMetrics?.claim_accounts_degraded ?? 0} degraded`}
                      hint={`${warningAlertCount} warning alerts / ${criticalAlertCount} critical alerts`}
                    />
                  </div>

                  {metricsLoading && !effectiveMetrics ? (
                    <div className="text-sm text-muted-foreground">Loading runtime metrics...</div>
                  ) : null}
                </div>
              )}
            </CardContent>
          </Card>
        </div>

        <div className="space-y-8">
          <Card>
            <CardHeader>
              <CardTitle>Connections</CardTitle>
              <CardDescription>Live daemon links and current operational health.</CardDescription>
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

          <Card>
            <CardHeader>
              <div className="flex items-center gap-2">
                <AlertTriangle className="h-5 w-5 text-primary" />
                <CardTitle>Active Alerts</CardTitle>
              </div>
              <CardDescription>Immediate alert feed from the daemon alert registry.</CardDescription>
            </CardHeader>
            <CardContent>
              {alertsError ? (
                <div className="rounded-lg border border-dashed p-4 text-sm text-muted-foreground">
                  Alerts are unavailable right now. {String(alertsError)}
                </div>
              ) : effectiveAlerts.length === 0 ? (
                <div className="rounded-lg border border-dashed p-6 text-center text-sm text-muted-foreground">
                  <Bell className="mx-auto mb-3 h-5 w-5" />
                  No active alerts
                </div>
              ) : (
                <div className="space-y-4">
                  <div className="flex flex-wrap gap-2 text-xs">
                    <Badge variant="outline">{effectiveAlerts.length} active</Badge>
                    <Badge variant="warning">{warningAlertCount} warning</Badge>
                    <Badge variant="destructive">{criticalAlertCount} critical</Badge>
                  </div>
                  <div className="space-y-3">
                    {effectiveAlerts.map((alert) => (
                      <div key={alert.alert_id} className="rounded-lg border p-4">
                        <div className="flex flex-wrap items-start justify-between gap-3">
                          <div className="space-y-1">
                            <div className="flex items-center gap-2">
                              <Badge variant={alertBadgeVariant(alert.severity)}>
                                {alert.severity}
                              </Badge>
                              <span className="text-sm font-semibold">{alert.kind}</span>
                            </div>
                            <div className="text-sm text-muted-foreground">
                              {alert.message}
                            </div>
                          </div>
                          <div className="text-right text-xs text-muted-foreground">
                            <div>{alert.source}</div>
                            <div>{alert.resource_type}</div>
                            <div>{alert.resource_id ?? '-'}</div>
                          </div>
                        </div>
                        <div className="mt-3 flex flex-wrap gap-3 text-xs text-muted-foreground">
                          <span>first seen {formatTimestamp(alert.first_seen_at)}</span>
                          <span>last seen {formatTimestamp(alert.last_seen_at)}</span>
                          <span>alert id {alert.alert_id}</span>
                        </div>
                      </div>
                    ))}
                  </div>
                  {alertsLoading && !effectiveAlerts.length ? (
                    <div className="text-sm text-muted-foreground">Loading alert registry...</div>
                  ) : null}
                </div>
              )}
            </CardContent>
          </Card>

          <Card>
            <CardHeader>
              <CardTitle>Control Panel</CardTitle>
              <CardDescription>System control actions stay separated from metrics reads.</CardDescription>
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
                  <p>
                    Use emergency halt from the domain-specific control surface when positions must
                    be unwound immediately.
                  </p>
                  <p>Restart performs a pause/resume cycle and may take 30-60 seconds.</p>
                </div>
              </div>
            </CardContent>
          </Card>
        </div>
      </div>
    </div>
  );
}
