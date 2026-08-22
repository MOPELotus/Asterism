import { useList, usePermissions } from "@refinedev/core";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { CalendarClock, KeyRound, RefreshCw, Search } from "lucide-react";
import { type FormEvent, useEffect, useMemo, useState } from "react";
import { Link, useParams, useSearchParams } from "react-router";

import {
  beginProviderAccountAuthSession,
  configureProviderAccountScanSchedule,
  createAuthBootstrapSession,
  createWellearnBatchExecution,
  getAuthBootstrapSession,
  getLatestProviderAccountAuthSession,
  getProviderAccount,
  getProviderAccountScanSchedule,
  listProviderCaptureRecipes,
  pollProviderAccountInteractiveAuthSession,
  scanProviderAccount,
  submitProviderAccountExternalOauthCallback,
  submitProviderAccountAuthSessionCredentials,
} from "@/api/generated/sdk.gen.ts";
import type { AuthBootstrapCreateResponse, AuthSessionBeginResponse, AuthMethod, ProviderMetadata, SessionKind } from "@/api/generated/types.gen.ts";
import { AsterismApiError, requireData } from "@/api/result.ts";
import { PageShell } from "@/components/page-shell.tsx";
import { QueryError, TableSkeleton } from "@/components/query-feedback.tsx";
import { StateBadge } from "@/components/state-badge.tsx";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert.tsx";
import { Badge } from "@/components/ui/badge.tsx";
import { Button, buttonVariants } from "@/components/ui/button.tsx";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card.tsx";
import { Input } from "@/components/ui/input.tsx";
import { Label } from "@/components/ui/label.tsx";
import { formatTimestamp } from "@/lib/format.ts";

const selectClassName = "h-9 w-full rounded-md border border-input bg-background px-3 text-sm outline-none focus:ring-2 focus:ring-ring disabled:opacity-50";
const supportedInlineMethods: AuthMethod[] = ["password", "imported_cookie", "imported_token"];

export function ProviderAccountDetailPage() {
  const { accountId = "" } = useParams();
  const [searchParams] = useSearchParams();
  const providers = useList<ProviderMetadata>({ resource: "providers", pagination: { pageSize: 100 } });
  const permissions = usePermissions<string[]>({});
  const canManageSystem = permissions.data?.includes("manage_system") ?? false;
  const queryClient = useQueryClient();

  const account = useQuery({ queryKey: ["provider-accounts", accountId], enabled: Boolean(accountId), queryFn: async () => requireData(await getProviderAccount({ path: { account_id: accountId } })) });
  const latestAuth = useQuery({ queryKey: ["provider-accounts", accountId, "auth-session"], enabled: Boolean(accountId), retry: false, queryFn: async () => optionalNotFound(getLatestProviderAccountAuthSession({ path: { account_id: accountId } })) });
  const schedule = useQuery({ queryKey: ["provider-accounts", accountId, "scan-schedule"], enabled: Boolean(accountId && canManageSystem), retry: false, queryFn: async () => optionalNotFound(getProviderAccountScanSchedule({ path: { account_id: accountId } })) });
  const provider = providers.result.data?.find((item) => item.id === account.data?.provider_id);
  const authMethods = provider?.auth_methods ?? [];
  const captureRecipes = useQuery({ queryKey: ["providers", provider?.id, "capture-recipes"], enabled: Boolean(provider?.id), retry: false, queryFn: async () => requireData(await listProviderCaptureRecipes({ path: { provider_id: provider!.id } })) });

  const [method, setMethod] = useState<AuthMethod>("password");
  const [username, setUsername] = useState("");
  const [password, setPassword] = useState("");
  const [credential, setCredential] = useState("");
  const [tokenPurpose, setTokenPurpose] = useState<"provider_access_token" | "provider_composite_session">("provider_access_token");
  const [sessionKind, setSessionKind] = useState<SessionKind>("provider_specific");
  const [scanReport, setScanReport] = useState<string | null>(null);
  const [scheduleEnabled, setScheduleEnabled] = useState(true);
  const [scheduleInterval, setScheduleInterval] = useState("");
  const [interactive, setInteractive] = useState<AuthSessionBeginResponse | null>(null);
  const [oauthCallback, setOauthCallback] = useState("");
  const [capture, setCapture] = useState<AuthBootstrapCreateResponse | null>(null);
  const [batchCourseId, setBatchCourseId] = useState(() => searchParams.get("courseId") ?? "");
  const [batchRemoteCourseId, setBatchRemoteCourseId] = useState(() => searchParams.get("remoteCourseId") ?? "");
  const [batchRemoteTaskId, setBatchRemoteTaskId] = useState("");
  const [batchFlow, setBatchFlow] = useState<"fanyuchang_duration" | "auto_duration">("fanyuchang_duration");
  const [batchUnitIndices, setBatchUnitIndices] = useState("");
  const [batchChildCount, setBatchChildCount] = useState("");
  const [batchTargets, setBatchTargets] = useState("");
  const [batchConfiguredMinutes, setBatchConfiguredMinutes] = useState("30");
  const [batchRandomRange, setBatchRandomRange] = useState("0");
  const [batchSampledOffset, setBatchSampledOffset] = useState("0");
  const [batchResult, setBatchResult] = useState<string | null>(null);

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
      if (!supportedInlineMethods.includes(method)) return started;
      return requireData(await submitProviderAccountAuthSessionCredentials({
        path: { account_id: accountId, session_id: started.session.id },
        body: credentialRequest(method, username, password, credential, tokenPurpose, sessionKind),
      }));
    },
    onSuccess: async (result) => {
      setUsername(""); setPassword(""); setCredential("");
      if ("challenge" in result) {
        setInteractive(result);
        const authorizationUrl = result.challenge.external_oauth?.authorization_url;
        if (authorizationUrl) window.open(authorizationUrl, "_blank", "noopener,noreferrer");
      } else {
        setInteractive(null);
      }
      await Promise.all([account.refetch(), latestAuth.refetch()]);
    },
    onSettled: () => setPassword(""),
  });
  const pollInteractive = useMutation({
    mutationFn: async () => {
      if (!interactive) throw new Error("没有进行中的交互认证会话");
      return requireData(await pollProviderAccountInteractiveAuthSession({ path: { account_id: accountId, session_id: interactive.session.id } }));
    },
    onSuccess: async (result) => {
      setInteractive(result.challenge ? { session: result.session, challenge: result.challenge } : null);
      await Promise.all([account.refetch(), latestAuth.refetch()]);
    },
  });
  const completeOauth = useMutation({
    mutationFn: async () => {
      if (!interactive || !oauthCallback.trim()) throw new Error("请粘贴浏览器最终回调 URL");
      return requireData(await submitProviderAccountExternalOauthCallback({ path: { account_id: accountId, session_id: interactive.session.id }, body: { callback_url: oauthCallback.trim() } }));
    },
    onSuccess: async () => { setOauthCallback(""); setInteractive(null); await Promise.all([account.refetch(), latestAuth.refetch()]); },
  });
  const startCapture = useMutation({
    mutationFn: async () => {
      if (!provider) throw new Error("Provider metadata 尚未加载");
      return requireData(await createAuthBootstrapSession({ body: { provider_id: provider.id, provider_account_id: accountId, purpose: account.data?.auth_state.state === "authenticated" ? "repair_session" : "reauthenticate", ...(captureRecipes.data?.items[0] ? { recipe_version: captureRecipes.data.items[0].version } : {}) } }));
    },
    onSuccess: setCapture,
  });
  const refreshCapture = useMutation({
    mutationFn: async () => {
      if (!capture) throw new Error("没有进行中的 Capture 会话");
      return requireData(await getAuthBootstrapSession({ path: { session_id: capture.session.id } }));
    },
    onSuccess: async (session) => { setCapture((current) => current ? { ...current, session } : current); if (session.state === "completed") await account.refetch(); },
  });
  const createBatch = useMutation({
    mutationFn: async () => {
      const expectedChildCount = Number(batchChildCount);
      const selectedUnitIndices = parseNumberList(batchUnitIndices);
      const duration = batchFlow === "fanyuchang_duration"
        ? { kind: "per_child_seconds" as const, target_seconds: parseNumberList(batchTargets) }
        : { kind: "auto_aggregate" as const, configured_minutes: Number(batchConfiguredMinutes), random_range_minutes: Number(batchRandomRange), sampled_offset_minutes: Number(batchSampledOffset) };
      return requireData(await createWellearnBatchExecution({
        path: { account_id: accountId, course_id: batchCourseId.trim() },
        headers: { "Idempotency-Key": crypto.randomUUID() },
        body: {
          course_remote_id: batchRemoteCourseId.trim(),
          expected_remote_task_id: batchRemoteTaskId.trim(),
          flow: batchFlow,
          expected_child_count: expectedChildCount,
          ...(selectedUnitIndices.length ? { selected_unit_indices: selectedUnitIndices } : {}),
          duration,
        },
      }));
    },
    onSuccess: (result) => setBatchResult(`${result.created ? "已创建" : "已复用"}批执行 ${result.batch_execution.id}`),
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

  const error = account.error ?? providers.query.error ?? captureRecipes.error ?? (latestAuth.error instanceof AsterismApiError && latestAuth.error.statusCode === 404 ? null : latestAuth.error) ?? authenticate.error ?? pollInteractive.error ?? completeOauth.error ?? startCapture.error ?? refreshCapture.error ?? createBatch.error ?? scan.error ?? saveSchedule.error;
  const canSubmitCredential = !supportedInlineMethods.includes(method) || (method === "password" ? Boolean(username.trim() && password) : Boolean(credential.trim()));
  const compatibleSessionKinds = useMemo(() => provider?.session_kinds ?? [], [provider]);

  if (account.isLoading) return <PageShell title="平台账号" description="正在读取账号状态。"><TableSkeleton /></PageShell>;
  if (!account.data) return <PageShell title="平台账号" description="账号不存在或当前身份不可访问。">{error ? <QueryError error={error} /> : null}</PageShell>;

  return <PageShell title={account.data.display_name} description={`${account.data.provider_id} · ${account.data.tenant ?? "默认租户"}`} actions={<div className="flex flex-wrap gap-2"><Link className={buttonVariants({ variant: "outline" })} to={`/courses?provider_account_id=${accountId}`}>查看课程</Link><Button variant="outline" disabled={scan.isPending || account.data.auth_state.state !== "authenticated"} onClick={() => scan.mutate()}><Search className="size-4" />{scan.isPending ? "巡查中…" : "立即巡查"}</Button></div>}>
    {error ? <QueryError error={error} /> : null}
    {scanReport ? <Alert><AlertTitle>巡查完成</AlertTitle><AlertDescription>{scanReport}</AlertDescription></Alert> : null}
    <div className="grid gap-4 sm:grid-cols-3">
      <Summary label="认证状态"><StateBadge state={account.data.auth_state.state} /></Summary>
      <Summary label="凭据数量">{account.data.credential_count}</Summary>
      <Summary label="更新时间">{formatTimestamp(account.data.updated_at)}</Summary>
    </div>

    <Card><CardHeader><CardTitle className="flex items-center gap-2"><KeyRound className="size-5" />Provider 认证</CardTitle></CardHeader><CardContent className="space-y-4">
      {latestAuth.data ? <div className="flex flex-wrap items-center gap-2 rounded-lg bg-muted p-3 text-sm"><span>最近会话</span><StateBadge state={latestAuth.data.state.state} /><Badge variant="outline">revision {latestAuth.data.revision}</Badge><span className="text-muted-foreground">{formatTimestamp(latestAuth.data.updated_at)}</span></div> : null}
      {!authMethods.length ? <Alert><AlertTitle>暂无认证方法</AlertTitle><AlertDescription>该 Provider 当前没有声明可用的认证入口。</AlertDescription></Alert> : (
        <form className="grid gap-4 md:grid-cols-2" onSubmit={(event: FormEvent) => { event.preventDefault(); authenticate.mutate(); }}>
          <div className="space-y-2 md:col-span-2"><Label htmlFor="auth-method">认证方法</Label><select id="auth-method" className={selectClassName} value={method} onChange={(event) => setMethod(event.target.value as AuthMethod)}>{authMethods.map((item) => <option key={item} value={item}>{authMethodLabel(item)}</option>)}</select></div>
          {method === "password" ? <><div className="space-y-2"><Label htmlFor="provider-username">Provider 用户名</Label><Input id="provider-username" autoComplete="username" value={username} onChange={(event) => setUsername(event.target.value)} /></div><div className="space-y-2"><Label htmlFor="provider-password">Provider 密码</Label><Input id="provider-password" type="password" autoComplete="current-password" value={password} onChange={(event) => setPassword(event.target.value)} /></div></> : null}
          {method === "imported_cookie" ? <div className="space-y-2 md:col-span-2"><Label htmlFor="provider-cookie">Cookie</Label><Input id="provider-cookie" type="password" autoComplete="off" value={credential} onChange={(event) => setCredential(event.target.value)} /></div> : null}
          {method === "imported_token" ? <><div className="space-y-2"><Label htmlFor="token-purpose">令牌格式</Label><select id="token-purpose" className={selectClassName} value={tokenPurpose} onChange={(event) => setTokenPurpose(event.target.value as typeof tokenPurpose)}><option value="provider_access_token">Access Token</option><option value="provider_composite_session">复合会话 JSON</option></select></div><div className="space-y-2"><Label htmlFor="session-kind">会话种类</Label><select id="session-kind" className={selectClassName} value={sessionKind} onChange={(event) => setSessionKind(event.target.value as SessionKind)}>{compatibleSessionKinds.map((kind) => <option key={kind} value={kind}>{kind}</option>)}</select></div><div className="space-y-2 md:col-span-2"><Label htmlFor="provider-token">令牌内容</Label><Input id="provider-token" type="password" autoComplete="off" value={credential} onChange={(event) => setCredential(event.target.value)} /></div></> : null}
          {!supportedInlineMethods.includes(method) ? <Alert className="md:col-span-2"><AlertTitle>交互认证</AlertTitle><AlertDescription>开始后按 Provider 返回的指引完成扫码、浏览器授权或本地辅助流程，再回到这里轮询结果。</AlertDescription></Alert> : null}
          <div className="md:col-span-2"><Button type="submit" disabled={authenticate.isPending || !canSubmitCredential}><RefreshCw className="size-4" />{authenticate.isPending ? "启动中…" : supportedInlineMethods.includes(method) ? "验证并保存凭据" : "启动交互认证"}</Button></div>
        </form>
      )}
      {interactive ? <div className="space-y-3 rounded-lg border p-4"><div className="flex flex-wrap items-center gap-2"><Badge variant="outline">{interactive.session.method}</Badge><StateBadge state={interactive.session.state.state} /><Badge variant="secondary">{interactive.challenge.waiting_for}</Badge></div>{interactive.challenge.user_action ? <p className="text-sm">{interactive.challenge.user_action}</p> : null}{interactive.challenge.external_oauth?.authorization_url ? <Button variant="outline" onClick={() => window.open(interactive.challenge.external_oauth!.authorization_url, "_blank", "noopener,noreferrer")}>打开授权页面</Button> : null}{interactive.challenge.waiting_for === "browser_callback" ? <div className="space-y-2"><Label htmlFor="oauth-callback">浏览器最终回调 URL</Label><Input id="oauth-callback" value={oauthCallback} onChange={(event) => setOauthCallback(event.target.value)} placeholder="https://…" /><Button disabled={completeOauth.isPending || !oauthCallback.trim()} onClick={() => completeOauth.mutate()}>{completeOauth.isPending ? "提交中…" : "提交回调并保存会话"}</Button></div> : null}<Button variant="outline" disabled={pollInteractive.isPending} onClick={() => pollInteractive.mutate()}><RefreshCw className="size-4" />{pollInteractive.isPending ? "轮询中…" : "检查认证进度"}</Button></div> : null}
    </CardContent></Card>

    {captureRecipes.data?.items.length ? <Card><CardHeader><CardTitle>Capture / 本地认证辅助</CardTitle></CardHeader><CardContent className="space-y-4"><p className="text-sm text-muted-foreground">适用于微信、二维码、浏览器存储或动态加密上下文。WebUI 创建一次性配对会话，本地 Asterism Capture 使用配对令牌接管。</p>{capture ? <div className="space-y-3 rounded-lg border p-4"><div className="flex flex-wrap items-center gap-2"><StateBadge state={capture.session.state} /><Badge variant="outline">recipe v{capture.session.required_recipe_version}</Badge><Badge variant="secondary">{capture.session.id}</Badge></div><div className="space-y-2"><Label>一次性配对令牌</Label><pre className="overflow-auto rounded-md bg-muted p-3 text-xs">{capture.pairing_token}</pre><Button size="sm" variant="outline" onClick={() => navigator.clipboard.writeText(capture.pairing_token)}>复制令牌</Button></div><p className="text-sm">入口：{captureRecipes.data.items.find((recipe) => recipe.version === capture.session.required_recipe_version)?.start_url}</p><Button variant="outline" disabled={refreshCapture.isPending} onClick={() => refreshCapture.mutate()}><RefreshCw className="size-4" />{refreshCapture.isPending ? "刷新中…" : "刷新 Capture 状态"}</Button></div> : <Button disabled={startCapture.isPending} onClick={() => startCapture.mutate()}>{startCapture.isPending ? "创建中…" : "创建 Capture 配对会话"}</Button>}</CardContent></Card> : null}

    {account.data.provider_id === "welearn" ? <Card><CardHeader><CardTitle>WELearn 课程批执行</CardTitle></CardHeader><CardContent className="space-y-4"><Alert><AlertTitle>直接接通现有批执行 Worker</AlertTitle><AlertDescription>使用巡查后页面/API 返回的完整规范化身份：Course 必须形如 course:&lt;cid&gt;，SCO 必须形如 sco:&lt;cid&gt;:&lt;scoid&gt;，不能只填裸数字。批执行会重新发现完整 Unit/SCO 并拒绝数量漂移。</AlertDescription></Alert><div className="grid gap-4 md:grid-cols-2"><Field label="本地 Course UUID"><Input value={batchCourseId} onChange={(event) => setBatchCourseId(event.target.value)} /></Field><Field label="规范化远端 Course ID"><Input value={batchRemoteCourseId} onChange={(event) => setBatchRemoteCourseId(event.target.value)} placeholder="course:123456" /></Field><Field label="规范化预期 SCO ID"><Input value={batchRemoteTaskId} onChange={(event) => setBatchRemoteTaskId(event.target.value)} placeholder="sco:123456:7890" /></Field><Field label="预期子任务数量"><Input type="number" min={1} value={batchChildCount} onChange={(event) => setBatchChildCount(event.target.value)} /></Field><Field label="批流程"><select className={selectClassName} value={batchFlow} onChange={(event) => setBatchFlow(event.target.value as typeof batchFlow)}><option value="fanyuchang_duration">Fanyuchang 时长 + 完成</option><option value="auto_duration">Auto 聚合时长 + 完成</option></select></Field><Field label="Unit 索引（逗号分隔；留空为全部）"><Input value={batchUnitIndices} onChange={(event) => setBatchUnitIndices(event.target.value)} /></Field>{batchFlow === "fanyuchang_duration" ? <Field label="逐子任务秒数（逗号分隔）"><Input value={batchTargets} onChange={(event) => setBatchTargets(event.target.value)} /></Field> : <><Field label="配置分钟"><Input type="number" min={1} max={300} value={batchConfiguredMinutes} onChange={(event) => setBatchConfiguredMinutes(event.target.value)} /></Field><Field label="随机范围分钟"><Input type="number" min={0} max={30} value={batchRandomRange} onChange={(event) => setBatchRandomRange(event.target.value)} /></Field><Field label="本次已冻结偏移分钟"><Input type="number" min={-30} max={30} value={batchSampledOffset} onChange={(event) => setBatchSampledOffset(event.target.value)} /></Field></>}</div>{batchResult ? <Alert><AlertTitle>批执行已调度</AlertTitle><AlertDescription>{batchResult}</AlertDescription></Alert> : null}<Button disabled={createBatch.isPending || !batchCourseId.trim() || !batchRemoteCourseId.trim() || !batchRemoteTaskId.trim() || !batchChildCount} onClick={() => createBatch.mutate()}><RefreshCw className="size-4" />{createBatch.isPending ? "调度中…" : "创建课程批执行"}</Button></CardContent></Card> : null}

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
function Field({ label, children }: { label: string; children: React.ReactNode }) { return <div className="space-y-2"><Label>{label}</Label>{children}</div>; }
function parseNumberList(value: string): number[] { return value.split(",").map((item) => item.trim()).filter(Boolean).map(Number); }
function authMethodLabel(method: AuthMethod) { return method === "password" ? "密码登录" : method === "imported_cookie" ? "导入 Cookie" : method === "imported_token" ? "导入令牌" : method === "qr_code" ? "二维码登录" : method === "external_browser_oauth" ? "外部浏览器 OAuth" : method === "assisted_session" ? "本地辅助会话" : method; }
