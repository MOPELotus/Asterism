import { useList, usePermissions } from "@refinedev/core";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { CalendarClock, KeyRound, RefreshCw, Search } from "lucide-react";
import { type FormEvent, useEffect, useMemo, useState } from "react";
import { useParams } from "react-router";

import {
  beginProviderAccountAuthSession,
  configureProviderAccountScanSchedule,
  getLatestProviderAccountAuthSession,
  getProviderAccount,
  getProviderAccountScanSchedule,
  scanProviderAccount,
  submitProviderAccountAuthSessionCredentials,
} from "@/api/generated/sdk.gen.ts";
import type { AuthMethod, ProviderMetadata, SessionKind } from "@/api/generated/types.gen.ts";
import { AsterismApiError, requireData } from "@/api/result.ts";
import { PageShell } from "@/components/page-shell.tsx";
import { QueryError, TableSkeleton } from "@/components/query-feedback.tsx";
import { StateBadge } from "@/components/state-badge.tsx";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert.tsx";
import { Badge } from "@/components/ui/badge.tsx";
import { Button } from "@/components/ui/button.tsx";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card.tsx";
import { Input } from "@/components/ui/input.tsx";
import { Label } from "@/components/ui/label.tsx";
import { formatTimestamp } from "@/lib/format.ts";

const selectClassName = "h-9 w-full rounded-md border border-input bg-background px-3 text-sm outline-none focus:ring-2 focus:ring-ring disabled:opacity-50";
const supportedInlineMethods: AuthMethod[] = ["password", "imported_cookie", "imported_token"];

export function ProviderAccountDetailPage() {
  const { accountId = "" } = useParams();
  const providers = useList<ProviderMetadata>({ resource: "providers", pagination: { pageSize: 100 } });
  const permissions = usePermissions<string[]>({});
  const canManageSystem = permissions.data?.includes("manage_system") ?? false;
  const queryClient = useQueryClient();

  const account = useQuery({ queryKey: ["provider-accounts", accountId], enabled: Boolean(accountId), queryFn: async () => requireData(await getProviderAccount({ path: { account_id: accountId } })) });
  const latestAuth = useQuery({ queryKey: ["provider-accounts", accountId, "auth-session"], enabled: Boolean(accountId), retry: false, queryFn: async () => optionalNotFound(getLatestProviderAccountAuthSession({ path: { account_id: accountId } })) });
  const schedule = useQuery({ queryKey: ["provider-accounts", accountId, "scan-schedule"], enabled: Boolean(accountId && canManageSystem), retry: false, queryFn: async () => optionalNotFound(getProviderAccountScanSchedule({ path: { account_id: accountId } })) });
  const provider = providers.result.data?.find((item) => item.id === account.data?.provider_id);
  const authMethods = provider?.auth_methods.filter((method) => supportedInlineMethods.includes(method)) ?? [];

  const [method, setMethod] = useState<AuthMethod>("password");
  const [username, setUsername] = useState("");
  const [password, setPassword] = useState("");
  const [credential, setCredential] = useState("");
  const [tokenPurpose, setTokenPurpose] = useState<"provider_access_token" | "provider_composite_session">("provider_access_token");
  const [sessionKind, setSessionKind] = useState<SessionKind>("provider_specific");
  const [scanReport, setScanReport] = useState<string | null>(null);
  const [scheduleEnabled, setScheduleEnabled] = useState(true);
  const [scheduleInterval, setScheduleInterval] = useState("");

  useEffect(() => {
    if (authMethods.length && !authMethods.includes(method)) setMethod(authMethods[0] ?? "password");
  }, [authMethods, method]);
  useEffect(() => {
    if (!provider || method !== "imported_token") return;
    const prefersJwt = provider.session_kinds.includes("jwt");
    setSessionKind(prefersJwt ? "jwt" : provider.session_kinds[0] ?? "provider_specific");
    setTokenPurpose(prefersJwt ? "provider_composite_session" : "provider_access_token");
  }, [method, provider]);
  useEffect(() => {
    if (!schedule.data) return;
    setScheduleEnabled(schedule.data.enabled);
    setScheduleInterval(String(schedule.data.desired_interval_seconds));
  }, [schedule.data]);

  const authenticate = useMutation({
    mutationFn: async () => {
      if (!provider) throw new Error("Provider metadata 尚未加载");
      const started = requireData(await beginProviderAccountAuthSession({ path: { account_id: accountId }, body: { method } }));
      const submitted = requireData(await submitProviderAccountAuthSessionCredentials({
        path: { account_id: accountId, session_id: started.session.id },
        body: credentialRequest(method, username, password, credential, tokenPurpose, sessionKind),
      }));
      return submitted;
    },
    onSuccess: async () => {
      setUsername(""); setPassword(""); setCredential("");
      await Promise.all([account.refetch(), latestAuth.refetch()]);
    },
    onSettled: () => setPassword(""),
  });
  const scan = useMutation({
    mutationFn: async () => requireData(await scanProviderAccount({ path: { account_id: accountId } })),
    onSuccess: (report) => {
      setScanReport(`课程 ${report.courses_seen}，新任务 ${report.tasks_created}，更新 ${report.tasks_updated}，未变 ${report.tasks_unchanged}`);
      void queryClient.invalidateQueries({ queryKey: ["tasks"] });
    },
  });
  const saveSchedule = useMutation({
    mutationFn: async () => requireData(await configureProviderAccountScanSchedule({ path: { account_id: accountId }, body: { enabled: scheduleEnabled, ...(scheduleInterval ? { desired_interval_seconds: Number(scheduleInterval) } : {}) } })),
    onSuccess: (value) => queryClient.setQueryData(["provider-accounts", accountId, "scan-schedule"], value),
  });

  const error = account.error ?? providers.query.error ?? (latestAuth.error instanceof AsterismApiError && latestAuth.error.statusCode === 404 ? null : latestAuth.error) ?? authenticate.error ?? scan.error ?? saveSchedule.error;
  const canSubmitCredential = method === "password" ? Boolean(username.trim() && password) : Boolean(credential.trim());
  const compatibleSessionKinds = useMemo(() => provider?.session_kinds ?? [], [provider]);

  if (account.isLoading) return <PageShell title="平台账号" description="正在读取账号状态。"><TableSkeleton /></PageShell>;
  if (!account.data) return <PageShell title="平台账号" description="账号不存在或当前身份不可访问。">{error ? <QueryError error={error} /> : null}</PageShell>;

  return <PageShell title={account.data.display_name} description={`${account.data.provider_id} · ${account.data.tenant ?? "默认租户"}`} actions={<Button variant="outline" disabled={scan.isPending || account.data.auth_state.state !== "authenticated"} onClick={() => scan.mutate()}><Search className="size-4" />{scan.isPending ? "巡查中…" : "立即巡查"}</Button>}>
    {error ? <QueryError error={error} /> : null}
    {scanReport ? <Alert><AlertTitle>巡查完成</AlertTitle><AlertDescription>{scanReport}</AlertDescription></Alert> : null}
    <div className="grid gap-4 sm:grid-cols-3">
      <Summary label="认证状态"><StateBadge state={account.data.auth_state.state} /></Summary>
      <Summary label="凭据数量">{account.data.credential_count}</Summary>
      <Summary label="更新时间">{formatTimestamp(account.data.updated_at)}</Summary>
    </div>

    <Card><CardHeader><CardTitle className="flex items-center gap-2"><KeyRound className="size-5" />Provider 认证</CardTitle></CardHeader><CardContent className="space-y-4">
      {latestAuth.data ? <div className="flex flex-wrap items-center gap-2 rounded-lg bg-muted p-3 text-sm"><span>最近会话</span><StateBadge state={latestAuth.data.state.state} /><Badge variant="outline">revision {latestAuth.data.revision}</Badge><span className="text-muted-foreground">{formatTimestamp(latestAuth.data.updated_at)}</span></div> : null}
      {!authMethods.length ? <Alert><AlertTitle>暂无内联认证方法</AlertTitle><AlertDescription>该 Provider 当前只支持后置的 Capture/外部流程，第一批暂不在 WebUI 开启。</AlertDescription></Alert> : (
        <form className="grid gap-4 md:grid-cols-2" onSubmit={(event: FormEvent) => { event.preventDefault(); authenticate.mutate(); }}>
          <div className="space-y-2 md:col-span-2"><Label htmlFor="auth-method">认证方法</Label><select id="auth-method" className={selectClassName} value={method} onChange={(event) => setMethod(event.target.value as AuthMethod)}>{authMethods.map((item) => <option key={item} value={item}>{authMethodLabel(item)}</option>)}</select></div>
          {method === "password" ? <><div className="space-y-2"><Label htmlFor="provider-username">Provider 用户名</Label><Input id="provider-username" autoComplete="username" value={username} onChange={(event) => setUsername(event.target.value)} /></div><div className="space-y-2"><Label htmlFor="provider-password">Provider 密码</Label><Input id="provider-password" type="password" autoComplete="current-password" value={password} onChange={(event) => setPassword(event.target.value)} /></div></> : null}
          {method === "imported_cookie" ? <div className="space-y-2 md:col-span-2"><Label htmlFor="provider-cookie">Cookie</Label><Input id="provider-cookie" type="password" autoComplete="off" value={credential} onChange={(event) => setCredential(event.target.value)} /></div> : null}
          {method === "imported_token" ? <><div className="space-y-2"><Label htmlFor="token-purpose">令牌格式</Label><select id="token-purpose" className={selectClassName} value={tokenPurpose} onChange={(event) => setTokenPurpose(event.target.value as typeof tokenPurpose)}><option value="provider_access_token">Access Token</option><option value="provider_composite_session">复合会话 JSON</option></select></div><div className="space-y-2"><Label htmlFor="session-kind">会话种类</Label><select id="session-kind" className={selectClassName} value={sessionKind} onChange={(event) => setSessionKind(event.target.value as SessionKind)}>{compatibleSessionKinds.map((kind) => <option key={kind} value={kind}>{kind}</option>)}</select></div><div className="space-y-2 md:col-span-2"><Label htmlFor="provider-token">令牌内容</Label><Input id="provider-token" type="password" autoComplete="off" value={credential} onChange={(event) => setCredential(event.target.value)} /></div></> : null}
          <div className="md:col-span-2"><Button type="submit" disabled={authenticate.isPending || !canSubmitCredential}><RefreshCw className="size-4" />{authenticate.isPending ? "验证并保存中…" : "验证并保存凭据"}</Button></div>
        </form>
      )}
    </CardContent></Card>

    {canManageSystem ? <Card><CardHeader><CardTitle className="flex items-center gap-2"><CalendarClock className="size-5" />定期巡查</CardTitle></CardHeader><CardContent className="space-y-4">
      {schedule.isLoading ? <TableSkeleton /> : <><label className="flex items-center gap-2 text-sm"><input type="checkbox" checked={scheduleEnabled} onChange={(event) => setScheduleEnabled(event.target.checked)} />启用章节新增及其他任务的定期巡查</label><div className="max-w-sm space-y-2"><Label htmlFor="scan-interval">期望间隔（秒；留空快照当前 Provider/账号默认）</Label><Input id="scan-interval" type="number" min={1} value={scheduleInterval} onChange={(event) => setScheduleInterval(event.target.value)} /></div>{schedule.data ? <p className="text-sm text-muted-foreground">最终间隔 {schedule.data.effective_interval_seconds} 秒；下次 {formatTimestamp(schedule.data.next_run_at)}</p> : null}<Button disabled={saveSchedule.isPending} onClick={() => saveSchedule.mutate()}>{saveSchedule.isPending ? "保存中…" : "保存巡查计划"}</Button></>}
    </CardContent></Card> : null}
  </PageShell>;
}

function credentialRequest(method: AuthMethod, username: string, password: string, credential: string, tokenPurpose: "provider_access_token" | "provider_composite_session", sessionKind: SessionKind) {
  if (method === "password") return { auth_method: method, acquired_via: "native_provider_login" as const, session_kind: "provider_specific" as const, fields: [{ purpose: "provider_username" as const, value: username.trim() }, { purpose: "provider_password" as const, value: password }] };
  if (method === "imported_cookie") return { auth_method: method, acquired_via: "manual_import" as const, session_kind: "cookie" as const, fields: [{ purpose: "provider_cookie" as const, value: credential }] };
  return { auth_method: method, acquired_via: "manual_import" as const, session_kind: sessionKind, fields: [{ purpose: tokenPurpose, value: credential }] };
}

async function optionalNotFound<T>(request: Promise<{ data?: T; error?: unknown; response?: Response }>): Promise<T | null> {
  try { return requireData(await request); } catch (error) { if (error instanceof AsterismApiError && error.statusCode === 404) return null; throw error; }
}

function Summary({ label, children }: { label: string; children: React.ReactNode }) { return <Card><CardHeader className="pb-2"><CardTitle className="text-sm text-muted-foreground">{label}</CardTitle></CardHeader><CardContent className="font-medium">{children}</CardContent></Card>; }
function authMethodLabel(method: AuthMethod) { return method === "password" ? "密码登录" : method === "imported_cookie" ? "导入 Cookie" : method === "imported_token" ? "导入令牌" : method; }
