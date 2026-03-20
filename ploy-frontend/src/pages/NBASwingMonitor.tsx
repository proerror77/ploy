import { useMutation } from '@tanstack/react-query';
import { AlertTriangle, Loader2, Pause, ShieldAlert, Wifi, WifiOff } from 'lucide-react';
import { useEffect, useState } from 'react';

import { Button } from '@/components/ui/Button';
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/Card';
import { Badge } from '@/components/ui/Badge';
import { api } from '@/services/api';
import { ws } from '@/services/websocket';

export function NBASwingMonitor() {
  const [wsConnected, setWsConnected] = useState(ws.isConnected());

  useEffect(() => {
    const unsubscribe = ws.onConnectionChange(setWsConnected);
    return unsubscribe;
  }, []);

  const pauseMutation = useMutation({
    mutationFn: () => api.pauseSystem('sports'),
  });

  const haltMutation = useMutation({
    mutationFn: () => api.haltSystem('sports'),
  });

  return (
    <div className="space-y-6 p-8">
      <div className="flex items-center justify-between">
        <div>
          <h1 className="text-3xl font-bold">NBA Swing Monitor</h1>
          <p className="mt-1 text-muted-foreground">
            Legacy sidecar surface retired from the platform-native control plane
          </p>
        </div>
        <Badge
          variant={wsConnected ? 'success' : 'secondary'}
          className="flex items-center gap-1.5"
        >
          {wsConnected ? <Wifi className="h-3 w-3" /> : <WifiOff className="h-3 w-3" />}
          {wsConnected ? 'Control plane live' : 'Polling only'}
        </Badge>
      </div>

      <Card className="border border-warning/40">
        <CardHeader>
          <CardTitle className="flex items-center gap-2">
            <AlertTriangle className="h-5 w-5 text-warning" />
            Operator Notice
          </CardTitle>
        </CardHeader>
        <CardContent className="space-y-3 text-sm text-muted-foreground">
          <p>
            This page no longer consumes the private <code>nba_update</code> websocket feed. That sidecar-only
            protocol is not part of the platform contract.
          </p>
          <p>
            Use <span className="font-medium text-foreground">Deployment Control</span> for
            enable/disable decisions and the shared system surfaces for orders, positions, risk, and
            audit state.
          </p>
          <p>
            If NBA-specific telemetry is needed again, it should be reintroduced behind the shared
            operator contract instead of a private websocket event.
          </p>
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle className="flex items-center gap-2">
            <ShieldAlert className="h-5 w-5 text-destructive" />
            Sports Domain Controls
          </CardTitle>
        </CardHeader>
        <CardContent className="space-y-4">
          <p className="text-sm text-muted-foreground">
            These actions operate on the <code>sports</code> domain through the shared control-plane API.
          </p>
          <div className="flex gap-4">
            <Button
              variant="outline"
              className="flex-1"
              onClick={() => pauseMutation.mutate()}
              disabled={pauseMutation.isPending}
            >
              {pauseMutation.isPending && <Loader2 className="mr-2 h-4 w-4 animate-spin" />}
              <Pause className="mr-2 h-4 w-4" />
              Pause Sports Domain
            </Button>
            <Button
              variant="destructive"
              className="flex-1"
              onClick={() => haltMutation.mutate()}
              disabled={haltMutation.isPending}
            >
              {haltMutation.isPending && <Loader2 className="mr-2 h-4 w-4 animate-spin" />}
              <ShieldAlert className="mr-2 h-4 w-4" />
              Emergency Halt Sports
            </Button>
          </div>
        </CardContent>
      </Card>
    </div>
  );
}
