import { useEffect, useRef } from 'react';
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/Card';
import { Button } from '@/components/ui/Button';
import { useStore } from '@/store';
import { formatTimestamp } from '@/lib/utils';
import { Trash2 } from 'lucide-react';

export function LiveMonitor() {
  const { logs, clearLogs } = useStore();
  const containerRef = useRef<HTMLDivElement>(null);
  const shouldAutoScroll = useRef(true);

  useEffect(() => {
    if (shouldAutoScroll.current && containerRef.current) {
      containerRef.current.scrollTop = containerRef.current.scrollHeight;
    }
  }, [logs]);

  const handleScroll = () => {
    if (containerRef.current) {
      const { scrollTop, scrollHeight, clientHeight } = containerRef.current;
      shouldAutoScroll.current = scrollTop + clientHeight >= scrollHeight - 50;
    }
  };

  const getLevelColor = (level: string) => {
    switch (level) {
      case 'ERROR':
        return 'text-destructive';
      case 'WARN':
        return 'text-warning';
      case 'INFO':
        return 'text-primary';
      case 'DEBUG':
        return 'text-muted-foreground';
      default:
        return '';
    }
  };

  return (
    <div className="p-8">
      <div className="mb-8 flex items-center justify-between">
        <div>
          <h1 className="text-3xl font-bold">实时监控</h1>
          <p className="text-muted-foreground">查看系统实时日志</p>
        </div>
        <Button variant="outline" onClick={clearLogs}>
          <Trash2 className="mr-2 h-4 w-4" />
          清空日志
        </Button>
      </div>

      <Card>
        <CardHeader>
          <CardTitle>系统日志 ({logs.length})</CardTitle>
        </CardHeader>
        <CardContent>
          <div
            ref={containerRef}
            onScroll={handleScroll}
            className="h-[600px] overflow-y-auto rounded-lg bg-black p-4 font-mono text-sm"
          >
            {logs.length === 0 ? (
              <div className="text-center text-muted-foreground">等待日志...</div>
            ) : (
              logs.map((log, index) => (
                <div key={index} className="mb-1 flex gap-2">
                  <span className="text-muted-foreground">
                    {formatTimestamp(log.timestamp)}
                  </span>
                  <span className={getLevelColor(log.level)}>[{log.level}]</span>
                  <span className="text-accent">[{log.component}]</span>
                  <span className="text-foreground">{log.message}</span>
                  {log.metadata && (
                    <span className="text-muted-foreground">
                      {JSON.stringify(log.metadata)}
                    </span>
                  )}
                </div>
              ))
            )}
          </div>
        </CardContent>
      </Card>
    </div>
  );
}
