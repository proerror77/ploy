import type { ReactNode } from 'react';
import { useEffect, useMemo, useState } from 'react';
import { useQuery, useQueryClient } from '@tanstack/react-query';
import {
  CartesianGrid,
  Legend,
  Line,
  LineChart,
  ResponsiveContainer,
  Tooltip,
  XAxis,
  YAxis,
} from 'recharts';
import {
  Activity,
  AlertTriangle,
  ArrowUpRight,
  Banknote,
  Cpu,
  Database,
  Gauge,
  Loader2,
  Radio,
  FileText,
  ShieldAlert,
  Timer,
  WalletCards,
  Wifi,
  WifiOff,
} from 'lucide-react';

import { Badge } from '@/components/ui/Badge';
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/Card';
import { api } from '@/services/api';
import { ws } from '@/services/websocket';
import { useStore } from '@/store';
import type {
  ActiveAlert,
  DryRunClosedTradeRow,
  DryRunEquityPoint,
  DryRunOpenPositionRow,
  DryRunPerformanceReport,
  DryRunStrategyReport,
  DeploymentSummary,
  LogEntry,
  PlatformMetrics,
  SystemStatus,
  TradingStateSnapshot,
} from '@/types';
import { cn, formatCurrency, formatDuration, formatNumber, formatTimestamp } from '@/lib/utils';

type Health = 'good' | 'watch' | 'bad' | 'neutral';

type EquityChartRow = {
  timestamp: number;
  label: string | null;
  [seriesKey: string]: number | string | null;
};

interface HealthCardProps {
  title: string;
  value: string;
  detail: string;
  icon: ReactNode;
  health: Health;
}

function toNumber(value: unknown): number {
  if (typeof value === 'number') return Number.isFinite(value) ? value : 0;
  if (typeof value === 'string') {
    const parsed = Number(value);
    return Number.isFinite(parsed) ? parsed : 0;
  }
  return 0;
}

function isDryRunId(value: string) {
  const normalized = value.toLowerCase();
  return (
    normalized.includes('dryrun') ||
    normalized.includes('dry-run') ||
    normalized.includes('paper')
  );
}

function isDryRunSnapshot(snapshot: TradingStateSnapshot) {
  return isDryRunId(snapshot.deployment_id) || isDryRunId(snapshot.runtime_mode);
}

function isDryRunDeployment(deployment: DeploymentSummary) {
  return isDryRunId(deployment.deployment_id);
}

function shouldShowUnreportedDryRunDeployment(deployment: DeploymentSummary, snapshot?: TradingStateSnapshot) {
  if (snapshot) return true;
  return (
    deployment.desired_state === 'running' ||
    deployment.observed_state === 'running' ||
    deployment.observed_state === 'starting' ||
    deployment.observed_state === 'degraded'
  );
}

function compactTime(timestamp?: string | null) {
  if (!timestamp) return '-';
  return new Date(timestamp).toLocaleTimeString('zh-CN', {
    hour: '2-digit',
    minute: '2-digit',
    second: '2-digit',
  });
}

function formatSecondsBrief(value?: number | null) {
  if (value == null || !Number.isFinite(value)) return '-';
  const seconds = Math.max(0, Math.round(value));
  const minutes = Math.floor(seconds / 60);
  const rest = seconds % 60;
  return minutes > 0 ? `${minutes}m${rest.toString().padStart(2, '0')}s` : `${rest}s`;
}

const aggregateSeriesKey = 'all_dry_run';
const strategyPalette = ['#0f172a', '#0f766e', '#2563eb', '#b45309', '#7c3aed', '#be123c', '#15803d'];
const emptyDryRunStrategies: DryRunStrategyReport[] = [];
const emptyDryRunEquityCurve: DryRunEquityPoint[] = [];
const emptyDryRunClosedTrades: DryRunClosedTradeRow[] = [];
const emptyDryRunOpenPositions: DryRunOpenPositionRow[] = [];

function strategySeriesKey(index: number) {
  return `strategy_${index}`;
}

function compactStrategyLabel(strategy: DryRunStrategyReport) {
  return strategy.label || strategy.deployment_id || strategy.strategy_id || strategy.runtime_mode;
}

function closedTradeStrategyLabel(row: DryRunClosedTradeRow) {
  return row.deployment_id || row.strategy_id || row.runtime_mode || 'unknown';
}

function equityPointTime(point: DryRunEquityPoint) {
  const timestamp = Date.parse(point.timestamp ?? '');
  return Number.isFinite(timestamp) ? timestamp : null;
}

function formatChartTimestamp(value: unknown) {
  const timestamp = typeof value === 'number' ? value : Number(value);
  if (!Number.isFinite(timestamp)) return '-';
  return new Date(timestamp).toLocaleString('zh-CN', {
    month: '2-digit',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
  });
}

function formatOptionalMetric(value?: number | null, digits = 2) {
  return value == null || !Number.isFinite(value) ? '-' : value.toFixed(digits);
}

function formatProfitFactor(value: DryRunStrategyReport['metrics']['profit_factor']) {
  if (value == null) return '-';
  return typeof value === 'number' ? value.toFixed(2) : value;
}

function ageSeconds(timestamp?: string | null) {
  if (!timestamp) return null;
  const created = new Date(timestamp).getTime();
  if (!Number.isFinite(created)) return null;
  return Math.max(0, Math.round((Date.now() - created) / 1000));
}

function HealthCard({ title, value, detail, icon, health }: HealthCardProps) {
  return (
    <Card className="overflow-hidden">
      <CardContent className="p-4">
        <div className="flex items-start justify-between gap-3">
          <div className="min-w-0">
            <div className="text-xs font-medium uppercase text-muted-foreground">{title}</div>
            <div className="mt-2 truncate text-2xl font-semibold tracking-normal">{value}</div>
            <div className="mt-1 text-xs text-muted-foreground">{detail}</div>
          </div>
          <div
            className={cn('rounded-md border p-2', {
              'border-green-200 bg-green-50 text-green-700': health === 'good',
              'border-amber-200 bg-amber-50 text-amber-700': health === 'watch',
              'border-red-200 bg-red-50 text-red-700': health === 'bad',
              'border-slate-200 bg-slate-50 text-slate-700': health === 'neutral',
            })}
          >
            {icon}
          </div>
        </div>
      </CardContent>
    </Card>
  );
}

function statusHealth(status?: string) {
  if (!status) return 'neutral' as const;
  if (status.startsWith('running')) return 'good' as const;
  if (status.startsWith('recovering') || status.startsWith('degraded')) return 'watch' as const;
  if (status.startsWith('error')) return 'bad' as const;
  return 'neutral' as const;
}

function pnlHealth(value: number) {
  if (value > 0) return 'good' as const;
  if (value < 0) return 'bad' as const;
  return 'neutral' as const;
}

function cpuHealth(value?: number | null) {
  if (value == null) return 'neutral' as const;
  if (value >= 90) return 'bad' as const;
  if (value >= 65) return 'watch' as const;
  return 'good' as const;
}

function orderHealth(rejections: number, activeOrders: number) {
  if (rejections > 0) return 'bad' as const;
  if (activeOrders > 0) return 'watch' as const;
  return 'good' as const;
}

function deploymentBadge(entry: DeploymentSummary) {
  if (entry.observed_state === 'running') return 'success' as const;
  if (entry.observed_state === 'failed') return 'destructive' as const;
  if (entry.observed_state === 'degraded' || entry.observed_state === 'starting') {
    return 'warning' as const;
  }
  return 'secondary' as const;
}

function recentWarningLogs(logs: LogEntry[]) {
  return logs
    .filter((log) => {
      const level = log.level.toLowerCase();
      return level.includes('warn') || level.includes('error') || level.includes('critical');
    })
    .slice(-8)
    .reverse();
}

function heartbeatLag(heartbeat: PlatformMetrics['heartbeats'][number]) {
  const age = ageSeconds(heartbeat.last_seen_at);
  if (age == null) return '-';
  return `${age}s / ${heartbeat.stale_after_seconds}s`;
}

export function OperatorCockpit() {
  const queryClient = useQueryClient();
  const { deployments, tradingSnapshots, logs, wsConnected } = useStore();
  const [lastEventAt, setLastEventAt] = useState<number | null>(null);

  const { data: status } = useQuery({
    queryKey: ['system', 'status'],
    queryFn: () => api.getSystemStatus(),
    refetchInterval: 15000,
  });

  const { data: metrics } = useQuery({
    queryKey: ['system', 'metrics'],
    queryFn: () => api.getSystemMetrics(),
    refetchInterval: 15000,
  });

  const { data: alerts = [] } = useQuery({
    queryKey: ['system', 'alerts'],
    queryFn: () => api.getSystemAlerts(),
    refetchInterval: 15000,
  });

  const { data: marketDataHealth, error: marketDataHealthError } = useQuery({
    queryKey: ['market-data', 'health'],
    queryFn: () => api.getMarketDataHealth(),
    refetchInterval: 30000,
    retry: false,
  });

  const { data: dryRunPerformance, error: dryRunPerformanceError } = useQuery<DryRunPerformanceReport>({
    queryKey: ['reports', 'dry-run'],
    queryFn: () => api.getDryRunPerformance(),
    refetchInterval: 30000,
    retry: false,
  });

  const { data: polledDeployments = [], isLoading: deploymentsLoading } = useQuery({
    queryKey: ['deployments'],
    queryFn: () => api.getDeployments(),
    refetchInterval: 15000,
  });

  const { data: polledTrading = [], isLoading: tradingLoading } = useQuery({
    queryKey: ['trading', 'state'],
    queryFn: () => api.getTradingState(),
    refetchInterval: 10000,
  });

  useEffect(() => {
    const unsub = ws.subscribe('*', (event) => {
      setLastEventAt(Date.now());
      if (event.type === 'system_snapshot') {
        queryClient.setQueryData<SystemStatus>(['system', 'status'], event.data.system);
      }
      if (event.type === 'metrics_snapshot') {
        queryClient.setQueryData<PlatformMetrics>(['system', 'metrics'], event.data.metrics);
      }
      if (event.type === 'alert_snapshot') {
        queryClient.setQueryData<ActiveAlert[]>(['system', 'alerts'], event.data.alerts);
      }
      if (event.type === 'deployment_snapshot') {
        queryClient.setQueryData<DeploymentSummary[]>(['deployments'], event.data.deployments);
      }
      if (event.type === 'trading_snapshot') {
        queryClient.setQueryData<TradingStateSnapshot[]>(['trading', 'state'], event.data.trading);
      }
    });
    return unsub;
  }, [queryClient]);

  const effectiveDeployments = deployments.length > 0 ? deployments : polledDeployments;
  const effectiveTrading = tradingSnapshots.length > 0 ? tradingSnapshots : polledTrading;
  const dryRunDeployments = effectiveDeployments.filter(isDryRunDeployment);
  const dryRunTrading = effectiveTrading.filter(isDryRunSnapshot);
  const visibleDeployments = dryRunDeployments.length > 0 ? dryRunDeployments : effectiveDeployments;
  const visibleTrading = dryRunTrading.length > 0 ? dryRunTrading : effectiveTrading;

  const isLoading = deploymentsLoading || tradingLoading;

  const totals = useMemo(() => {
    return visibleTrading.reduce(
      (acc, snapshot) => {
        const netPnl = toNumber(snapshot.pnl.net_pnl);
        const grossExposure = toNumber(snapshot.risk.total_gross_exposure);
        const activeOrders = snapshot.orders.filter((order) => {
          const state = order.state.toLowerCase();
          return !['filled', 'canceled', 'cancelled', 'rejected', 'failed'].includes(state);
        }).length;
        const rejectedOrders = snapshot.orders.filter((order) => {
          const state = order.state.toLowerCase();
          return state.includes('reject') || state.includes('fail') || Boolean(order.last_error);
        }).length;
        const buyIntents = snapshot.intents.filter(
          (intent) => intent.side.toLowerCase() === 'buy' && intent.purpose === 'entry'
        ).length;

        acc.netPnl += netPnl;
        acc.realizedPnl += toNumber(snapshot.pnl.realized_pnl);
        acc.unrealizedPnl += toNumber(snapshot.pnl.unrealized_pnl);
        acc.totalFees += toNumber(snapshot.pnl.total_fees);
        acc.grossExposure += grossExposure;
        acc.pendingIntents += snapshot.risk.pending_intents;
        acc.openPositions += snapshot.risk.open_positions;
        acc.activeOrders += activeOrders;
        acc.rejectedOrders += rejectedOrders;
        acc.buyIntents += buyIntents;
        acc.fills += snapshot.fills.length;
        return acc;
      },
      {
        netPnl: 0,
        realizedPnl: 0,
        unrealizedPnl: 0,
        totalFees: 0,
        grossExposure: 0,
        pendingIntents: 0,
        openPositions: 0,
        activeOrders: 0,
        rejectedOrders: 0,
        buyIntents: 0,
        fills: 0,
      }
    );
  }, [visibleTrading]);

  const reportedSummary = dryRunPerformance?.summary;
  const reportedNetPnl = reportedSummary ? toNumber(reportedSummary.realized_pnl) : totals.netPnl;
  const reportedFees = reportedSummary ? toNumber(reportedSummary.total_fees) : totals.totalFees;
  const reportedOpenPositions = reportedSummary?.open_positions ?? totals.openPositions;
  const reportedOpenExposure = reportedSummary
    ? toNumber(reportedSummary.open_exposure)
    : totals.grossExposure;
  const reportedFills = reportedSummary?.total_trades ?? totals.fills;
  const reportedWinRate = reportedSummary == null ? null : toNumber(reportedSummary.win_rate_pct);
  const dryRunWindows = dryRunPerformance?.by_window ?? [];
  const dryRunPairing = dryRunPerformance?.pairing;
  const dryRunStrategies = dryRunPerformance?.strategies ?? emptyDryRunStrategies;
  const dryRunEquityCurve = dryRunPerformance?.equity_curve ?? emptyDryRunEquityCurve;
  const dryRunRecentClosed = dryRunPerformance?.recent_closed ?? emptyDryRunClosedTrades;
  const dryRunOpenPositions = dryRunPerformance?.open_positions ?? emptyDryRunOpenPositions;
  const pairingHasMismatch =
    dryRunPairing != null &&
    (dryRunPairing.mixed_event_groups > 0 ||
      dryRunPairing.current_view_rows !== dryRunPairing.side_aware_rows);
  const strategyEquity = useMemo(() => {
    const series = [
      ...(dryRunEquityCurve.length > 0
        ? [
            {
              key: aggregateSeriesKey,
              label: 'All dry-run',
              color: strategyPalette[0],
            },
          ]
        : []),
      ...dryRunStrategies
        .filter((strategy) => strategy.equity_curve.length > 0)
        .map((strategy, index) => ({
          key: strategySeriesKey(index),
          label: compactStrategyLabel(strategy),
          color: strategyPalette[(index + 1) % strategyPalette.length],
        })),
    ];

    const rows = new Map<number, EquityChartRow>();
    for (const point of dryRunEquityCurve) {
      const timestamp = equityPointTime(point);
      if (timestamp == null) continue;
      const row = rows.get(timestamp) ?? { timestamp, label: point.timestamp ?? null };
      row[aggregateSeriesKey] = point.cumulative;
      rows.set(timestamp, row);
    }
    dryRunStrategies.forEach((strategy, index) => {
      const key = strategySeriesKey(index);
      for (const point of strategy.equity_curve) {
        const timestamp = equityPointTime(point);
        if (timestamp == null) continue;
        const row = rows.get(timestamp) ?? { timestamp, label: point.timestamp ?? null };
        row[key] = point.cumulative;
        rows.set(timestamp, row);
      }
    });

    return {
      series,
      data: Array.from(rows.values()).sort((a, b) => toNumber(a.timestamp) - toNumber(b.timestamp)),
    };
  }, [dryRunEquityCurve, dryRunStrategies]);
  const rankedStrategies = useMemo(() => {
    return [...dryRunStrategies].sort(
      (a, b) =>
        toNumber(b.summary.realized_pnl) - toNumber(a.summary.realized_pnl) ||
        b.summary.closed_trades - a.summary.closed_trades
    );
  }, [dryRunStrategies]);
  const dryRunStrategyByDeployment = useMemo(() => {
    const byDeployment = new Map<string, DryRunStrategyReport>();
    for (const strategy of dryRunStrategies) {
      if (strategy.deployment_id) {
        byDeployment.set(strategy.deployment_id, strategy);
      }
    }
    return byDeployment;
  }, [dryRunStrategies]);
  const tradingByDeployment = useMemo(
    () => new Map(dryRunTrading.map((snapshot) => [snapshot.deployment_id, snapshot])),
    [dryRunTrading]
  );
  const unreportedDryRunDeployments = dryRunDeployments.filter(
    (deployment) =>
      !dryRunStrategyByDeployment.has(deployment.deployment_id) &&
      shouldShowUnreportedDryRunDeployment(deployment, tradingByDeployment.get(deployment.deployment_id))
  );

  const eventAge = lastEventAt == null ? null : Math.max(0, Math.round((Date.now() - lastEventAt) / 1000));
  const cpuPressure =
    metrics?.host_cpu_pressure_milli_percent == null
      ? null
      : metrics.host_cpu_pressure_milli_percent / 1000;
  const loadAverage =
    metrics?.host_load_average_1m_milli == null
      ? null
      : metrics.host_load_average_1m_milli / 1000;
  const warningLogs = recentWarningLogs(logs);
  const failedDeployments = visibleDeployments.filter(
    (entry) => entry.observed_state === 'failed' || entry.observed_state === 'degraded'
  );
  const staleHeartbeats = metrics?.heartbeats.filter((heartbeat) => heartbeat.state === 'stale') ?? [];
  const staleMarketSources =
    marketDataHealth?.sources.filter((source) => {
      const age = ageSeconds(source.latest_at);
      return age == null || age > source.stale_after_seconds;
    }) ?? [];

  if (isLoading) {
    return (
      <div className="flex h-full items-center justify-center">
        <Loader2 className="h-8 w-8 animate-spin text-muted-foreground" />
      </div>
    );
  }

  return (
    <div className="min-h-full bg-[#f7f8f5] p-6 text-[#171a16]">
      <div className="mb-5 flex flex-col gap-4 xl:flex-row xl:items-end xl:justify-between">
        <div>
          <div className="mb-2 flex flex-wrap items-center gap-2">
            <Badge variant="outline">Operator cockpit</Badge>
            <Badge variant={wsConnected ? 'success' : 'warning'}>
              {wsConnected ? 'SSE live' : 'Polling fallback'}
            </Badge>
            {dryRunTrading.length === 0 && effectiveTrading.length > 0 ? (
              <Badge variant="warning">Showing all runtime snapshots</Badge>
            ) : null}
          </div>
          <h1 className="text-3xl font-semibold tracking-normal">Ploy Operator Cockpit</h1>
          <p className="mt-1 text-sm text-muted-foreground">
            Canonical frontend surface for platform runtime, strategy decisions, account exposure,
            fills, connectivity, and warning signals. Dry-run deployments are prioritized when present.
          </p>
        </div>
        <div className="grid grid-cols-2 gap-2 text-right text-xs text-muted-foreground sm:grid-cols-4">
          <div className="rounded-md border bg-white px-3 py-2">
            <div className="font-medium text-foreground">{visibleDeployments.length}</div>
            <div>deployments</div>
          </div>
          <div className="rounded-md border bg-white px-3 py-2">
            <div className="font-medium text-foreground">{visibleTrading.length}</div>
            <div>snapshots</div>
          </div>
          <div className="rounded-md border bg-white px-3 py-2">
            <div className="font-medium text-foreground">{alerts.length}</div>
            <div>alerts</div>
          </div>
          <div className="rounded-md border bg-white px-3 py-2">
            <div className="font-medium text-foreground">
              {eventAge == null ? '-' : `${eventAge}s`}
            </div>
            <div>event age</div>
          </div>
        </div>
      </div>

      <div className="mb-5 grid grid-cols-1 gap-3 md:grid-cols-2 2xl:grid-cols-6">
        <HealthCard
          title="Platform"
          value={status?.status ?? 'unknown'}
          detail={`uptime ${status ? formatDuration(status.uptime_seconds) : '-'}`}
          icon={<Activity className="h-5 w-5" />}
          health={statusHealth(status?.status)}
        />
        <HealthCard
          title="CPU Pressure"
          value={cpuPressure == null ? '-' : `${cpuPressure.toFixed(1)}%`}
          detail={`load ${loadAverage?.toFixed(2) ?? '-'} · rss ${metrics?.process_memory_mb ?? '-'}MB`}
          icon={<Cpu className="h-5 w-5" />}
          health={cpuHealth(cpuPressure)}
        />
        <HealthCard
          title="Net PnL"
          value={formatCurrency(reportedNetPnl)}
          detail={
            reportedSummary
              ? `${reportedSummary.closed_trades} closed · fees ${formatCurrency(reportedFees)}`
              : `realized ${formatCurrency(totals.realizedPnl)} · unreal ${formatCurrency(totals.unrealizedPnl)}`
          }
          icon={<Banknote className="h-5 w-5" />}
          health={pnlHealth(reportedNetPnl)}
        />
        <HealthCard
          title="Buy & Run"
          value={`${totals.buyIntents} entries`}
          detail={`${totals.activeOrders} active orders · ${totals.rejectedOrders} rejects`}
          icon={<ArrowUpRight className="h-5 w-5" />}
          health={orderHealth(totals.rejectedOrders, totals.activeOrders)}
        />
        <HealthCard
          title="Account Exposure"
          value={formatCurrency(reportedOpenExposure)}
          detail={`${reportedOpenPositions} open positions · ${totals.pendingIntents} pending`}
          icon={<WalletCards className="h-5 w-5" />}
          health={totals.pendingIntents > 5 ? 'watch' : 'good'}
        />
        <HealthCard
          title="Connectivity"
          value={status?.database_connected ? 'DB online' : 'DB offline'}
          detail={`${staleHeartbeats.length} stale sources · ${status?.error_count_1h ?? 0} errors/h`}
          icon={status?.database_connected ? <Wifi className="h-5 w-5" /> : <WifiOff className="h-5 w-5" />}
          health={!status?.database_connected || staleHeartbeats.length > 0 ? 'bad' : 'good'}
        />
      </div>

      {dryRunPerformance ? (
        <Card className="mb-5">
          <CardHeader className="pb-3">
            <div className="flex items-center justify-between gap-3">
              <CardTitle className="flex items-center gap-2 text-lg">
                <FileText className="h-5 w-5" />
                Dry-run Strategy Report
              </CardTitle>
              <Badge variant={dryRunStrategies.length > 1 ? 'success' : 'secondary'}>
                {dryRunStrategies.length} strategies
              </Badge>
            </div>
          </CardHeader>
          <CardContent>
            <div className="grid grid-cols-1 gap-5 2xl:grid-cols-[1.2fr_0.8fr]">
              <div className="rounded-md border bg-white p-4">
                <div className="mb-3 flex items-start justify-between gap-3">
                  <div>
                    <div className="font-medium">Cumulative equity curve</div>
                    <div className="text-xs text-muted-foreground">
                      {dryRunPerformance.metrics.equity_points} closed trades · x-axis is close time · max drawdown{' '}
                      {formatCurrency(dryRunPerformance.metrics.max_drawdown)}
                    </div>
                  </div>
                  <Badge variant={reportedNetPnl >= 0 ? 'success' : 'destructive'}>
                    {formatCurrency(reportedNetPnl)}
                  </Badge>
                </div>
                {strategyEquity.series.length > 0 && strategyEquity.data.length > 0 ? (
                  <div className="h-[320px]">
                    <ResponsiveContainer width="100%" height="100%">
                      <LineChart data={strategyEquity.data}>
                        <CartesianGrid strokeDasharray="3 3" vertical={false} />
                        <XAxis
                          dataKey="timestamp"
                          type="number"
                          domain={['dataMin', 'dataMax']}
                          tick={{ fontSize: 12 }}
                          tickFormatter={formatChartTimestamp}
                        />
                        <YAxis
                          tick={{ fontSize: 12 }}
                          tickFormatter={(value) => `$${Number(value).toFixed(0)}`}
                          width={64}
                        />
                        <Tooltip
                          formatter={(value, name) => {
                            const seriesName = String(name);
                            return [
                              formatCurrency(toNumber(value)),
                              strategyEquity.series.find((entry) => entry.key === seriesName)?.label ?? seriesName,
                            ];
                          }}
                          labelFormatter={formatChartTimestamp}
                        />
                        <Legend
                          formatter={(value) => (
                            <span className="text-xs text-muted-foreground">{value}</span>
                          )}
                        />
                        {strategyEquity.series.map((series) => (
                          <Line
                            key={series.key}
                            type="monotone"
                            dataKey={series.key}
                            name={series.label}
                            stroke={series.color}
                            strokeWidth={series.key === aggregateSeriesKey ? 3 : 2}
                            dot={false}
                          />
                        ))}
                      </LineChart>
                    </ResponsiveContainer>
                  </div>
                ) : (
                  <div className="flex h-[320px] items-center justify-center rounded-md bg-muted text-sm text-muted-foreground">
                    No closed-trade equity points yet.
                  </div>
                )}
              </div>

              <div className="rounded-md border bg-white p-4">
                <div className="mb-3 flex items-start justify-between gap-3">
                  <div>
                    <div className="font-medium">Strategy attribution</div>
                    <div className="text-xs text-muted-foreground">
                      grouped by strategy_id and deployment_id
                    </div>
                  </div>
                  <Badge variant={pairingHasMismatch ? 'warning' : 'success'}>
                    {dryRunPairing?.mixed_event_groups ?? 0} mixed events
                  </Badge>
                </div>
                <div className="overflow-hidden rounded-md border">
                  <div className="grid grid-cols-[1.5fr_0.8fr_0.8fr_0.8fr_0.8fr] bg-muted px-3 py-2 text-xs font-medium uppercase text-muted-foreground">
                    <span>strategy</span>
                    <span>closed</span>
                    <span>win</span>
                    <span>sharpe</span>
                    <span className="text-right">pnl</span>
                  </div>
                  {rankedStrategies.length === 0 && unreportedDryRunDeployments.length === 0 ? (
                    <div className="px-3 py-8 text-center text-sm text-muted-foreground">
                      No strategy-level dry-run rows yet.
                    </div>
                  ) : (
                    <>
                      {rankedStrategies.map((strategy) => {
                        const pnl = toNumber(strategy.summary.realized_pnl);
                        return (
                          <div
                            key={`${strategy.runtime_mode}:${strategy.strategy_id}:${strategy.deployment_id}`}
                            className="grid grid-cols-[1.5fr_0.8fr_0.8fr_0.8fr_0.8fr] items-center border-t px-3 py-2 text-sm"
                          >
                            <div className="min-w-0">
                              <div className="truncate font-medium">{compactStrategyLabel(strategy)}</div>
                              <div className="truncate text-xs text-muted-foreground">
                                {strategy.strategy_id || 'unknown'} · {strategy.runtime_mode || 'unknown'}
                              </div>
                            </div>
                            <div>{strategy.summary.closed_trades}</div>
                            <div>{formatOptionalMetric(strategy.summary.win_rate_pct, 1)}%</div>
                            <div>{formatOptionalMetric(strategy.metrics.sharpe, 2)}</div>
                            <div className={cn('text-right font-medium', pnl < 0 ? 'text-destructive' : 'text-success')}>
                              {formatCurrency(pnl)}
                              <div className="text-xs font-normal text-muted-foreground">
                                PF {formatProfitFactor(strategy.metrics.profit_factor)}
                              </div>
                            </div>
                          </div>
                        );
                      })}
                      {unreportedDryRunDeployments.map((deployment) => (
                        <div
                          key={`unreported:${deployment.deployment_id}`}
                          className="grid grid-cols-[1.5fr_0.8fr_0.8fr_0.8fr_0.8fr] items-center border-t px-3 py-2 text-sm"
                        >
                          <div className="min-w-0">
                            <div className="truncate font-medium">{deployment.deployment_id}</div>
                            <div className="truncate text-xs text-muted-foreground">
                              running deployment · no dry-run report rows
                            </div>
                          </div>
                          <div>0</div>
                          <div>-</div>
                          <div>-</div>
                          <div className="text-right text-muted-foreground">
                            no data
                          </div>
                        </div>
                      ))}
                    </>
                  )}
                </div>
              </div>
            </div>

            <div className="mt-5 rounded-md border bg-white">
              <div className="flex items-start justify-between gap-3 border-b px-4 py-3">
                <div>
                  <div className="font-medium">Closed trade ledger</div>
                  <div className="text-xs text-muted-foreground">
                    strategy/deployment attribution from dry-run track records
                  </div>
                </div>
                <Badge variant="secondary">{dryRunRecentClosed.length} recent</Badge>
              </div>
              {dryRunRecentClosed.length === 0 ? (
                <div className="px-4 py-8 text-center text-sm text-muted-foreground">
                  No closed dry-run trades yet.
                </div>
              ) : (
                <div className="overflow-x-auto">
                  <div className="min-w-[1040px]">
                    <div className="grid grid-cols-[1.4fr_1.1fr_0.75fr_0.75fr_0.75fr_0.75fr_1fr_1fr] bg-muted px-4 py-2 text-xs font-medium uppercase text-muted-foreground">
                      <span>strategy</span>
                      <span>symbol</span>
                      <span>side</span>
                      <span>exit</span>
                      <span>qty</span>
                      <span>notional</span>
                      <span className="text-right">pnl</span>
                      <span className="text-right">closed</span>
                    </div>
                    {dryRunRecentClosed.map((trade) => {
                      const pnl = toNumber(trade.net_pnl);
                      return (
                        <div
                          key={trade.trade_key ?? `${trade.strategy_id}:${trade.symbol}:${trade.closed_at}`}
                          className="grid grid-cols-[1.4fr_1.1fr_0.75fr_0.75fr_0.75fr_0.75fr_1fr_1fr] items-center border-t px-4 py-2 text-sm"
                        >
                          <div className="min-w-0">
                            <div className="truncate font-medium">{closedTradeStrategyLabel(trade)}</div>
                            <div className="truncate text-xs text-muted-foreground">
                              {trade.strategy_id || 'unknown'} · {trade.runtime_mode || 'unknown'}
                            </div>
                          </div>
                          <div className="min-w-0">
                            <div className="truncate">{trade.symbol || 'unknown'}</div>
                            <div className="truncate text-xs text-muted-foreground">
                              {trade.event_id || trade.trade_key || '-'}
                            </div>
                          </div>
                          <div>{trade.market_side || '-'}</div>
                          <div>{trade.exit_type}</div>
                          <div>{formatNumber(toNumber(trade.quantity))}</div>
                          <div>{formatCurrency(toNumber(trade.notional))}</div>
                          <div className={cn('text-right font-medium', pnl < 0 ? 'text-destructive' : 'text-success')}>
                            {formatCurrency(pnl)}
                          </div>
                          <div className="text-right text-xs text-muted-foreground">
                            {trade.closed_at ? formatTimestamp(trade.closed_at) : '-'}
                          </div>
                        </div>
                      );
                    })}
                  </div>
                </div>
              )}
            </div>

            {dryRunOpenPositions.length > 0 ? (
              <div className="mt-5 rounded-md border bg-white">
                <div className="flex items-start justify-between gap-3 border-b px-4 py-3">
                  <div>
                    <div className="font-medium">Open dry-run positions</div>
                    <div className="text-xs text-muted-foreground">
                      current exposure grouped by original strategy/deployment
                    </div>
                  </div>
                  <Badge variant="warning">{dryRunOpenPositions.length} open</Badge>
                </div>
                <div className="overflow-x-auto">
                  <div className="min-w-[900px]">
                    <div className="grid grid-cols-[1.4fr_1.2fr_0.8fr_0.8fr_0.8fr_1fr] bg-muted px-4 py-2 text-xs font-medium uppercase text-muted-foreground">
                      <span>strategy</span>
                      <span>symbol</span>
                      <span>side</span>
                      <span>qty</span>
                      <span>notional</span>
                      <span className="text-right">opened</span>
                    </div>
                    {dryRunOpenPositions.map((position) => (
                      <div
                        key={position.trade_key ?? `${position.strategy_id}:${position.symbol}:${position.opened_at}`}
                        className="grid grid-cols-[1.4fr_1.2fr_0.8fr_0.8fr_0.8fr_1fr] items-center border-t px-4 py-2 text-sm"
                      >
                        <div className="min-w-0">
                          <div className="truncate font-medium">
                            {position.deployment_id || position.strategy_id || position.runtime_mode || 'unknown'}
                          </div>
                          <div className="truncate text-xs text-muted-foreground">
                            {position.strategy_id || 'unknown'} · {position.runtime_mode || 'unknown'}
                          </div>
                        </div>
                        <div className="truncate">{position.symbol || 'unknown'}</div>
                        <div>{position.market_side || '-'}</div>
                        <div>{formatNumber(toNumber(position.quantity))}</div>
                        <div>{formatCurrency(toNumber(position.notional))}</div>
                        <div className="text-right text-xs text-muted-foreground">
                          {position.opened_at ? formatTimestamp(position.opened_at) : '-'}
                        </div>
                      </div>
                    ))}
                  </div>
                </div>
              </div>
            ) : null}
          </CardContent>
        </Card>
      ) : null}

      <div className="grid grid-cols-1 gap-5 xl:grid-cols-12">
        <div className="space-y-5 xl:col-span-8">
          <Card>
            <CardHeader className="pb-3">
              <div className="flex items-center justify-between gap-3">
                <CardTitle className="flex items-center gap-2 text-lg">
                  <Gauge className="h-5 w-5" />
                  Strategy Runtime
                </CardTitle>
                <Badge variant={failedDeployments.length > 0 ? 'warning' : 'success'}>
                  {failedDeployments.length} degraded
                </Badge>
              </div>
            </CardHeader>
            <CardContent>
              <div className="overflow-hidden rounded-md border">
                <div className="grid grid-cols-[1.4fr_0.8fr_0.9fr_0.9fr_0.9fr] bg-muted px-4 py-2 text-xs font-medium uppercase text-muted-foreground">
                  <span>deployment</span>
                  <span>state</span>
                  <span>pnl</span>
                  <span>orders</span>
                  <span>account</span>
                </div>
                {visibleDeployments.length === 0 ? (
                  <div className="px-4 py-8 text-center text-sm text-muted-foreground">
                    No deployment snapshots available.
                  </div>
                ) : (
                  visibleDeployments.map((deployment) => {
                    const snapshot = visibleTrading.find(
                      (entry) => entry.deployment_id === deployment.deployment_id
                    );
                    const risk = snapshot?.risk;
                    const isDryRun = isDryRunDeployment(deployment);
                    const reportStrategy = isDryRun
                      ? dryRunStrategyByDeployment.get(deployment.deployment_id)
                      : undefined;
                    const hasPnl = reportStrategy != null || snapshot != null;
                    const pnl = reportStrategy
                      ? toNumber(reportStrategy.summary.realized_pnl)
                      : toNumber(snapshot?.pnl.net_pnl);
                    const fees = reportStrategy
                      ? toNumber(reportStrategy.summary.total_fees)
                      : toNumber(snapshot?.pnl.total_fees);
                    const exposure = reportStrategy
                      ? toNumber(reportStrategy.summary.open_exposure)
                      : toNumber(risk?.total_gross_exposure);
                    return (
                      <div
                        key={deployment.deployment_id}
                        className="grid grid-cols-[1.4fr_0.8fr_0.9fr_0.9fr_0.9fr] items-center border-t px-4 py-3 text-sm"
                      >
                        <div className="min-w-0">
                          <div className="truncate font-medium">{deployment.deployment_id}</div>
                          <div className="truncate text-xs text-muted-foreground">
                            exposure cap {deployment.max_gross_exposure ?? '-'}
                          </div>
                        </div>
                        <div>
                          <Badge variant={deploymentBadge(deployment)}>
                            {deployment.desired_state}/{deployment.observed_state}
                          </Badge>
                          <div className="mt-1 text-xs text-muted-foreground">
                            {deployment.deployment_state ?? 'enabled'}
                          </div>
                        </div>
                        <div className={cn('font-medium', pnl < 0 ? 'text-destructive' : 'text-success')}>
                          {hasPnl ? formatCurrency(pnl) : '-'}
                          <div className="text-xs font-normal text-muted-foreground">
                            {hasPnl ? `fees ${formatCurrency(fees)}` : 'no report rows'}
                          </div>
                        </div>
                        <div>
                          <div>{risk?.active_orders ?? 0} active</div>
                          <div className="text-xs text-muted-foreground">
                            {risk?.pending_intents ?? 0} pending
                          </div>
                        </div>
                        <div>
                          <div>{deployment.account_id ?? 'default'}</div>
                          <div className="text-xs text-muted-foreground">
                            {hasPnl ? formatCurrency(exposure) : '-'}
                          </div>
                        </div>
                      </div>
                    );
                  })
                )}
              </div>
            </CardContent>
          </Card>

          <Card>
            <CardHeader className="pb-3">
              <CardTitle className="flex items-center gap-2 text-lg">
                <FileText className="h-5 w-5" />
                Orders, Fills, Positions
              </CardTitle>
            </CardHeader>
            <CardContent>
              <div className="grid grid-cols-2 gap-3 lg:grid-cols-4">
                <div className="rounded-md border bg-white p-4">
                  <div className="text-xs text-muted-foreground">fills</div>
                  <div className="mt-2 text-2xl font-semibold">{formatNumber(reportedFills)}</div>
                </div>
                <div className="rounded-md border bg-white p-4">
                  <div className="text-xs text-muted-foreground">open positions</div>
                  <div className="mt-2 text-2xl font-semibold">{formatNumber(reportedOpenPositions)}</div>
                </div>
                <div className="rounded-md border bg-white p-4">
                  <div className="text-xs text-muted-foreground">pending intents</div>
                  <div className="mt-2 text-2xl font-semibold">{formatNumber(totals.pendingIntents)}</div>
                </div>
                <div className="rounded-md border bg-white p-4">
                  <div className="text-xs text-muted-foreground">available memory</div>
                  <div className="mt-2 text-2xl font-semibold">
                    {metrics?.host_memory_available_mb == null
                      ? '-'
                      : `${metrics.host_memory_available_mb}MB`}
                  </div>
                </div>
              </div>

              <div className="mt-4 grid grid-cols-1 gap-3 lg:grid-cols-2">
                {dryRunPerformance ? (
                  <div className="rounded-md border bg-white p-4">
                    <div className="mb-3 flex items-start justify-between gap-3">
                      <div className="min-w-0">
                        <div className="font-medium">Dry-run DB report</div>
                        <div className="text-xs text-muted-foreground">
                          generated {compactTime(dryRunPerformance.generated_at)} · latest close{' '}
                          {compactTime(reportedSummary?.latest_closed_at)}
                        </div>
                      </div>
                      <Badge variant={reportedNetPnl >= 0 ? 'success' : 'destructive'}>
                        {reportedWinRate == null ? '-' : `${reportedWinRate.toFixed(1)}%`}
                      </Badge>
                    </div>
                    <div className="grid grid-cols-3 gap-2 text-sm">
                      <div>
                        <div className="text-xs text-muted-foreground">trades</div>
                        <div className="font-medium">{reportedSummary?.total_trades ?? 0}</div>
                      </div>
                      <div>
                        <div className="text-xs text-muted-foreground">wins/losses</div>
                        <div className="font-medium">
                          {reportedSummary?.wins ?? 0}/{reportedSummary?.losses ?? 0}
                        </div>
                      </div>
                      <div>
                        <div className="text-xs text-muted-foreground">net pnl</div>
                        <div className={cn('font-medium', reportedNetPnl < 0 ? 'text-destructive' : 'text-success')}>
                          {formatCurrency(reportedNetPnl)}
                        </div>
                      </div>
                    </div>
                    {dryRunWindows.length > 0 ? (
                      <div className="mt-4 space-y-2">
                        {dryRunWindows.map((window) => {
                          const pnl = toNumber(window.realized_pnl);
                          const winRate = toNumber(window.win_rate_pct);
                          return (
                            <div
                              key={window.window_label}
                              className="grid grid-cols-[0.45fr_0.8fr_0.8fr_0.8fr] items-center gap-2 rounded-md border px-3 py-2 text-xs"
                            >
                              <Badge variant="outline">{window.window_label}</Badge>
                              <div>
                                <span className="text-muted-foreground">trades </span>
                                <span className="font-medium">{window.total_trades}</span>
                              </div>
                              <div>
                                <span className="text-muted-foreground">win </span>
                                <span className="font-medium">{winRate.toFixed(1)}%</span>
                              </div>
                              <div className={cn('text-right font-medium', pnl < 0 ? 'text-destructive' : 'text-success')}>
                                {formatCurrency(pnl)}
                              </div>
                              <div className="col-span-4 text-muted-foreground">
                                entry TTR {formatSecondsBrief(window.min_entry_ttr_secs)}-
                                {formatSecondsBrief(window.max_entry_ttr_secs)} · avg entry{' '}
                                {window.avg_entry == null ? '-' : toNumber(window.avg_entry).toFixed(4)}
                              </div>
                            </div>
                          );
                        })}
                      </div>
                    ) : null}
                    {dryRunPairing ? (
                      <div className="mt-3 flex items-center justify-between rounded-md border px-3 py-2 text-xs">
                        <span className="text-muted-foreground">PnL pairing by Event ID</span>
                        <Badge variant={pairingHasMismatch ? 'warning' : 'success'}>
                          {dryRunPairing.mixed_event_groups} mixed · {dryRunPairing.current_view_rows}/
                          {dryRunPairing.side_aware_rows}
                        </Badge>
                      </div>
                    ) : null}
                  </div>
                ) : dryRunPerformanceError ? (
                  <div className="rounded-md border bg-white p-4 text-sm text-muted-foreground">
                    Dry-run DB report endpoint is not available on the connected `ployd` yet.
                  </div>
                ) : null}
                {visibleTrading.slice(0, 4).map((snapshot) => (
                  <div key={snapshot.deployment_id} className="rounded-md border bg-white p-4">
                    <div className="mb-3 flex items-start justify-between gap-3">
                      <div className="min-w-0">
                        <div className="truncate font-medium">{snapshot.deployment_id}</div>
                        <div className="text-xs text-muted-foreground">{snapshot.runtime_mode}</div>
                      </div>
                      <Badge variant={snapshot.risk.active_orders > 0 ? 'warning' : 'secondary'}>
                        {snapshot.orders.length} orders
                      </Badge>
                    </div>
                    <div className="grid grid-cols-3 gap-2 text-sm">
                      <div>
                        <div className="text-xs text-muted-foreground">positions</div>
                        <div className="font-medium">{snapshot.positions.length}</div>
                      </div>
                      <div>
                        <div className="text-xs text-muted-foreground">intents</div>
                        <div className="font-medium">{snapshot.intents.length}</div>
                      </div>
                      <div>
                        <div className="text-xs text-muted-foreground">fills</div>
                        <div className="font-medium">{snapshot.fills.length}</div>
                      </div>
                    </div>
                  </div>
                ))}
              </div>
            </CardContent>
          </Card>
        </div>

        <div className="space-y-5 xl:col-span-4">
          <Card>
            <CardHeader className="pb-3">
              <CardTitle className="flex items-center gap-2 text-lg">
                <Database className="h-5 w-5" />
                DRON / Market Data
              </CardTitle>
            </CardHeader>
            <CardContent className="space-y-3">
              {marketDataHealthError ? (
                <div className="rounded-md border bg-white p-3 text-sm text-muted-foreground">
                  Market-data health endpoint is not available on the connected `ployd` yet.
                </div>
              ) : null}

              {(marketDataHealth?.sources ?? []).map((source) => {
                const age = ageSeconds(source.latest_at);
                const stale = age == null || age > source.stale_after_seconds;
                return (
                  <div key={source.source_id} className="rounded-md border bg-white p-3">
                    <div className="flex items-start justify-between gap-2">
                      <div className="min-w-0">
                        <div className="truncate text-sm font-medium">{source.source_id}</div>
                        <div className="text-xs text-muted-foreground">
                          {source.table_name} · rows ~{formatNumber(source.approx_rows)}
                        </div>
                      </div>
                      <Badge variant={stale ? 'warning' : 'success'}>
                        {age == null ? 'missing' : `${age}s`}
                      </Badge>
                    </div>
                    <div className="mt-2 text-xs text-muted-foreground">
                      latest {source.latest_at ? formatTimestamp(source.latest_at) : '-'} · stale
                      after {source.stale_after_seconds}s
                    </div>
                  </div>
                );
              })}

              {marketDataHealth?.deribit_iv_samples?.length ? (
                <div className="rounded-md border bg-white p-3">
                  <div className="mb-2 flex items-center justify-between">
                    <div className="text-sm font-medium">Deribit IV latest</div>
                    <Badge variant={staleMarketSources.some((s) => s.source_id === 'deribit_iv') ? 'warning' : 'success'}>
                      {marketDataHealth.deribit_iv_samples.length}
                    </Badge>
                  </div>
                  <div className="space-y-2">
                    {marketDataHealth.deribit_iv_samples.slice(0, 4).map((sample) => (
                      <div
                        key={`${sample.instrument_name}-${sample.fetched_at}`}
                        className="grid grid-cols-[1fr_0.5fr_0.6fr] gap-2 text-xs"
                      >
                        <span className="truncate">{sample.instrument_name}</span>
                        <span>{sample.mark_iv ?? '-'}</span>
                        <span className="text-right">{sample.underlying_price ?? '-'}</span>
                      </div>
                    ))}
                  </div>
                </div>
              ) : null}

              {marketDataHealth?.deribit_greeks_samples?.length ? (
                <div className="rounded-md border bg-white p-3">
                  <div className="mb-2 flex items-center justify-between">
                    <div className="text-sm font-medium">Deribit Greeks latest</div>
                    <Badge variant={staleMarketSources.some((s) => s.source_id === 'deribit_atm_greeks') ? 'warning' : 'success'}>
                      {marketDataHealth.deribit_greeks_samples.length}
                    </Badge>
                  </div>
                  <div className="space-y-2">
                    {marketDataHealth.deribit_greeks_samples.slice(0, 4).map((sample) => (
                      <div
                        key={`${sample.instrument_name}-${sample.fetched_at}`}
                        className="grid grid-cols-[1fr_0.5fr_0.5fr] gap-2 text-xs"
                      >
                        <span className="truncate">{sample.instrument_name}</span>
                        <span>δ {sample.delta ?? '-'}</span>
                        <span className="text-right">ν {sample.vega ?? '-'}</span>
                      </div>
                    ))}
                  </div>
                </div>
              ) : null}
            </CardContent>
          </Card>

          <Card>
            <CardHeader className="pb-3">
              <CardTitle className="flex items-center gap-2 text-lg">
                <Radio className="h-5 w-5" />
                Connectivity & Latency
              </CardTitle>
            </CardHeader>
            <CardContent className="space-y-3">
              <div className="grid grid-cols-2 gap-3 text-sm">
                <div className="rounded-md border bg-white p-3">
                  <div className="text-xs text-muted-foreground">event freshness</div>
                  <div className="mt-1 font-medium">{eventAge == null ? '-' : `${eventAge}s`}</div>
                </div>
                <div className="rounded-md border bg-white p-3">
                  <div className="text-xs text-muted-foreground">last trade</div>
                  <div className="mt-1 font-medium">{compactTime(status?.last_trade_time)}</div>
                </div>
                <div className="rounded-md border bg-white p-3">
                  <div className="text-xs text-muted-foreground">reconcile</div>
                  <div className="mt-1 font-medium">
                    {compactTime(status?.last_live_reconcile_success_at)}
                  </div>
                </div>
                <div className="rounded-md border bg-white p-3">
                  <div className="text-xs text-muted-foreground">failures</div>
                  <div className="mt-1 font-medium">{status?.live_reconcile_failures ?? 0}</div>
                </div>
              </div>

              <div className="space-y-2">
                {(metrics?.heartbeats ?? []).length === 0 ? (
                  <div className="rounded-md border p-3 text-sm text-muted-foreground">
                    No heartbeat sources reported.
                  </div>
                ) : (
                  metrics?.heartbeats.map((heartbeat) => (
                    <div key={heartbeat.source_id} className="rounded-md border bg-white p-3">
                      <div className="flex items-start justify-between gap-2">
                        <div className="min-w-0">
                          <div className="truncate text-sm font-medium">{heartbeat.source_id}</div>
                          <div className="text-xs text-muted-foreground">
                            {heartbeat.source_kind} · lag {heartbeatLag(heartbeat)}
                          </div>
                        </div>
                        <Badge variant={heartbeat.state === 'healthy' ? 'success' : 'destructive'}>
                          {heartbeat.state}
                        </Badge>
                      </div>
                      {heartbeat.message ? (
                        <div className="mt-2 text-xs text-muted-foreground">{heartbeat.message}</div>
                      ) : null}
                    </div>
                  ))
                )}
              </div>
            </CardContent>
          </Card>

          <Card>
            <CardHeader className="pb-3">
              <CardTitle className="flex items-center gap-2 text-lg">
                <ShieldAlert className="h-5 w-5" />
                Attention Queue
              </CardTitle>
            </CardHeader>
            <CardContent className="space-y-3">
              {alerts.length === 0 && warningLogs.length === 0 ? (
                <div className="rounded-md border bg-white p-4 text-sm text-muted-foreground">
                  No active alerts or warning logs in the current stream.
                </div>
              ) : null}

              {alerts.slice(0, 5).map((alert) => (
                <div key={alert.alert_id} className="rounded-md border bg-white p-3">
                  <div className="flex items-start justify-between gap-2">
                    <div className="flex items-start gap-2">
                      <AlertTriangle className="mt-0.5 h-4 w-4 text-amber-600" />
                      <div>
                        <div className="text-sm font-medium">{alert.message}</div>
                        <div className="text-xs text-muted-foreground">
                          {alert.source_id} · {formatTimestamp(alert.triggered_at)}
                        </div>
                      </div>
                    </div>
                    <Badge variant={alert.severity === 'critical' ? 'destructive' : 'warning'}>
                      {alert.severity}
                    </Badge>
                  </div>
                </div>
              ))}

              {warningLogs.map((log) => (
                <div key={`${log.timestamp}-${log.component}-${log.message}`} className="rounded-md border bg-white p-3">
                  <div className="flex items-start justify-between gap-2">
                    <div className="min-w-0">
                      <div className="truncate text-sm font-medium">{log.message}</div>
                      <div className="text-xs text-muted-foreground">
                        {log.component} · {compactTime(log.timestamp)}
                      </div>
                    </div>
                    <Badge variant={log.level.toLowerCase().includes('error') ? 'destructive' : 'warning'}>
                      {log.level}
                    </Badge>
                  </div>
                </div>
              ))}
            </CardContent>
          </Card>

          <Card>
            <CardHeader className="pb-3">
              <CardTitle className="flex items-center gap-2 text-lg">
                <Timer className="h-5 w-5" />
                Watch List
              </CardTitle>
            </CardHeader>
            <CardContent className="space-y-2 text-sm">
              <div className="flex items-center justify-between rounded-md border bg-white p-3">
                <span className="text-muted-foreground">DB connection</span>
                <Badge variant={status?.database_connected ? 'success' : 'destructive'}>
                  {status?.database_connected ? 'online' : 'offline'}
                </Badge>
              </div>
              <div className="flex items-center justify-between rounded-md border bg-white p-3">
                <span className="text-muted-foreground">SSE connection</span>
                <Badge variant={wsConnected ? 'success' : 'warning'}>
                  {wsConnected ? 'live' : 'polling'}
                </Badge>
              </div>
              <div className="flex items-center justify-between rounded-md border bg-white p-3">
                <span className="text-muted-foreground">control-plane errors</span>
                <Badge variant={(status?.error_count_1h ?? 0) > 0 ? 'warning' : 'success'}>
                  {status?.error_count_1h ?? 0}/h
                </Badge>
              </div>
              <div className="flex items-center justify-between rounded-md border bg-white p-3">
                <span className="text-muted-foreground">memory available</span>
                <span className="font-medium">
                  {metrics?.host_memory_available_mb == null
                    ? '-'
                    : `${metrics.host_memory_available_mb}MB`}
                </span>
              </div>
              <div className="flex items-center justify-between rounded-md border bg-white p-3">
                <span className="flex items-center gap-2 text-muted-foreground">
                  <Database className="h-4 w-4" />
                  runtime snapshots
                </span>
                <span className="font-medium">{visibleTrading.length}</span>
              </div>
            </CardContent>
          </Card>
        </div>
      </div>
    </div>
  );
}
