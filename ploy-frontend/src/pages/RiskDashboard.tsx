import { useQuery } from '@tanstack/react-query';
import { api } from '@/services/api';
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/Card';
import { Badge } from '@/components/ui/Badge';
import { cn, formatCurrency } from '@/lib/utils';
import {
  ShieldAlert,
  ShieldCheck,
  ShieldOff,
  AlertTriangle,
  Clock,
  Loader2,
  Inbox,
} from 'lucide-react';

export function RiskDashboard() {
  const { data: risk, isLoading, error } = useQuery({
    queryKey: ['risk', 'data'],
    queryFn: () => api.getRiskData(),
    refetchInterval: 30000,
  });

  if (isLoading) {
    return (
      <div className="flex h-full items-center justify-center">
        <Loader2 className="h-8 w-8 animate-spin text-muted-foreground" />
      </div>
    );
  }

  if (error) {
    return (
      <div className="flex h-full items-center justify-center">
        <div className="text-center">
          <ShieldOff className="mx-auto h-12 w-12 text-destructive/50 mb-4" />
          <p className="text-destructive">Failed to load risk data</p>
          <p className="text-sm text-muted-foreground mt-1">{String(error)}</p>
        </div>
      </div>
    );
  }

  const riskState = risk?.risk_state ?? 'Normal';

  const riskConfig = {
    Normal: {
      color: 'bg-green-500',
      textColor: 'text-green-700',
      bgColor: 'bg-green-50 border-green-200',
      icon: ShieldCheck,
      label: 'NORMAL',
    },
    Elevated: {
      color: 'bg-yellow-500',
      textColor: 'text-yellow-700',
      bgColor: 'bg-yellow-50 border-yellow-200',
      icon: AlertTriangle,
      label: 'ELEVATED',
    },
    Halted: {
      color: 'bg-red-500',
      textColor: 'text-red-700',
      bgColor: 'bg-red-50 border-red-200',
      icon: ShieldOff,
      label: 'HALTED',
    },
  };

  const config = riskConfig[riskState];
  const RiskIcon = config.icon;

  const dailyPnl = risk?.daily_pnl_usd ?? 0;
  const dailyLimit = risk?.daily_loss_limit_usd ?? 100;
  const lossRatio = dailyLimit > 0 ? Math.abs(Math.min(0, dailyPnl)) / dailyLimit : 0;
  const progressPct = Math.min(lossRatio * 100, 100);
  const progressColor =
    progressPct > 80 ? 'bg-red-500' : progressPct > 50 ? 'bg-yellow-500' : 'bg-green-500';

  return (
    <div className="p-8">
      <div className="mb-8">
        <h1 className="text-3xl font-bold">Risk Monitor</h1>
        <p className="text-muted-foreground">Real-time risk state and position exposure</p>
      </div>

      {/* Risk State Badge */}
      <div className={cn('mb-8 flex items-center gap-4 rounded-lg border p-6', config.bgColor)}>
        <div className={cn('flex h-16 w-16 items-center justify-center rounded-full', config.color)}>
          <RiskIcon className="h-8 w-8 text-white" />
        </div>
        <div>
          <div className={cn('text-3xl font-bold', config.textColor)}>{config.label}</div>
          <div className="text-sm text-muted-foreground mt-1">
            Current risk state across all domains
          </div>
        </div>
      </div>

      <div className="grid grid-cols-1 gap-8 lg:grid-cols-2">
        {/* Daily P&L Progress */}
        <Card>
          <CardHeader>
            <CardTitle>Daily Loss Tracker</CardTitle>
          </CardHeader>
          <CardContent>
            <div className="space-y-4">
              <div className="flex items-center justify-between">
                <span className="text-sm text-muted-foreground">Daily P&L</span>
                <span
                  className={cn('text-lg font-bold', {
                    'text-success': dailyPnl >= 0,
                    'text-destructive': dailyPnl < 0,
                  })}
                >
                  {formatCurrency(dailyPnl)}
                </span>
              </div>

              <div>
                <div className="flex items-center justify-between text-sm mb-2">
                  <span className="text-muted-foreground">
                    Loss Usage: {formatCurrency(Math.abs(Math.min(0, dailyPnl)))} / {formatCurrency(dailyLimit)}
                  </span>
                  <span className="font-medium">{progressPct.toFixed(1)}%</span>
                </div>
                <div className="h-3 w-full rounded-full bg-muted">
                  <div
                    className={cn('h-full rounded-full transition-all duration-500', progressColor)}
                    style={{ width: `${progressPct}%` }}
                  />
                </div>
              </div>

              <div className="flex items-center justify-between rounded-lg bg-muted p-3">
                <span className="text-sm text-muted-foreground">Queue Depth</span>
                <Badge variant={risk?.queue_depth ? 'warning' : 'secondary'}>
                  {risk?.queue_depth ?? 0} pending
                </Badge>
              </div>
            </div>
          </CardContent>
        </Card>

        {/* Circuit Breaker History */}
        <Card>
          <CardHeader>
            <CardTitle className="flex items-center gap-2">
              <ShieldAlert className="h-5 w-5" />
              Circuit Breaker Events
            </CardTitle>
          </CardHeader>
          <CardContent>
            {!risk?.circuit_breaker_events || risk.circuit_breaker_events.length === 0 ? (
              <div className="py-8 text-center">
                <ShieldCheck className="mx-auto h-10 w-10 text-success/50 mb-2" />
                <p className="text-sm text-muted-foreground">No circuit breaker events</p>
              </div>
            ) : (
              <div className="space-y-3 max-h-80 overflow-y-auto">
                {risk.circuit_breaker_events.slice(0, 10).map((event, idx) => (
                  <div key={idx} className="flex items-start gap-3 rounded-lg border p-3">
                    <AlertTriangle className="mt-0.5 h-4 w-4 flex-shrink-0 text-warning" />
                    <div className="flex-1 min-w-0">
                      <div className="flex items-center gap-2">
                        <Badge variant="outline" className="text-xs">
                          {event.state}
                        </Badge>
                        <span className="text-xs text-muted-foreground flex items-center gap-1">
                          <Clock className="h-3 w-3" />
                          {new Date(event.timestamp).toLocaleString('en-US', {
                            month: 'short',
                            day: 'numeric',
                            hour: '2-digit',
                            minute: '2-digit',
                          })}
                        </span>
                      </div>
                      <p className="text-sm mt-1 truncate">{event.reason}</p>
                    </div>
                  </div>
                ))}
              </div>
            )}
          </CardContent>
        </Card>
      </div>

      {/* Position Exposure Table */}
      <Card className="mt-8">
        <CardHeader>
          <CardTitle>Position Exposure</CardTitle>
        </CardHeader>
        <CardContent>
          {!risk?.positions || risk.positions.length === 0 ? (
            <div className="py-8 text-center">
              <Inbox className="mx-auto h-10 w-10 text-muted-foreground/50 mb-2" />
              <p className="text-sm text-muted-foreground">No open positions</p>
            </div>
          ) : (
            <div className="overflow-x-auto">
              <table className="w-full">
                <thead>
                  <tr className="border-b text-left text-sm text-muted-foreground">
                    <th className="pb-3 font-medium">Market</th>
                    <th className="pb-3 font-medium">Side</th>
                    <th className="pb-3 font-medium text-right">Size</th>
                    <th className="pb-3 font-medium text-right">P&L</th>
                  </tr>
                </thead>
                <tbody>
                  {risk.positions.map((pos, idx) => (
                    <tr key={idx} className="border-b last:border-0">
                      <td className="py-3 font-medium">{pos.market}</td>
                      <td className="py-3">
                        <Badge variant={pos.side === 'Yes' ? 'success' : 'destructive'}>
                          {pos.side}
                        </Badge>
                      </td>
                      <td className="py-3 text-right font-medium">
                        {formatCurrency(pos.size)}
                      </td>
                      <td
                        className={cn('py-3 text-right font-medium', {
                          'text-success': pos.pnl_usd >= 0,
                          'text-destructive': pos.pnl_usd < 0,
                        })}
                      >
                        {pos.pnl_usd >= 0 ? '+' : ''}
                        {formatCurrency(pos.pnl_usd)}
                      </td>
                    </tr>
                  ))}
                </tbody>
                <tfoot>
                  <tr className="border-t font-bold">
                    <td className="pt-3" colSpan={2}>Total Exposure</td>
                    <td className="pt-3 text-right">
                      {formatCurrency(
                        risk.positions.reduce((sum, p) => sum + p.size, 0)
                      )}
                    </td>
                    <td
                      className={cn('pt-3 text-right', {
                        'text-success': risk.positions.reduce((s, p) => s + p.pnl_usd, 0) >= 0,
                        'text-destructive': risk.positions.reduce((s, p) => s + p.pnl_usd, 0) < 0,
                      })}
                    >
                      {risk.positions.reduce((s, p) => s + p.pnl_usd, 0) >= 0 ? '+' : ''}
                      {formatCurrency(
                        risk.positions.reduce((sum, p) => sum + p.pnl_usd, 0)
                      )}
                    </td>
                  </tr>
                </tfoot>
              </table>
            </div>
          )}
        </CardContent>
      </Card>
    </div>
  );
}
