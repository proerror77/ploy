import { useQuery, useMutation, useQueryClient } from '@tanstack/react-query';
import { api } from '@/services/api';
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/Card';
import { Button } from '@/components/ui/Button';
import { Badge } from '@/components/ui/Badge';
import { formatCurrency } from '@/lib/utils';
import { Activity, DollarSign, Target, Pause, Play, Loader2 } from 'lucide-react';

export function StrategyMonitor() {
  const queryClient = useQueryClient();

  const { data: strategies, isLoading, error } = useQuery({
    queryKey: ['strategies', 'running'],
    queryFn: () => api.getRunningStrategies(),
    refetchInterval: 10000,
  });

  const pauseMutation = useMutation({
    mutationFn: () => api.pauseSystem(),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['strategies', 'running'] });
    },
  });

  const resumeMutation = useMutation({
    mutationFn: () => api.resumeSystem(),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['strategies', 'running'] });
    },
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
          <p className="text-destructive">Failed to load strategies</p>
          <p className="text-sm text-muted-foreground mt-1">{String(error)}</p>
        </div>
      </div>
    );
  }

  const getStatusVariant = (status: string) => {
    switch (status) {
      case 'running': return 'success' as const;
      case 'paused': return 'warning' as const;
      case 'error': return 'destructive' as const;
      default: return 'secondary' as const;
    }
  };

  const getDomainColor = (domain: string) => {
    switch (domain) {
      case 'crypto': return 'bg-blue-500/10 text-blue-700 border-blue-200';
      case 'sports': return 'bg-green-500/10 text-green-700 border-green-200';
      case 'politics': return 'bg-purple-500/10 text-purple-700 border-purple-200';
      default: return '';
    }
  };

  return (
    <div className="p-8">
      <div className="mb-8 flex items-center justify-between">
        <div>
          <h1 className="text-3xl font-bold">Strategy Monitor</h1>
          <p className="text-muted-foreground">Monitor running strategies and real-time status</p>
        </div>
        <div className="flex gap-2">
          <Button
            variant="outline"
            size="sm"
            onClick={() => pauseMutation.mutate()}
            disabled={pauseMutation.isPending}
          >
            <Pause className="mr-2 h-4 w-4" />
            Pause All
          </Button>
          <Button
            size="sm"
            onClick={() => resumeMutation.mutate()}
            disabled={resumeMutation.isPending}
          >
            <Play className="mr-2 h-4 w-4" />
            Resume All
          </Button>
        </div>
      </div>

      {/* Strategy Cards */}
      <div className="space-y-4">
        {!strategies || strategies.length === 0 ? (
          <Card>
            <CardContent className="py-12 text-center">
              <Activity className="mx-auto h-12 w-12 text-muted-foreground/50 mb-4" />
              <p className="text-lg font-medium text-muted-foreground">No active strategies</p>
              <p className="text-sm text-muted-foreground mt-1">
                Start a strategy from the System Control page
              </p>
            </CardContent>
          </Card>
        ) : (
          strategies.map((strategy, idx) => (
            <Card key={`${strategy.name}-${idx}`} className="border-l-4 border-l-primary">
              <CardHeader>
                <div className="flex items-center justify-between">
                  <div className="flex items-center gap-3">
                    <Activity className="h-6 w-6 text-primary" />
                    <div>
                      <CardTitle>{strategy.name}</CardTitle>
                      <div className="flex items-center gap-2 mt-1">
                        <span className={`inline-flex items-center rounded-md border px-2 py-0.5 text-xs font-medium ${getDomainColor(strategy.domain)}`}>
                          {strategy.domain}
                        </span>
                      </div>
                    </div>
                  </div>
                  <Badge variant={getStatusVariant(strategy.status)}>
                    {strategy.status}
                  </Badge>
                </div>
              </CardHeader>
              <CardContent>
                <div className="grid grid-cols-2 md:grid-cols-4 gap-4">
                  <div className="rounded-lg border bg-card p-4">
                    <div className="flex items-center gap-2 text-muted-foreground">
                      <DollarSign className="h-4 w-4" />
                      <span className="text-sm">P&L</span>
                    </div>
                    <div
                      className={`mt-2 text-2xl font-bold ${
                        strategy.pnl_usd >= 0 ? 'text-success' : 'text-destructive'
                      }`}
                    >
                      {formatCurrency(strategy.pnl_usd)}
                    </div>
                  </div>

                  <div className="rounded-lg border bg-card p-4">
                    <div className="flex items-center gap-2 text-muted-foreground">
                      <Target className="h-4 w-4" />
                      <span className="text-sm">Orders</span>
                    </div>
                    <div className="mt-2 text-2xl font-bold">
                      {strategy.order_count}
                    </div>
                  </div>

                  <div className="rounded-lg border bg-card p-4 col-span-2 flex items-center justify-end gap-2">
                    {strategy.status === 'running' ? (
                      <Button
                        variant="outline"
                        size="sm"
                        onClick={() => pauseMutation.mutate()}
                        disabled={pauseMutation.isPending}
                      >
                        <Pause className="mr-2 h-4 w-4" />
                        Pause
                      </Button>
                    ) : (
                      <Button
                        size="sm"
                        onClick={() => resumeMutation.mutate()}
                        disabled={resumeMutation.isPending}
                      >
                        <Play className="mr-2 h-4 w-4" />
                        Resume
                      </Button>
                    )}
                  </div>
                </div>
              </CardContent>
            </Card>
          ))
        )}
      </div>
    </div>
  );
}
