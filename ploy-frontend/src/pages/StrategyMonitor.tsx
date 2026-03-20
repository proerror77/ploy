import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { Activity, Loader2, Pause, Play, Target } from 'lucide-react';

import { Button } from '@/components/ui/Button';
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/Card';
import { Badge } from '@/components/ui/Badge';
import { api } from '@/services/api';

import type { StrategyControlEntry } from '@/types';

function getStatusVariant(entry: StrategyControlEntry) {
  if (!entry.enabled) return 'secondary' as const;
  if (entry.running_agents.length > 0) return 'success' as const;
  return 'warning' as const;
}

function getStatusLabel(entry: StrategyControlEntry) {
  if (!entry.enabled) return 'disabled';
  if (entry.running_agents.length > 0) return 'running';
  return 'enabled';
}

function getDomainColor(domain: string) {
  switch (domain) {
    case 'crypto':
      return 'bg-blue-500/10 text-blue-700 border-blue-200';
    case 'sports':
      return 'bg-green-500/10 text-green-700 border-green-200';
    case 'politics':
      return 'bg-amber-500/10 text-amber-700 border-amber-200';
    case 'economics':
      return 'bg-slate-500/10 text-slate-700 border-slate-200';
    default:
      return 'bg-muted text-muted-foreground border-border';
  }
}

function formatEvaluationScore(score: number | null) {
  if (score === null) return 'Pending';
  return `${(score * 100).toFixed(0)}%`;
}

export function StrategyMonitor() {
  const queryClient = useQueryClient();

  const { data, isLoading, error } = useQuery({
    queryKey: ['strategies', 'control'],
    queryFn: () => api.getStrategiesControl(),
    refetchInterval: 10000,
  });

  const pauseAllMutation = useMutation({
    mutationFn: () => api.pauseSystem(),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['strategies', 'control'] });
      queryClient.invalidateQueries({ queryKey: ['strategies', 'running'] });
      queryClient.invalidateQueries({ queryKey: ['system', 'status'] });
    },
  });

  const resumeAllMutation = useMutation({
    mutationFn: () => api.resumeSystem(),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['strategies', 'control'] });
      queryClient.invalidateQueries({ queryKey: ['strategies', 'running'] });
      queryClient.invalidateQueries({ queryKey: ['system', 'status'] });
    },
  });

  const updateStrategyMutation = useMutation({
    mutationFn: ({ deploymentId, enabled }: { deploymentId: string; enabled: boolean }) =>
      api.updateStrategyControl(deploymentId, { enabled }),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['strategies', 'control'] });
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
          <p className="text-destructive">Failed to load deployment control state</p>
          <p className="mt-1 text-sm text-muted-foreground">{String(error)}</p>
        </div>
      </div>
    );
  }

  const items = data?.items ?? [];

  return (
    <div className="p-8">
      <div className="mb-8 flex items-center justify-between">
        <div>
          <h1 className="text-3xl font-bold">Deployment Control</h1>
          <p className="text-muted-foreground">
            Manage strategy deployments through the control plane instead of local UI state
          </p>
        </div>
        <div className="flex gap-2">
          <Button
            variant="outline"
            size="sm"
            onClick={() => pauseAllMutation.mutate()}
            disabled={pauseAllMutation.isPending}
          >
            <Pause className="mr-2 h-4 w-4" />
            Pause All
          </Button>
          <Button
            size="sm"
            onClick={() => resumeAllMutation.mutate()}
            disabled={resumeAllMutation.isPending}
          >
            <Play className="mr-2 h-4 w-4" />
            Resume All
          </Button>
        </div>
      </div>

      {data && (
        <div className="mb-6 flex flex-wrap gap-3 text-sm text-muted-foreground">
          <span>Account: {data.account_id ?? 'unscoped'}</span>
          <span>Ingress: {data.ingress_mode ?? 'n/a'}</span>
          <span>Updated: {new Date(data.updated_at).toLocaleString()}</span>
        </div>
      )}

      <div className="space-y-4">
        {items.length === 0 ? (
          <Card>
            <CardContent className="py-12 text-center">
              <Activity className="mx-auto mb-4 h-12 w-12 text-muted-foreground/50" />
              <p className="text-lg font-medium text-muted-foreground">No deployments registered</p>
              <p className="mt-1 text-sm text-muted-foreground">
                Apply deployment manifests before using the operator console.
              </p>
            </CardContent>
          </Card>
        ) : (
          items.map((entry) => {
            const isMutating =
              updateStrategyMutation.isPending &&
              updateStrategyMutation.variables?.deploymentId === entry.deployment_id;

            return (
              <Card key={entry.deployment_id} className="border-l-4 border-l-primary">
                <CardHeader>
                  <div className="flex items-center justify-between gap-4">
                    <div className="space-y-2">
                      <div className="flex items-center gap-3">
                        <Activity className="h-6 w-6 text-primary" />
                        <CardTitle>{entry.strategy}</CardTitle>
                        <Badge variant={getStatusVariant(entry)}>{getStatusLabel(entry)}</Badge>
                      </div>
                      <div className="flex flex-wrap items-center gap-2 text-sm text-muted-foreground">
                        <span className="font-mono">{entry.deployment_id}</span>
                        <span
                          className={`inline-flex items-center rounded-md border px-2 py-0.5 text-xs font-medium ${getDomainColor(entry.domain)}`}
                        >
                          {entry.domain}
                        </span>
                        <span>Version {entry.strategy_version}</span>
                        <span>{entry.timeframe}</span>
                        <span>{entry.lifecycle_stage}</span>
                      </div>
                    </div>

                    {entry.enabled ? (
                      <Button
                        variant="outline"
                        size="sm"
                        onClick={() =>
                          updateStrategyMutation.mutate({
                            deploymentId: entry.deployment_id,
                            enabled: false,
                          })
                        }
                        disabled={isMutating}
                      >
                        <Pause className="mr-2 h-4 w-4" />
                        Disable
                      </Button>
                    ) : (
                      <Button
                        size="sm"
                        onClick={() =>
                          updateStrategyMutation.mutate({
                            deploymentId: entry.deployment_id,
                            enabled: true,
                          })
                        }
                        disabled={isMutating}
                      >
                        <Play className="mr-2 h-4 w-4" />
                        Enable
                      </Button>
                    )}
                  </div>
                </CardHeader>

                <CardContent>
                  <div className="grid grid-cols-1 gap-4 md:grid-cols-4">
                    <div className="rounded-lg border bg-card p-4">
                      <div className="text-sm text-muted-foreground">Evaluation</div>
                      <div className="mt-2 text-2xl font-bold">
                        {formatEvaluationScore(entry.last_evaluation_score)}
                      </div>
                      <div className="mt-1 text-xs text-muted-foreground">
                        {entry.latest_evaluation_stage ?? 'No stage recorded'}
                      </div>
                    </div>

                    <div className="rounded-lg border bg-card p-4">
                      <div className="flex items-center gap-2 text-sm text-muted-foreground">
                        <Target className="h-4 w-4" />
                        Profiles
                      </div>
                      <div className="mt-2 font-medium">{entry.allocator_profile}</div>
                      <div className="mt-1 text-xs text-muted-foreground">
                        Risk: {entry.risk_profile}
                      </div>
                    </div>

                    <div className="rounded-lg border bg-card p-4">
                      <div className="text-sm text-muted-foreground">Priority / Cooldown</div>
                      <div className="mt-2 text-2xl font-bold">{entry.priority}</div>
                      <div className="mt-1 text-xs text-muted-foreground">
                        {entry.cooldown_secs}s cooldown
                      </div>
                    </div>

                    <div className="rounded-lg border bg-card p-4">
                      <div className="text-sm text-muted-foreground">Runtime</div>
                      <div className="mt-2 font-medium">{entry.domain_ingress_mode}</div>
                      <div className="mt-1 text-xs text-muted-foreground">
                        Agents: {entry.running_agents.length > 0 ? entry.running_agents.join(', ') : 'none'}
                      </div>
                    </div>
                  </div>
                </CardContent>
              </Card>
            );
          })
        )}
      </div>
    </div>
  );
}
