import { useGetIdentity, useLogout, usePermissions } from "@refinedev/core";
import {
  Activity,
  CreditCard,
  Gauge,
  LogOut,
  PanelLeft,
  PlugZap,
  ScrollText,
  Settings2,
  Users,
  KeyRound,
  Radar,
} from "lucide-react";
import { useState } from "react";
import { NavLink, Outlet } from "react-router";

import type { WebIdentity } from "@/auth-provider.ts";
import { Button } from "@/components/ui/button.tsx";
import { cn } from "@/lib/utils.ts";

const primaryNavigation = [
  { to: "/", label: "概览", icon: Gauge, end: true },
  { to: "/provider-accounts", label: "学习平台", icon: PlugZap },
  { to: "/executions", label: "执行记录", icon: Activity },
  { to: "/credits", label: "点数", icon: CreditCard },
];

export function AppLayout() {
  const [mobileOpen, setMobileOpen] = useState(false);
  const identity = useGetIdentity<WebIdentity>();
  const permissions = usePermissions<string[]>({});
  const logout = useLogout();
  const canManageSystem = permissions.data?.includes("manage_system") ?? false;
  const canManageUsers = permissions.data?.includes("manage_users") ?? false;
  const canReadAudit = permissions.data?.some((permission) => permission === "view_any_audit" || permission === "view_own_audit") ?? false;

  return (
    <div className="min-h-screen bg-background">
      <aside
        className={cn(
          "fixed inset-y-0 left-0 z-40 w-64 border-r bg-card transition-transform lg:translate-x-0",
          mobileOpen ? "translate-x-0" : "-translate-x-full",
        )}
      >
        <div className="flex h-16 items-center gap-3 border-b px-5">
          <div className="grid size-9 place-items-center rounded-xl bg-primary font-semibold text-primary-foreground">
            A
          </div>
          <div>
            <div className="font-semibold leading-tight">Asterism</div>
            <div className="text-xs text-muted-foreground">学习任务控制台</div>
          </div>
        </div>
        <nav className="space-y-1 p-3" aria-label="主要导航">
          {primaryNavigation.map((item) => (
            <NavItem key={item.to} {...item} onNavigate={() => setMobileOpen(false)} />
          ))}
          {canManageSystem ? (
            <NavItem
              to="/admin/runtime-settings"
              label="运行设置"
              icon={Settings2}
              onNavigate={() => setMobileOpen(false)}
            />
          ) : null}
          {canManageSystem ? <NavItem to="/admin/protocol-observations" label="协议观察" icon={Radar} onNavigate={() => setMobileOpen(false)} /> : null}
          {canManageSystem ? <NavItem to="/admin/ai-config" label="AI 配置" icon={Settings2} onNavigate={() => setMobileOpen(false)} /> : null}
          {permissions.data?.includes("manage_pricing") ? <NavItem to="/admin/pricing-catalog" label="点数定价" icon={CreditCard} onNavigate={() => setMobileOpen(false)} /> : null}
          {canManageUsers ? <NavItem to="/admin/users" label="用户管理" icon={Users} onNavigate={() => setMobileOpen(false)} /> : null}
          {canReadAudit ? <NavItem to="/admin/audit" label="审计" icon={ScrollText} onNavigate={() => setMobileOpen(false)} /> : null}
          {canManageSystem ? <NavItem to="/admin/service-tokens" label="服务令牌" icon={KeyRound} onNavigate={() => setMobileOpen(false)} /> : null}
        </nav>
        <div className="absolute inset-x-0 bottom-0 border-t p-3">
          <div className="mb-2 rounded-lg bg-muted px-3 py-2">
            <div className="truncate text-sm font-medium">{identity.data?.name ?? "正在验证会话"}</div>
            <div className="truncate text-xs text-muted-foreground">
              {identity.data?.user_id ?? identity.data?.service_token_id ?? "—"}
            </div>
          </div>
          <Button
            variant="ghost"
            className="w-full justify-start"
            disabled={logout.isPending}
            onClick={() => logout.mutate()}
          >
            <LogOut className="size-4" />
            退出登录
          </Button>
        </div>
      </aside>

      {mobileOpen ? (
        <button
          className="fixed inset-0 z-30 bg-black/40 lg:hidden"
          aria-label="关闭导航"
          onClick={() => setMobileOpen(false)}
        />
      ) : null}

      <div className="lg:pl-64">
        <header className="sticky top-0 z-20 flex h-16 items-center border-b bg-background/90 px-4 backdrop-blur lg:px-8">
          <Button
            variant="ghost"
            size="icon"
            className="lg:hidden"
            aria-label="打开导航"
            onClick={() => setMobileOpen(true)}
          >
            <PanelLeft className="size-5" />
          </Button>
          <div className="ml-auto flex items-center gap-2 text-xs text-muted-foreground">
            <span className="size-2 rounded-full bg-primary" />
            本地服务已连接
          </div>
        </header>
        <main className="mx-auto max-w-screen-2xl p-4 sm:p-6 lg:p-8">
          <Outlet />
        </main>
      </div>
    </div>
  );
}

function NavItem({
  to,
  label,
  icon: Icon,
  end,
  onNavigate,
}: {
  to: string;
  label: string;
  icon: typeof Gauge;
  end?: boolean;
  onNavigate: () => void;
}) {
  return (
    <NavLink
      to={to}
      end={end}
      onClick={onNavigate}
      className={({ isActive }) =>
        cn(
          "flex items-center gap-3 rounded-lg px-3 py-2 text-sm font-medium transition-colors",
          isActive ? "bg-primary text-primary-foreground" : "text-muted-foreground hover:bg-muted hover:text-foreground",
        )
      }
    >
      <Icon className="size-4" />
      {label}
    </NavLink>
  );
}
