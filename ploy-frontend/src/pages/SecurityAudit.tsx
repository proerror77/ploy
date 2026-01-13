import { useState } from 'react';
import { useQuery } from '@tanstack/react-query';
import { api } from '@/services/api';
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/Card';
import { Badge } from '@/components/ui/Badge';
import { formatTimestamp } from '@/lib/utils';
import type { SecurityEvent } from '@/types';

export function SecurityAudit() {
  const [severityFilter, setSeverityFilter] = useState<string>('all');

  const { data: events, isLoading } = useQuery({
    queryKey: ['security', 'events', severityFilter],
    queryFn: () =>
      api.getSecurityEvents({
        limit: 100,
        severity: severityFilter === 'all' ? undefined : severityFilter,
      }),
    refetchInterval: 10000,
  });

  const getSeverityBadge = (severity: SecurityEvent['severity']) => {
    const variants: Record<SecurityEvent['severity'], 'success' | 'warning' | 'destructive' | 'secondary'> = {
      LOW: 'secondary',
      MEDIUM: 'warning',
      HIGH: 'destructive',
      CRITICAL: 'destructive',
    };
    return <Badge variant={variants[severity]}>{severity}</Badge>;
  };

  const getEventTypeLabel = (type: SecurityEvent['event_type']) => {
    const labels: Record<SecurityEvent['event_type'], string> = {
      DUPLICATE_ORDER: '重复订单',
      VERSION_CONFLICT: '版本冲突',
      STALE_QUOTE: '过期报价',
      NONCE_RECOVERY: 'Nonce恢复',
    };
    return labels[type];
  };

  return (
    <div className="p-8">
      <div className="mb-8">
        <h1 className="text-3xl font-bold">安全审计</h1>
        <p className="text-muted-foreground">监控系统安全事件</p>
      </div>

      <Card>
        <CardHeader>
          <div className="flex items-center justify-between">
            <CardTitle>安全事件</CardTitle>
            <div className="flex gap-2">
              {['all', 'CRITICAL', 'HIGH', 'MEDIUM', 'LOW'].map((severity) => (
                <button
                  key={severity}
                  onClick={() => setSeverityFilter(severity)}
                  className={`rounded-md px-3 py-1 text-sm ${
                    severityFilter === severity
                      ? 'bg-primary text-primary-foreground'
                      : 'bg-secondary text-secondary-foreground hover:bg-secondary/80'
                  }`}
                >
                  {severity === 'all' ? '全部' : severity}
                </button>
              ))}
            </div>
          </div>
        </CardHeader>
        <CardContent>
          {isLoading ? (
            <div className="py-8 text-center text-muted-foreground">加载中...</div>
          ) : !events || events.length === 0 ? (
            <div className="py-8 text-center text-muted-foreground">
              暂无安全事件
            </div>
          ) : (
            <div className="space-y-3">
              {events.map((event) => (
                <div
                  key={event.id}
                  className="rounded-lg border p-4 transition-colors hover:bg-accent"
                >
                  <div className="mb-2 flex items-start justify-between">
                    <div className="flex-1">
                      <div className="mb-1 flex items-center gap-2">
                        {getSeverityBadge(event.severity)}
                        <span className="font-medium">
                          {getEventTypeLabel(event.event_type)}
                        </span>
                      </div>
                      <p className="text-sm text-muted-foreground">
                        {event.details}
                      </p>
                    </div>
                    <div className="text-right text-sm text-muted-foreground">
                      {formatTimestamp(event.timestamp)}
                    </div>
                  </div>
                  {event.metadata && (
                    <details className="mt-2">
                      <summary className="cursor-pointer text-sm text-muted-foreground">
                        查看详情
                      </summary>
                      <pre className="mt-2 overflow-x-auto rounded-md bg-muted p-2 text-xs">
                        {JSON.stringify(event.metadata, null, 2)}
                      </pre>
                    </details>
                  )}
                </div>
              ))}
            </div>
          )}
        </CardContent>
      </Card>
    </div>
  );
}
