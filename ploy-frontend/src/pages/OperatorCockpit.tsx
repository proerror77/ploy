import { useMemo, useState, type ReactNode } from 'react';
import { useQuery } from '@tanstack/react-query';
import {
  Bar,
  BarChart,
  CartesianGrid,
  Cell,
  Line,
  LineChart,
  ResponsiveContainer,
  Tooltip,
  XAxis,
  YAxis,
} from 'recharts';
import {
  AlertTriangle,
  BarChart3,
  CheckCircle2,
  Clock3,
  Database,
  LineChart as LineChartIcon,
  ListChecks,
  Loader2,
  Percent,
  Sigma,
  Target,
  TrendingDown,
  TrendingUp,
} from 'lucide-react';

import { Badge } from '@/components/ui/Badge';
import { api } from '@/services/api';
import type {
  DryRunClosedTradeRow,
  DryRunEquityPoint,
  DryRunPerformanceReport,
  DryRunStrategyReport,
  DeploymentSummary,
  TradingStateSnapshot,
} from '@/types';
import { cn, formatCurrency, formatNumber, formatTimestamp } from '@/lib/utils';

type Tone = 'good' | 'warn' | 'bad' | 'muted';
type TradeResultFilter = 'all' | 'loss' | 'win';

interface StrategyView {
  key: string;
  label: string;
  report: DryRunStrategyReport;
  isAggregate: boolean;
}

interface Metric {
  label: string;
  value: string;
  detail: string;
  tone: Tone;
}

interface StrategyLine {
  viewKey: string;
  dataKey: string;
  label: string;
  color: string;
  report: DryRunStrategyReport;
  points: DryRunEquityPoint[];
}

type CurveRow = {
  index: number;
  timestamp?: string | null;
  symbol?: string | null;
} & Record<string, number | string | null | undefined>;

interface RuntimeStrategyRow {
  deploymentId: string;
  runtimeMode: string;
  desiredState?: string;
  observedState?: string;
  positions: number;
  orders: number;
  fills: number;
}

const STRATEGY_COLORS = ['#ff4d4d', '#00e090', '#f6b21a', '#42a5ff', '#e665ff', '#ff8a3d'];

function toNumber(value: unknown): number {
  if (typeof value === 'number') return Number.isFinite(value) ? value : 0;
  if (typeof value === 'string') {
    const parsed = Number(value);
    return Number.isFinite(parsed) ? parsed : 0;
  }
  return 0;
}

function formatCompactCurrency(value: number) {
  return new Intl.NumberFormat('en-US', {
    style: 'currency',
    currency: 'USD',
    maximumFractionDigits: Math.abs(value) >= 1000 ? 0 : 2,
  }).format(value);
}

function formatPrice(value: string | number | null | undefined) {
  if (value == null) return '-';
  const parsed = toNumber(value);
  return Number.isFinite(parsed) ? parsed.toFixed(4) : '-';
}

function formatPct(value: number | string | null | undefined, digits = 1) {
  if (value == null) return 'N/A';
  const parsed = toNumber(value);
  if (!Number.isFinite(parsed)) return 'N/A';
  return `${parsed.toFixed(digits)}%`;
}

function shortDateTime(value?: string | null) {
  if (!value) return '-';
  return new Date(value).toLocaleString('zh-CN', {
    month: '2-digit',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
  });
}

function shortDate(value?: string | null) {
  if (!value) return '-';
  return new Date(value).toLocaleDateString('zh-CN', {
    month: '2-digit',
    day: '2-digit',
  });
}

function timeOnly(value?: string | null) {
  if (!value) return '-';
  return new Date(value).toLocaleTimeString('zh-CN', {
    hour: '2-digit',
    minute: '2-digit',
    second: '2-digit',
  });
}

function toneClass(tone: Tone) {
  return {
    'text-[#00e090]': tone === 'good',
    'text-[#f5b93f]': tone === 'warn',
    'text-[#ff4d4d]': tone === 'bad',
    'text-[#8f98a8]': tone === 'muted',
  };
}

function borderToneClass(tone: Tone) {
  return {
    'border-[#06472f] bg-[#041b14]': tone === 'good',
    'border-[#4d3510] bg-[#1d1507]': tone === 'warn',
    'border-[#5a1919] bg-[#200909]': tone === 'bad',
    'border-[#252a33] bg-[#0b0d10]': tone === 'muted',
  };
}

function pnlTone(value: number): Tone {
  if (value > 0) return 'good';
  if (value < 0) return 'bad';
  return 'muted';
}

function mean(values: number[]) {
  if (values.length === 0) return 0;
  return values.reduce((sum, value) => sum + value, 0) / values.length;
}

function stddev(values: number[]) {
  if (values.length < 2) return 0;
  const avg = mean(values);
  const variance = values.reduce((sum, value) => sum + (value - avg) ** 2, 0) / (values.length - 1);
  return Math.sqrt(variance);
}

function buildTradeEquity(trades: DryRunClosedTradeRow[]): DryRunEquityPoint[] {
  let cumulative = 0;
  let peak = 0;
  return [...trades]
    .sort((a, b) => new Date(a.closed_at).getTime() - new Date(b.closed_at).getTime())
    .map((trade, index) => {
      const pnl = toNumber(trade.net_pnl);
      cumulative += pnl;
      peak = Math.max(peak, cumulative);
      return {
        index: index + 1,
        label: `${index + 1}`,
        timestamp: trade.closed_at,
        symbol: trade.symbol,
        pnl,
        cumulative,
        drawdown: cumulative - peak,
      };
    });
}

function fallbackEquity(report: DryRunStrategyReport): DryRunEquityPoint[] {
  const trades = report.closed_trades ?? report.recent_closed ?? [];
  if (trades.length > 0) return buildTradeEquity(trades);

  let cumulative = 0;
  let peak = 0;
  return [...(report.daily ?? [])]
    .sort((a, b) => a.trading_day_cst.localeCompare(b.trading_day_cst))
    .map((row, index) => {
      const pnl = toNumber(row.net_pnl);
      cumulative += pnl;
      peak = Math.max(peak, cumulative);
      return {
        index: index + 1,
        label: row.trading_day_cst.slice(5),
        timestamp: row.trading_day_cst,
        pnl,
        cumulative,
        drawdown: cumulative - peak,
      };
    });
}

function maxDrawdown(points: DryRunEquityPoint[]) {
  return points.reduce((worst, point) => Math.min(worst, toNumber(point.drawdown)), 0);
}

function profitFactor(trades: DryRunClosedTradeRow[]) {
  const grossProfit = trades.reduce((sum, trade) => {
    const pnl = toNumber(trade.net_pnl);
    return pnl > 0 ? sum + pnl : sum;
  }, 0);
  const grossLoss = Math.abs(
    trades.reduce((sum, trade) => {
      const pnl = toNumber(trade.net_pnl);
      return pnl < 0 ? sum + pnl : sum;
    }, 0)
  );
  if (grossLoss === 0) return grossProfit > 0 ? Infinity : null;
  return grossProfit / grossLoss;
}

function sharpeRatio(points: DryRunEquityPoint[]) {
  const returns = points.map((point) => toNumber(point.pnl));
  const sigma = stddev(returns);
  if (returns.length < 2 || sigma === 0) return null;
  return (mean(returns) / sigma) * Math.sqrt(returns.length);
}

function strategyKey(strategy: DryRunStrategyReport, index: number) {
  return [
    strategy.runtime_mode ?? 'dry_run',
    strategy.strategy_id ?? 'unknown',
    strategy.deployment_id ?? '',
    index,
  ].join('::');
}

function strategyLabel(strategy: DryRunStrategyReport, index: number) {
  return strategy.label || strategy.deployment_id || strategy.strategy_id || `strategy-${index + 1}`;
}

function strategyIdentity(runtimeMode?: string | null, strategyId?: string | null, deploymentId?: string | null) {
  return [
    runtimeMode || 'dry_run',
    strategyId || 'unknown',
    deploymentId || '',
  ].join('::');
}

function buildViews(report?: DryRunPerformanceReport): StrategyView[] {
  if (!report) return [];
  const views: StrategyView[] = [
    {
      key: 'ALL',
      label: 'ALL DRY-RUN',
      report,
      isAggregate: true,
    },
  ];
  for (const [index, strategy] of (report.strategies ?? []).entries()) {
    views.push({
      key: strategyKey(strategy, index),
      label: strategyLabel(strategy, index),
      report: strategy,
      isAggregate: false,
    });
  }
  return views;
}

function panelTitleLabel(view: StrategyView) {
  return view.isAggregate ? '全量 Dry Run' : view.label;
}

function sampleEquity(points: DryRunEquityPoint[], maxPoints = 700) {
  if (points.length <= maxPoints) return points;
  const sampled: DryRunEquityPoint[] = [];
  const step = (points.length - 1) / (maxPoints - 1);
  for (let index = 0; index < maxPoints; index += 1) {
    sampled.push(points[Math.round(index * step)]);
  }
  return sampled;
}

function strategyColor(index: number) {
  return STRATEGY_COLORS[index % STRATEGY_COLORS.length];
}

function buildStrategyLines(report?: DryRunPerformanceReport): StrategyLine[] {
  return (report?.strategies ?? []).map((strategy, index) => {
    const rawPoints = strategy.equity_curve?.length ? strategy.equity_curve : fallbackEquity(strategy);
    return {
      viewKey: strategyKey(strategy, index),
      dataKey: `strategy_${index}`,
      label: strategyLabel(strategy, index),
      color: strategyColor(index),
      report: strategy,
      points: sampleEquity(rawPoints),
    };
  });
}

function buildCurveRows(lines: StrategyLine[]): CurveRow[] {
  const rows = new Map<number, CurveRow>();
  for (const line of lines) {
    for (const point of line.points) {
      const row = rows.get(point.index) ?? { index: point.index };
      row.timestamp = row.timestamp ?? point.timestamp;
      row.symbol = row.symbol ?? point.symbol;
      row[line.dataKey] = point.cumulative;
      rows.set(point.index, row);
    }
  }
  return [...rows.values()].sort((a, b) => a.index - b.index);
}

function isDryRunRuntime(snapshot: TradingStateSnapshot) {
  const mode = snapshot.runtime_mode.toLowerCase();
  const deploymentId = snapshot.deployment_id.toLowerCase();
  return mode === 'dryrun' || mode === 'dry_run' || mode === 'paper' || deploymentId.includes('dryrun') || deploymentId.includes('dry-run');
}

function runtimeStrategyName(deploymentId: string) {
  return deploymentId.replace(/^pm5d\./, '').replace(/\.dryrun$/, '');
}

function tradeStrategyLine(trade: DryRunClosedTradeRow, lines: StrategyLine[]) {
  const identity = strategyIdentity(trade.runtime_mode, trade.strategy_id, trade.deployment_id);
  return lines.find((line) => line.viewKey.startsWith(`${identity}::`));
}

function tradeStrategyLabel(trade: DryRunClosedTradeRow, line?: StrategyLine) {
  return line?.label || trade.deployment_id || trade.strategy_id || trade.runtime_mode || 'unknown';
}

function Panel({
  title,
  subtitle,
  icon,
  children,
  action,
  className,
}: {
  title: string;
  subtitle?: string;
  icon: ReactNode;
  children: ReactNode;
  action?: ReactNode;
  className?: string;
}) {
  return (
    <section className={cn('border border-[#242833] bg-[#07090d]', className)}>
      <div className="flex items-start justify-between gap-3 border-b border-[#20242d] bg-[#0d1016] px-4 py-3">
        <div className="flex min-w-0 items-start gap-3">
          <div className="mt-0.5 text-[#f6b21a]">{icon}</div>
          <div className="min-w-0">
            <h2 className="text-sm font-semibold text-[#f6b21a]">{title}</h2>
            {subtitle ? <p className="mt-1 text-xs leading-5 text-[#8f98a8]">{subtitle}</p> : null}
          </div>
        </div>
        {action}
      </div>
      <div className="p-4">{children}</div>
    </section>
  );
}

function MetricCard({ metric }: { metric: Metric }) {
  return (
    <div className={cn('border px-4 py-3', borderToneClass(metric.tone))}>
      <div className="text-xs text-[#8f98a8]">{metric.label}</div>
      <div className={cn('mt-2 text-2xl font-semibold tabular-nums', toneClass(metric.tone))}>
        {metric.value}
      </div>
      <div className="mt-2 min-h-[18px] text-xs text-[#8f98a8]">{metric.detail}</div>
    </div>
  );
}

function StatusPill({ ok, children }: { ok: boolean; children: ReactNode }) {
  return (
    <span
      className={cn(
        'inline-flex items-center gap-1 border px-2 py-1 text-xs',
        ok ? 'border-[#0d5138] bg-[#041d15] text-[#00e090]' : 'border-[#5a1919] bg-[#200909] text-[#ff4d4d]'
      )}
    >
      {ok ? <CheckCircle2 className="h-3 w-3" /> : <AlertTriangle className="h-3 w-3" />}
      {children}
    </span>
  );
}

export function OperatorCockpit() {
  const [selectedKey, setSelectedKey] = useState('ALL');
  const [tradeResultFilter, setTradeResultFilter] = useState<TradeResultFilter>('all');

  const {
    data: report,
    error: reportError,
    isLoading: reportLoading,
  } = useQuery<DryRunPerformanceReport>({
    queryKey: ['reports', 'dry-run'],
    queryFn: () => api.getDryRunPerformance(),
    refetchInterval: 30000,
    retry: false,
  });

  const { data: status } = useQuery({
    queryKey: ['system', 'status'],
    queryFn: () => api.getSystemStatus(),
    refetchInterval: 15000,
    retry: false,
  });

  const { data: tradingState } = useQuery<TradingStateSnapshot[]>({
    queryKey: ['trading', 'state'],
    queryFn: () => api.getTradingState(),
    refetchInterval: 30000,
    retry: false,
  });

  const { data: deployments } = useQuery<DeploymentSummary[]>({
    queryKey: ['deployments'],
    queryFn: () => api.getDeployments(),
    refetchInterval: 30000,
    retry: false,
  });

  const views = useMemo(() => buildViews(report), [report]);
  const selectedView = views.find((view) => view.key === selectedKey) ?? views[0];
  const selectedReport = selectedView?.report;
  const summary = selectedReport?.summary;
  const closedTradeTotal = summary?.closed_trades ?? 0;
  const closedTrades = useMemo(
    () => selectedReport?.closed_trades ?? selectedReport?.recent_closed ?? [],
    [selectedReport]
  );
  const equityCurve = useMemo(() => {
    if (selectedReport?.equity_curve?.length) return selectedReport.equity_curve;
    return selectedReport ? fallbackEquity(selectedReport) : [];
  }, [selectedReport]);
  const chartEquity = useMemo(() => sampleEquity(equityCurve), [equityCurve]);
  const strategyLines = useMemo(() => buildStrategyLines(report), [report]);
  const aggregateLine = useMemo<StrategyLine | null>(() => {
    if (!selectedReport) return null;
    return {
      viewKey: 'ALL',
      dataKey: 'aggregate',
      label: panelTitleLabel(selectedView),
      color: toNumber(selectedReport.summary.realized_pnl) >= 0 ? '#00e090' : '#ff4d4d',
      report: selectedReport,
      points: chartEquity,
    };
  }, [chartEquity, selectedReport, selectedView]);
  const visibleLines = useMemo(() => {
    if (selectedView?.isAggregate) {
      return strategyLines.length > 0 ? strategyLines : aggregateLine ? [aggregateLine] : [];
    }
    const selectedStrategyLine = strategyLines.find((line) => line.viewKey === selectedView?.key);
    if (selectedStrategyLine) return [selectedStrategyLine];
    return aggregateLine ? [aggregateLine] : [];
  }, [aggregateLine, selectedView, strategyLines]);
  const curveRows = useMemo(() => buildCurveRows(visibleLines), [visibleLines]);
  const dryRunRuntimeRows = useMemo<RuntimeStrategyRow[]>(() => {
    const deploymentById = new Map((deployments ?? []).map((deployment) => [deployment.deployment_id, deployment]));
    return (tradingState ?? [])
      .filter(isDryRunRuntime)
      .filter((snapshot) => {
        const deployment = deploymentById.get(snapshot.deployment_id);
        if (!deployment) return true;
        return deployment.desired_state === 'running' || deployment.observed_state === 'running';
      })
      .map((snapshot) => {
        const deployment = deploymentById.get(snapshot.deployment_id);
        return {
          deploymentId: snapshot.deployment_id,
          runtimeMode: snapshot.runtime_mode,
          desiredState: deployment?.desired_state,
          observedState: deployment?.observed_state,
          positions: snapshot.positions?.length ?? 0,
          orders: snapshot.orders?.length ?? 0,
          fills: snapshot.fills?.length ?? 0,
        };
      });
  }, [deployments, tradingState]);
  const filteredClosedTrades = useMemo(() => {
    if (tradeResultFilter === 'loss') {
      return closedTrades.filter((trade) => toNumber(trade.net_pnl) < 0);
    }
    if (tradeResultFilter === 'win') {
      return closedTrades.filter((trade) => toNumber(trade.net_pnl) > 0);
    }
    return closedTrades;
  }, [closedTrades, tradeResultFilter]);
  const visibleClosedTrades = useMemo(() => filteredClosedTrades.slice(0, 250), [filteredClosedTrades]);
  const metrics = selectedReport?.metrics;

  const realizedPnl = summary ? toNumber(summary.realized_pnl) : 0;
  const fees = summary ? toNumber(summary.total_fees) : 0;
  const winRate = summary ? toNumber(summary.win_rate_pct) : null;
  const backendPf = metrics?.profit_factor === 'Infinity' ? Infinity : metrics?.profit_factor ?? null;
  const pf = backendPf ?? profitFactor(closedTrades);
  const sharpe = metrics?.sharpe ?? sharpeRatio(equityCurve);
  const drawdown = metrics?.max_drawdown ?? maxDrawdown(equityCurve);
  const avgTrade =
    metrics?.avg_trade ??
    (closedTrades.length > 0
      ? closedTrades.reduce((sum, trade) => sum + toNumber(trade.net_pnl), 0) / closedTrades.length
      : null);

  const topMetrics: Metric[] = [
    {
      label: '累计收益',
      value: formatCompactCurrency(realizedPnl),
      detail: `fees ${formatCompactCurrency(fees)} · open ${summary?.open_positions ?? 0}`,
      tone: pnlTone(realizedPnl),
    },
    {
      label: '胜率',
      value: formatPct(winRate),
      detail: `${summary?.wins ?? 0} win / ${summary?.losses ?? 0} loss`,
      tone: winRate == null ? 'muted' : winRate >= 55 ? 'good' : winRate >= 45 ? 'warn' : 'bad',
    },
    {
      label: 'Sharpe',
      value: sharpe == null ? 'N/A' : sharpe.toFixed(2),
      detail: `${equityCurve.length} equity points`,
      tone: sharpe == null ? 'muted' : sharpe >= 1 ? 'good' : sharpe >= 0 ? 'warn' : 'bad',
    },
    {
      label: 'Profit Factor',
      value: pf == null ? 'N/A' : pf === Infinity ? '∞' : pf.toFixed(2),
      detail: `${formatCompactCurrency(metrics?.gross_profit ?? 0)} / ${formatCompactCurrency(metrics?.gross_loss ?? 0)}`,
      tone: pf == null ? 'muted' : pf >= 1.5 ? 'good' : pf >= 1 ? 'warn' : 'bad',
    },
    {
      label: '最大回撤',
      value: formatCompactCurrency(drawdown),
      detail: `${shortDate(equityCurve[0]?.timestamp)} → ${shortDate(equityCurve[equityCurve.length - 1]?.timestamp)}`,
      tone: drawdown < 0 ? 'bad' : 'muted',
    },
    {
      label: '平均每笔',
      value: avgTrade == null ? 'N/A' : formatCompactCurrency(avgTrade),
      detail: `${closedTradeTotal} closed trades`,
      tone: avgTrade == null ? 'muted' : pnlTone(avgTrade),
    },
  ];

  const windowBars = (selectedReport?.by_window ?? []).map((row) => ({
    name: row.window_label,
    pnl: toNumber(row.realized_pnl),
    trades: row.total_trades,
    winRate: toNumber(row.win_rate_pct),
  }));

  const symbolRows = [...(selectedReport?.symbols ?? [])]
    .sort((a, b) => toNumber(b.net_pnl) - toNumber(a.net_pnl))
    .slice(0, 10);

  if (reportLoading && !report) {
    return (
      <div className="flex min-h-full min-w-[1280px] items-center justify-center bg-black text-[#d8dde6]">
        <div className="flex items-center gap-3 border border-[#242833] bg-[#07090d] px-4 py-3 text-sm">
          <Loader2 className="h-4 w-4 animate-spin text-[#f6b21a]" />
          正在读取 dry-run strategy report...
        </div>
      </div>
    );
  }

  return (
    <div className="min-h-full min-w-[1280px] bg-black p-4 text-[#d8dde6]">
      <div className="mx-auto max-w-[1720px] space-y-3">
        <header className="border border-[#242833] bg-[#07090d]">
          <div className="flex items-center justify-between gap-4 border-b border-[#20242d] px-4 py-3">
            <div className="flex min-w-0 items-center gap-3">
              <div className="text-xl font-semibold tracking-wide text-white">DRY-RUN STRATEGY PERFORMANCE</div>
              <div className="h-6 w-px bg-[#242833]" />
              <div className="truncate text-sm text-[#9aa4b5]">
                {panelTitleLabel(selectedView ?? { key: 'ALL', label: 'ALL DRY-RUN', report: report as DryRunStrategyReport, isAggregate: true })}
              </div>
            </div>
            <div className="flex items-center gap-2">
              <StatusPill ok={Boolean(report)}>report {report ? 'online' : 'missing'}</StatusPill>
              <StatusPill ok={Boolean(status?.database_connected)}>DB {status?.database_connected ? 'online' : 'unknown'}</StatusPill>
              <span className="text-xs text-[#8f98a8]">
                generated {report?.generated_at ? formatTimestamp(report.generated_at) : '-'}
              </span>
            </div>
          </div>

          <div className="grid grid-cols-6 divide-x divide-[#20242d]">
            <div className="px-4 py-3">
              <div className="text-xs text-[#8f98a8]">Report / Runtime</div>
              <div className="mt-1 text-lg font-semibold tabular-nums text-white">
                {report?.strategies?.length ?? 0} / {dryRunRuntimeRows.length}
              </div>
            </div>
            <div className="px-4 py-3">
              <div className="text-xs text-[#8f98a8]">Closed Trades</div>
              <div className="mt-1 text-lg font-semibold tabular-nums text-white">{summary?.closed_trades ?? 0}</div>
            </div>
            <div className="px-4 py-3">
              <div className="text-xs text-[#8f98a8]">Total Trades</div>
              <div className="mt-1 text-lg font-semibold tabular-nums text-white">{summary?.total_trades ?? 0}</div>
            </div>
            <div className="px-4 py-3">
              <div className="text-xs text-[#8f98a8]">Open Exposure</div>
              <div className="mt-1 text-lg font-semibold tabular-nums text-white">
                {formatCompactCurrency(summary ? toNumber(summary.open_exposure) : 0)}
              </div>
            </div>
            <div className="px-4 py-3">
              <div className="text-xs text-[#8f98a8]">Latest Close</div>
              <div className="mt-1 text-lg font-semibold tabular-nums text-white">
                {shortDateTime(summary?.latest_closed_at)}
              </div>
            </div>
            <div className="px-4 py-3">
              <div className="text-xs text-[#8f98a8]">API</div>
              <div className="mt-1 font-mono text-sm text-[#f6b21a]">/api/reports/dry-run</div>
            </div>
          </div>
        </header>

        {!report ? (
          <Panel
            title="Dry-run report 未接入"
            subtitle="无法展示曲线、成交明细、胜率、Sharpe、总体获利。"
            icon={<AlertTriangle className="h-4 w-4" />}
          >
            <div className="border border-[#5a1919] bg-[#200909] p-4 text-sm leading-6 text-[#ffb0b0]">
              `/api/reports/dry-run` 错误：
              <span className="ml-2 font-mono">{reportError instanceof Error ? reportError.message : 'unknown'}</span>
            </div>
          </Panel>
        ) : null}

        <Panel
          title="策略拆分"
          subtitle="ALL 是全量 dry-run；下方每一行是一个 runtime_mode / strategy_id / deployment_id。"
          icon={<Database className="h-4 w-4" />}
          action={<Badge variant="outline">{views.length} views</Badge>}
        >
          <div className="overflow-hidden border border-[#20242d]">
            <table className="w-full border-collapse text-sm">
              <thead>
                <tr className="border-b border-[#20242d] bg-[#0d1016] text-left text-xs text-[#8f98a8]">
                  <th className="px-3 py-2 font-medium">View</th>
                  <th className="px-3 py-2 font-medium">Runtime</th>
                  <th className="px-3 py-2 font-medium">Strategy</th>
                  <th className="px-3 py-2 text-right font-medium">Trades</th>
                  <th className="px-3 py-2 text-right font-medium">PnL</th>
                  <th className="px-3 py-2 text-right font-medium">Win</th>
                  <th className="px-3 py-2 text-right font-medium">Sharpe</th>
                  <th className="px-3 py-2 text-right font-medium">Max DD</th>
                  <th className="px-3 py-2 text-right font-medium">Curve</th>
                  <th className="px-3 py-2 text-right font-medium">Latest</th>
                </tr>
              </thead>
              <tbody>
                {views.map((view) => {
                  const rowSummary = view.report.summary;
                  const rowMetrics = view.report.metrics;
                  const rowPnl = toNumber(rowSummary.realized_pnl);
                  const rowSharpe = rowMetrics?.sharpe;
                  const active = view.key === selectedView?.key;
                  const lineColor = strategyLines.find((line) => line.viewKey === view.key)?.color;
                  return (
                    <tr
                      key={view.key}
                      className={cn(
                        'cursor-pointer border-b border-[#161a22] hover:bg-[#10151d]',
                        active && 'bg-[#141108]'
                      )}
                      onClick={() => setSelectedKey(view.key)}
                    >
                      <td className="px-3 py-2">
                        <button
                          type="button"
                          className={cn(
                            'border px-2 py-1 text-xs font-semibold',
                            active
                              ? 'border-[#f6b21a] bg-[#221807] text-[#f6b21a]'
                              : 'border-[#303643] bg-[#0b0d10] text-[#d8dde6]'
                          )}
                          onClick={() => setSelectedKey(view.key)}
                        >
                          {lineColor ? (
                            <span className="mr-2 inline-flex h-2 w-4" style={{ backgroundColor: lineColor }} />
                          ) : null}
                          {view.label}
                        </button>
                      </td>
                      <td className="px-3 py-2 font-mono text-xs text-[#8f98a8]">
                        {view.isAggregate ? 'all' : view.report.runtime_mode || '-'}
                      </td>
                      <td className="px-3 py-2 text-white">
                        {view.isAggregate ? 'all strategies' : view.report.strategy_id || '-'}
                      </td>
                      <td className="px-3 py-2 text-right tabular-nums">{rowSummary.closed_trades}</td>
                      <td className={cn('px-3 py-2 text-right font-semibold tabular-nums', rowPnl >= 0 ? 'text-[#00e090]' : 'text-[#ff4d4d]')}>
                        {formatCompactCurrency(rowPnl)}
                      </td>
                      <td className="px-3 py-2 text-right tabular-nums">{formatPct(rowSummary.win_rate_pct)}</td>
                      <td className="px-3 py-2 text-right tabular-nums">{rowSharpe == null ? 'N/A' : rowSharpe.toFixed(2)}</td>
                      <td className="px-3 py-2 text-right tabular-nums text-[#ff8f8f]">
                        {formatCompactCurrency(rowMetrics?.max_drawdown ?? 0)}
                      </td>
                      <td className="px-3 py-2 text-right tabular-nums">{rowMetrics?.equity_points ?? view.report.equity_curve?.length ?? 0}</td>
                      <td className="px-3 py-2 text-right font-mono text-xs text-[#8f98a8]">
                        {shortDateTime(rowSummary.latest_closed_at)}
                      </td>
                    </tr>
                  );
                })}
                {dryRunRuntimeRows.length > 0 ? (
                  <tr className="border-y border-[#20242d] bg-[#0d1016]">
                    <td colSpan={10} className="px-3 py-2 text-xs font-semibold text-[#f6b21a]">
                      运行中的 dry-run / paper deployment
                      <span className="ml-2 text-[#8f98a8]">
                        这些来自 `/api/trading/state`；如果 Trades/PnL 为空，说明成交表还没有按 deployment_id 归因。
                      </span>
                    </td>
                  </tr>
                ) : null}
                {dryRunRuntimeRows.map((row, index) => {
                  const lineColor = strategyColor(index + strategyLines.length);
                  return (
                    <tr key={row.deploymentId} className="border-b border-[#161a22] bg-[#090b0f]">
                      <td className="px-3 py-2">
                        <div className="inline-flex items-center gap-2 border border-[#303643] bg-[#0b0d10] px-2 py-1 text-xs font-semibold text-[#d8dde6]">
                          <span className="h-2 w-4" style={{ backgroundColor: lineColor }} />
                          DEPLOYMENT
                        </div>
                      </td>
                      <td className="px-3 py-2 font-mono text-xs text-[#8f98a8]">{row.runtimeMode}</td>
                      <td className="px-3 py-2">
                        <div className="font-semibold text-white">{runtimeStrategyName(row.deploymentId)}</div>
                        <div className="mt-1 font-mono text-xs text-[#8f98a8]">{row.deploymentId}</div>
                      </td>
                      <td className="px-3 py-2 text-right text-xs text-[#f5b93f]">未归因</td>
                      <td className="px-3 py-2 text-right text-xs text-[#8f98a8]">no report row</td>
                      <td className="px-3 py-2 text-right text-[#8f98a8]">-</td>
                      <td className="px-3 py-2 text-right text-[#8f98a8]">-</td>
                      <td className="px-3 py-2 text-right text-[#8f98a8]">-</td>
                      <td className="px-3 py-2 text-right text-xs text-[#8f98a8]">
                        pos {row.positions} · orders {row.orders} · fills {row.fills}
                      </td>
                      <td className="px-3 py-2 text-right text-xs text-[#8f98a8]">
                        {row.desiredState ?? '-'} / {row.observedState ?? '-'}
                      </td>
                    </tr>
                  );
                })}
              </tbody>
            </table>
          </div>
        </Panel>

        <section className="grid grid-cols-6 gap-3">
          {topMetrics.map((metric) => (
            <MetricCard key={metric.label} metric={metric} />
          ))}
        </section>

        <section className="grid grid-cols-[minmax(0,1.55fr)_420px] gap-3">
          <Panel
            title={selectedView?.isAggregate ? '多策略 Equity Curve' : '单策略 Equity Curve'}
            subtitle={
              selectedView?.isAggregate
                ? `ALL 模式每条线代表一个 strategy_id；当前显示 ${visibleLines.length} 条策略线。`
                : `${panelTitleLabel(selectedView)} · ${equityCurve.length} 个全量平仓点 · 图上采样 ${chartEquity.length} 点`
            }
            icon={<LineChartIcon className="h-4 w-4" />}
            action={<Badge variant="outline">{closedTradeTotal} trades</Badge>}
            className="min-h-[520px]"
          >
            <div className="mb-3 grid grid-cols-1 gap-2 xl:grid-cols-2">
              {visibleLines.map((line) => {
                const linePnl = toNumber(line.report.summary.realized_pnl);
                const active = line.viewKey === selectedView?.key;
                return (
                  <button
                    key={line.viewKey}
                    type="button"
                    className={cn(
                      'flex items-center justify-between gap-3 border bg-[#0b0d10] px-3 py-2 text-left',
                      active ? 'border-[#f6b21a]' : 'border-[#20242d]'
                    )}
                    onClick={() => setSelectedKey(line.viewKey)}
                  >
                    <span className="flex min-w-0 items-center gap-2">
                      <span className="h-2 w-8 shrink-0" style={{ backgroundColor: line.color }} />
                      <span className="truncate font-mono text-xs text-white">{line.label}</span>
                    </span>
                    <span className="flex items-center gap-4 text-xs">
                      <span className={cn('font-semibold tabular-nums', linePnl >= 0 ? 'text-[#00e090]' : 'text-[#ff4d4d]')}>
                        {formatCompactCurrency(linePnl)}
                      </span>
                      <span className="tabular-nums text-[#8f98a8]">{line.report.metrics?.equity_points ?? line.points.length} pts</span>
                    </span>
                  </button>
                );
              })}
            </div>

            {curveRows.length === 0 ? (
              <div className="flex h-[420px] items-center justify-center border border-dashed border-[#303643] text-sm text-[#8f98a8]">
                暂无 equity_curve / closed_trades。
              </div>
            ) : (
              <ResponsiveContainer width="100%" height={420}>
                <LineChart data={curveRows} margin={{ top: 10, right: 20, left: 0, bottom: 0 }}>
                  <CartesianGrid stroke="#1f2430" strokeDasharray="3 3" />
                  <XAxis
                    dataKey="index"
                    stroke="#6f7785"
                    tick={{ fill: '#8f98a8', fontSize: 11 }}
                    minTickGap={42}
                    tickFormatter={(value) => `#${value}`}
                  />
                  <YAxis
                    stroke="#6f7785"
                    tick={{ fill: '#8f98a8', fontSize: 11 }}
                    tickFormatter={(value) => formatCompactCurrency(Number(value))}
                  />
                  <Tooltip
                    cursor={{ stroke: '#f6b21a', strokeWidth: 1 }}
                    contentStyle={{ background: '#07090d', border: '1px solid #303643', color: '#d8dde6' }}
                    formatter={(value: number, name) => [formatCurrency(value), name]}
                    labelFormatter={(_, payload) => {
                      const point = payload?.[0]?.payload as CurveRow | undefined;
                      return point?.timestamp ? `#${point.index} · ${shortDateTime(point.timestamp)} · ${point.symbol ?? ''}` : '';
                    }}
                  />
                  {visibleLines.map((line) => (
                    <Line
                      key={line.dataKey}
                      type="monotone"
                      dataKey={line.dataKey}
                      name={line.label}
                      stroke={line.color}
                      strokeWidth={2}
                      dot={false}
                      isAnimationActive={false}
                      connectNulls
                    />
                  ))}
                </LineChart>
              </ResponsiveContainer>
            )}
          </Panel>

          <Panel title="策略报告" subtitle={panelTitleLabel(selectedView)} icon={<Sigma className="h-4 w-4" />} className="min-h-[520px]">
            <div className="grid grid-cols-2 gap-2">
              <div className="border border-[#20242d] bg-[#0b0d10] p-3">
                <div className="flex items-center gap-2 text-xs text-[#8f98a8]">
                  <Percent className="h-3 w-3" />
                  Win Rate
                </div>
                <div className="mt-2 text-xl font-semibold text-white">{formatPct(winRate)}</div>
              </div>
              <div className="border border-[#20242d] bg-[#0b0d10] p-3">
                <div className="flex items-center gap-2 text-xs text-[#8f98a8]">
                  <TrendingUp className="h-3 w-3" />
                  Sharpe
                </div>
                <div className="mt-2 text-xl font-semibold text-white">{sharpe == null ? 'N/A' : sharpe.toFixed(2)}</div>
              </div>
              <div className="border border-[#20242d] bg-[#0b0d10] p-3">
                <div className="flex items-center gap-2 text-xs text-[#8f98a8]">
                  <Target className="h-3 w-3" />
                  Profit Factor
                </div>
                <div className="mt-2 text-xl font-semibold text-white">{pf == null ? 'N/A' : pf === Infinity ? '∞' : pf.toFixed(2)}</div>
              </div>
              <div className="border border-[#20242d] bg-[#0b0d10] p-3">
                <div className="flex items-center gap-2 text-xs text-[#8f98a8]">
                  <TrendingDown className="h-3 w-3" />
                  Max DD
                </div>
                <div className="mt-2 text-xl font-semibold text-[#ff8f8f]">{formatCompactCurrency(drawdown)}</div>
              </div>
            </div>

            <div className="mt-3 h-[248px] border border-[#20242d] bg-[#0b0d10] p-3">
              {windowBars.length === 0 ? (
                <div className="flex h-full items-center justify-center text-sm text-[#8f98a8]">暂无 window report</div>
              ) : (
                <ResponsiveContainer width="100%" height="100%">
                  <BarChart data={windowBars}>
                    <CartesianGrid stroke="#1f2430" strokeDasharray="3 3" />
                    <XAxis dataKey="name" stroke="#6f7785" tick={{ fill: '#8f98a8', fontSize: 11 }} />
                    <YAxis stroke="#6f7785" tick={{ fill: '#8f98a8', fontSize: 11 }} />
                    <Tooltip
                      contentStyle={{ background: '#07090d', border: '1px solid #303643', color: '#d8dde6' }}
                      formatter={(value: number, name) => [
                        name === 'pnl' ? formatCurrency(value) : value,
                        name === 'pnl' ? 'PnL' : name,
                      ]}
                    />
                    <Bar dataKey="pnl">
                      {windowBars.map((bar) => (
                        <Cell key={bar.name} fill={bar.pnl >= 0 ? '#00b878' : '#ff4d4d'} />
                      ))}
                    </Bar>
                  </BarChart>
                </ResponsiveContainer>
              )}
            </div>

            <div className="mt-3 grid grid-cols-2 gap-2 text-xs">
              <div className="border border-[#20242d] bg-[#0b0d10] p-3">
                <div className="text-[#8f98a8]">Gross Profit</div>
                <div className="mt-1 font-semibold text-[#00e090]">{formatCompactCurrency(metrics?.gross_profit ?? 0)}</div>
              </div>
              <div className="border border-[#20242d] bg-[#0b0d10] p-3">
                <div className="text-[#8f98a8]">Gross Loss</div>
                <div className="mt-1 font-semibold text-[#ff4d4d]">{formatCompactCurrency(metrics?.gross_loss ?? 0)}</div>
              </div>
            </div>
          </Panel>
        </section>

        <section className="grid grid-cols-[minmax(0,1.4fr)_420px] gap-3">
          <Panel
            title="成交明细"
            subtitle={`${panelTitleLabel(selectedView)} · 显示 ${visibleClosedTrades.length} / ${filteredClosedTrades.length} 条筛选结果，统计和曲线使用全量 ${closedTradeTotal} 条`}
            icon={<ListChecks className="h-4 w-4" />}
            action={<Badge variant="outline">{closedTradeTotal} rows</Badge>}
          >
            <div className="mb-3 flex items-center gap-2">
              {[
                { key: 'all' as const, label: '全部成交', count: closedTrades.length },
                {
                  key: 'loss' as const,
                  label: '只看亏损',
                  count: closedTrades.filter((trade) => toNumber(trade.net_pnl) < 0).length,
                },
                {
                  key: 'win' as const,
                  label: '只看盈利',
                  count: closedTrades.filter((trade) => toNumber(trade.net_pnl) > 0).length,
                },
              ].map((item) => (
                <button
                  key={item.key}
                  type="button"
                  className={cn(
                    'border px-3 py-1.5 text-xs font-semibold',
                    tradeResultFilter === item.key
                      ? 'border-[#f6b21a] bg-[#221807] text-[#f6b21a]'
                      : 'border-[#303643] bg-[#0b0d10] text-[#8f98a8] hover:bg-[#10151d]'
                  )}
                  onClick={() => setTradeResultFilter(item.key)}
                >
                  {item.label}
                  <span className="ml-2 font-mono tabular-nums">{item.count}</span>
                </button>
              ))}
            </div>
            <div className="max-h-[560px] overflow-auto border border-[#20242d]">
              <table className="min-w-[1240px] w-full border-collapse text-sm">
                <thead className="sticky top-0 z-10">
                  <tr className="border-b border-[#20242d] bg-[#0d1016] text-left text-xs text-[#8f98a8]">
                    <th className="px-3 py-2 font-medium">时间</th>
                    <th className="px-3 py-2 font-medium">下单策略</th>
                    <th className="px-3 py-2 font-medium">Symbol</th>
                    <th className="px-3 py-2 font-medium">Window</th>
                    <th className="px-3 py-2 font-medium">Side</th>
                    <th className="px-3 py-2 text-right font-medium">Entry</th>
                    <th className="px-3 py-2 text-right font-medium">Exit</th>
                    <th className="px-3 py-2 text-right font-medium">Qty</th>
                    <th className="px-3 py-2 text-right font-medium">Notional</th>
                    <th className="px-3 py-2 font-medium">Exit</th>
                    <th className="px-3 py-2 text-right font-medium">结果 / PnL</th>
                  </tr>
                </thead>
                <tbody>
                  {visibleClosedTrades.length === 0 ? (
                    <tr>
                      <td colSpan={11} className="px-3 py-10 text-center text-[#8f98a8]">
                        暂无 closed_trades。
                      </td>
                    </tr>
                  ) : (
                    visibleClosedTrades.map((trade, index) => {
                      const pnl = toNumber(trade.net_pnl);
                      const line = tradeStrategyLine(trade, strategyLines);
                      const label = tradeStrategyLabel(trade, line);
                      const color = line?.color ?? '#8f98a8';
                      return (
                        <tr key={`${trade.closed_at}-${trade.trade_key ?? trade.symbol}-${index}`} className="border-b border-[#161a22]">
                          <td className="px-3 py-2 font-mono text-xs text-[#8f98a8]">{timeOnly(trade.closed_at)}</td>
                          <td className="px-3 py-2">
                            <div className="flex min-w-0 items-center gap-2">
                              <span className="h-2 w-8 shrink-0" style={{ backgroundColor: color }} />
                              <div className="min-w-0">
                                <div className="truncate font-semibold text-white">{label}</div>
                                <div className="mt-0.5 font-mono text-xs text-[#8f98a8]">
                                  {trade.runtime_mode || '-'} · {trade.deployment_id || trade.strategy_id || '-'}
                                </div>
                              </div>
                            </div>
                          </td>
                          <td className="px-3 py-2 text-white">{trade.symbol}</td>
                          <td className="px-3 py-2 text-[#8f98a8]">{trade.window_label ?? '-'}</td>
                          <td className="px-3 py-2">{trade.market_side}</td>
                          <td className="px-3 py-2 text-right tabular-nums">{formatPrice(trade.entry_price)}</td>
                          <td className="px-3 py-2 text-right tabular-nums">{formatPrice(trade.exit_price)}</td>
                          <td className="px-3 py-2 text-right tabular-nums">{formatNumber(toNumber(trade.quantity))}</td>
                          <td className="px-3 py-2 text-right tabular-nums">{formatCompactCurrency(toNumber(trade.notional))}</td>
                          <td className="px-3 py-2 text-[#8f98a8]">{trade.exit_type}</td>
                          <td className="px-3 py-2 text-right">
                            <div className={cn('font-semibold tabular-nums', pnl >= 0 ? 'text-[#00e090]' : 'text-[#ff4d4d]')}>
                              {formatCompactCurrency(pnl)}
                            </div>
                            <div className={cn('mt-0.5 text-xs font-semibold', pnl >= 0 ? 'text-[#00e090]' : 'text-[#ff8f8f]')}>
                              {pnl >= 0 ? '盈利' : '亏损'} · {label}
                            </div>
                          </td>
                        </tr>
                      );
                    })
                  )}
                </tbody>
              </table>
            </div>
          </Panel>

          <div className="space-y-3">
            <Panel title="按标的表现" subtitle={panelTitleLabel(selectedView)} icon={<BarChart3 className="h-4 w-4" />}>
              <div className="space-y-2">
                {symbolRows.length === 0 ? (
                  <div className="border border-dashed border-[#303643] p-4 text-sm text-[#8f98a8]">暂无 symbols report。</div>
                ) : (
                  symbolRows.map((row) => {
                    const pnl = toNumber(row.net_pnl);
                    const trades = row.trades || 1;
                    const winRateForSymbol = (row.wins / trades) * 100;
                    return (
                      <div key={`${row.symbol}-${row.window_label ?? 'all'}`} className="border border-[#20242d] bg-[#0b0d10] p-3">
                        <div className="flex items-center justify-between gap-3">
                          <div>
                            <div className="font-semibold text-white">{row.symbol}</div>
                            <div className="mt-1 text-xs text-[#8f98a8]">
                              {row.trades} trades · win {formatPct(winRateForSymbol)}
                            </div>
                          </div>
                          <div className={cn('font-semibold tabular-nums', pnl >= 0 ? 'text-[#00e090]' : 'text-[#ff4d4d]')}>
                            {formatCompactCurrency(pnl)}
                          </div>
                        </div>
                      </div>
                    );
                  })
                )}
              </div>
            </Panel>

            <Panel title="当前未平仓" subtitle={panelTitleLabel(selectedView)} icon={<Clock3 className="h-4 w-4" />}>
              <div className="space-y-2">
                {(selectedReport?.open_positions ?? []).length === 0 ? (
                  <div className="border border-dashed border-[#303643] p-4 text-sm text-[#8f98a8]">当前没有未平仓记录。</div>
                ) : (
                  selectedReport?.open_positions.slice(0, 10).map((position, index) => (
                    <div key={`${position.opened_at}-${position.trade_key ?? position.symbol}-${index}`} className="border border-[#20242d] bg-[#0b0d10] p-3">
                      <div className="flex items-center justify-between gap-3">
                        <div>
                          <div className="font-semibold text-white">{position.symbol}</div>
                          <div className="mt-1 text-xs text-[#8f98a8]">
                            {position.strategy_id || '-'} · {position.market_side} · {shortDateTime(position.opened_at)}
                          </div>
                        </div>
                        <div className="text-right">
                          <div className="font-semibold text-white">{formatCompactCurrency(toNumber(position.notional))}</div>
                          <div className="mt-1 text-xs text-[#8f98a8]">entry {formatPrice(position.entry_price)}</div>
                        </div>
                      </div>
                    </div>
                  ))
                )}
              </div>
            </Panel>
          </div>
        </section>
      </div>
    </div>
  );
}
