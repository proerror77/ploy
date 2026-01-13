import { useState, useEffect } from 'react';
import { Card } from '@/components/ui/Card';
import { Badge } from '@/components/ui/Badge';
import { Button } from '@/components/ui/Button';
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
  BarChart3
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
  const [state, setState] = useState<NBASwingState>({
    state: 'WATCH',
    currentGame: null,
    prediction: null,
    marketData: null,
    position: null,
    signals: [],
    filters: null,
  });

  // Mock data for demonstration
  useEffect(() => {
    // In production, this would connect to WebSocket
    const mockData: NBASwingState = {
      state: 'MANAGING',
      currentGame: {
        gameId: 'game_123',
        homeTeam: 'Lakers',
        awayTeam: 'Warriors',
        homeScore: 85,
        awayScore: 90,
        quarter: 3,
        timeRemaining: 8.5,
        possession: 'home',
      },
      prediction: {
        winProb: 0.28,
        confidence: 0.95,
        uncertainty: 0.05,
        features: {
          pointDiff: -5,
          timeRemaining: 8.5,
          quarter: 3,
        },
      },
      marketData: {
        marketId: 'market_456',
        teamName: 'Lakers',
        price: 0.22,
        bestBid: 0.21,
        bestAsk: 0.23,
        spreadBps: 90,
        bidDepth: 2500,
        askDepth: 2200,
        dataLatencyMs: 850,
      },
      position: {
        entryPrice: 0.15,
        currentPrice: 0.22,
        size: 1000,
        unrealizedPnl: 70,
        unrealizedPnlPct: 0.467,
        peakPrice: 0.25,
        entryTime: '2026-01-13T20:15:00Z',
      },
      signals: [
        {
          timestamp: '2026-01-13T20:15:00Z',
          type: 'ENTRY',
          reason: 'Edge: 10.00%, Net EV: 8.50%, Confidence: 95.0%',
          edge: 0.10,
          netEv: 0.085,
          confidence: 0.95,
        },
        {
          timestamp: '2026-01-13T20:10:00Z',
          type: 'REJECTED',
          reason: 'Insufficient edge: 3.2% < 5.0%',
        },
      ],
      filters: {
        passed: true,
        reasons: [],
        warnings: ['Elevated spread: 90 bps'],
      },
    };

    setState(mockData);
  }, []);

  const getStateColor = (state: string) => {
    switch (state) {
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
    <div className="space-y-6">
      {/* Header */}
      <div className="flex items-center justify-between">
        <div>
          <h1 className="text-3xl font-bold">NBA Swing Strategy</h1>
          <p className="text-gray-600 mt-1">Model-based Value Trading</p>
        </div>
        <Badge className={`${getStateColor(state.state)} text-white px-4 py-2 text-lg`}>
          {state.state}
        </Badge>
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
              <div className="text-gray-400 text-xl">vs</div>
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

      {/* Key Metrics */}
      <div className="grid grid-cols-1 md:grid-cols-4 gap-4">
        {/* Win Probability */}
        {state.prediction && (
          <Card className="p-4">
            <div className="flex items-center justify-between mb-2">
              <span className="text-sm text-gray-600">Model Win Prob</span>
              <Target className="w-4 h-4 text-blue-500" />
            </div>
            <div className="text-3xl font-bold">{(state.prediction.winProb * 100).toFixed(1)}%</div>
            <div className="text-xs text-gray-500 mt-1">
              Confidence: {(state.prediction.confidence * 100).toFixed(0)}%
            </div>
          </Card>
        )}

        {/* Market Price */}
        {state.marketData && (
          <Card className="p-4">
            <div className="flex items-center justify-between mb-2">
              <span className="text-sm text-gray-600">Market Price</span>
              <DollarSign className="w-4 h-4 text-green-500" />
            </div>
            <div className="text-3xl font-bold">{state.marketData.price.toFixed(3)}</div>
            <div className="text-xs text-gray-500 mt-1">
              Spread: {state.marketData.spreadBps} bps
            </div>
          </Card>
        )}

        {/* Edge */}
        <Card className="p-4">
          <div className="flex items-center justify-between mb-2">
            <span className="text-sm text-gray-600">Edge</span>
            {edge > 0 ? (
              <TrendingUp className="w-4 h-4 text-green-500" />
            ) : (
              <TrendingDown className="w-4 h-4 text-red-500" />
            )}
          </div>
          <div className={`text-3xl font-bold ${edge > 0 ? 'text-green-600' : 'text-red-600'}`}>
            {(edge * 100).toFixed(1)}%
          </div>
          <div className="text-xs text-gray-500 mt-1">
            {edge > 0.05 ? 'Strong' : edge > 0.02 ? 'Moderate' : 'Weak'}
          </div>
        </Card>

        {/* Position PnL */}
        {state.position && (
          <Card className="p-4">
            <div className="flex items-center justify-between mb-2">
              <span className="text-sm text-gray-600">Unrealized PnL</span>
              <BarChart3 className="w-4 h-4 text-purple-500" />
            </div>
            <div className={`text-3xl font-bold ${state.position.unrealizedPnl > 0 ? 'text-green-600' : 'text-red-600'}`}>
              ${state.position.unrealizedPnl.toFixed(2)}
            </div>
            <div className="text-xs text-gray-500 mt-1">
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
              <div className="text-sm text-gray-600">Entry Price</div>
              <div className="text-lg font-semibold">{state.position.entryPrice.toFixed(3)}</div>
            </div>
            <div>
              <div className="text-sm text-gray-600">Current Price</div>
              <div className="text-lg font-semibold">{state.position.currentPrice.toFixed(3)}</div>
            </div>
            <div>
              <div className="text-sm text-gray-600">Peak Price</div>
              <div className="text-lg font-semibold">{state.position.peakPrice.toFixed(3)}</div>
            </div>
            <div>
              <div className="text-sm text-gray-600">Size</div>
              <div className="text-lg font-semibold">${state.position.size}</div>
            </div>
          </div>

          {/* Progress Bar */}
          <div className="mt-4">
            <div className="flex justify-between text-xs text-gray-600 mb-1">
              <span>Entry</span>
              <span>Current</span>
              <span>Peak</span>
            </div>
            <div className="relative h-2 bg-gray-200 rounded-full">
              <div
                className="absolute h-full bg-green-500 rounded-full"
                style={{
                  width: `${((state.position.currentPrice - state.position.entryPrice) / (state.position.peakPrice - state.position.entryPrice)) * 100}%`
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
              <CheckCircle className="w-5 h-5 text-green-500" />
            ) : (
              <XCircle className="w-5 h-5 text-red-500" />
            )}
            Market Filters
          </h2>

          {state.filters.passed ? (
            <div className="text-green-600 font-medium">✓ All filters passed</div>
          ) : (
            <div className="space-y-2">
              {state.filters.reasons.map((reason, i) => (
                <div key={i} className="flex items-center gap-2 text-red-600">
                  <XCircle className="w-4 h-4" />
                  {reason}
                </div>
              ))}
            </div>
          )}

          {state.filters.warnings.length > 0 && (
            <div className="mt-4 space-y-2">
              <div className="text-sm font-medium text-gray-700">Warnings:</div>
              {state.filters.warnings.map((warning, i) => (
                <div key={i} className="flex items-center gap-2 text-yellow-600 text-sm">
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
              <div className="text-sm text-gray-600">Best Bid</div>
              <div className="text-lg font-semibold">{state.marketData.bestBid.toFixed(3)}</div>
            </div>
            <div>
              <div className="text-sm text-gray-600">Best Ask</div>
              <div className="text-lg font-semibold">{state.marketData.bestAsk.toFixed(3)}</div>
            </div>
            <div>
              <div className="text-sm text-gray-600">Bid Depth</div>
              <div className="text-lg font-semibold">${state.marketData.bidDepth}</div>
            </div>
            <div>
              <div className="text-sm text-gray-600">Ask Depth</div>
              <div className="text-lg font-semibold">${state.marketData.askDepth}</div>
            </div>
          </div>

          <div className="mt-4 flex items-center gap-2 text-sm">
            <Clock className="w-4 h-4 text-gray-500" />
            <span className="text-gray-600">
              Data Latency: <span className="font-medium">{state.marketData.dataLatencyMs}ms</span>
            </span>
          </div>
        </Card>
      )}

      {/* Signal History */}
      <Card className="p-6">
        <h2 className="text-xl font-semibold mb-4">Signal History</h2>
        <div className="space-y-3">
          {state.signals.map((signal, i) => (
            <div key={i} className="flex items-start gap-3 p-3 bg-gray-50 rounded-lg">
              <div className="flex-shrink-0 mt-1">
                {signal.type === 'ENTRY' && <CheckCircle className="w-5 h-5 text-green-500" />}
                {signal.type === 'EXIT' && <XCircle className="w-5 h-5 text-orange-500" />}
                {signal.type === 'REJECTED' && <XCircle className="w-5 h-5 text-red-500" />}
              </div>
              <div className="flex-1">
                <div className="flex items-center gap-2 mb-1">
                  <Badge variant={signal.type === 'ENTRY' ? 'default' : 'outline'}>
                    {signal.type}
                  </Badge>
                  <span className="text-xs text-gray-500">
                    {new Date(signal.timestamp).toLocaleTimeString()}
                  </span>
                </div>
                <div className="text-sm text-gray-700">{signal.reason}</div>
                {signal.edge !== undefined && (
                  <div className="text-xs text-gray-500 mt-1">
                    Edge: {(signal.edge * 100).toFixed(1)}% |
                    Net EV: {(signal.netEv! * 100).toFixed(1)}% |
                    Confidence: {(signal.confidence! * 100).toFixed(0)}%
                  </div>
                )}
              </div>
            </div>
          ))}
        </div>
      </Card>

      {/* Control Buttons */}
      <div className="flex gap-4">
        <Button variant="outline" className="flex-1">
          Pause Strategy
        </Button>
        <Button variant="destructive" className="flex-1">
          Emergency Halt
        </Button>
      </div>
    </div>
  );
}
