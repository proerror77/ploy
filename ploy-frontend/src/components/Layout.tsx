import { Link, Outlet, useLocation } from 'react-router-dom';
import { useEffect, useState } from 'react';
import { useQueryClient } from '@tanstack/react-query';
import { cn } from '@/lib/utils';
import { api } from '@/services/api';
import { useStore } from '@/store';
import {
  LayoutDashboard,
  History,
  Activity,
  Target,
  Power,
  Shield,
  TrendingUp,
  ShieldAlert,
} from 'lucide-react';

const navigation = [
  { name: '仪表盘', href: '/', icon: LayoutDashboard },
  { name: '交易历史', href: '/trades', icon: History },
  { name: '实时日志', href: '/monitor', icon: Activity },
  { name: '策略监控', href: '/monitor-strategy', icon: Target },
  { name: 'NBA Swing', href: '/nba-swing', icon: TrendingUp },
  { name: 'Risk Monitor', href: '/risk', icon: ShieldAlert },
  { name: '系统控制', href: '/control', icon: Power },
  { name: '安全审计', href: '/security', icon: Shield },
];

export function Layout() {
  const location = useLocation();
  const queryClient = useQueryClient();
  const { wsConnected, systemStatus } = useStore();
  const [authStatus, setAuthStatus] = useState<'checking' | 'authed' | 'guest'>('checking');
  const [authBusy, setAuthBusy] = useState(false);
  const [authError, setAuthError] = useState('');
  const [adminToken, setAdminToken] = useState('');

  useEffect(() => {
    let active = true;
    api
      .getAuthSession()
      .then((session) => {
        if (!active) return;
        setAuthStatus(session.authenticated ? 'authed' : 'guest');
      })
      .catch(() => {
        if (!active) return;
        setAuthStatus('guest');
      });
    return () => {
      active = false;
    };
  }, []);

  const refreshAfterAuthChange = async () => {
    await queryClient.invalidateQueries();
  };

  const login = async () => {
    if (!adminToken.trim()) {
      setAuthError('请输入 Admin Token');
      return;
    }
    setAuthBusy(true);
    setAuthError('');
    try {
      await api.login(adminToken.trim());
      setAdminToken('');
      setAuthStatus('authed');
      await refreshAfterAuthChange();
    } catch (error: any) {
      setAuthError(error?.message ?? '登录失败');
    } finally {
      setAuthBusy(false);
    }
  };

  const logout = async () => {
    setAuthBusy(true);
    setAuthError('');
    try {
      await api.logout();
      setAuthStatus('guest');
      await refreshAfterAuthChange();
    } catch (error: any) {
      setAuthError(error?.message ?? '退出失败');
    } finally {
      setAuthBusy(false);
    }
  };

  return (
    <div className="flex h-screen bg-background">
      {/* Sidebar */}
      <div className="w-64 border-r bg-card">
        <div className="flex h-16 items-center border-b px-6">
          <h1 className="text-xl font-bold">Ploy Trading</h1>
        </div>
        <nav className="space-y-1 p-4">
          {navigation.map((item) => {
            const Icon = item.icon;
            const isActive = location.pathname === item.href;
            return (
              <Link
                key={item.name}
                to={item.href}
                className={cn(
                  'flex items-center gap-3 rounded-lg px-3 py-2 text-sm font-medium transition-colors',
                  isActive
                    ? 'bg-primary text-primary-foreground'
                    : 'text-muted-foreground hover:bg-accent hover:text-accent-foreground'
                )}
              >
                <Icon className="h-5 w-5" />
                {item.name}
              </Link>
            );
          })}
        </nav>

        {/* Auth + Status indicators */}
        <div className="absolute bottom-0 w-64 border-t bg-card p-4">
          <div className="mb-3 border-b pb-3">
            <div className="mb-2 flex items-center justify-between text-xs">
              <span className="text-muted-foreground">控制面认证</span>
              <span
                className={cn('font-medium', {
                  'text-success': authStatus === 'authed',
                  'text-muted-foreground': authStatus === 'checking',
                  'text-destructive': authStatus === 'guest',
                })}
              >
                {authStatus === 'authed' && '已认证'}
                {authStatus === 'checking' && '检查中'}
                {authStatus === 'guest' && '未认证'}
              </span>
            </div>
            {authStatus !== 'authed' ? (
              <div className="space-y-2">
                <input
                  type="password"
                  value={adminToken}
                  onChange={(e) => setAdminToken(e.target.value)}
                  placeholder="Admin token"
                  className="w-full rounded border bg-background px-2 py-1 text-xs"
                  autoComplete="off"
                />
                <button
                  onClick={login}
                  disabled={authBusy}
                  className="w-full rounded bg-primary px-2 py-1 text-xs font-medium text-primary-foreground disabled:opacity-50"
                >
                  {authBusy ? '认证中...' : '登录'}
                </button>
              </div>
            ) : (
              <button
                onClick={logout}
                disabled={authBusy}
                className="w-full rounded border px-2 py-1 text-xs disabled:opacity-50"
              >
                {authBusy ? '处理中...' : '退出'}
              </button>
            )}
            {authError && <p className="mt-2 text-xs text-destructive">{authError}</p>}
          </div>

          <div className="space-y-2 text-sm">
            <div className="flex items-center justify-between">
              <span className="text-muted-foreground">WebSocket</span>
              <div
                className={cn('h-2 w-2 rounded-full', {
                  'bg-success': wsConnected,
                  'bg-destructive': !wsConnected,
                })}
              />
            </div>
            <div className="flex items-center justify-between">
              <span className="text-muted-foreground">系统状态</span>
              <span
                className={cn('text-xs font-medium', {
                  'text-success': systemStatus === 'running',
                  'text-muted-foreground': systemStatus === 'stopped',
                  'text-destructive': systemStatus === 'error',
                })}
              >
                {systemStatus === 'running' && '运行中'}
                {systemStatus === 'stopped' && '已停止'}
                {systemStatus === 'error' && '错误'}
              </span>
            </div>
          </div>
        </div>
      </div>

      {/* Main content */}
      <div className="flex-1 overflow-auto">
        <Outlet />
      </div>
    </div>
  );
}
