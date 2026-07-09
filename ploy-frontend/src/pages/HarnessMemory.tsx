import { useQuery } from '@tanstack/react-query';
import { Brain, RefreshCw } from 'lucide-react';

import { Badge } from '@/components/ui/Badge';
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/Card';
import { api } from '@/services/api';

function formatTime(value?: string) {
  if (!value) return '-';
  const time = Date.parse(value);
  if (!Number.isFinite(time)) return '-';
  return new Date(time).toLocaleString('zh-CN', {
    month: '2-digit',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
  });
}

export function HarnessMemory() {
  const memoryQuery = useQuery({
    queryKey: ['agent', 'harness-memory'],
    queryFn: () => api.getHarnessMemory(),
    refetchInterval: 10000,
  });

  const memory = memoryQuery.data;
  const events = memory?.events ?? [];

  return (
    <div className="min-h-full bg-[#f4f7f2] p-6 text-[#111827]">
      <header className="mb-6 flex flex-col gap-3 lg:flex-row lg:items-center lg:justify-between">
        <div className="flex items-center gap-3">
          <div className="flex h-11 w-11 items-center justify-center rounded-md border border-[#bfdbfe] bg-[#eff6ff] text-[#1d4ed8]">
            <Brain className="h-5 w-5" />
          </div>
          <div>
            <div className="text-xs font-semibold text-[#64748b]">Harness Memory</div>
            <h1 className="text-2xl font-semibold tracking-normal">自改进上下文</h1>
          </div>
        </div>
        <Badge variant={memoryQuery.isError ? 'destructive' : 'success'}>
          {memoryQuery.isError ? 'offline' : `${memory?.event_count ?? 0} events`}
        </Badge>
      </header>

      <div className="grid gap-5 xl:grid-cols-[1fr_380px]">
        <Card>
          <CardHeader>
            <CardTitle>harness-context.md</CardTitle>
          </CardHeader>
          <CardContent>
            {memoryQuery.isLoading ? (
              <div className="flex items-center gap-2 text-sm text-[#64748b]">
                <RefreshCw className="h-4 w-4 animate-spin" />
                loading context
              </div>
            ) : (
              <pre className="max-h-[640px] overflow-auto rounded-md bg-[#101820] p-4 text-xs leading-5 text-[#f8fafc]">
                {memory?.context?.trim() || '# Harness Meta-Context\n\n(no context yet)'}
              </pre>
            )}
          </CardContent>
        </Card>

        <Card>
          <CardHeader>
            <CardTitle>Proposal Queue</CardTitle>
          </CardHeader>
          <CardContent>
            {events.length === 0 ? (
              <div className="rounded-md border border-[#d9e3dd] bg-white p-4 text-sm text-[#64748b]">
                No harness proposals yet.
              </div>
            ) : (
              <div className="space-y-3">
                {events.map((event, index) => (
                  <div
                    key={`${event.run_id ?? 'event'}-${event.created_at ?? index}`}
                    className="rounded-md border border-[#d9e3dd] bg-white p-3"
                  >
                    <div className="flex items-center justify-between gap-2">
                      <Badge variant="outline">{event.category ?? 'harness'}</Badge>
                      <span className="text-xs text-[#64748b]">{formatTime(event.created_at)}</span>
                    </div>
                    <div className="mt-2 text-sm font-medium">
                      {event.summary ?? event.run_id ?? 'harness learning'}
                    </div>
                    {event.suggested_change && (
                      <div className="mt-2 text-xs leading-5 text-[#475569]">
                        {event.suggested_change}
                      </div>
                    )}
                    {event.subagent_profile && (
                      <div className="mt-2">
                        <Badge variant="secondary">{event.subagent_profile}</Badge>
                      </div>
                    )}
                  </div>
                ))}
              </div>
            )}
          </CardContent>
        </Card>
      </div>
    </div>
  );
}
