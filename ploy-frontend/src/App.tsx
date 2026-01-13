import { BrowserRouter, Routes, Route } from 'react-router-dom';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { useEffect } from 'react';
import { Layout } from '@/components/Layout';
import { Dashboard } from '@/pages/Dashboard';
import { TradeHistory } from '@/pages/TradeHistory';
import { LiveMonitor } from '@/pages/LiveMonitor';
import { StrategyMonitor } from '@/pages/StrategyMonitor';
import { SystemControl } from '@/pages/SystemControl';
import { SecurityAudit } from '@/pages/SecurityAudit';
import { NBASwingMonitor } from '@/pages/NBASwingMonitor';
import { ws } from '@/services/websocket';
import { useStore } from '@/store';

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
    updatePositions,
    updateMarketData,
    setSystemStatus,
  } = useStore();

  useEffect(() => {
    // Connect to WebSocket
    ws.connect();

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
          // Update positions list (in real implementation, this would merge/update)
          updatePositions([event.data]);
          break;
        case 'market':
          updateMarketData(event.data);
          break;
        case 'status':
          setSystemStatus(event.data.status);
          break;
      }
    });

    return () => {
      unsubscribe();
      ws.disconnect();
    };
  }, [
    setWsConnected,
    addLog,
    addTrade,
    updatePositions,
    updateMarketData,
    setSystemStatus,
  ]);

  return (
    <QueryClientProvider client={queryClient}>
      <BrowserRouter>
        <Routes>
          <Route path="/" element={<Layout />}>
            <Route index element={<Dashboard />} />
            <Route path="trades" element={<TradeHistory />} />
            <Route path="monitor" element={<LiveMonitor />} />
            <Route path="monitor-strategy" element={<StrategyMonitor />} />
            <Route path="nba-swing" element={<NBASwingMonitor />} />
            <Route path="control" element={<SystemControl />} />
            <Route path="security" element={<SecurityAudit />} />
          </Route>
        </Routes>
      </BrowserRouter>
    </QueryClientProvider>
  );
}

export default App;
