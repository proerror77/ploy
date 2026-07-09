import { Link, Outlet, useLocation } from 'react-router-dom';
import { useEffect, useRef, useState } from 'react';
import { useQueryClient } from '@tanstack/react-query';
import { cn } from '@/lib/utils';
import { api } from '@/services/api';
import { LiveParityBanner } from '@/components/LiveParityBanner';
import { useStore } from '@/store';
import {
  Bot,
  Brain,
  ClipboardCheck,
  FileText,
  LayoutDashboard,
  History,
  Target,
  TrendingUp,
  ShieldAlert,
  GitCompare,
  Gauge,
  RadioTower,
  ServerCog,
} from 'lucide-react';

function getErrorMessage(error: unknown, fallback: string) {
  if (error instanceof Error && error.message) {
    return error.message;
  }

  return fallback;
}

type NavigationItem = {
  name: string;
  description: string;
  href: string;
  icon: typeof LayoutDashboard;
  aliases?: string[];
};

const navigationSections: Array<{
  label: string;
  description: string;
  items: NavigationItem[];
}> = [
  {
    label: 'Command',
    description: '全局态势',
    items: [
      { name: '总览', description: '账户、状态、P&L', href: '/', icon: LayoutDashboard },
      { name: '运营驾驶舱', description: '关键操作入口', href: '/cockpit', icon: Gauge },
    ],
  },
  {
    label: 'Agentic Research',
    description: '策略发现到证据',
    items: [
      { name: '策略构建器', description: '自动 agent run', href: '/builder', icon: Bot },
      { name: 'Harness Memory', description: '上下文与 proposal', href: '/harness', icon: Brain },
      { name: 'Dry-run 报表', description: '回放与候选证据', href: '/dry-run', icon: FileText, aliases: ['/reports/'] },
      { name: 'Dry/Live 对比', description: '执行路径校验', href: '/parity', icon: GitCompare },
      { name: 'NBA Legacy', description: '体育事件旧链路', href: '/nba-swing', icon: TrendingUp },
    ],
  },
  {
    label: 'Execution',
    description: '部署、订单、运行时',
    items: [
      { name: '部署控制', description: '资源生命周期', href: '/deployments', icon: Target },
      { name: '交易历史', description: '订单与成交', href: '/trades', icon: History },
      { name: '实时日志', description: '事件流与故障', href: '/monitor', icon: RadioTower },
    ],
  },
  {
    label: 'Governance',
    description: '风险、权限、系统',
    items: [
      { name: 'Risk Monitor', description: '风险与限额', href: '/risk', icon: ShieldAlert },
      { name: '系统控制', description: '守护进程控制', href: '/control', icon: ServerCog },
      { name: '安全审计', description: '认证与审计', href: '/security', icon: ClipboardCheck },
    ],
  },
];

const navigationItems = navigationSections.flatMap((section) =>
  section.items.map((item) => ({
    ...item,
    sectionLabel: section.label === 'Agentic Research' ? 'Agentic' : section.label,
  }))
);

function isNavigationItemActive(pathname: string, item: NavigationItem) {
  if (item.href === '/') return pathname === item.href;
  return (
    pathname === item.href ||
    pathname.startsWith(`${item.href}/`) ||
    item.aliases?.some((alias) => pathname.startsWith(alias))
  );
}

export function Layout() {
  const location = useLocation();
  const queryClient = useQueryClient();
  const { wsConnected, systemStatus } = useStore();
  const mobileNavRef = useRef<HTMLElement | null>(null);

  const systemStatusLabel: Record<string, string> = {
    starting: '启动中',
    running: '运行中',
    recovering: '恢复中',
    degraded: '降级',
    stopped: '已停止',
    error: '错误',
  };
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

  useEffect(() => {
    if (typeof window === 'undefined' || window.innerWidth >= 1024) return;

    mobileNavRef.current
      ?.querySelector('[data-active-nav="true"]')
      ?.scrollIntoView({ block: 'nearest', inline: 'center' });
  }, [location.pathname]);

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
    } catch (error: unknown) {
      setAuthError(getErrorMessage(error, '登录失败'));
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
    } catch (error: unknown) {
      setAuthError(getErrorMessage(error, '退出失败'));
    } finally {
      setAuthBusy(false);
    }
  };

  return (
    <div className="flex min-h-screen flex-col bg-background lg:h-screen lg:flex-row">
      {/* Sidebar */}
      <div className="shrink-0 border-b bg-[#f8faf8] text-[#111827] lg:flex lg:h-screen lg:w-[17rem] lg:flex-col lg:border-b-0 lg:border-r lg:border-[#d9e3dd]">
        <div className="border-b border-[#d9e3dd] px-5 py-4">
          <div className="flex items-center justify-between gap-3">
            <div>
              <div className="text-[11px] font-semibold uppercase tracking-[0.18em] text-[#0f8a5f]">
                Ploy
              </div>
              <h1 className="mt-1 text-xl font-semibold leading-none">Agent OS</h1>
            </div>
            <div
              className={cn('rounded-md border px-2.5 py-1 text-xs font-semibold', {
                'border-[#b8d8c7] bg-[#e7f6ee] text-[#047857]': systemStatus === 'running',
                'border-[#fde68a] bg-[#fffbeb] text-[#92400e]':
                  systemStatus === 'starting' || systemStatus === 'recovering' || systemStatus === 'degraded',
                'border-[#fecdd3] bg-[#fff1f2] text-[#be123c]': systemStatus === 'error',
                'border-[#cbd5d1] bg-white text-[#64748b]': systemStatus === 'stopped',
              })}
            >
              {systemStatusLabel[systemStatus] ?? systemStatus}
            </div>
          </div>
          <div className="mt-3 grid grid-cols-[1fr_auto] gap-2 rounded-md border border-[#d9e3dd] bg-white px-3 py-2 text-xs">
            <span className="text-[#64748b]">Control plane</span>
            <span
              className={cn('font-semibold', {
                'text-[#047857]': authStatus === 'authed',
                'text-[#64748b]': authStatus === 'checking',
                'text-[#be123c]': authStatus === 'guest',
              })}
            >
              {authStatus === 'authed' && 'authenticated'}
              {authStatus === 'checking' && 'checking'}
              {authStatus === 'guest' && 'locked'}
            </span>
          </div>
        </div>
        <nav
          ref={mobileNavRef}
          className="flex gap-2 overflow-x-auto border-b border-[#d9e3dd] p-3 lg:hidden"
        >
          {navigationItems.map((item) => {
            const Icon = item.icon;
            const isActive = isNavigationItemActive(location.pathname, item);
            return (
              <Link
                key={item.name}
                to={item.href}
                data-active-nav={isActive ? 'true' : undefined}
                className={cn(
                  'grid w-36 shrink-0 grid-cols-[1.75rem_minmax(0,1fr)] items-center gap-2 rounded-md border px-2.5 py-2 text-left transition-colors',
                  isActive
                    ? 'border-[#b8d8c7] bg-white text-[#111827] shadow-sm'
                    : 'border-transparent bg-[#f1f6f2] text-[#475569]'
                )}
              >
                <span
                  className={cn(
                    'flex h-7 w-7 items-center justify-center rounded-md',
                    isActive ? 'bg-[#e7f6ee] text-[#047857]' : 'bg-white text-[#64748b]'
                  )}
                >
                  <Icon className="h-3.5 w-3.5" />
                </span>
                <span className="min-w-0">
                  <span className="block truncate text-[10px] font-semibold uppercase tracking-[0.12em] text-[#0f8a5f]">
                    {item.sectionLabel}
                  </span>
                  <span className="block truncate text-sm font-semibold">{item.name}</span>
                </span>
              </Link>
            );
          })}
        </nav>

        <nav className="hidden lg:block lg:flex-1 lg:space-y-5 lg:overflow-y-auto lg:p-4">
          {navigationSections.map((section) => (
            <div key={section.label}>
              <div className="mb-2 flex items-end justify-between gap-3 px-1">
                <div>
                  <div className="text-[11px] font-semibold uppercase tracking-[0.16em] text-[#0f8a5f]">
                    {section.label}
                  </div>
                  <div className="mt-0.5 text-xs text-[#64748b]">{section.description}</div>
                </div>
              </div>
              <div className="space-y-1">
                {section.items.map((item) => {
                  const Icon = item.icon;
                  const isActive = isNavigationItemActive(location.pathname, item);
                  return (
                    <Link
                      key={item.name}
                      to={item.href}
                      className={cn(
                        'group grid grid-cols-[2rem_minmax(0,1fr)_0.25rem] items-center gap-3 rounded-md border px-2.5 py-2.5 text-left transition-colors',
                        isActive
                          ? 'border-[#b8d8c7] bg-white text-[#111827] shadow-sm'
                          : 'border-transparent text-[#475569] hover:border-[#d9e3dd] hover:bg-white'
                      )}
                    >
                      <span
                        className={cn(
                          'flex h-8 w-8 items-center justify-center rounded-md',
                          isActive
                            ? 'bg-[#e7f6ee] text-[#047857]'
                            : 'bg-[#eef3ef] text-[#64748b] group-hover:text-[#0f8a5f]'
                        )}
                      >
                        <Icon className="h-4 w-4" />
                      </span>
                      <span className="min-w-0">
                        <span className="block truncate text-sm font-semibold">{item.name}</span>
                        <span className="mt-0.5 block truncate text-[11px] text-[#64748b]">
                          {item.description}
                        </span>
                      </span>
                      <span
                        className={cn(
                          'h-9 rounded-full',
                          isActive ? 'bg-[#0f8a5f]' : 'bg-transparent group-hover:bg-[#d9e3dd]'
                        )}
                      />
                    </Link>
                  );
                })}
              </div>
            </div>
          ))}
        </nav>

        {/* Auth + Status indicators */}
        <div className="border-t border-[#d9e3dd] bg-[#fbfdf9] p-4">
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
              <form
                className="space-y-2"
                onSubmit={(event) => {
                  event.preventDefault();
                  void login();
                }}
              >
                <input
                  id="ploy-admin-token"
                  name="ploy_admin_token"
                  type="password"
                  value={adminToken}
                  onChange={(e) => setAdminToken(e.target.value)}
                  placeholder="Admin token"
                  className="w-full rounded border bg-background px-2 py-1 text-xs"
                  autoComplete="off"
                />
                <button
                  type="submit"
                  disabled={authBusy}
                  className="w-full rounded bg-primary px-2 py-1 text-xs font-medium text-primary-foreground disabled:opacity-50"
                >
                  {authBusy ? '认证中...' : '登录'}
                </button>
              </form>
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
                  'text-amber-600': systemStatus === 'starting' || systemStatus === 'recovering',
                  'text-success': systemStatus === 'running',
                  'text-yellow-600': systemStatus === 'degraded',
                  'text-muted-foreground': systemStatus === 'stopped',
                  'text-destructive': systemStatus === 'error',
                })}
              >
                {systemStatusLabel[systemStatus] ?? systemStatus}
              </span>
            </div>
          </div>
        </div>
      </div>

      {/* Main content */}
      <div className="min-w-0 flex-1 overflow-auto">
        <LiveParityBanner />
        <Outlet />
      </div>
    </div>
  );
}
