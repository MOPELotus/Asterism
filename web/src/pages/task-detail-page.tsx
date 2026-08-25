import { usePermissions } from "@refinedev/core";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Activity, Ban, CheckCircle2, Clock3, Copy, ExternalLink, EyeOff, FileQuestion, Hourglass, MonitorUp, Play, RefreshCw, Settings2 } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { Link, useNavigate, useParams, useSearchParams } from "react-router";

import { approveTask, cancelTask, createBrowserBridgeSession, delayTask, executeTask, generateUaiDiscussionInvocationDraft, getBrowserBridgeSession, getTask, getTaskCompletionWorkflows, getTaskDetail, getTaskDuration, getTaskProgress, getTaskQuestions, ignoreTask, optInScoreImprovement, prepareExecutionInvocationDraft, scanProviderAccount } from "@/api/generated/sdk.gen.ts";
import type { BrowserBridgeCreateResponse, Task } from "@/api/generated/types.gen.ts";
import { requireData } from "@/api/result.ts";
import { PageShell } from "@/components/page-shell.tsx";
import { QueryError, TableSkeleton } from "@/components/query-feedback.tsx";
import { StateBadge } from "@/components/state-badge.tsx";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert.tsx";
import { Badge } from "@/components/ui/badge.tsx";
import { Button, buttonVariants } from "@/components/ui/button.tsx";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card.tsx";
import { Input } from "@/components/ui/input.tsx";
import { Label } from "@/components/ui/label.tsx";
import { formatTimestamp, shortId } from "@/lib/format.ts";
import { remoteStateLabel, taskTypeLabels } from "@/lib/learning-display.ts";

const EXECUTABLE_CAPABILITIES = ["resource_execution", "submission_execute", "duration_report", "discussion", "artifact_upload", "oral_submission", "practice"] as const;
type ExecutableCapability = (typeof EXECUTABLE_CAPABILITIES)[number];

const UAI_DISCUSSION_INPUT_TYPE = "uai.discussion.reply-input.v1";
const UAI_WORKER_DISCUSSION_INPUT_TYPE = "uai.worker.generated-text.v1";
const UAI_ARTIFACT_INPUT_TYPE = "uai.artifact-upload.mp3-input.v1";
const UAI_ORAL_INPUT_TYPE = "uai.compound-oral.authorization.v1";

export function TaskDetailPage() {
  const { taskId = "" } = useParams();
  const [searchParams] = useSearchParams();
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  const permissions = usePermissions<string[]>({});
  const canManageSystem = permissions.data?.includes("manage_system") ?? false;
  const canManagePricing = permissions.data?.includes("manage_pricing") ?? false;
  const [requestedCapabilities, setRequestedCapabilities] = useState<ExecutableCapability[]>([]);
  // Submission Drafts are created by the dedicated answer-review page.  The
  // task page deliberately has no free-form Draft-id input.
  const submissionDraftId = "";
  const [invocationDraftId, setInvocationDraftId] = useState("");
  const [discussionContent, setDiscussionContent] = useState("");
  const [aiProfile, setAiProfile] = useState<"economy" | "gpt_only">("economy");
  const [aiRoute, setAiRoute] = useState<"timed" | "untimed" | "escalation">("untimed");
  const [artifactFile, setArtifactFile] = useState<File>();
  const [browserSession, setBrowserSession] = useState<BrowserBridgeCreateResponse>();
  const [formalAssessmentConfirmed, setFormalAssessmentConfirmed] = useState(false);
  const [delayedUntil, setDelayedUntil] = useState(() => new Date(Date.now() + 2 * 60 * 60 * 1000).toISOString().slice(0, 16));
  const [actionNotice, setActionNotice] = useState<string>();
  const idempotencyKey = useRef(crypto.randomUUID());
  const approveKey = useRef(crypto.randomUUID());
  const cancelKey = useRef(crypto.randomUUID());
  const delayKey = useRef(crypto.randomUUID());
  const ignoreKey = useRef(crypto.randomUUID());
  const invocationKey = useRef(crypto.randomUUID());

  const task = useQuery({ queryKey: ["tasks", taskId], enabled: Boolean(taskId), queryFn: async () => requireData(await getTask({ path: { task_id: taskId } })) });
  const completionWorkflows = useQuery({ queryKey: ["tasks", taskId, "completion-workflows"], enabled: Boolean(taskId), retry: false, queryFn: async () => requireData(await getTaskCompletionWorkflows({ path: { task_id: taskId } })) });
  const detail = useQuery({ queryKey: ["tasks", taskId, "remote-detail"], enabled: false, retry: false, queryFn: async () => requireData(await getTaskDetail({ path: { task_id: taskId } })) });
  const progress = useQuery({ queryKey: ["tasks", taskId, "progress"], enabled: false, retry: false, queryFn: async () => requireData(await getTaskProgress({ path: { task_id: taskId } })) });
  const duration = useQuery({ queryKey: ["tasks", taskId, "duration"], enabled: false, retry: false, queryFn: async () => requireData(await getTaskDuration({ path: { task_id: taskId } })) });
  const questions = useQuery({ queryKey: ["tasks", taskId, "questions"], enabled: false, retry: false, queryFn: async () => requireData(await getTaskQuestions({ path: { task_id: taskId } })) });
  const browserSnapshot = useQuery({
    queryKey: ["browser-bridge-sessions", browserSession?.session.id],
    enabled: false,
    retry: false,
    queryFn: async () => requireData(await getBrowserBridgeSession({ path: { session_id: browserSession!.session.id } })),
  });
  useEffect(() => {
    if (!task.data || requestedCapabilities.length) return;
    setRequestedCapabilities(recommendedExecutionCapabilities(task.data));
  }, [requestedCapabilities.length, task.data]);
  useEffect(() => {
    if (task.data?.assessment_class === "formal" && searchParams.get("confirm") === "1") {
      setFormalAssessmentConfirmed(true);
      setActionNotice("已从 QQ 确认链接打开；请检查题目和答案后，再点击最终执行/提交。页面不会自动提交。");
    }
  }, [searchParams, task.data?.assessment_class]);
  const prepareInvocation = useMutation({
    mutationFn: async () => {
      const input = await encodeUaiInvocationInput(requestedCapabilities, discussionContent, artifactFile, task.data?.source_type === "discussion" && task.data.capabilities.includes("resource_execution"));
      return requireData(await prepareExecutionInvocationDraft({
        path: { task_id: taskId },
        headers: {
          "Idempotency-Key": invocationKey.current,
          "x-asterism-invocation-input-type": input.inputType,
          "x-asterism-requested-capabilities": requestedCapabilities.join(","),
          ...(submissionDraftId.trim() ? { "x-asterism-submission-draft-id": submissionDraftId.trim() } : {}),
        },
        body: input.body,
      }));
    },
    onSuccess: (draft) => {
      invocationKey.current = crypto.randomUUID();
      setInvocationDraftId(draft.draft_id);
    },
  });
  const generateDiscussion = useMutation({
    mutationFn: async () => requireData(await generateUaiDiscussionInvocationDraft({
      path: { task_id: taskId },
      headers: { "idempotency-key": invocationKey.current },
      body: { profile: aiProfile },
    })),
    onSuccess: (draft) => {
      invocationKey.current = crypto.randomUUID();
      setDiscussionContent(draft.generated_text);
      setInvocationDraftId(draft.invocation_draft_id);
    },
  });
  const createBridge = useMutation({
    mutationFn: async () => requireData(await createBrowserBridgeSession({ path: { task_id: taskId } })),
    onSuccess: (created) => {
      setBrowserSession(created);
      void queryClient.removeQueries({ queryKey: ["browser-bridge-sessions"] });
    },
  });
  const execute = useMutation({
    mutationFn: async () => requireData(await executeTask({
      path: { task_id: taskId },
      headers: { "Idempotency-Key": idempotencyKey.current },
      body: {
        requested_capabilities: requestedCapabilities,
        ...(requestedCapabilities.includes("submission_execute") && submissionDraftId.trim() ? { submission_draft_id: submissionDraftId.trim() } : {}),
        ...(invocationDraftId.trim() ? { invocation_draft_id: invocationDraftId.trim() } : {}),
        ...(task.data?.assessment_class === "formal" && formalAssessmentConfirmed ? { formal_assessment_confirmation: true } : {}),
        ...(strictRetryRequired && completionWorkflows.data?.strict_completion ? { strict_completion_retry_confirmation: { workflow_id: completionWorkflows.data.strict_completion.workflow.id, expected_revision: completionWorkflows.data.strict_completion.revision } } : {}),
        ...(scoreImprovementCanBind && completionWorkflows.data?.score_improvement ? { score_improvement_retake_confirmation: { workflow_id: completionWorkflows.data.score_improvement.workflow.id, expected_revision: completionWorkflows.data.score_improvement.revision } } : {}),
        ...(canManagePricing ? { ai_profile: aiProfile, ai_route: aiRoute } : {}),
      },
    })),
    onSuccess: ({ execution }) => {
      idempotencyKey.current = crypto.randomUUID();
      navigate(`/executions/${execution.id}`);
    },
  });
  const scoreImprovementOptIn = useMutation({
    mutationFn: async () => requireData(await optInScoreImprovement({ path: { task_id: taskId }, body: { explicitly_opted_in: true } })),
    onSuccess: async () => { await completionWorkflows.refetch(); },
  });
  const scanAccount = useMutation({
    mutationFn: async () => requireData(await scanProviderAccount({ path: { account_id: task.data!.provider_account_id } })),
    onSuccess: async (report) => {
      setActionNotice(`巡查完成：更新 ${report.tasks_updated}，新增 ${report.tasks_created}；正在刷新当前任务。`);
      await queryClient.invalidateQueries({ queryKey: ["tasks", taskId] });
      await queryClient.invalidateQueries({ queryKey: ["tasks"] });
    },
  });
  const refreshTask = async (message: string) => {
    setActionNotice(message);
    await queryClient.invalidateQueries({ queryKey: ["tasks", taskId] });
    await queryClient.invalidateQueries({ queryKey: ["tasks"] });
  };
  const approve = useMutation({
    mutationFn: async () => requireData(await approveTask({ path: { task_id: taskId }, headers: { "Idempotency-Key": approveKey.current } })),
    onSuccess: async () => { approveKey.current = crypto.randomUUID(); await refreshTask("任务已批准并回到可调度状态；正式测评保护仍独立生效。"); },
  });
  const cancel = useMutation({
    mutationFn: async () => requireData(await cancelTask({ path: { task_id: taskId }, headers: { "Idempotency-Key": cancelKey.current } })),
    onSuccess: async () => { cancelKey.current = crypto.randomUUID(); await refreshTask("任务已取消；如存在尚未领取的 Execution，其 Job 与积分预留也已原子撤销。"); },
  });
  const delay = useMutation({
    mutationFn: async () => requireData(await delayTask({ path: { task_id: taskId }, headers: { "Idempotency-Key": delayKey.current }, body: { delayed_until: new Date(delayedUntil).toISOString() } })),
    onSuccess: async () => { delayKey.current = crypto.randomUUID(); await refreshTask("待执行任务已延迟，Execution 与 Scheduler Job 使用同一新时间。"); },
  });
  const ignore = useMutation({
    mutationFn: async () => requireData(await ignoreTask({ path: { task_id: taskId }, headers: { "Idempotency-Key": ignoreKey.current } })),
    onSuccess: async () => { ignoreKey.current = crypto.randomUUID(); await refreshTask("任务已忽略；远端任务未被修改。"); },
  });

  const error = task.error ?? completionWorkflows.error ?? detail.error ?? progress.error ?? duration.error ?? questions.error ?? browserSnapshot.error ?? prepareInvocation.error ?? generateDiscussion.error ?? createBridge.error ?? execute.error ?? scoreImprovementOptIn.error ?? scanAccount.error ?? approve.error ?? cancel.error ?? delay.error ?? ignore.error;
  if (task.isLoading) return <PageShell title="任务详情" description="正在读取任务。"><TableSkeleton /></PageShell>;
  if (!task.data) return <PageShell title="任务详情" description="任务不存在或当前身份不可访问。">{error ? <QueryError error={error} /> : null}</PageShell>;

  const executableCapabilities = EXECUTABLE_CAPABILITIES.filter((capability) => task.data.capabilities.includes(capability));
  const needsDraft = requestedCapabilities.includes("submission_execute");
  const needsReviewedWorkerAnswers = requestedCapabilities.includes("resource_execution") && task.data.capabilities.includes("question_inventory") && task.data.capabilities.includes("answer_resolve");
  const isUaiWorkerDiscussion = task.data.source_type === "discussion" && task.data.capabilities.includes("resource_execution");
  const needsInvocation = isUaiWorkerDiscussion || requestedCapabilities.some((capability) => ["discussion", "artifact_upload", "oral_submission"].includes(capability));
  const invocationShapeSupported = isUaiWorkerDiscussion || isSupportedUaiInvocationShape(requestedCapabilities);
  const executable = executableCapabilities.length > 0;
  const isFormalAssessment = task.data.assessment_class === "formal";
  const strictCompletion = completionWorkflows.data?.strict_completion;
  const strictRetryRequired = strictCompletion?.workflow.state === "active" && strictCompletion.workflow.attempts_started > 0 && (isFormalAssessment || requestedCapabilities.includes("submission_execute"));
  const scoreImprovement = completionWorkflows.data?.score_improvement;
  const scoreImprovementRetakeReady = scoreImprovement?.workflow.state === "ready" && task.data.orchestration_state === "succeeded";
  const scoreImprovementCanBind = scoreImprovementRetakeReady && ["pending", "in_progress"].includes(task.data.remote_state);
  const policyBlocked = isFormalAssessment && !formalAssessmentConfirmed;
  const lifecyclePending = approve.isPending || cancel.isPending || delay.isPending || ignore.isPending;
  const canApprove = task.data.orchestration_state === "waiting_approval";
  const canCancel = ["waiting_approval", "scheduled", "credit_blocked", "human_required", "retry_waiting", "failed"].includes(task.data.orchestration_state);
  const canDelay = task.data.orchestration_state === "scheduled" && Boolean(delayedUntil) && new Date(delayedUntil).getTime() > Date.now();
  const canIgnore = ["discovered", "ready", "waiting_approval", "credit_blocked", "human_required", "failed"].includes(task.data.orchestration_state);
  const canExecuteState = ["discovered", "ready", "failed"].includes(task.data.orchestration_state) || (task.data.orchestration_state === "human_required" && strictRetryRequired) || scoreImprovementCanBind;

  return <PageShell title={task.data.title} description={`${taskTypeLabels[task.data.source_type]} · ${remoteStateLabel(task.data.remote_state)}`} actions={canManageSystem ? <Link className={buttonVariants({ variant: "outline" })} to={`/admin/runtime-settings?scope=task&target=${taskId}`}><Settings2 className="size-4" />高级设置</Link> : undefined}>
    {error ? <QueryError error={error} /> : null}
    <div className="grid gap-4 sm:grid-cols-2 xl:grid-cols-4">
      <Summary label="远端状态"><StateBadge state={task.data.remote_state} /></Summary>
      <Summary label="编排状态"><StateBadge state={task.data.orchestration_state} /></Summary>
      <Summary label="任务性质"><StateBadge state={task.data.assessment_class} /></Summary>
      <Summary label="截止时间">{formatTimestamp(task.data.due_at)}</Summary>
    </div>
    {task.data.provider_summary ? <Card><CardHeader><CardTitle>平台任务信息</CardTitle></CardHeader><CardContent className="flex flex-wrap gap-2">{typeof task.data.provider_summary.required === "boolean" ? <Badge variant={task.data.provider_summary.required ? "default" : "outline"}>{task.data.provider_summary.required ? "必做" : "选做"}</Badge> : null}{typeof task.data.provider_summary.finish_progress === "number" ? <Badge variant="secondary">平台进度 {task.data.provider_summary.finish_progress}%</Badge> : null}{task.data.provider_summary.score_task === true ? <Badge variant="secondary">计分任务</Badge> : null}{typeof task.data.provider_summary.task_score === "number" ? <Badge variant="outline">得分 {task.data.provider_summary.task_score}</Badge> : null}{typeof task.data.provider_summary.position === "number" ? <Badge variant="secondary">官方顺序 {task.data.provider_summary.position}</Badge> : null}{typeof task.data.provider_summary.job_count === "number" ? <Badge variant="outline">{task.data.provider_summary.job_count} 个执行点</Badge> : null}{task.data.provider_summary.locked === true ? <Badge variant="outline">尚未开放</Badge> : null}</CardContent></Card> : null}

    <Card><CardHeader><CardTitle>任务操作</CardTitle></CardHeader><CardContent className="space-y-4">
      {executable ? <p className="text-sm text-muted-foreground">系统已根据平台和任务类型准备好执行方式，无需选择内部能力。</p> : null}
      {actionNotice ? <Alert><AlertTitle>操作已提交</AlertTitle><AlertDescription>{actionNotice}</AlertDescription></Alert> : null}
      {isFormalAssessment && executable ? <Alert className="border-amber-300 bg-amber-50 dark:border-amber-800 dark:bg-amber-950/30"><AlertTitle>正式测评需要本次明确确认</AlertTitle><AlertDescription><label className="mt-2 flex items-start gap-2"><input className="mt-1" type="checkbox" checked={formalAssessmentConfirmed} onChange={(event) => setFormalAssessmentConfirmed(event.target.checked)} /><span>我确认由当前账号执行所选能力；该确认只随本次请求提交，默认仍为拒绝。确认链接只帮助打开本页，不会自动提交。</span></label></AlertDescription></Alert> : null}
      {scoreImprovementRetakeReady ? <Alert><AlertTitle>{scoreImprovementCanBind ? "远端重做已就绪" : "先在远端创建新重做 Attempt"}</AlertTitle><AlertDescription>{scoreImprovementCanBind ? <>Core 会把 workflow {shortId(scoreImprovement!.workflow.id)} 的 revision {scoreImprovement!.revision} 与这次 Execution 原子绑定并消耗一次重试；测评提交仍需选择重做后的新快照和 Draft。</> : <><p>当前远端仍是 completed，Core 不会复用旧答卷。请用下方 BrowserBridge 打开结果页并明确点击“重做”，再立即巡查；状态刷新为 pending/in_progress 后读取新题目。</p><Button className="mt-3" type="button" variant="outline" disabled={scanAccount.isPending} onClick={() => scanAccount.mutate()}><RefreshCw className="size-4" />{scanAccount.isPending ? "巡查中…" : "已重做，立即巡查并刷新"}</Button></>}</AlertDescription></Alert> : null}
      {needsDraft || needsReviewedWorkerAnswers ? <Alert><AlertTitle>先读取题目</AlertTitle><AlertDescription className="space-y-3"><p>系统会读取当前题目、准备答案并在提交前展示审核结果，不需要填写任何内部编号。</p><Button variant="outline" disabled={questions.isFetching} onClick={async () => { const result = await questions.refetch(); if (result.data) navigate(`/tasks/${taskId}/question-snapshots/${result.data.snapshot_id}`); }}><FileQuestion className="size-4" />{questions.isFetching ? "正在读取…" : "读取题目并开始作答"}</Button></AlertDescription></Alert> : null}
      {needsInvocation ? <div className="max-w-2xl space-y-3 rounded-lg border p-4">
        <div><p className="font-medium">完成任务所需内容</p><p className="text-sm text-muted-foreground">填写或选择平台要求的内容后，系统会安全保存并提交本次任务。</p></div>
        {requestedCapabilities.includes("discussion") || isUaiWorkerDiscussion ? <div className="space-y-2"><Label htmlFor="discussion-content">讨论回复</Label><textarea id="discussion-content" className="min-h-28 w-full rounded-md border bg-background px-3 py-2 text-sm" value={discussionContent} onChange={(event) => { setDiscussionContent(event.target.value); setInvocationDraftId(""); }} placeholder="输入将提交的完整回复内容" />{isUaiWorkerDiscussion ? <div className="flex flex-wrap items-end gap-2"><div className="space-y-1"><Label htmlFor="discussion-ai-profile">生成组合</Label><select id="discussion-ai-profile" className="h-9 rounded-md border bg-background px-3 text-sm" value={aiProfile} onChange={(event) => setAiProfile(event.target.value as "economy" | "gpt_only")}><option value="economy">默认省钱组合</option><option value="gpt_only">GPT-only 保质组合</option></select></div><Button type="button" variant="outline" disabled={generateDiscussion.isPending} onClick={() => generateDiscussion.mutate()}>{generateDiscussion.isPending ? "正在读取题目并生成…" : "AI 读取题目并生成草稿"}</Button></div> : null}</div> : null}
        {requestedCapabilities.includes("artifact_upload") ? <div className="space-y-2"><Label htmlFor="artifact-file">MP3 文件</Label><Input id="artifact-file" type="file" accept="audio/mpeg,.mp3" onChange={(event) => { setArtifactFile(event.target.files?.[0]); setInvocationDraftId(""); }} /></div> : null}
        {requestedCapabilities.includes("oral_submission") ? <Alert><AlertTitle>口语任务</AlertTitle><AlertDescription>系统将根据当前题目和已有语音内容准备本次提交。</AlertDescription></Alert> : null}
        {!invocationShapeSupported ? <p className="text-sm text-destructive">当前任务还不能自动准备，请重新同步后再试。</p> : null}
        <div className="flex flex-wrap items-center gap-2"><Button type="button" variant="outline" disabled={!invocationShapeSupported || prepareInvocation.isPending || ((requestedCapabilities.includes("discussion") || isUaiWorkerDiscussion) && !discussionContent.trim()) || (requestedCapabilities.includes("artifact_upload") && !artifactFile) || (needsDraft && !submissionDraftId.trim())} onClick={() => prepareInvocation.mutate()}>{prepareInvocation.isPending ? "正在准备…" : invocationDraftId ? "按当前文本重新准备" : "准备提交内容"}</Button>{invocationDraftId ? <Badge variant="secondary">内容已准备</Badge> : null}</div>
      </div> : null}
      {canManagePricing && executable ? <div className="flex flex-wrap items-end gap-2 rounded-lg border bg-muted/20 p-3"><div className="space-y-1"><Label htmlFor="execution-ai-profile">本次执行 AI 组合</Label><select id="execution-ai-profile" className="h-9 rounded-md border bg-background px-3 text-sm" value={aiProfile} onChange={(event) => setAiProfile(event.target.value as "economy" | "gpt_only")}><option value="economy">默认省钱组合</option><option value="gpt_only">GPT-only 保质组合</option></select></div><div className="space-y-1"><Label htmlFor="execution-ai-route">本次执行路由</Label><select id="execution-ai-route" className="h-9 rounded-md border bg-background px-3 text-sm" value={aiRoute} onChange={(event) => setAiRoute(event.target.value as "timed" | "untimed" | "escalation")}><option value="timed">限时</option><option value="untimed">不限时</option><option value="escalation">升级/仲裁</option></select></div><p className="max-w-md text-xs text-muted-foreground">选择会随 Execution 冻结；不会修改部署默认组合。</p></div> : null}
      <div className="flex flex-wrap gap-2">
        {!needsDraft && !needsReviewedWorkerAnswers ? <Button disabled={!executable || requestedCapabilities.length === 0 || !canExecuteState || policyBlocked || execute.isPending || lifecyclePending || (needsInvocation && !invocationDraftId.trim())} onClick={() => execute.mutate()}><Play className="size-4" />{execute.isPending ? "正在开始…" : scoreImprovementCanBind ? "继续重做" : "开始执行"}</Button> : null}
        <Button variant="outline" disabled={!canApprove || lifecyclePending || execute.isPending} onClick={() => approve.mutate()}><CheckCircle2 className="size-4" />{approve.isPending ? "批准中…" : "批准"}</Button>
        <Button variant="outline" disabled={!canIgnore || lifecyclePending || execute.isPending} onClick={() => { if (window.confirm("忽略后 Asterism 不会自动处理此任务，确认继续？")) ignore.mutate(); }}><EyeOff className="size-4" />{ignore.isPending ? "忽略中…" : "忽略"}</Button>
        <Button variant="destructive" disabled={!canCancel || lifecyclePending || execute.isPending} onClick={() => { if (window.confirm("取消会撤销尚未领取的执行和积分预留，确认继续？")) cancel.mutate(); }}><Ban className="size-4" />{cancel.isPending ? "取消中…" : "取消"}</Button>
      </div>
      <div className="grid max-w-xl gap-2 sm:grid-cols-[1fr_auto] sm:items-end"><div className="space-y-2"><Label htmlFor="delayed-until">延迟至</Label><Input id="delayed-until" type="datetime-local" value={delayedUntil} onChange={(event) => setDelayedUntil(event.target.value)} /></div><Button variant="outline" disabled={!canDelay || lifecyclePending || execute.isPending} onClick={() => delay.mutate()}><Hourglass className="size-4" />{delay.isPending ? "延迟中…" : "延迟待执行任务"}</Button></div>
      {!executable ? <p className="text-sm text-muted-foreground">当前任务仅支持查看状态，暂时没有可执行操作。</p> : null}
      {executable && !canExecuteState && !policyBlocked ? <p className="text-sm text-muted-foreground">当前编排状态不可直接执行；等待审批时先批准，已调度时可延迟或取消。</p> : null}
    </CardContent></Card>

    <Card><CardHeader><CardTitle>完成与重试</CardTitle></CardHeader><CardContent className="space-y-3">
      {strictCompletion ? <div className="flex flex-wrap items-center gap-2"><Badge variant="outline">Strict {strictCompletion.workflow.state}</Badge><Badge variant="secondary">已启动 {strictCompletion.workflow.attempts_started} 次</Badge>{strictCompletion.workflow.last_diagnosis ? <Badge variant="warning">{strictCompletion.workflow.last_diagnosis}</Badge> : null}{strictRetryRequired ? <Badge variant="warning">本次执行将确认重试 revision {strictCompletion.revision}</Badge> : null}</div> : <p className="text-sm text-muted-foreground">尚无 Strict Completion 工作流。</p>}
      {completionWorkflows.data?.score_improvement ? <div className="flex flex-wrap items-center gap-2"><Badge variant="outline">提分 {completionWorkflows.data.score_improvement.workflow.state}</Badge><Badge variant="secondary">已启动 {completionWorkflows.data.score_improvement.workflow.attempts_started} 次</Badge></div> : task.data.remote_state === "completed" ? <Button variant="outline" disabled={scoreImprovementOptIn.isPending} onClick={() => { if (window.confirm("这会显式创建一次受限提分工作流，但不会立即发起远端重考。确认继续？")) scoreImprovementOptIn.mutate(); }}>{scoreImprovementOptIn.isPending ? "创建中…" : "显式启用提分工作流"}</Button> : <p className="text-sm text-muted-foreground">任务完成并收集到精确结果证据后，可在这里显式启用受限提分。</p>}
    </CardContent></Card>

    {task.data.capabilities.includes("browser_bridge") ? <Card><CardHeader><CardTitle className="flex items-center gap-2"><MonitorUp className="size-5" />BrowserBridge</CardTitle></CardHeader><CardContent className="space-y-4">
      <p className="text-sm text-muted-foreground">创建一次性浏览器会话后，用本地 Helper 的配对入口打开远端页面。令牌只在创建响应中显示。</p>
      <div className="flex flex-wrap gap-2"><Button disabled={createBridge.isPending} onClick={() => createBridge.mutate()}>{createBridge.isPending ? "正在创建…" : "创建浏览器会话"}</Button>{browserSession ? <Button variant="outline" disabled={browserSnapshot.isFetching} onClick={() => void browserSnapshot.refetch()}><RefreshCw className="size-4" />刷新状态</Button> : null}</div>
      {browserSession ? <div className="space-y-3 rounded-lg border p-4 text-sm">
        <div className="flex flex-wrap items-center gap-2"><StateBadge state={browserSnapshot.data?.session.state ?? browserSession.session.state} /><Badge variant="outline">{shortId(browserSession.session.id)}</Badge></div>
        <SecretRow label="配对令牌" value={browserSession.pairing_token} />
        <div className="space-y-1"><p className="font-medium">启动地址</p><div className="flex flex-wrap items-center gap-2"><code className="max-w-full break-all rounded bg-muted px-2 py-1">{browserSession.spec.start_url}</code><Button type="button" size="sm" variant="outline" onClick={() => window.open(browserSession.spec.start_url, "_blank", "noopener,noreferrer")}><ExternalLink className="size-4" />打开</Button></div></div>
        <p className="text-muted-foreground">允许来源：{browserSession.spec.allowed_origins.join("、")} · {browserSession.spec.headless ? "无头" : "可见窗口"}</p>
      </div> : null}
    </CardContent></Card> : null}

    <div className="grid gap-4 lg:grid-cols-3">
      <ReadCard title="远端详情" icon={RefreshCw} loading={detail.isFetching} onRead={() => void detail.refetch()}>{detail.data ? <JsonPreview value={detail.data.detail.normalized_detail} /> : <EmptyRead />}</ReadCard>
      <ReadCard title="实时进度" icon={Activity} loading={progress.isFetching} onRead={() => void progress.refetch()}>{progress.data ? <div className="space-y-2 text-sm"><StateBadge state={progress.data.progress.remote_state} /><p>进度 {progress.data.progress.percent == null ? "—" : `${progress.data.progress.percent}%`}</p><p>时长 {progress.data.progress.duration_seconds == null ? "—" : `${progress.data.progress.duration_seconds} 秒`}</p><p className="text-muted-foreground">{formatTimestamp(progress.data.progress.updated_at)}</p></div> : <EmptyRead />}</ReadCard>
      <ReadCard title="学习时长" icon={Clock3} loading={duration.isFetching} onRead={() => void duration.refetch()}>{duration.data ? <div><div className="text-3xl font-semibold">{duration.data.duration.duration_seconds}<span className="ml-1 text-sm font-normal text-muted-foreground">秒</span></div><p className="mt-2 text-sm text-muted-foreground">{formatTimestamp(duration.data.duration.updated_at)}</p></div> : <EmptyRead />}</ReadCard>
    </div>

    {task.data.capabilities.includes("question_inventory") ? <Card><CardHeader className="flex-row items-center justify-between"><CardTitle className="flex items-center gap-2"><FileQuestion className="size-5" />题目快照</CardTitle><Button variant="outline" disabled={questions.isFetching} onClick={() => void questions.refetch()}>{questions.isFetching ? "读取中…" : "读取并解析"}</Button></CardHeader><CardContent>{questions.data ? <div className="space-y-4"><div className="flex flex-wrap items-center gap-2"><Badge variant="outline">snapshot {shortId(questions.data.snapshot_id)}</Badge><Badge variant="secondary">{questions.data.questions.length} 题</Badge><span className="text-sm text-muted-foreground">{formatTimestamp(questions.data.captured_at)}</span><Link className={buttonVariants({ variant: "default", size: "sm" })} to={`/tasks/${taskId}/question-snapshots/${questions.data.snapshot_id}`}>进入答案审核</Link></div>{questions.data.questions.map((question) => <div key={question.id} className="rounded-lg border p-4"><div className="mb-2 flex items-center gap-2"><Badge variant="outline">#{question.position}</Badge><Badge variant="secondary">{question.kind}</Badge></div><p className="whitespace-pre-wrap text-sm">{question.stem}</p></div>)}</div> : <p className="text-sm text-muted-foreground">尚未读取当前题目快照。</p>}</CardContent></Card> : null}
  </PageShell>;
}

function isSupportedUaiInvocationShape(capabilities: readonly ExecutableCapability[]) {
  const value = capabilities.join(",");
  return value === "discussion" || value === "artifact_upload" || value === "submission_execute,artifact_upload" || value === "submission_execute,oral_submission";
}

function recommendedExecutionCapabilities(task: Task): ExecutableCapability[] {
  const capabilities = task.capabilities;
  if (task.source_type === "discussion" && capabilities.includes("resource_execution")) return ["resource_execution"];
  if (capabilities.includes("oral_submission")) return capabilities.includes("submission_execute") ? ["submission_execute", "oral_submission"] : ["oral_submission"];
  if (capabilities.includes("artifact_upload")) return capabilities.includes("submission_execute") ? ["submission_execute", "artifact_upload"] : ["artifact_upload"];
  if (capabilities.includes("discussion")) return ["discussion"];
  if (capabilities.includes("submission_execute")) return ["submission_execute"];
  if (capabilities.includes("resource_execution")) return capabilities.includes("duration_report") ? ["resource_execution", "duration_report"] : ["resource_execution"];
  if (capabilities.includes("duration_report")) return ["duration_report"];
  if (capabilities.includes("practice")) return ["practice"];
  return [];
}

async function encodeUaiInvocationInput(capabilities: readonly ExecutableCapability[], discussionContent: string, artifactFile?: File, workerDiscussion = false) {
  const encoder = new TextEncoder();
  if (workerDiscussion && capabilities.length === 1 && capabilities[0] === "resource_execution") {
    return { inputType: UAI_WORKER_DISCUSSION_INPUT_TYPE, body: new Blob([encoder.encode(discussionContent.trim())]) };
  }
  if (capabilities.length === 1 && capabilities[0] === "discussion") {
    const content = encoder.encode(discussionContent.trim());
    const prefix = encoder.encode(`${UAI_DISCUSSION_INPUT_TYPE}\0`);
    const length = new Uint8Array(4);
    new DataView(length.buffer).setUint32(0, content.byteLength, false);
    return { inputType: UAI_DISCUSSION_INPUT_TYPE, body: new Blob([prefix, length, content]) };
  }
  if (capabilities.includes("artifact_upload") && artifactFile) {
    const filename = encoder.encode(artifactFile.name);
    const prefix = encoder.encode(`${UAI_ARTIFACT_INPUT_TYPE}\0`);
    const filenameLength = new Uint8Array(2);
    new DataView(filenameLength.buffer).setUint16(0, filename.byteLength, false);
    const bytes = new Uint8Array(await artifactFile.arrayBuffer());
    const artifactLength = new Uint8Array(4);
    new DataView(artifactLength.buffer).setUint32(0, bytes.byteLength, false);
    return { inputType: UAI_ARTIFACT_INPUT_TYPE, body: new Blob([prefix, filenameLength, filename, artifactLength, bytes]) };
  }
  if (capabilities.includes("oral_submission")) return { inputType: UAI_ORAL_INPUT_TYPE, body: new Blob([encoder.encode(`${UAI_ORAL_INPUT_TYPE}\0`)]) };
  throw new Error("当前 capability 组合缺少可编码的 UAI 私有输入");
}

function SecretRow({ label, value }: { label: string; value: string }) {
  return <div className="space-y-1"><p className="font-medium">{label}</p><div className="flex flex-wrap items-center gap-2"><code className="max-w-full break-all rounded bg-muted px-2 py-1">{value}</code><Button type="button" size="sm" variant="outline" onClick={() => void navigator.clipboard.writeText(value)}><Copy className="size-4" />复制</Button></div></div>;
}

function ReadCard({ title, icon: Icon, loading, onRead, children }: { title: string; icon: typeof Activity; loading: boolean; onRead: () => void; children: React.ReactNode }) { return <Card><CardHeader className="flex-row items-center justify-between"><CardTitle className="flex items-center gap-2"><Icon className="size-4" />{title}</CardTitle><Button size="sm" variant="outline" disabled={loading} onClick={onRead}>{loading ? "读取中" : "读取"}</Button></CardHeader><CardContent>{children}</CardContent></Card>; }
function EmptyRead() { return <p className="text-sm text-muted-foreground">按需从 Provider 读取，不使用扫描缓存推断。</p>; }
function JsonPreview({ value }: { value: unknown }) { return <pre className="max-h-64 overflow-auto whitespace-pre-wrap break-words rounded-md bg-muted p-3 text-xs">{JSON.stringify(value, null, 2)}</pre>; }
function Summary({ label, children }: { label: string; children: React.ReactNode }) { return <Card><CardHeader className="pb-2"><CardTitle className="text-sm text-muted-foreground">{label}</CardTitle></CardHeader><CardContent className="font-medium">{children}</CardContent></Card>; }
