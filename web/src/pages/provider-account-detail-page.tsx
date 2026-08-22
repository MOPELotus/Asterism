import { useList, usePermissions } from "@refinedev/core";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { CalendarClock, Copy, KeyRound, RefreshCw, Search, Trash2 } from "lucide-react";
import { type FormEvent, useEffect, useMemo, useState } from "react";
import { Link, useNavigate, useParams, useSearchParams } from "react-router";

import {
  beginProviderAccountAuthSession,
  configureProviderAccountScanSchedule,
  createAuthBootstrapSession,
  createWellearnBatchExecution,
  deleteProviderAccount,
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
import { AsterismApiError, ensureSuccess, requireData } from "@/api/result.ts";
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
  const navigate = useNavigate();
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
      if (!provider) throw new Error("平台信息尚未加载");
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
      if (!provider) throw new Error("平台信息尚未加载");
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
  const remove = useMutation({
    mutationFn: async () => {
      if (!window.confirm(`确定删除平台账号“${account.data?.display_name ?? ""}”吗？本地保存的认证信息、巡查计划及关联记录将一并清理，此操作无法撤销。`)) return false;
      ensureSuccess(await deleteProviderAccount({ path: { account_id: accountId } }));
      return true;
    },
    onSuccess: async (deleted) => {
      if (!deleted) return;
      await queryClient.invalidateQueries({ queryKey: ["provider-accounts"] });
      navigate("/provider-accounts", { replace: true });
    },
  });

  const error = account.error ?? providers.query.error ?? captureRecipes.error ?? (latestAuth.error instanceof AsterismApiError && latestAuth.error.statusCode === 404 ? null : latestAuth.error) ?? authenticate.error ?? pollInteractive.error ?? completeOauth.error ?? startCapture.error ?? refreshCapture.error ?? createBatch.error ?? scan.error ?? saveSchedule.error ?? remove.error;
  const canSubmitCredential = !supportedInlineMethods.includes(method) || (method === "password" ? Boolean(username.trim() && password) : Boolean(credential.trim()));
  const compatibleSessionKinds = useMemo(() => provider?.session_kinds ?? [], [provider]);

  if (account.isLoading) return <PageShell title="平台账号" description="正在读取账号状态。"><TableSkeleton /></PageShell>;
  if (!account.data) return <PageShell title="平台账号" description="账号不存在或当前身份不可访问。">{error ? <QueryError error={error} /> : null}</PageShell>;

  return <PageShell title={account.data.display_name} description={account.data.provider_id} actions={<div className="flex flex-wrap gap-2"><Link className={buttonVariants({ variant: "outline" })} to={`/courses?provider_account_id=${accountId}`}>查看课程</Link><Button variant="outline" disabled={scan.isPending || account.data.auth_state.state !== "authenticated"} onClick={() => scan.mutate()}><Search className="size-4" />{scan.isPending ? "巡查中…" : "立即巡查"}</Button><Button variant="destructive" disabled={remove.isPending} onClick={() => remove.mutate()}><Trash2 className="size-4" />{remove.isPending ? "删除中…" : "删除账号"}</Button></div>}>
    {error ? <QueryError error={error} /> : null}
    {scanReport ? <Alert><AlertTitle>巡查完成</AlertTitle><AlertDescription>{scanReport}</AlertDescription></Alert> : null}
    <div className="grid gap-4 sm:grid-cols-3">
      <Summary label="认证状态"><StateBadge state={account.data.auth_state.state} /></Summary>
      <Summary label="凭据数量">{account.data.credential_count}</Summary>
      <Summary label="更新时间">{formatTimestamp(account.data.updated_at)}</Summary>
    </div>

    <Card><CardHeader><CardTitle className="flex items-center gap-2"><KeyRound className="size-5" />平台认证</CardTitle></CardHeader><CardContent className="space-y-4">
      {latestAuth.data ? <div className="flex flex-wrap items-center gap-2 rounded-lg bg-muted p-3 text-sm"><span>最近认证</span><StateBadge state={latestAuth.data.state.state} /><Badge variant="outline">版本 {latestAuth.data.revision}</Badge><span className="text-muted-foreground">{formatTimestamp(latestAuth.data.updated_at)}</span></div> : null}
      {!authMethods.length ? <Alert><AlertTitle>暂无认证方法</AlertTitle><AlertDescription>该平台当前没有提供可用的认证入口。</AlertDescription></Alert> : (
        <form className="grid gap-4 md:grid-cols-2" onSubmit={(event: FormEvent) => { event.preventDefault(); authenticate.mutate(); }}>
          <div className="space-y-2 md:col-span-2"><Label htmlFor="auth-method">认证方法</Label><select id="auth-method" className={selectClassName} value={method} onChange={(event) => setMethod(event.target.value as AuthMethod)}>{authMethods.map((item) => <option key={item} value={item}>{authMethodLabel(item)}</option>)}</select></div>
          {method === "password" ? <><div className="space-y-2"><Label htmlFor="provider-username">平台用户名</Label><Input id="provider-username" autoComplete="username" value={username} onChange={(event) => setUsername(event.target.value)} /></div><div className="space-y-2"><Label htmlFor="provider-password">平台密码</Label><Input id="provider-password" type="password" autoComplete="current-password" value={password} onChange={(event) => setPassword(event.target.value)} /></div></> : null}
          {method === "imported_cookie" ? <div className="space-y-2 md:col-span-2"><Label htmlFor="provider-cookie">Cookie</Label><Input id="provider-cookie" type="password" autoComplete="off" value={credential} onChange={(event) => setCredential(event.target.value)} /></div> : null}
          {method === "imported_token" ? <><div className="space-y-2"><Label htmlFor="token-purpose">令牌格式</Label><select id="token-purpose" className={selectClassName} value={tokenPurpose} onChange={(event) => setTokenPurpose(event.target.value as typeof tokenPurpose)}><option value="provider_access_token">Access Token</option><option value="provider_composite_session">复合会话 JSON</option></select></div><div className="space-y-2"><Label htmlFor="session-kind">会话种类</Label><select id="session-kind" className={selectClassName} value={sessionKind} onChange={(event) => setSessionKind(event.target.value as SessionKind)}>{compatibleSessionKinds.map((kind) => <option key={kind} value={kind}>{kind}</option>)}</select></div><div className="space-y-2 md:col-span-2"><Label htmlFor="provider-token">令牌内容</Label><Input id="provider-token" type="password" autoComplete="off" value={credential} onChange={(event) => setCredential(event.target.value)} /></div></> : null}
          {!supportedInlineMethods.includes(method) ? <Alert className="md:col-span-2"><AlertTitle>交互认证</AlertTitle><AlertDescription>{method === "assisted_session" ? "请使用下方出现的本地认证辅助工具完成登录信息配对。" : "启动后按照页面给出的中文步骤完成扫码或微信授权，再回到这里检查认证结果。"}</AlertDescription></Alert> : null}
          {method !== "assisted_session" ? <div className="md:col-span-2"><Button type="submit" disabled={authenticate.isPending || !canSubmitCredential}><RefreshCw className="size-4" />{authenticate.isPending ? "启动中…" : supportedInlineMethods.includes(method) ? "验证并保存凭据" : "启动交互认证"}</Button></div> : null}
        </form>
      )}
      {interactive ? <div className="space-y-3 rounded-lg border p-4"><div className="flex flex-wrap items-center gap-2"><Badge variant="outline">{authMethodLabel(interactive.session.method)}</Badge><StateBadge state={interactive.session.state.state} /><Badge variant="secondary">{waitingForLabel(interactive.challenge.waiting_for)}</Badge></div><p className="text-sm">{interactiveInstructions(account.data.provider_id, interactive.challenge.waiting_for)}</p>{interactive.challenge.external_oauth?.authorization_url ? <div className="space-y-2"><Label>微信授权链接</Label><pre className="overflow-auto whitespace-pre-wrap break-all rounded-md bg-muted p-3 text-xs">{interactive.challenge.external_oauth.authorization_url}</pre><Button variant="outline" onClick={() => navigator.clipboard.writeText(interactive.challenge.external_oauth!.authorization_url)}><Copy className="size-4" />复制授权链接</Button><p className="text-sm text-muted-foreground">复制后发送到微信“文件传输助手”，并在微信内打开。Asterism 不会在当前浏览器中自动打开该地址。</p></div> : null}{interactive.challenge.waiting_for === "browser_callback" ? <div className="space-y-2"><Label htmlFor="oauth-callback">授权完成后的最终跳转地址</Label><Input id="oauth-callback" value={oauthCallback} onChange={(event) => setOauthCallback(event.target.value)} placeholder="请完整粘贴以 https:// 开头的最终地址" /><p className="text-sm text-muted-foreground">在微信中完成授权后，复制最终打开页面的完整地址并粘贴到这里，不要只复制验证码或其中一段参数。</p><Button disabled={completeOauth.isPending || !oauthCallback.trim()} onClick={() => completeOauth.mutate()}>{completeOauth.isPending ? "提交中…" : "提交地址并保存认证"}</Button></div> : null}<Button variant="outline" disabled={pollInteractive.isPending} onClick={() => pollInteractive.mutate()}><RefreshCw className="size-4" />{pollInteractive.isPending ? "检查中…" : "检查认证进度"}</Button></div> : null}
    </CardContent></Card>

    {method === "assisted_session" && captureRecipes.data?.items.length ? <Card><CardHeader><CardTitle>本地认证辅助</CardTitle></CardHeader><CardContent className="space-y-4"><p className="text-sm text-muted-foreground">仅当平台登录依赖微信内页面、浏览器会话或动态加密信息时使用。点击下方按钮创建一次性配对，然后在本机认证辅助程序中输入配对令牌；令牌只用于本次认证，完成或过期后立即失效。</p>{capture ? <div className="space-y-3 rounded-lg border p-4"><div className="flex flex-wrap items-center gap-2"><StateBadge state={capture.session.state} /><Badge variant="outline">流程版本 {capture.session.required_recipe_version}</Badge><Badge variant="secondary">{capture.session.id}</Badge></div><div className="space-y-2"><Label>一次性配对令牌</Label><pre className="overflow-auto rounded-md bg-muted p-3 text-xs">{capture.pairing_token}</pre><Button size="sm" variant="outline" onClick={() => navigator.clipboard.writeText(capture.pairing_token)}><Copy className="size-4" />复制令牌</Button></div><p className="break-all text-sm">认证入口：{captureRecipes.data.items.find((recipe) => recipe.version === capture.session.required_recipe_version)?.start_url}</p><Button variant="outline" disabled={refreshCapture.isPending} onClick={() => refreshCapture.mutate()}><RefreshCw className="size-4" />{refreshCapture.isPending ? "刷新中…" : "检查辅助认证状态"}</Button></div> : <Button disabled={startCapture.isPending} onClick={() => startCapture.mutate()}>{startCapture.isPending ? "创建中…" : "创建一次性配对"}</Button>}</CardContent></Card> : null}

    {account.data.provider_id === "welearn" ? <Card><CardHeader><CardTitle>WELearn 课程批量执行</CardTitle></CardHeader><CardContent className="space-y-4"><Alert><AlertTitle>使用巡查结果创建批量任务</AlertTitle><AlertDescription>请填写巡查后课程页面或接口返回的完整标识：课程必须形如 course:&lt;cid&gt;，学习单元必须形如 sco:&lt;cid&gt;:&lt;scoid&gt;，不能只填裸数字。执行前会重新读取完整学习单元清单，数量发生变化时会停止并提示。</AlertDescription></Alert><div className="grid gap-4 md:grid-cols-2"><Field label="本地课程标识"><Input value={batchCourseId} onChange={(event) => setBatchCourseId(event.target.value)} /></Field><Field label="远端课程标识"><Input value={batchRemoteCourseId} onChange={(event) => setBatchRemoteCourseId(event.target.value)} placeholder="course:123456" /></Field><Field label="预期学习单元标识"><Input value={batchRemoteTaskId} onChange={(event) => setBatchRemoteTaskId(event.target.value)} placeholder="sco:123456:7890" /></Field><Field label="预期子任务数量"><Input type="number" min={1} value={batchChildCount} onChange={(event) => setBatchChildCount(event.target.value)} /></Field><Field label="执行方式"><select className={selectClassName} value={batchFlow} onChange={(event) => setBatchFlow(event.target.value as typeof batchFlow)}><option value="fanyuchang_duration">逐单元时长并完成</option><option value="auto_duration">自动汇总时长并完成</option></select></Field><Field label="学习单元序号（逗号分隔；留空为全部）"><Input value={batchUnitIndices} onChange={(event) => setBatchUnitIndices(event.target.value)} /></Field>{batchFlow === "fanyuchang_duration" ? <Field label="逐子任务秒数（逗号分隔）"><Input value={batchTargets} onChange={(event) => setBatchTargets(event.target.value)} /></Field> : <><Field label="配置分钟"><Input type="number" min={1} max={300} value={batchConfiguredMinutes} onChange={(event) => setBatchConfiguredMinutes(event.target.value)} /></Field><Field label="随机范围分钟"><Input type="number" min={0} max={30} value={batchRandomRange} onChange={(event) => setBatchRandomRange(event.target.value)} /></Field><Field label="本次固定偏移分钟"><Input type="number" min={-30} max={30} value={batchSampledOffset} onChange={(event) => setBatchSampledOffset(event.target.value)} /></Field></>}</div>{batchResult ? <Alert><AlertTitle>批量执行已调度</AlertTitle><AlertDescription>{batchResult}</AlertDescription></Alert> : null}<Button disabled={createBatch.isPending || !batchCourseId.trim() || !batchRemoteCourseId.trim() || !batchRemoteTaskId.trim() || !batchChildCount} onClick={() => createBatch.mutate()}><RefreshCw className="size-4" />{createBatch.isPending ? "调度中…" : "创建课程批量执行"}</Button></CardContent></Card> : null}

    {canManageSystem ? <Card><CardHeader><CardTitle className="flex items-center gap-2"><CalendarClock className="size-5" />定期巡查</CardTitle></CardHeader><CardContent className="space-y-4">
      {schedule.isLoading ? <TableSkeleton /> : <><label className="flex items-center gap-2 text-sm"><input type="checkbox" checked={scheduleEnabled} onChange={(event) => setScheduleEnabled(event.target.checked)} />启用章节新增及其他任务的定期巡查</label><div className="max-w-sm space-y-2"><Label htmlFor="scan-interval">期望间隔（秒；留空则使用当前平台和账号的默认值）</Label><Input id="scan-interval" type="number" min={1} value={scheduleInterval} onChange={(event) => setScheduleInterval(event.target.value)} /></div>{schedule.data ? <p className="text-sm text-muted-foreground">最终间隔 {schedule.data.effective_interval_seconds} 秒；下次 {formatTimestamp(schedule.data.next_run_at)}</p> : null}<Button disabled={saveSchedule.isPending} onClick={() => saveSchedule.mutate()}>{saveSchedule.isPending ? "保存中…" : "保存巡查计划"}</Button></>}
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
function authMethodLabel(method: AuthMethod) { return method === "password" ? "密码登录" : method === "imported_cookie" ? "导入浏览器会话" : method === "imported_token" ? "导入令牌" : method === "qr_code" ? "二维码登录" : method === "external_browser_oauth" ? "微信授权" : method === "assisted_session" ? "本地辅助会话" : method; }
function waitingForLabel(value: string) { return value === "browser_callback" ? "等待授权结果" : value === "user_action" ? "等待用户操作" : value === "provider_poll" ? "等待平台确认" : "等待认证"; }
function interactiveInstructions(providerId: string, waitingFor: string) {
  if (providerId === "cidaren" && waitingFor === "browser_callback") return "词达人需要在微信内完成授权。请复制下方链接，发送到微信文件传输助手后在微信中打开；授权完成后，再把最终跳转地址完整粘贴回来。";
  if (waitingFor === "browser_callback") return "请复制下方授权链接，在平台要求的应用中完成授权，然后把最终跳转地址完整粘贴回来。";
  return "请按照平台页面完成当前操作，完成后点击“检查认证进度”。";
}
