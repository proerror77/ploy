import { BrowserRouter, Routes, Route } from 'react-router-dom';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { lazy, Suspense, useEffect } from 'react';
import { Layout } from '@/components/Layout';
import { ws } from '@/services/websocket';
import { useStore } from '@/store';

const Dashboard = lazy(() =>
  import('@/pages/Dashboard').then((module) => ({ default: module.Dashboard }))
);
const TradeHistory = lazy(() =>
  import('@/pages/TradeHistory').then((module) => ({ default: module.TradeHistory }))
);
const LiveMonitor = lazy(() =>
  import('@/pages/LiveMonitor').then((module) => ({ default: module.LiveMonitor }))
);
const StrategyMonitor = lazy(() =>
  import('@/pages/StrategyMonitor').then((module) => ({ default: module.StrategyMonitor }))
);
const SystemControl = lazy(() =>
  import('@/pages/SystemControl').then((module) => ({ default: module.SystemControl }))
);
const SecurityAudit = lazy(() =>
  import('@/pages/SecurityAudit').then((module) => ({ default: module.SecurityAudit }))
);
const NBASwingMonitor = lazy(() =>
  import('@/pages/NBASwingMonitor').then((module) => ({ default: module.NBASwingMonitor }))
);
const RiskDashboard = lazy(() =>
  import('@/pages/RiskDashboard').then((module) => ({ default: module.RiskDashboard }))
);
const ResearchRuns = lazy(() =>
  import('@/pages/ResearchRuns').then((module) => ({ default: module.ResearchRuns }))
);
const RunDetail = lazy(() =>
  import('@/pages/RunDetail').then((module) => ({ default: module.RunDetail }))
);
const OversightAlerts = lazy(() =>
  import('@/pages/OversightAlerts').then((module) => ({ default: module.OversightAlerts }))
);
const ProposalDetail = lazy(() =>
  import('@/pages/ProposalDetail').then((module) => ({ default: module.ProposalDetail }))
);

const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      retry: 1,
      refetchOnWindowFocus: false,
    },
  },
});

function App() {
  const {
    setWsConnected,
    addLog,
    addTrade,
    setDeployments,
    setTradingSnapshots,
    setOversightReport,
    setProposals,
    updatePositions,
    updateMarketData,
    setSystemStatus,
  } = useStore();

  useEffect(() => {
    // Connect to WebSocket
    ws.connect();

    // Track connection state
    const unsubConnection = ws.onConnectionChange((connected) => {
      setWsConnected(connected);
    });

    // Subscribe to all events
    const unsubscribe = ws.subscribe('*', (event) => {
      switch (event.type) {
        case 'log':
          addLog(event.data);
          break;
        case 'trade':
          addTrade(event.data);
          break;
        case 'position':
          updatePositions([event.data]);
          break;
        case 'market':
          updateMarketData(event.data);
          break;
        case 'status':
          setSystemStatus(event.data.status);
          break;
        case 'system_snapshot':
          setSystemStatus(event.data.system.status);
          break;
        case 'deployment_snapshot':
          setDeployments(event.data.deployments);
          break;
        case 'trading_snapshot':
          setTradingSnapshots(event.data.trading);
          break;
        case 'oversight_snapshot':
          setOversightReport(event.data.oversight);
          break;
        case 'proposal_snapshot':
          setProposals(event.data.proposals);
          break;
      }
    });

    return () => {
      unsubConnection();
      unsubscribe();
      ws.disconnect();
    };
  }, [
    setWsConnected,
    addLog,
    addTrade,
    setDeployments,
    setTradingSnapshots,
    setOversightReport,
    setProposals,
    updatePositions,
    updateMarketData,
    setSystemStatus,
  ]);

  return (
    <QueryClientProvider client={queryClient}>
      <BrowserRouter>
        <Suspense fallback={<RouteFallback />}>
          <Routes>
            <Route path="/" element={<Layout />}>
              <Route index element={<Dashboard />} />
              <Route path="trades" element={<TradeHistory />} />
              <Route path="monitor" element={<LiveMonitor />} />
              <Route path="deployments" element={<StrategyMonitor />} />
              <Route path="monitor-strategy" element={<StrategyMonitor />} />
              <Route path="nba-swing" element={<NBASwingMonitor />} />
              <Route path="risk" element={<RiskDashboard />} />
              <Route path="research-runs" element={<ResearchRuns />} />
              <Route path="research-runs/:runId" element={<RunDetail />} />
              <Route path="oversight" element={<OversightAlerts />} />
              <Route path="oversight/proposals/:proposalId" element={<ProposalDetail />} />
              <Route path="control" element={<SystemControl />} />
              <Route path="security" element={<SecurityAudit />} />
            </Route>
          </Routes>
        </Suspense>
      </BrowserRouter>
    </QueryClientProvider>
  );
}

function RouteFallback() {
  return (
    <div className="flex h-screen items-center justify-center bg-background text-muted-foreground">
      Loading console...
    </div>
  );
}

export default App;
