import { useState, useEffect } from 'react';
import { useMutation } from '@tanstack/react-query';
import { api } from '@/services/api';
import { Card } from '@/components/ui/Card';
import { Badge } from '@/components/ui/Badge';
import { Button } from '@/components/ui/Button';
import { ws } from '@/services/websocket';
import type { WebSocketEvent } from '@/services/websocket';
import {
  Activity,
  TrendingUp,
  TrendingDown,
  AlertTriangle,
  CheckCircle,
  XCircle,
  Clock,
  DollarSign,
  Target,
  BarChart3,
  Wifi,
  WifiOff,
  Loader2,
} from 'lucide-react';

interface NBASwingState {
  state: 'WATCH' | 'ARMED' | 'ENTERING' | 'MANAGING' | 'EXITING' | 'EXITED' | 'HALT';
  currentGame: GameInfo | null;
  prediction: WinProbPrediction | null;
  marketData: MarketData | null;
  position: Position | null;
  signals: Signal[];
  filters: FilterResult | null;
}

interface GameInfo {
  gameId: string;
  homeTeam: string;
  awayTeam: string;
  homeScore: number;
  awayScore: number;
  quarter: number;
  timeRemaining: number;
  possession: string;
}

interface WinProbPrediction {
  winProb: number;
  confidence: number;
  uncertainty: number;
  features: {
    pointDiff: number;
    timeRemaining: number;
    quarter: number;
  };
}

interface MarketData {
  marketId: string;
  teamName: string;
  price: number;
  bestBid: number;
  bestAsk: number;
  spreadBps: number;
  bidDepth: number;
  askDepth: number;
  dataLatencyMs: number;
}

interface Position {
  entryPrice: number;
  currentPrice: number;
  size: number;
  unrealizedPnl: number;
  unrealizedPnlPct: number;
  peakPrice: number;
  entryTime: string;
}

interface Signal {
  timestamp: string;
  type: 'ENTRY' | 'EXIT' | 'REJECTED';
  reason: string;
  edge?: number;
  netEv?: number;
  confidence?: number;
}

interface FilterResult {
  passed: boolean;
  reasons: string[];
  warnings: string[];
}

export function NBASwingMonitor() {
  const [wsConnected, setWsConnected] = useState(ws.isConnected());
  const [state, setState] = useState<NBASwingState>({
    state: 'WATCH',
    currentGame: null,
    prediction: null,
    marketData: null,
    position: null,
    signals: [],
    filters: null,
  });

  // Track sidecar WebSocket connection
  useEffect(() => {
    const unsub = ws.onConnectionChange(setWsConnected);
    return unsub;
  }, []);

  // Subscribe to NBA update events from sidecar
  useEffect(() => {
    const unsub = ws.subscribe('nba_update', (event: WebSocketEvent) => {
      if (event.type === 'nba_update') {
        const data = event.data;
        setState((prev) => ({
          ...prev,
          state: (data.state as NBASwingState['state']) ?? prev.state,
          currentGame: data.game ?? prev.currentGame,
          prediction: data.prediction
            ? {
                winProb: data.prediction.winProb,
                confidence: data.prediction.confidence,
                uncertainty: 1 - data.prediction.confidence,
                features: prev.prediction?.features ?? { pointDiff: 0, timeRemaining: 0, quarter: 0 },
              }
            : prev.prediction,
        }));
      }
    });
    return unsub;
  }, []);

  const pauseMutation = useMutation({
    mutationFn: () => api.pauseSystem('sports'),
  });

  const haltMutation = useMutation({
    mutationFn: () => api.haltSystem('sports'),
  });

  const getStateColor = (s: string) => {
    switch (s) {
      case 'WATCH': return 'bg-gray-500';
      case 'ARMED': return 'bg-yellow-500';
      case 'ENTERING': return 'bg-blue-500';
      case 'MANAGING': return 'bg-green-500';
      case 'EXITING': return 'bg-orange-500';
      case 'EXITED': return 'bg-purple-500';
      case 'HALT': return 'bg-red-500';
      default: return 'bg-gray-500';
    }
  };

  const edge = state.prediction && state.marketData
    ? state.prediction.winProb - state.marketData.price
    : 0;

  return (
    <div className="p-8 space-y-6">
      {/* Header */}
      <div className="flex items-center justify-between">
        <div>
          <h1 className="text-3xl font-bold">NBA Swing Strategy</h1>
          <p className="text-muted-foreground mt-1">Model-based Value Trading</p>
        </div>
        <div className="flex items-center gap-3">
          <Badge
            variant={wsConnected ? 'success' : 'destructive'}
            className="flex items-center gap-1.5"
          >
            {wsConnected ? <Wifi className="h-3 w-3" /> : <WifiOff className="h-3 w-3" />}
            {wsConnected ? 'Sidecar Connected' : 'Sidecar Disconnected'}
          </Badge>
          <Badge className={`${getStateColor(state.state)} text-white px-4 py-2 text-lg`}>
            {state.state}
          </Badge>
        </div>
      </div>

      {/* Current Game */}
      {state.currentGame && (
        <Card className="p-6">
          <div className="flex items-center justify-between mb-4">
            <h2 className="text-xl font-semibold flex items-center gap-2">
              <Activity className="w-5 h-5" />
              Live Game
            </h2>
            <Badge variant="outline">Q{state.currentGame.quarter} - {state.currentGame.timeRemaining.toFixed(1)} min</Badge>
          </div>

          <div className="grid grid-cols-3 gap-4">
            <div className="text-center">
              <div className="text-2xl font-bold">{state.currentGame.homeTeam}</div>
              <div className="text-4xl font-bold mt-2">{state.currentGame.homeScore}</div>
              {state.currentGame.possession === 'home' && (
                <Badge className="mt-2">Possession</Badge>
              )}
            </div>

            <div className="flex items-center justify-center">
              <div className="text-muted-foreground text-xl">vs</div>
            </div>

            <div className="text-center">
              <div className="text-2xl font-bold">{state.currentGame.awayTeam}</div>
              <div className="text-4xl font-bold mt-2">{state.currentGame.awayScore}</div>
              {state.currentGame.possession === 'away' && (
                <Badge className="mt-2">Possession</Badge>
              )}
            </div>
          </div>
        </Card>
      )}

      {!state.currentGame && (
        <Card className="p-6">
          <div className="py-8 text-center">
            <Activity className="mx-auto h-12 w-12 text-muted-foreground/50 mb-4" />
            <p className="text-lg font-medium text-muted-foreground">No live game</p>
            <p className="text-sm text-muted-foreground mt-1">
              {wsConnected ? 'Waiting for NBA game data from sidecar...' : 'Connect to sidecar to receive game data'}
            </p>
          </div>
        </Card>
      )}

      {/* Key Metrics */}
      <div className="grid grid-cols-1 md:grid-cols-4 gap-4">
        {state.prediction && (
          <Card className="p-4">
            <div className="flex items-center justify-between mb-2">
              <span className="text-sm text-muted-foreground">Model Win Prob</span>
              <Target className="w-4 h-4 text-blue-500" />
            </div>
            <div className="text-3xl font-bold">{(state.prediction.winProb * 100).toFixed(1)}%</div>
            <div className="text-xs text-muted-foreground mt-1">
              Confidence: {(state.prediction.confidence * 100).toFixed(0)}%
            </div>
          </Card>
        )}

        {state.marketData && (
          <Card className="p-4">
            <div className="flex items-center justify-between mb-2">
              <span className="text-sm text-muted-foreground">Market Price</span>
              <DollarSign className="w-4 h-4 text-green-500" />
            </div>
            <div className="text-3xl font-bold">{state.marketData.price.toFixed(3)}</div>
            <div className="text-xs text-muted-foreground mt-1">
              Spread: {state.marketData.spreadBps} bps
            </div>
          </Card>
        )}

        <Card className="p-4">
          <div className="flex items-center justify-between mb-2">
            <span className="text-sm text-muted-foreground">Edge</span>
            {edge > 0 ? (
              <TrendingUp className="w-4 h-4 text-green-500" />
            ) : (
              <TrendingDown className="w-4 h-4 text-red-500" />
            )}
          </div>
          <div className={`text-3xl font-bold ${edge > 0 ? 'text-success' : 'text-destructive'}`}>
            {(edge * 100).toFixed(1)}%
          </div>
          <div className="text-xs text-muted-foreground mt-1">
            {edge > 0.05 ? 'Strong' : edge > 0.02 ? 'Moderate' : 'Weak'}
          </div>
        </Card>

        {state.position && (
          <Card className="p-4">
            <div className="flex items-center justify-between mb-2">
              <span className="text-sm text-muted-foreground">Unrealized PnL</span>
              <BarChart3 className="w-4 h-4 text-purple-500" />
            </div>
            <div className={`text-3xl font-bold ${state.position.unrealizedPnl > 0 ? 'text-success' : 'text-destructive'}`}>
              ${state.position.unrealizedPnl.toFixed(2)}
            </div>
            <div className="text-xs text-muted-foreground mt-1">
              {(state.position.unrealizedPnlPct * 100).toFixed(1)}% return
            </div>
          </Card>
        )}
      </div>

      {/* Position Details */}
      {state.position && (
        <Card className="p-6">
          <h2 className="text-xl font-semibold mb-4">Position Details</h2>
          <div className="grid grid-cols-2 md:grid-cols-4 gap-4">
            <div>
              <div className="text-sm text-muted-foreground">Entry Price</div>
              <div className="text-lg font-semibold">{state.position.entryPrice.toFixed(3)}</div>
            </div>
            <div>
              <div className="text-sm text-muted-foreground">Current Price</div>
              <div className="text-lg font-semibold">{state.position.currentPrice.toFixed(3)}</div>
            </div>
            <div>
              <div className="text-sm text-muted-foreground">Peak Price</div>
              <div className="text-lg font-semibold">{state.position.peakPrice.toFixed(3)}</div>
            </div>
            <div>
              <div className="text-sm text-muted-foreground">Size</div>
              <div className="text-lg font-semibold">${state.position.size}</div>
            </div>
          </div>

          <div className="mt-4">
            <div className="flex justify-between text-xs text-muted-foreground mb-1">
              <span>Entry</span>
              <span>Current</span>
              <span>Peak</span>
            </div>
            <div className="relative h-2 bg-muted rounded-full">
              <div
                className="absolute h-full bg-success rounded-full"
                style={{
                  width: `${Math.max(0, Math.min(100, ((state.position.currentPrice - state.position.entryPrice) / (state.position.peakPrice - state.position.entryPrice || 1)) * 100))}%`
                }}
              />
            </div>
          </div>
        </Card>
      )}

      {/* Market Filters */}
      {state.filters && (
        <Card className="p-6">
          <h2 className="text-xl font-semibold mb-4 flex items-center gap-2">
            {state.filters.passed ? (
              <CheckCircle className="w-5 h-5 text-success" />
            ) : (
              <XCircle className="w-5 h-5 text-destructive" />
            )}
            Market Filters
          </h2>

          {state.filters.passed ? (
            <div className="text-success font-medium">All filters passed</div>
          ) : (
            <div className="space-y-2">
              {state.filters.reasons.map((reason, i) => (
                <div key={i} className="flex items-center gap-2 text-destructive">
                  <XCircle className="w-4 h-4" />
                  {reason}
                </div>
              ))}
            </div>
          )}

          {state.filters.warnings.length > 0 && (
            <div className="mt-4 space-y-2">
              <div className="text-sm font-medium text-muted-foreground">Warnings:</div>
              {state.filters.warnings.map((warning, i) => (
                <div key={i} className="flex items-center gap-2 text-warning text-sm">
                  <AlertTriangle className="w-4 h-4" />
                  {warning}
                </div>
              ))}
            </div>
          )}
        </Card>
      )}

      {/* Market Data */}
      {state.marketData && (
        <Card className="p-6">
          <h2 className="text-xl font-semibold mb-4">Market Data</h2>
          <div className="grid grid-cols-2 md:grid-cols-4 gap-4">
            <div>
              <div className="text-sm text-muted-foreground">Best Bid</div>
              <div className="text-lg font-semibold">{state.marketData.bestBid.toFixed(3)}</div>
            </div>
            <div>
              <div className="text-sm text-muted-foreground">Best Ask</div>
              <div className="text-lg font-semibold">{state.marketData.bestAsk.toFixed(3)}</div>
            </div>
            <div>
              <div className="text-sm text-muted-foreground">Bid Depth</div>
              <div className="text-lg font-semibold">${state.marketData.bidDepth}</div>
            </div>
            <div>
              <div className="text-sm text-muted-foreground">Ask Depth</div>
              <div className="text-lg font-semibold">${state.marketData.askDepth}</div>
            </div>
          </div>

          <div className="mt-4 flex items-center gap-2 text-sm">
            <Clock className="w-4 h-4 text-muted-foreground" />
            <span className="text-muted-foreground">
              Data Latency: <span className="font-medium">{state.marketData.dataLatencyMs}ms</span>
            </span>
          </div>
        </Card>
      )}

      {/* Signal History */}
      <Card className="p-6">
        <h2 className="text-xl font-semibold mb-4">Signal History</h2>
        <div className="space-y-3">
          {state.signals.length === 0 ? (
            <p className="text-center py-4 text-muted-foreground">No signals recorded</p>
          ) : (
            state.signals.map((signal, i) => (
              <div key={i} className="flex items-start gap-3 p-3 bg-muted rounded-lg">
                <div className="flex-shrink-0 mt-1">
                  {signal.type === 'ENTRY' && <CheckCircle className="w-5 h-5 text-success" />}
                  {signal.type === 'EXIT' && <XCircle className="w-5 h-5 text-warning" />}
                  {signal.type === 'REJECTED' && <XCircle className="w-5 h-5 text-destructive" />}
                </div>
                <div className="flex-1">
                  <div className="flex items-center gap-2 mb-1">
                    <Badge variant={signal.type === 'ENTRY' ? 'default' : 'outline'}>
                      {signal.type}
                    </Badge>
                    <span className="text-xs text-muted-foreground">
                      {new Date(signal.timestamp).toLocaleTimeString()}
                    </span>
                  </div>
                  <div className="text-sm">{signal.reason}</div>
                  {signal.edge !== undefined && (
                    <div className="text-xs text-muted-foreground mt-1">
                      Edge: {(signal.edge * 100).toFixed(1)}% |
                      Net EV: {(signal.netEv! * 100).toFixed(1)}% |
                      Confidence: {(signal.confidence! * 100).toFixed(0)}%
                    </div>
                  )}
                </div>
              </div>
            ))
          )}
        </div>
      </Card>

      {/* Control Buttons */}
      <div className="flex gap-4">
        <Button
          variant="outline"
          className="flex-1"
          onClick={() => pauseMutation.mutate()}
          disabled={pauseMutation.isPending}
        >
          {pauseMutation.isPending && <Loader2 className="mr-2 h-4 w-4 animate-spin" />}
          Pause Strategy
        </Button>
        <Button
          variant="destructive"
          className="flex-1"
          onClick={() => haltMutation.mutate()}
          disabled={haltMutation.isPending}
        >
          {haltMutation.isPending && <Loader2 className="mr-2 h-4 w-4 animate-spin" />}
          Emergency Halt
        </Button>
      </div>
    </div>
  );
}
