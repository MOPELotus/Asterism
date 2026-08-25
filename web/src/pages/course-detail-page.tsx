import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { CheckSquare2, ChevronRight, Play } from "lucide-react";
import { useMemo, useState } from "react";
import { Link, useParams } from "react-router";

import { configureCourseAutomation, executeTask, getCourse, getCourseAutomation, getCourseProgress, getProviderAccount, listTasks } from "@/api/generated/sdk.gen.ts";
import type { Task } from "@/api/generated/types.gen.ts";
import { requireData } from "@/api/result.ts";
import { PageShell } from "@/components/page-shell.tsx";
import { QueryError, TableSkeleton } from "@/components/query-feedback.tsx";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert.tsx";
import { Badge } from "@/components/ui/badge.tsx";
import { Button, buttonVariants } from "@/components/ui/button.tsx";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card.tsx";
import { formatTimestamp } from "@/lib/format.ts";
import { groupTasks, providerName, remoteStateLabel, taskActionLabel, taskTypeLabels } from "@/lib/learning-display.ts";

type DirectCapability = "resource_execution" | "duration_report" | "practice";
type AiProfile = "economy" | "gpt_only";

export function CourseDetailPage() {
  const { courseId = "" } = useParams();
  const queryClient = useQueryClient();
  const [selected, setSelected] = useState<string[]>([]);
  const [startedCount, setStartedCount] = useState<number>();
  const [taskPage, setTaskPage] = useState(1);
  const course = useQuery({ queryKey: ["courses", courseId], enabled: Boolean(courseId), queryFn: async () => requireData(await getCourse({ path: { course_id: courseId } })) });
  const account = useQuery({ queryKey: ["provider-accounts", course.data?.provider_account_id], enabled: Boolean(course.data?.provider_account_id), queryFn: async () => requireData(await getProviderAccount({ path: { account_id: course.data!.provider_account_id } })) });
  const isChaoxing = account.data?.provider_id === "chaoxing";
  const readOnly = false;
  const tasks = useQuery({
    queryKey: ["courses", courseId, "tasks", isChaoxing ? "all" : taskPage],
    enabled: Boolean(courseId && account.data),
    queryFn: () => loadCourseTasks(courseId, isChaoxing, taskPage),
  });
  const progress = useQuery({ queryKey: ["courses", courseId, "progress"], enabled: Boolean(courseId), retry: false, queryFn: async () => requireData(await getCourseProgress({ path: { course_id: courseId } })) });
  const automation = useQuery({ queryKey: ["courses", courseId, "automation"], enabled: Boolean(courseId), retry: false, queryFn: async () => requireData(await getCourseAutomation({ path: { course_id: courseId } })) });
  const automationMutation = useMutation({ mutationFn: async (input: { enabled: boolean; ai_profile: AiProfile | null }) => requireData(await configureCourseAutomation({ path: { course_id: courseId }, body: input })), onSuccess: (value) => queryClient.setQueryData(["courses", courseId, "automation"], value) });
  const automationEnabled = (automation.data as { enabled?: boolean } | undefined)?.enabled === true;
  const automationProfile = (automation.data as { ai_profile?: AiProfile | null } | undefined)?.ai_profile ?? null;
  const courseTasks = useMemo(() => {
    const visible = (tasks.data?.items ?? []).filter((task) => task.remote_state !== "removed");
    if (!isChaoxing) return visible;
    return visible.map((task, index) => ({ task, index })).sort((left, right) => {
      if (left.task.source_type !== "chapter" || right.task.source_type !== "chapter") {
        return left.index - right.index;
      }
      const leftPosition = providerNumber(left.task, "position");
      const rightPosition = providerNumber(right.task, "position");
      if (leftPosition != null && rightPosition != null && leftPosition !== rightPosition) return leftPosition - rightPosition;
      return left.task.title.localeCompare(right.task.title, "zh-CN", { numeric: true })
        || left.task.id.localeCompare(right.task.id);
    }).map(({ task }) => task);
  }, [isChaoxing, tasks.data?.items]);
  const directTasks = useMemo(
    () => courseTasks.filter((task) => selectableTask(task, account.data?.provider_id, readOnly)),
    [account.data?.provider_id, courseTasks, readOnly],
  );
  const allDirectSelected = directTasks.length > 0
    && directTasks.every((task) => selected.includes(task.id));
  const startSelected = useMutation({
    mutationFn: async () => {
      const chosen = directTasks.filter((task) => selected.includes(task.id));
      for (const task of chosen) {
        const capabilities = directCapabilities(task);
        if (!capabilities.length) continue;
        requireData(await executeTask({ path: { task_id: task.id }, headers: { "Idempotency-Key": crypto.randomUUID() }, body: { requested_capabilities: capabilities } }));
      }
      return chosen.length;
    },
    onSuccess: (count) => { setStartedCount(count); setSelected([]); },
  });
  const error = course.error ?? account.error ?? tasks.error ?? progress.error ?? automation.error ?? startSelected.error ?? automationMutation.error;

  if (course.isLoading) return <PageShell title="课程详情" description="正在读取课程。"><TableSkeleton /></PageShell>;
  if (!course.data) return <PageShell title="课程详情" description="课程不存在或当前账号无法访问。">{error ? <QueryError error={error} /> : null}</PageShell>;

  const allTasks = courseTasks;
  const chaoxingGrade = isChaoxing ? courseGradeSummary(course.data.metadata) : undefined;
  return <PageShell title={course.data.title} description={`${providerName(account.data?.provider_id ?? "")} · ${[course.data.term, course.data.teacher].filter(Boolean).join(" · ") || "课程任务"}`} actions={<Link className={buttonVariants({ variant: "outline" })} to={`/provider-accounts/${course.data.provider_account_id}`}>返回账号</Link>}>
    {error ? <QueryError error={error} /> : null}
    {startedCount ? <Alert><AlertTitle>已开始执行</AlertTitle><AlertDescription>{startedCount} 个任务已加入执行队列，可在“执行记录”中查看进度。</AlertDescription></Alert> : null}
    <div className="grid gap-4 sm:grid-cols-3">
      <Summary label={isChaoxing ? "知识点与独立任务" : "全部任务"} value={`${isChaoxing ? courseTasks.length : progress.data?.progress.countable_task_count ?? tasks.data?.total ?? 0} 个`} />
      <Summary label="待完成" value={`${progress.data?.progress.remaining_task_count ?? allTasks.filter((task) => task.remote_state !== "completed").length} 个`} />
      <Summary label="最近同步" value={formatTimestamp(course.data.last_seen_at)} />
    </div>
    {progress.data ? <Card><CardHeader><CardTitle>课程进度</CardTitle></CardHeader><CardContent className="flex flex-wrap gap-2"><Badge variant="secondary">剩余 {progress.data.progress.remaining_task_count}</Badge><Badge variant="secondary">需要人工处理 {progress.data.progress.human_required_task_count}</Badge>{progress.data.progress.duration ? <Badge variant="outline">学习 {Math.round(progress.data.progress.duration.observed_seconds / 60)} 分钟</Badge> : null}</CardContent></Card> : null}
    <Card><CardHeader><CardTitle>自动巡检后执行</CardTitle></CardHeader><CardContent className="flex flex-wrap items-center justify-between gap-3"><div><p className="text-sm">巡检发现新增且可安全执行的课程任务后自动加入队列。</p><p className="text-xs text-muted-foreground">默认关闭；正式作业/考试和需要人工确认的任务不会自动提交。</p></div><div className="flex flex-wrap items-center gap-2"><label className="text-sm text-muted-foreground" htmlFor="course-ai-profile">讨论模型</label><select id="course-ai-profile" aria-label="课程自动执行 AI 组合" className="h-9 rounded-md border bg-background px-2 text-sm" disabled={automation.isLoading || automationMutation.isPending} value={automationProfile ?? ""} onChange={(event) => automationMutation.mutate({ enabled: automationEnabled, ai_profile: event.target.value ? event.target.value as AiProfile : null })}><option value="">继承管理员默认</option><option value="economy">经济组合</option><option value="gpt_only">GPT-only</option></select><Button variant={automationEnabled ? "default" : "outline"} disabled={automation.isLoading || automationMutation.isPending} onClick={() => automationMutation.mutate({ enabled: !automationEnabled, ai_profile: automationProfile })}>{automationMutation.isPending ? "保存中…" : automationEnabled ? "已启用" : "启用自动巡检执行"}</Button></div></CardContent></Card>
    {chaoxingGrade ? <Card><CardHeader><CardTitle>学习通成绩构成</CardTitle><p className="text-sm text-muted-foreground">只读同步平台当前显示的成绩、权重、完成条件与剩余缺口；没有明确显示的字段不会推算。</p></CardHeader><CardContent className="space-y-3">{chaoxingGrade.overall_score != null ? <Badge>综合成绩 {chaoxingGrade.overall_score} 分</Badge> : null}<div className="grid gap-2 md:grid-cols-2 xl:grid-cols-3">{chaoxingGrade.components.map((component) => <div key={component.type} className="rounded-lg border p-3"><p className="font-medium">{gradeComponentLabel(component.type)}</p><div className="mt-2 flex flex-wrap gap-1.5">{component.weight_percent != null ? <Badge variant="secondary">权重 {component.weight_percent}%</Badge> : null}{component.score != null ? <Badge variant="outline">得分 {component.score}</Badge> : null}{component.completion_percent != null ? <Badge variant="outline">完成 {component.completion_percent}%</Badge> : null}{component.observed_minutes != null ? <Badge variant="outline">已计 {component.observed_minutes} 分钟</Badge> : null}{component.required_minutes != null ? <Badge variant="outline">要求 {component.required_minutes} 分钟</Badge> : null}{component.remaining_gap != null && component.remaining_gap > 0 ? <Badge variant="warning">剩余缺口 {component.remaining_gap}{component.required_minutes != null ? " 分钟" : "%"}</Badge> : null}</div>{component.completion_condition ? <p className="mt-2 text-xs text-muted-foreground">完成条件：{component.completion_condition}</p> : null}</div>)}</div></CardContent></Card> : null}
    <Card><CardHeader className="gap-3 sm:flex-row sm:items-center sm:justify-between"><div><CardTitle>课程任务</CardTitle><p className="mt-1 text-sm text-muted-foreground">普通学习任务可以勾选后批量开始；作业、考试和需要填写内容的任务请直接进入处理。</p></div><div className="flex flex-wrap gap-2">{account.data?.provider_id === "uai" && directTasks.length ? <Button variant="outline" onClick={() => setSelected(allDirectSelected ? [] : directTasks.map((task) => task.id))}>{allDirectSelected ? "取消全选" : "全选必做未完成"}</Button> : null}<Button disabled={!selected.length || startSelected.isPending} onClick={() => startSelected.mutate()}><Play className="size-4" />{startSelected.isPending ? "正在开始…" : `开始所选任务${selected.length ? `（${selected.length}）` : ""}`}</Button></div></CardHeader><CardContent>
      {tasks.isLoading ? <TableSkeleton /> : allTasks.length ? <div className="space-y-6">{groupTasks(allTasks).map(([type, grouped]) => <section key={type}><div className="mb-2 flex items-center justify-between gap-2"><div className="flex items-center gap-2"><h2 className="font-semibold">{taskTypeLabels[type]}</h2><Badge variant="secondary">{grouped.length}</Badge></div>{isChaoxing && type === "chapter" && directTasks.length ? <Button size="sm" variant="outline" onClick={() => setSelected(allDirectSelected ? [] : directTasks.map((task) => task.id))}>{allDirectSelected ? "取消全选" : "全选知识点"}</Button> : null}</div><div className="divide-y rounded-xl border">{grouped.map((task) => {
        const selectable = selectableTask(task, account.data?.provider_id, readOnly);
        const checked = selected.includes(task.id);
        const required = providerBoolean(task, "required");
        const finishProgress = providerNumber(task, "finish_progress");
        return <div key={task.id} className="flex items-center gap-3 p-3">
          {selectable ? <input aria-label={`选择 ${task.title}`} type="checkbox" checked={checked} onChange={(event) => setSelected((current) => event.target.checked ? [...current, task.id] : current.filter((id) => id !== task.id))} /> : <CheckSquare2 className="size-4 shrink-0 text-muted-foreground/40" />}
          <div className="min-w-0 flex-1"><div className="flex min-w-0 items-center gap-2"><Link className="block truncate font-medium hover:text-primary" to={`/tasks/${task.id}`}>{task.title}</Link>{required != null ? <Badge variant={required ? "default" : "outline"}>{required ? "必做" : "选做"}</Badge> : null}</div><p className="text-xs text-muted-foreground">{remoteStateLabel(task.remote_state)}{finishProgress != null ? ` · ${finishProgress}%` : ""}{task.due_at ? ` · 截止 ${formatTimestamp(task.due_at)}` : ""}</p></div>
          <Link className={buttonVariants({ variant: readOnly || task.remote_state === "completed" ? "outline" : "default", size: "sm" })} to={`/tasks/${task.id}`}>{readOnly ? "查看详情" : taskActionLabel(task)}<ChevronRight className="size-4" /></Link>
        </div>;
      })}</div></section>)}{!isChaoxing && (tasks.data?.total ?? 0) > 200 ? <div className="flex items-center justify-between border-t pt-4"><span className="text-sm text-muted-foreground">第 {taskPage} / {Math.ceil((tasks.data?.total ?? 0) / 200)} 页</span><div className="flex gap-2"><Button variant="outline" disabled={taskPage <= 1} onClick={() => { setSelected([]); setTaskPage((page) => Math.max(1, page - 1)); }}>上一页</Button><Button variant="outline" disabled={taskPage >= Math.ceil((tasks.data?.total ?? 0) / 200)} onClick={() => { setSelected([]); setTaskPage((page) => page + 1); }}>下一页</Button></div></div> : null}</div> : <p className="rounded-lg border border-dashed p-8 text-center text-sm text-muted-foreground">尚未发现任务，请返回账号页面重新同步。</p>}
    </CardContent></Card>
  </PageShell>;
}

function directCapabilities(task: Task): DirectCapability[] {
  if (task.capabilities.includes("resource_execution")) {
    return task.capabilities.includes("duration_report")
      ? ["resource_execution", "duration_report"]
      : ["resource_execution"];
  }
  if (task.capabilities.includes("duration_report")) return ["duration_report"];
  if (task.capabilities.includes("practice") && !task.capabilities.includes("submission_execute")) return ["practice"];
  return [];
}

function selectableTask(task: Task, providerId: string | undefined, readOnly: boolean): boolean {
  if (readOnly || !directCapabilities(task).length || task.assessment_class === "formal") return false;
  if (providerId === "chaoxing") {
    return task.source_type === "chapter" && !["completed", "not_open", "expired", "removed"].includes(task.remote_state);
  }
  // Worker-backed UAI discussions require freshly generated or manually
  // reviewed plain text and an encrypted invocation draft. They must never be
  // swept into the generic one-click ResourceExecution loop.
  if (providerId === "uai") {
    if (task.source_type === "discussion") return false;
    if (providerBoolean(task, "required") !== true) return false;
  }
  return task.remote_state !== "completed";
}

function providerBoolean(task: Task, key: string): boolean | undefined {
  const value = task.provider_summary?.[key];
  return typeof value === "boolean" ? value : undefined;
}

function providerNumber(task: Task, key: string): number | undefined {
  const value = task.provider_summary?.[key];
  return typeof value === "number" && Number.isFinite(value) ? value : undefined;
}


async function loadCourseTasks(courseId: string, loadAll: boolean, page: number) {
  const limit = 200;
  if (!loadAll) {
    return requireData(await listTasks({ query: { course_id: courseId, limit, offset: (page - 1) * limit } }));
  }
  const items: Task[] = [];
  let total = 0;
  do {
    const result = requireData(await listTasks({ query: { course_id: courseId, limit, offset: items.length } }));
    total = result.total;
    items.push(...result.items);
  } while (items.length < total);
  return { items, total, limit: Math.max(1, items.length), offset: 0 };
}

function Summary({ label, value }: { label: string; value: string }) {
  return <Card><CardHeader className="pb-2"><CardTitle className="text-sm text-muted-foreground">{label}</CardTitle></CardHeader><CardContent className="text-lg font-semibold">{value}</CardContent></Card>;
}

type CourseGradeComponent = {
  type: string;
  weight_percent?: number;
  completion_percent?: number;
  score?: number;
  required_minutes?: number;
  observed_minutes?: number;
  completion_condition?: string;
  remaining_gap?: number;
};

type CourseGradeSummary = { overall_score?: number | null; components: CourseGradeComponent[] };

function courseGradeSummary(metadata: unknown): CourseGradeSummary | undefined {
  if (!metadata || typeof metadata !== "object") return undefined;
  const providerSummary = (metadata as Record<string, unknown>).provider_summary;
  if (!providerSummary || typeof providerSummary !== "object") return undefined;
  const grade = (providerSummary as Record<string, unknown>).grade;
  if (!grade || typeof grade !== "object") return undefined;
  const value = grade as Record<string, unknown>;
  if (!Array.isArray(value.components)) return undefined;
  const components = value.components.filter((component): component is CourseGradeComponent => Boolean(component) && typeof component === "object" && typeof (component as CourseGradeComponent).type === "string");
  const overall = typeof value.overall_score === "number" && Number.isFinite(value.overall_score) ? value.overall_score : null;
  return components.length || overall != null ? { components, overall_score: overall } : undefined;
}

function gradeComponentLabel(type: string): string {
  return ({ video: "视频", chapter_test: "章节任务", homework: "作业", exam: "考试", reading: "阅读", live: "直播", discussion: "讨论", check_in: "签到", document: "文档", visit: "访问", class_activity: "课堂互动" } as Record<string, string>)[type] ?? type;
}
