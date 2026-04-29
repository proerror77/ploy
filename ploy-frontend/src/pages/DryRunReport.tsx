import { useMemo } from 'react';
import { Link, useParams, useSearchParams } from 'react-router-dom';
import { useQuery } from '@tanstack/react-query';
import {
  ArrowLeft,
  ArrowUpRight,
  BarChart3,
  ChevronRight,
  CircleDollarSign,
  LineChart as LineChartIcon,
  ListChecks,
  Loader2,
  PieChart,
  TableProperties,
} from 'lucide-react';
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

import { Badge } from '@/components/ui/Badge';
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/Card';
import { api } from '@/services/api';
import { useStore } from '@/store';
import type {
  DeploymentSummary,
  DryRunClosedTradeRow,
  DryRunEquityPoint,
  DryRunOpenPositionRow,
  DryRunPerformanceReport,
  DryRunStrategyReport,
  DryRunSymbolRow,
  DryRunWindowRow,
  TradingStateSnapshot,
} from '@/types';
import { cn, formatCurrency, formatNumber, formatTimestamp } from '@/lib/utils';

type EquityChartRow = {
  timestamp: number;
  label: string | null;
  [seriesKey: string]: number | string | null;
};

type EquitySeries = {
  key: string;
  label: string;
  color: string;
  points: DryRunEquityPoint[];
};

type StrategySurfaceRow = {
  key: string;
  routeId: string;
  label: string;
  strategyId: string;
  deploymentId: string;
  runtimeMode: string;
  color: string;
  health: 'healthy' | 'watch' | 'degraded' | 'empty';
  performance: 'profitable' | 'losing' | 'flat' | 'empty';
  attentionRank: number;
  pnl: number;
  todayPnl: number;
  todayTrades: number;
  todayClosedTrades: number;
  todayWins: number;
  todayLosses: number;
  closedTrades: number;
  winRate: number | null;
  sharpe: number | null;
  profitFactor: string;
  maxDrawdown: number | null;
  openExposure: number;
  openPositions: number;
  latestClosedAt: string | null;
  latestAgeHours: number | null;
  observedState: string;
  strategy?: DryRunStrategyReport;
  deployment?: DeploymentSummary;
  snapshot?: TradingStateSnapshot;
};

const aggregateSeriesKey = 'all_dry_run';
const strategyPalette = ['#111827', '#0f766e', '#2563eb', '#b45309', '#7c3aed', '#be123c', '#15803d', '#0891b2'];
const cstDayFormatter = new Intl.DateTimeFormat('en-CA', {
  day: '2-digit',
  month: '2-digit',
  timeZone: 'Asia/Shanghai',
  year: 'numeric',
});
const emptyStrategies: DryRunStrategyReport[] = [];
const emptyDeployments: DeploymentSummary[] = [];
const emptyTrading: TradingStateSnapshot[] = [];
const emptyTrades: DryRunClosedTradeRow[] = [];
const emptyPositions: DryRunOpenPositionRow[] = [];
const emptyWindows: DryRunWindowRow[] = [];
const emptySymbols: DryRunSymbolRow[] = [];

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
  return normalized.includes('dryrun') || normalized.includes('dry-run') || normalized.includes('paper');
}

function isDryRunDeployment(deployment: DeploymentSummary) {
  return isDryRunId(deployment.deployment_id);
}

function isDrillDeployment(deployment: DeploymentSummary) {
  const deploymentId = deployment.deployment_id.toLowerCase();
  const accountId = deployment.account_id?.toLowerCase() ?? '';
  return deploymentId.startsWith('example.') || deploymentId.includes('.drill') || accountId.includes('drill');
}

function snapshotHasActivity(snapshot: TradingStateSnapshot) {
  return (
    snapshot.orders.length > 0 ||
    snapshot.fills.length > 0 ||
    snapshot.positions.length > 0 ||
    snapshot.risk.active_orders > 0 ||
    snapshot.risk.open_positions > 0 ||
    snapshot.risk.pending_intents > 0 ||
    toNumber(snapshot.pnl.net_pnl) !== 0 ||
    toNumber(snapshot.risk.total_gross_exposure) !== 0
  );
}

function shouldShowUnreportedDryRunDeployment(deployment: DeploymentSummary, snapshot?: TradingStateSnapshot) {
  if (isDrillDeployment(deployment)) return false;
  if (snapshot && snapshotHasActivity(snapshot)) return true;
  return (
    deployment.desired_state === 'running' ||
    deployment.observed_state === 'running' ||
    deployment.observed_state === 'starting' ||
    deployment.observed_state === 'degraded'
  );
}

function isDryRunSnapshot(snapshot: TradingStateSnapshot) {
  return isDryRunId(snapshot.deployment_id) || isDryRunId(snapshot.runtime_mode);
}

function strategyLabel(strategy: DryRunStrategyReport) {
  return strategy.label || strategy.deployment_id || strategy.strategy_id || strategy.runtime_mode || 'unknown';
}

function tradeStrategyLabel(row: DryRunClosedTradeRow | DryRunOpenPositionRow) {
  return row.deployment_id || row.strategy_id || row.runtime_mode || 'unknown';
}

function routeIdFor(strategy: DryRunStrategyReport) {
  return strategy.deployment_id || strategy.strategy_id || strategy.runtime_mode || 'unknown';
}

function safeDecode(value?: string) {
  if (!value) return null;
  try {
    return decodeURIComponent(value);
  } catch {
    return value;
  }
}

function formatOptionalMetric(value?: number | null, digits = 2) {
  return value == null || !Number.isFinite(value) ? '-' : value.toFixed(digits);
}

function formatProfitFactor(value: DryRunStrategyReport['metrics']['profit_factor'] | undefined) {
  if (value == null) return '-';
  return typeof value === 'number' ? value.toFixed(2) : value;
}

function formatShortTime(timestamp?: string | null) {
  if (!timestamp) return '-';
  return new Date(timestamp).toLocaleString('zh-CN', {
    month: '2-digit',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
  });
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

function tradingDayCst(date = new Date()) {
  const parts = cstDayFormatter.formatToParts(date).reduce<Record<string, string>>((acc, part) => {
    acc[part.type] = part.value;
    return acc;
  }, {});
  return `${parts.year}-${parts.month}-${parts.day}`;
}

function rowForTradingDay<T extends { trading_day_cst?: string | null }>(rows: T[] | undefined, tradingDay: string) {
  return rows?.find((row) => row.trading_day_cst === tradingDay) ?? null;
}

function equityPointTime(point: DryRunEquityPoint) {
  const timestamp = Date.parse(point.timestamp ?? '');
  return Number.isFinite(timestamp) ? timestamp : null;
}

function buildEquityChart(series: EquitySeries[]) {
  const visibleSeries = series.filter((entry) => entry.points.length > 0);
  const rows = new Map<number, EquityChartRow>();

  for (const entry of visibleSeries) {
    for (const point of entry.points) {
      const timestamp = equityPointTime(point);
      if (timestamp == null) continue;
      const row = rows.get(timestamp) ?? { timestamp, label: point.timestamp ?? null };
      row[entry.key] = point.cumulative;
      rows.set(timestamp, row);
    }
  }

  return {
    series: visibleSeries,
    data: Array.from(rows.values()).sort((a, b) => a.timestamp - b.timestamp),
  };
}

function healthBadge(row: StrategySurfaceRow) {
  if (row.health === 'degraded') return 'destructive' as const;
  if (row.health === 'watch' || row.health === 'empty') return 'warning' as const;
  return 'success' as const;
}

function performanceBadge(row: StrategySurfaceRow) {
  if (row.performance === 'losing') return 'destructive' as const;
  if (row.performance === 'profitable') return 'success' as const;
  if (row.performance === 'empty') return 'warning' as const;
  return 'secondary' as const;
}

function strategyPerformance(todayPnl: number, todayClosedTrades: number, cumulativePnl: number, cumulativeClosedTrades: number) {
  if (todayClosedTrades === 0 && cumulativeClosedTrades === 0 && cumulativePnl === 0) return 'empty' as const;
  const referencePnl = todayClosedTrades > 0 ? todayPnl : cumulativePnl;
  if (referencePnl > 0) return 'profitable' as const;
  if (referencePnl < 0) return 'losing' as const;
  return 'flat' as const;
}

function hoursSince(timestamp?: string | null) {
  if (!timestamp) return null;
  const parsed = Date.parse(timestamp);
  if (!Number.isFinite(parsed)) return null;
  return Math.max(0, (Date.now() - parsed) / 3_600_000);
}

function strategyHealth(deployment: DeploymentSummary | undefined, snapshot: TradingStateSnapshot | undefined, latestAgeHours: number | null, closedTrades: number) {
  const observed = deployment?.observed_state ?? 'unknown';
  if (observed === 'failed' || observed === 'degraded') return 'degraded' as const;
  if (observed === 'stopped' || observed === 'paused') return 'watch' as const;
  if (closedTrades === 0 && !snapshot) return 'empty' as const;
  if (latestAgeHours != null && latestAgeHours > 24 && (snapshot?.risk.open_positions ?? 0) > 0) {
    return 'watch' as const;
  }
  if (latestAgeHours != null && latestAgeHours > 72 && closedTrades > 0) return 'watch' as const;
  return 'healthy' as const;
}

function attentionRank(health: StrategySurfaceRow['health'], performance: StrategySurfaceRow['performance'], todayPnl: number) {
  if (health === 'degraded') return 0;
  if (health === 'watch') return 1;
  if (performance === 'losing') return 2;
  if (health === 'empty') return 3;
  if (todayPnl < 0) return 4;
  return 5;
}

function MetricTile({
  label,
  value,
  detail,
  tone = 'neutral',
}: {
  label: string;
  value: string;
  detail: string;
  tone?: 'neutral' | 'good' | 'bad' | 'watch';
}) {
  return (
    <div
      className={cn('rounded-md border bg-white p-4', {
        'border-emerald-200 bg-emerald-50/50': tone === 'good',
        'border-red-200 bg-red-50/50': tone === 'bad',
        'border-amber-200 bg-amber-50/50': tone === 'watch',
      })}
    >
      <div className="text-xs font-medium uppercase text-muted-foreground">{label}</div>
      <div className="mt-2 text-2xl font-semibold tracking-normal">{value}</div>
      <div className="mt-1 text-xs text-muted-foreground">{detail}</div>
    </div>
  );
}

function EquityPanel({ title, subtitle, chart }: { title: string; subtitle: string; chart: ReturnType<typeof buildEquityChart> }) {
  return (
    <Card className="rounded-md shadow-none">
      <CardHeader className="px-4 pb-3 pt-4">
        <div className="flex items-start justify-between gap-3">
          <div>
            <CardTitle className="flex items-center gap-2 text-base">
              <LineChartIcon className="h-4 w-4" />
              {title}
            </CardTitle>
            <div className="mt-1 text-xs text-muted-foreground">{subtitle}</div>
          </div>
          <Badge variant="outline">{chart.series.length} lines</Badge>
        </div>
      </CardHeader>
      <CardContent className="px-4 pb-4">
        {chart.series.length > 0 && chart.data.length > 0 ? (
          <div className="h-[360px]">
            <ResponsiveContainer width="100%" height="100%">
              <LineChart data={chart.data} margin={{ left: 4, right: 14, top: 10, bottom: 0 }}>
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
                  width={70}
                />
                <Tooltip
                  formatter={(value, name) => {
                    const seriesName = String(name);
                    return [
                      formatCurrency(toNumber(value)),
                      chart.series.find((entry) => entry.key === seriesName)?.label ?? seriesName,
                    ];
                  }}
                  labelFormatter={formatChartTimestamp}
                />
                <Legend formatter={(value) => <span className="text-xs text-muted-foreground">{value}</span>} />
                {chart.series.map((series) => (
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
          <div className="flex h-[360px] items-center justify-center rounded-md bg-muted text-sm text-muted-foreground">
            No closed-trade equity points.
          </div>
        )}
      </CardContent>
    </Card>
  );
}

function TradeLedger({ trades, limit = 24 }: { trades: DryRunClosedTradeRow[]; limit?: number }) {
  const rows = trades.slice(0, limit);
  return (
    <div className="overflow-hidden rounded-md border bg-white">
      <div className="grid grid-cols-[1.35fr_1.15fr_0.65fr_0.65fr_0.75fr_0.8fr_0.9fr_1fr] bg-muted px-4 py-2 text-xs font-medium uppercase text-muted-foreground">
        <span>strategy</span>
        <span>symbol</span>
        <span>side</span>
        <span>exit</span>
        <span>qty</span>
        <span>notional</span>
        <span className="text-right">pnl</span>
        <span className="text-right">closed</span>
      </div>
      {rows.length === 0 ? (
        <div className="px-4 py-8 text-center text-sm text-muted-foreground">No closed dry-run trades.</div>
      ) : (
        rows.map((trade) => {
          const pnl = toNumber(trade.net_pnl);
          return (
            <div
              key={trade.trade_key ?? `${trade.strategy_id}:${trade.symbol}:${trade.closed_at}`}
              className="grid grid-cols-[1.35fr_1.15fr_0.65fr_0.65fr_0.75fr_0.8fr_0.9fr_1fr] items-center border-t px-4 py-2 text-sm"
            >
              <div className="min-w-0">
                <div className="truncate font-medium">{tradeStrategyLabel(trade)}</div>
                <div className="truncate text-xs text-muted-foreground">{trade.strategy_id || 'unknown'}</div>
              </div>
              <div className="min-w-0">
                <div className="truncate">{trade.symbol || 'unknown'}</div>
                <div className="truncate text-xs text-muted-foreground">{trade.event_id || trade.trade_key || '-'}</div>
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
        })
      )}
    </div>
  );
}

function OpenPositionsTable({ positions }: { positions: DryRunOpenPositionRow[] }) {
  return (
    <div className="overflow-x-auto rounded-md border bg-white">
      <div className="grid min-w-[680px] grid-cols-[1.35fr_1.15fr_0.6fr_0.65fr_0.85fr_1fr] bg-muted px-4 py-2 text-xs font-medium uppercase text-muted-foreground">
        <span>strategy</span>
        <span>symbol</span>
        <span>side</span>
        <span>qty</span>
        <span>notional</span>
        <span className="text-right">opened</span>
      </div>
      {positions.length === 0 ? (
        <div className="px-4 py-8 text-center text-sm text-muted-foreground">No open dry-run positions.</div>
      ) : (
        positions.map((position) => (
          <div
            key={position.trade_key ?? `${position.strategy_id}:${position.symbol}:${position.opened_at}`}
            className="grid min-w-[680px] grid-cols-[1.35fr_1.15fr_0.6fr_0.65fr_0.85fr_1fr] items-center border-t px-4 py-2 text-sm"
          >
            <div className="min-w-0">
              <div className="truncate font-medium">{tradeStrategyLabel(position)}</div>
              <div className="truncate text-xs text-muted-foreground">{position.strategy_id || 'unknown'}</div>
            </div>
            <div className="truncate">{position.symbol || 'unknown'}</div>
            <div>{position.market_side || '-'}</div>
            <div>{formatNumber(toNumber(position.quantity))}</div>
            <div>{formatCurrency(toNumber(position.notional))}</div>
            <div className="text-right text-xs text-muted-foreground">
              {position.opened_at ? formatTimestamp(position.opened_at) : '-'}
            </div>
          </div>
        ))
      )}
    </div>
  );
}

function SmallTable({
  title,
  icon,
  children,
}: {
  title: string;
  icon: React.ReactNode;
  children: React.ReactNode;
}) {
  return (
    <Card className="rounded-md shadow-none">
      <CardHeader className="px-4 pb-3 pt-4">
        <CardTitle className="flex items-center gap-2 text-base">
          {icon}
          {title}
        </CardTitle>
      </CardHeader>
      <CardContent className="px-4 pb-4">{children}</CardContent>
    </Card>
  );
}

export function DryRunReport() {
  const { deploymentId } = useParams();
  const [searchParams] = useSearchParams();
  const selectedId = safeDecode(deploymentId) ?? searchParams.get('strategy_id');
  const { deployments: pushedDeployments, tradingSnapshots, wsConnected } = useStore();

  const { data: report, error: reportError, isLoading: reportLoading } = useQuery<DryRunPerformanceReport>({
    queryKey: ['reports', 'dry-run'],
    queryFn: () => api.getDryRunPerformance(),
    refetchInterval: 30000,
    retry: false,
  });

  const { data: polledDeployments = emptyDeployments } = useQuery({
    queryKey: ['deployments'],
    queryFn: () => api.getDeployments(),
    refetchInterval: 15000,
  });

  const { data: polledTrading = emptyTrading } = useQuery({
    queryKey: ['trading', 'state'],
    queryFn: () => api.getTradingState(),
    refetchInterval: 10000,
  });

  const deployments = polledDeployments.length > 0 ? polledDeployments : wsConnected ? pushedDeployments : emptyDeployments;
  const trading = polledTrading.length > 0 ? polledTrading : wsConnected ? tradingSnapshots : emptyTrading;
  const dryRunDeployments = deployments.filter(isDryRunDeployment);
  const dryRunSnapshots = trading.filter(isDryRunSnapshot);
  const strategies = report?.strategies ?? emptyStrategies;
  const deploymentById = useMemo(() => new Map(dryRunDeployments.map((item) => [item.deployment_id, item])), [dryRunDeployments]);
  const snapshotByDeployment = useMemo(() => new Map(dryRunSnapshots.map((item) => [item.deployment_id, item])), [dryRunSnapshots]);
  const currentDay = tradingDayCst();

  const strategyRows = useMemo<StrategySurfaceRow[]>(() => {
    const reported = strategies.map((strategy, index) => {
      const pnl = toNumber(strategy.summary.realized_pnl);
      const deployment = deploymentById.get(strategy.deployment_id);
      const snapshot = snapshotByDeployment.get(strategy.deployment_id);
      const today = rowForTradingDay(strategy.daily, currentDay);
      const todayPnl = toNumber(today?.net_pnl);
      const todayClosedTrades = today?.closed_trade_count ?? 0;
      const latestAge = hoursSince(strategy.summary.latest_closed_at);
      const health = strategyHealth(deployment, snapshot, latestAge, strategy.summary.closed_trades);
      const performance = strategyPerformance(todayPnl, todayClosedTrades, pnl, strategy.summary.closed_trades);
      const row: StrategySurfaceRow = {
        key: `${strategy.runtime_mode}:${strategy.strategy_id}:${strategy.deployment_id}`,
        routeId: routeIdFor(strategy),
        label: strategyLabel(strategy),
        strategyId: strategy.strategy_id || 'unknown',
        deploymentId: strategy.deployment_id || 'unknown',
        runtimeMode: strategy.runtime_mode || 'unknown',
        color: strategyPalette[(index + 1) % strategyPalette.length],
        health,
        performance,
        attentionRank: attentionRank(health, performance, todayPnl),
        pnl,
        todayPnl,
        todayTrades: today?.trade_count ?? 0,
        todayClosedTrades,
        todayWins: today?.wins ?? 0,
        todayLosses: today?.losses ?? 0,
        closedTrades: strategy.summary.closed_trades,
        winRate: strategy.summary.win_rate_pct,
        sharpe: strategy.metrics.sharpe ?? null,
        profitFactor: formatProfitFactor(strategy.metrics.profit_factor),
        maxDrawdown: strategy.metrics.max_drawdown,
        openExposure: toNumber(strategy.summary.open_exposure),
        openPositions: strategy.summary.open_positions,
        latestClosedAt: strategy.summary.latest_closed_at ?? null,
        latestAgeHours: latestAge,
        observedState: deployment?.observed_state ?? 'unknown',
        strategy,
        deployment,
        snapshot,
      };
      return row;
    });

    const reportedDeploymentIds = new Set(reported.map((row) => row.deploymentId));
    const missing = dryRunDeployments
      .filter((deployment) => {
        if (reportedDeploymentIds.has(deployment.deployment_id)) return false;
        return shouldShowUnreportedDryRunDeployment(deployment, snapshotByDeployment.get(deployment.deployment_id));
      })
      .map((deployment): StrategySurfaceRow => {
        const snapshot = snapshotByDeployment.get(deployment.deployment_id);
        const pnl = toNumber(snapshot?.pnl.net_pnl);
        const health = strategyHealth(deployment, snapshot, null, 0);
        const performance = strategyPerformance(0, 0, pnl, 0);
        const row: StrategySurfaceRow = {
          key: `missing:${deployment.deployment_id}`,
          routeId: deployment.deployment_id,
          label: deployment.deployment_id,
          strategyId: 'unknown',
          deploymentId: deployment.deployment_id,
          runtimeMode: snapshot?.runtime_mode ?? 'dry-run',
          color: '#94a3b8',
          health,
          performance,
          attentionRank: attentionRank(health, performance, 0),
          pnl,
          todayPnl: 0,
          todayTrades: 0,
          todayClosedTrades: 0,
          todayWins: 0,
          todayLosses: 0,
          closedTrades: 0,
          winRate: null,
          sharpe: null,
          profitFactor: '-',
          maxDrawdown: null,
          openExposure: toNumber(snapshot?.risk.total_gross_exposure),
          openPositions: snapshot?.risk.open_positions ?? 0,
          latestClosedAt: null,
          latestAgeHours: null,
          observedState: deployment.observed_state,
          deployment,
          snapshot,
        };
        return row;
      });

    return [...reported, ...missing].sort((a, b) => {
      return a.attentionRank - b.attentionRank || toNumber(a.todayPnl) - toNumber(b.todayPnl) || b.todayClosedTrades - a.todayClosedTrades;
    });
  }, [currentDay, deploymentById, dryRunDeployments, snapshotByDeployment, strategies]);

  const selectedRow = selectedId
    ? strategyRows.find(
        (row) =>
          row.routeId === selectedId ||
          row.deploymentId === selectedId ||
          row.strategyId === selectedId ||
          row.label === selectedId
      )
    : null;

  const overviewChart = useMemo(() => {
    const series: EquitySeries[] = [
      {
        key: aggregateSeriesKey,
        label: 'All dry-run',
        color: strategyPalette[0],
        points: report?.equity_curve ?? [],
      },
      ...strategies.map((strategy, index) => ({
        key: `strategy_${index}`,
        label: strategyLabel(strategy),
        color: strategyPalette[(index + 1) % strategyPalette.length],
        points: strategy.equity_curve,
      })),
    ];
    return buildEquityChart(series);
  }, [report?.equity_curve, strategies]);

  const detailChart = useMemo(() => {
    if (!selectedRow?.strategy) return buildEquityChart([]);
    return buildEquityChart([
      {
        key: 'selected_strategy',
        label: selectedRow.label,
        color: selectedRow.color,
        points: selectedRow.strategy.equity_curve,
      },
    ]);
  }, [selectedRow]);

  const summary = report?.summary;
  const metrics = report?.metrics;
  const closedTrades = report?.closed_trades ?? emptyTrades;
  const recentTrades = report?.recent_closed ?? emptyTrades;
  const openPositions = report?.open_positions ?? emptyPositions;
  const todayPortfolio = rowForTradingDay(report?.daily, currentDay);
  const redTodayStrategies = strategyRows.filter((row) => row.todayClosedTrades > 0 && row.todayPnl < 0).length;
  const greenTodayStrategies = strategyRows.filter((row) => row.todayClosedTrades > 0 && row.todayPnl > 0).length;
  const watchStrategies = strategyRows.filter((row) => row.health === 'watch' || row.health === 'degraded').length;
  const emptyStrategyCount = strategyRows.filter((row) => row.health === 'empty').length;
  const portfolioPnl = toNumber(summary?.realized_pnl);
  const todayPortfolioPnl = toNumber(todayPortfolio?.net_pnl);
  const todayPortfolioClosed = todayPortfolio?.closed_trade_count ?? 0;
  const todayPortfolioTrades = todayPortfolio?.trade_count ?? 0;
  const strategyTrades = selectedRow?.strategy?.closed_trades ?? emptyTrades;
  const strategyPositions = selectedRow?.strategy?.open_positions ?? emptyPositions;
  const strategyWindows = selectedRow?.strategy?.by_window ?? emptyWindows;
  const strategySymbols = selectedRow?.strategy?.symbols ?? emptySymbols;

  if (reportLoading) {
    return (
      <div className="flex h-full min-w-[1280px] items-center justify-center bg-[#f6f7f4]">
        <Loader2 className="h-8 w-8 animate-spin text-muted-foreground" />
      </div>
    );
  }

  if (reportError || !report) {
    return (
      <div className="min-h-full min-w-[1280px] bg-[#f6f7f4] p-6 text-[#171a16]">
        <Card className="rounded-md shadow-none">
          <CardContent className="p-6">
            <div className="text-lg font-semibold">Dry-run report unavailable</div>
            <div className="mt-2 text-sm text-muted-foreground">
              {reportError instanceof Error ? reportError.message : 'No dry-run report payload returned.'}
            </div>
          </CardContent>
        </Card>
      </div>
    );
  }

  if (selectedId) {
    return (
      <div className="min-h-full min-w-[1280px] bg-[#f6f7f4] p-6 text-[#171a16]">
        <div className="mb-5 flex items-start justify-between gap-4">
          <div>
            <Link
              to="/dry-run"
              className="mb-3 inline-flex items-center gap-2 rounded-md border bg-white px-3 py-2 text-sm font-medium text-muted-foreground hover:text-foreground"
            >
              <ArrowLeft className="h-4 w-4" />
              All dry-run strategies
            </Link>
            <div className="flex items-center gap-3">
              <h1 className="text-3xl font-semibold tracking-normal">
                {selectedRow?.label ?? selectedId}
              </h1>
              {selectedRow ? <Badge variant={healthBadge(selectedRow)}>{selectedRow.health}</Badge> : null}
              {selectedRow ? <Badge variant={performanceBadge(selectedRow)}>{selectedRow.performance}</Badge> : null}
            </div>
            <div className="mt-1 text-sm text-muted-foreground">
              {selectedRow?.strategyId ?? 'unknown'} · {selectedRow?.deploymentId ?? 'unknown'} · generated{' '}
              {formatShortTime(report.generated_at)}
            </div>
          </div>
          <div className="grid grid-cols-3 gap-2 text-right text-xs text-muted-foreground">
            <div className="rounded-md border bg-white px-3 py-2">
              <div className="font-medium text-foreground">{selectedRow?.runtimeMode ?? '-'}</div>
              <div>runtime</div>
            </div>
            <div className="rounded-md border bg-white px-3 py-2">
              <div className="font-medium text-foreground">{selectedRow?.deployment?.observed_state ?? '-'}</div>
              <div>observed</div>
            </div>
            <div className="rounded-md border bg-white px-3 py-2">
              <div className="font-medium text-foreground">{formatShortTime(selectedRow?.latestClosedAt)}</div>
              <div>latest close</div>
            </div>
          </div>
        </div>

        <div className="mb-5 grid grid-cols-6 gap-3">
          <MetricTile
            label="today pnl"
            value={selectedRow ? formatCurrency(selectedRow.todayPnl) : '-'}
            detail={`${selectedRow?.todayClosedTrades ?? 0} closed today · cumulative ${selectedRow ? formatCurrency(selectedRow.pnl) : '-'}`}
            tone={selectedRow && selectedRow.todayPnl < 0 ? 'bad' : selectedRow && selectedRow.todayPnl > 0 ? 'good' : 'neutral'}
          />
          <MetricTile
            label="today wins"
            value={`${selectedRow?.todayWins ?? 0}/${selectedRow?.todayLosses ?? 0}`}
            detail={`${selectedRow?.todayTrades ?? 0} trades today · all-time win ${selectedRow?.winRate == null ? '-' : `${selectedRow.winRate.toFixed(1)}%`}`}
          />
          <MetricTile
            label="sharpe"
            value={formatOptionalMetric(selectedRow?.sharpe)}
            detail={`drawdown ${selectedRow?.maxDrawdown == null ? '-' : formatCurrency(selectedRow.maxDrawdown)}`}
            tone={selectedRow?.maxDrawdown != null && selectedRow.maxDrawdown < 0 ? 'watch' : 'neutral'}
          />
          <MetricTile
            label="open exposure"
            value={selectedRow ? formatCurrency(selectedRow.openExposure) : '-'}
            detail={`${selectedRow?.openPositions ?? 0} open positions`}
          />
          <MetricTile
            label="runtime health"
            value={selectedRow?.observedState ?? '-'}
            detail={`${formatNumber(selectedRow?.snapshot?.risk.active_orders ?? 0)} active orders · ${selectedRow?.snapshot?.risk.pending_intents ?? 0} pending`}
            tone={selectedRow?.health === 'degraded' ? 'bad' : selectedRow?.health === 'watch' ? 'watch' : 'neutral'}
          />
          <MetricTile
            label="fees"
            value={selectedRow?.strategy ? formatCurrency(selectedRow.strategy.summary.total_fees) : '-'}
            detail={selectedRow?.deployment?.account_id ?? 'default account'}
          />
        </div>

        <div className="grid grid-cols-[1.05fr_0.95fr] gap-5">
          <div className="space-y-5">
            <EquityPanel
              title="Strategy equity"
              subtitle="Cumulative closed-trade PnL by close time"
              chart={detailChart}
            />
          </div>
          <div className="space-y-5">
            <SmallTable title="Window quality" icon={<BarChart3 className="h-4 w-4" />}>
              <div className="overflow-hidden rounded-md border bg-white">
                <div className="grid grid-cols-[0.8fr_0.8fr_0.8fr_1fr] bg-muted px-3 py-2 text-xs font-medium uppercase text-muted-foreground">
                  <span>window</span>
                  <span>closed</span>
                  <span>win</span>
                  <span className="text-right">pnl</span>
                </div>
                {strategyWindows.length === 0 ? (
                  <div className="px-3 py-6 text-center text-sm text-muted-foreground">No window rows.</div>
                ) : (
                  strategyWindows.map((window) => (
                    <div
                      key={window.window_label}
                      className="grid grid-cols-[0.8fr_0.8fr_0.8fr_1fr] items-center border-t px-3 py-2 text-sm"
                    >
                      <span className="font-medium">{window.window_label}</span>
                      <span>{window.closed_trades}</span>
                      <span>{toNumber(window.win_rate_pct).toFixed(1)}%</span>
                      <span className={cn('text-right font-medium', window.realized_pnl < 0 ? 'text-destructive' : 'text-success')}>
                        {formatCurrency(toNumber(window.realized_pnl))}
                      </span>
                    </div>
                  ))
                )}
              </div>
            </SmallTable>
            <SmallTable title="Symbol contribution" icon={<PieChart className="h-4 w-4" />}>
              <div className="overflow-hidden rounded-md border bg-white">
                <div className="grid grid-cols-[1.2fr_0.7fr_0.7fr_1fr] bg-muted px-3 py-2 text-xs font-medium uppercase text-muted-foreground">
                  <span>symbol</span>
                  <span>trades</span>
                  <span>win/loss</span>
                  <span className="text-right">pnl</span>
                </div>
                {strategySymbols.length === 0 ? (
                  <div className="px-3 py-6 text-center text-sm text-muted-foreground">No symbol rows.</div>
                ) : (
                  strategySymbols.slice(0, 12).map((symbol) => (
                    <div
                      key={symbol.symbol}
                      className="grid grid-cols-[1.2fr_0.7fr_0.7fr_1fr] items-center border-t px-3 py-2 text-sm"
                    >
                      <span className="truncate font-medium">{symbol.symbol}</span>
                      <span>{symbol.trades}</span>
                      <span>
                        {symbol.wins}/{symbol.losses}
                      </span>
                      <span className={cn('text-right font-medium', symbol.net_pnl < 0 ? 'text-destructive' : 'text-success')}>
                        {formatCurrency(toNumber(symbol.net_pnl))}
                      </span>
                    </div>
                  ))
                )}
              </div>
            </SmallTable>
            <SmallTable title="Open positions" icon={<ListChecks className="h-4 w-4" />}>
              <OpenPositionsTable positions={strategyPositions} />
            </SmallTable>
          </div>
        </div>

        <div className="mt-5">
          <SmallTable title="Recent trades" icon={<TableProperties className="h-4 w-4" />}>
            <TradeLedger trades={strategyTrades} limit={40} />
          </SmallTable>
        </div>
      </div>
    );
  }

  return (
    <div className="min-h-full min-w-[1280px] bg-[#f6f7f4] p-6 text-[#171a16]">
      <div className="mb-5 flex items-end justify-between gap-4">
        <div>
          <div className="mb-2 flex items-center gap-2">
            <Badge variant="outline">Dry-run</Badge>
            <Badge variant={report.pairing.mixed_event_groups > 0 ? 'warning' : 'success'}>
              {report.pairing.mixed_event_groups} mixed events
            </Badge>
          </div>
          <h1 className="text-3xl font-semibold tracking-normal">Dry-run Report</h1>
          <div className="mt-1 text-sm text-muted-foreground">
            generated {formatShortTime(report.generated_at)} · latest close {formatShortTime(summary?.latest_closed_at)}
          </div>
        </div>
        <div className="grid grid-cols-4 gap-2 text-right text-xs text-muted-foreground">
          <div className="rounded-md border bg-white px-3 py-2">
            <div className="font-medium text-foreground">{strategyRows.length}</div>
            <div>strategies</div>
          </div>
          <div className="rounded-md border bg-white px-3 py-2">
            <div className="font-medium text-foreground">{greenTodayStrategies}</div>
            <div>green today</div>
          </div>
          <div className="rounded-md border bg-white px-3 py-2">
            <div className="font-medium text-foreground">{redTodayStrategies}</div>
            <div>red today</div>
          </div>
          <div className="rounded-md border bg-white px-3 py-2">
            <div className="font-medium text-foreground">{watchStrategies}</div>
            <div>watch</div>
          </div>
        </div>
      </div>

      <div className="mb-5 grid grid-cols-6 gap-3">
        <MetricTile
          label="today pnl"
          value={formatCurrency(todayPortfolioPnl)}
          detail={`${todayPortfolioClosed} closed today · cumulative ${formatCurrency(portfolioPnl)}`}
          tone={todayPortfolioPnl < 0 ? 'bad' : todayPortfolioPnl > 0 ? 'good' : 'neutral'}
        />
        <MetricTile
          label="today wins"
          value={`${todayPortfolio?.wins ?? 0}/${todayPortfolio?.losses ?? 0}`}
          detail={`${todayPortfolioTrades} trades today · all-time win ${summary == null ? '-' : `${summary.win_rate_pct.toFixed(1)}%`}`}
        />
        <MetricTile
          label="sharpe"
          value={formatOptionalMetric(metrics?.sharpe)}
          detail={`PF ${formatProfitFactor(metrics?.profit_factor)}`}
        />
        <MetricTile
          label="max drawdown"
          value={metrics ? formatCurrency(metrics.max_drawdown) : '-'}
          detail={`${metrics?.equity_points ?? 0} equity points`}
          tone={metrics && metrics.max_drawdown < 0 ? 'watch' : 'neutral'}
        />
        <MetricTile
          label="open exposure"
          value={summary ? formatCurrency(summary.open_exposure) : '-'}
          detail={`${summary?.open_positions ?? 0} open positions`}
        />
        <MetricTile
          label="no data"
          value={formatNumber(emptyStrategyCount)}
          detail={`${recentTrades.length} recent trades · ${closedTrades.length} retained rows`}
        />
      </div>

      <div className="grid grid-cols-[1.05fr_0.95fr] gap-5">
        <div className="space-y-5">
          <EquityPanel
            title="All dry-run equity"
            subtitle="Aggregate line plus every strategy line by close time"
            chart={overviewChart}
          />
        </div>

        <div className="space-y-5">
          <Card className="rounded-md shadow-none">
            <CardHeader className="px-4 pb-3 pt-4">
              <CardTitle className="flex items-center gap-2 text-base">
                <BarChart3 className="h-4 w-4" />
                Strategy ranking
              </CardTitle>
            </CardHeader>
            <CardContent className="px-4 pb-4">
              <div className="overflow-hidden rounded-md border bg-white">
                <div className="grid grid-cols-[1.35fr_0.7fr_0.6fr_0.65fr_0.6fr_0.85fr_0.85fr_0.35fr] bg-muted px-3 py-2 text-xs font-medium uppercase text-muted-foreground">
                  <span>strategy</span>
                  <span>health</span>
                  <span>today</span>
                  <span>closed</span>
                  <span>win</span>
                  <span className="text-right">today pnl</span>
                  <span className="text-right">all pnl</span>
                  <span />
                </div>
                {strategyRows.length === 0 ? (
                  <div className="px-3 py-8 text-center text-sm text-muted-foreground">No dry-run strategy rows.</div>
                ) : (
                  strategyRows.map((row) => (
                    <Link
                      key={row.key}
                      to={`/dry-run/${encodeURIComponent(row.routeId)}`}
                      className="grid grid-cols-[1.35fr_0.7fr_0.6fr_0.65fr_0.6fr_0.85fr_0.85fr_0.35fr] items-center border-t px-3 py-2 text-sm hover:bg-muted/60"
                    >
                      <div className="min-w-0">
                        <div className="flex items-center gap-2">
                          <span className="h-2.5 w-2.5 rounded-full" style={{ backgroundColor: row.color }} />
                          <span className="truncate font-medium">{row.label}</span>
                        </div>
                        <div className="truncate text-xs text-muted-foreground">{row.strategyId} · {row.runtimeMode}</div>
                      </div>
                      <Badge variant={healthBadge(row)} className="w-fit">{row.health}</Badge>
                      <span>{row.todayClosedTrades}</span>
                      <span>{row.closedTrades}</span>
                      <span>{row.winRate == null ? '-' : `${row.winRate.toFixed(1)}%`}</span>
                      <span className={cn('text-right font-medium', row.todayPnl < 0 ? 'text-destructive' : 'text-success')}>
                        {formatCurrency(row.todayPnl)}
                      </span>
                      <span className={cn('text-right font-medium', row.pnl < 0 ? 'text-destructive' : 'text-success')}>
                        {formatCurrency(row.pnl)}
                      </span>
                      <ChevronRight className="ml-auto h-4 w-4 text-muted-foreground" />
                    </Link>
                  ))
                )}
              </div>
            </CardContent>
          </Card>

          <Card className="rounded-md shadow-none">
            <CardHeader className="px-4 pb-3 pt-4">
              <CardTitle className="flex items-center gap-2 text-base">
                <CircleDollarSign className="h-4 w-4" />
                Open dry-run positions
              </CardTitle>
            </CardHeader>
            <CardContent className="px-4 pb-4">
              <OpenPositionsTable positions={openPositions} />
            </CardContent>
          </Card>

          <Card className="rounded-md shadow-none">
            <CardHeader className="px-4 pb-3 pt-4">
              <CardTitle className="flex items-center gap-2 text-base">
                <ArrowUpRight className="h-4 w-4" />
                Attention queue
              </CardTitle>
            </CardHeader>
            <CardContent className="space-y-2 px-4 pb-4">
              {strategyRows.filter((row) => row.attentionRank < 5).length === 0 ? (
                <div className="rounded-md border bg-white px-3 py-4 text-sm text-muted-foreground">No health or today-PnL issues.</div>
              ) : (
                strategyRows
                  .filter((row) => row.attentionRank < 5)
                  .slice(0, 6)
                  .map((row) => (
                    <Link
                      key={`attention:${row.key}`}
                      to={`/dry-run/${encodeURIComponent(row.routeId)}`}
                      className="flex items-center justify-between gap-3 rounded-md border bg-white px-3 py-2 text-sm hover:bg-muted/60"
                    >
                      <div className="min-w-0">
                        <div className="truncate font-medium">{row.label}</div>
                        <div className="text-xs text-muted-foreground">
                          today {formatCurrency(row.todayPnl)} · health {row.health} · latest {formatShortTime(row.latestClosedAt)}
                        </div>
                      </div>
                      <Badge variant={healthBadge(row)}>{row.health}</Badge>
                    </Link>
                  ))
              )}
            </CardContent>
          </Card>
        </div>
      </div>

      <div className="mt-5">
        <Card className="rounded-md shadow-none">
          <CardHeader className="px-4 pb-3 pt-4">
            <CardTitle className="flex items-center gap-2 text-base">
              <TableProperties className="h-4 w-4" />
              Recent closed trades
            </CardTitle>
          </CardHeader>
          <CardContent className="px-4 pb-4">
            <TradeLedger trades={recentTrades} />
          </CardContent>
        </Card>
      </div>
    </div>
  );
}
