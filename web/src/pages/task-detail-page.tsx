import { usePermissions } from "@refinedev/core";
import { useMutation, useQuery } from "@tanstack/react-query";
import { Activity, Clock3, FileQuestion, Play, RefreshCw, Settings2 } from "lucide-react";
import { useRef, useState } from "react";
import { Link, useNavigate, useParams } from "react-router";

import { executeTask, getTask, getTaskDetail, getTaskDuration, getTaskProgress, getTaskQuestions } from "@/api/generated/sdk.gen.ts";
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

export function TaskDetailPage() {
  const { taskId = "" } = useParams();
  const navigate = useNavigate();
  const permissions = usePermissions<string[]>({});
  const canManageSystem = permissions.data?.includes("manage_system") ?? false;
  const [submissionDraftId, setSubmissionDraftId] = useState("");
  const idempotencyKey = useRef(crypto.randomUUID());

  const task = useQuery({ queryKey: ["tasks", taskId], enabled: Boolean(taskId), queryFn: async () => requireData(await getTask({ path: { task_id: taskId } })) });
  const detail = useQuery({ queryKey: ["tasks", taskId, "remote-detail"], enabled: false, retry: false, queryFn: async () => requireData(await getTaskDetail({ path: { task_id: taskId } })) });
  const progress = useQuery({ queryKey: ["tasks", taskId, "progress"], enabled: false, retry: false, queryFn: async () => requireData(await getTaskProgress({ path: { task_id: taskId } })) });
  const duration = useQuery({ queryKey: ["tasks", taskId, "duration"], enabled: false, retry: false, queryFn: async () => requireData(await getTaskDuration({ path: { task_id: taskId } })) });
  const questions = useQuery({ queryKey: ["tasks", taskId, "questions"], enabled: false, retry: false, queryFn: async () => requireData(await getTaskQuestions({ path: { task_id: taskId } })) });
  const execute = useMutation({
    mutationFn: async () => requireData(await executeTask({ path: { task_id: taskId }, headers: { "Idempotency-Key": idempotencyKey.current }, body: submissionDraftId.trim() ? { submission_draft_id: submissionDraftId.trim() } : {} })),
    onSuccess: ({ execution }) => {
      idempotencyKey.current = crypto.randomUUID();
      navigate(`/executions/${execution.id}`);
    },
  });

  const error = task.error ?? detail.error ?? progress.error ?? duration.error ?? questions.error ?? execute.error;
  if (task.isLoading) return <PageShell title="任务详情" description="正在读取任务。"><TableSkeleton /></PageShell>;
  if (!task.data) return <PageShell title="任务详情" description="任务不存在或当前身份不可访问。">{error ? <QueryError error={error} /> : null}</PageShell>;

  const needsDraft = task.data.capabilities.includes("submission_execute");
  const executable = task.data.capabilities.includes("resource_execution") || needsDraft;
  const policyBlocked = task.data.assessment_class !== "routine";

  return <PageShell title={task.data.title} description={`${task.data.source_type} · ${shortId(task.data.id)}`} actions={canManageSystem ? <Link className={buttonVariants({ variant: "outline" })} to={`/admin/runtime-settings?scope=task&target=${taskId}`}><Settings2 className="size-4" />任务运行设置</Link> : undefined}>
    {error ? <QueryError error={error} /> : null}
    <div className="grid gap-4 sm:grid-cols-2 xl:grid-cols-4">
      <Summary label="远端状态"><StateBadge state={task.data.remote_state} /></Summary>
      <Summary label="编排状态"><StateBadge state={task.data.orchestration_state} /></Summary>
      <Summary label="任务性质"><StateBadge state={task.data.assessment_class} /></Summary>
      <Summary label="截止时间">{formatTimestamp(task.data.due_at)}</Summary>
    </div>

    <Card><CardHeader><CardTitle>能力与操作</CardTitle></CardHeader><CardContent className="space-y-4">
      <div className="flex flex-wrap gap-2">{task.data.capabilities.map((capability) => <Badge key={capability} variant="secondary">{capability}</Badge>)}</div>
      {policyBlocked && executable ? <Alert className="border-amber-300 bg-amber-50 dark:border-amber-800 dark:bg-amber-950/30"><AlertTitle>策略阻止自动执行</AlertTitle><AlertDescription>正式或未知性质任务必须先通过统一审批策略；当前 API 尚未提供对应批准动作，因此 WebUI 不会绕过 Core 直接执行。</AlertDescription></Alert> : null}
      {needsDraft ? <div className="max-w-xl space-y-2"><Label htmlFor="submission-draft">Submission Draft ID</Label><Input id="submission-draft" value={submissionDraftId} onChange={(event) => setSubmissionDraftId(event.target.value)} placeholder="测评任务必须绑定已审核的不可变 Draft" /></div> : null}
      <Button disabled={!executable || policyBlocked || execute.isPending || (needsDraft && !submissionDraftId.trim())} onClick={() => execute.mutate()}><Play className="size-4" />{execute.isPending ? "正在调度…" : "通过 Core 调度执行"}</Button>
      {!executable ? <p className="text-sm text-muted-foreground">此任务未声明可执行 capability，只提供只读检查。</p> : null}
    </CardContent></Card>

    <div className="grid gap-4 lg:grid-cols-3">
      <ReadCard title="远端详情" icon={RefreshCw} loading={detail.isFetching} onRead={() => void detail.refetch()}>{detail.data ? <JsonPreview value={detail.data.detail.normalized_detail} /> : <EmptyRead />}</ReadCard>
      <ReadCard title="实时进度" icon={Activity} loading={progress.isFetching} onRead={() => void progress.refetch()}>{progress.data ? <div className="space-y-2 text-sm"><StateBadge state={progress.data.progress.remote_state} /><p>进度 {progress.data.progress.percent == null ? "—" : `${progress.data.progress.percent}%`}</p><p>时长 {progress.data.progress.duration_seconds == null ? "—" : `${progress.data.progress.duration_seconds} 秒`}</p><p className="text-muted-foreground">{formatTimestamp(progress.data.progress.updated_at)}</p></div> : <EmptyRead />}</ReadCard>
      <ReadCard title="学习时长" icon={Clock3} loading={duration.isFetching} onRead={() => void duration.refetch()}>{duration.data ? <div><div className="text-3xl font-semibold">{duration.data.duration.duration_seconds}<span className="ml-1 text-sm font-normal text-muted-foreground">秒</span></div><p className="mt-2 text-sm text-muted-foreground">{formatTimestamp(duration.data.duration.updated_at)}</p></div> : <EmptyRead />}</ReadCard>
    </div>

    {task.data.capabilities.includes("question_inventory") ? <Card><CardHeader className="flex-row items-center justify-between"><CardTitle className="flex items-center gap-2"><FileQuestion className="size-5" />题目快照</CardTitle><Button variant="outline" disabled={questions.isFetching} onClick={() => void questions.refetch()}>{questions.isFetching ? "读取中…" : "读取并解析"}</Button></CardHeader><CardContent>{questions.data ? <div className="space-y-4"><div className="flex flex-wrap gap-2"><Badge variant="outline">snapshot {shortId(questions.data.snapshot_id)}</Badge><Badge variant="secondary">{questions.data.questions.length} 题</Badge><span className="text-sm text-muted-foreground">{formatTimestamp(questions.data.captured_at)}</span></div>{questions.data.questions.map((question) => <div key={question.id} className="rounded-lg border p-4"><div className="mb-2 flex items-center gap-2"><Badge variant="outline">#{question.position + 1}</Badge><Badge variant="secondary">{question.kind}</Badge></div><p className="whitespace-pre-wrap text-sm">{question.stem}</p></div>)}</div> : <p className="text-sm text-muted-foreground">尚未读取当前题目快照。</p>}</CardContent></Card> : null}
  </PageShell>;
}

function ReadCard({ title, icon: Icon, loading, onRead, children }: { title: string; icon: typeof Activity; loading: boolean; onRead: () => void; children: React.ReactNode }) { return <Card><CardHeader className="flex-row items-center justify-between"><CardTitle className="flex items-center gap-2"><Icon className="size-4" />{title}</CardTitle><Button size="sm" variant="outline" disabled={loading} onClick={onRead}>{loading ? "读取中" : "读取"}</Button></CardHeader><CardContent>{children}</CardContent></Card>; }
function EmptyRead() { return <p className="text-sm text-muted-foreground">按需从 Provider 读取，不使用扫描缓存推断。</p>; }
function JsonPreview({ value }: { value: unknown }) { return <pre className="max-h-64 overflow-auto whitespace-pre-wrap break-words rounded-md bg-muted p-3 text-xs">{JSON.stringify(value, null, 2)}</pre>; }
function Summary({ label, children }: { label: string; children: React.ReactNode }) { return <Card><CardHeader className="pb-2"><CardTitle className="text-sm text-muted-foreground">{label}</CardTitle></CardHeader><CardContent className="font-medium">{children}</CardContent></Card>; }
