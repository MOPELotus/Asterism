import { useMutation, useQuery } from "@tanstack/react-query";
import { CheckSquare2, ChevronRight, Play } from "lucide-react";
import { useMemo, useState } from "react";
import { Link, useParams } from "react-router";

import { executeTask, getCourse, getCourseProgress, getProviderAccount, listTasks } from "@/api/generated/sdk.gen.ts";
import type { Course, Task } from "@/api/generated/types.gen.ts";
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

export function CourseDetailPage() {
  const { courseId = "" } = useParams();
  const [selected, setSelected] = useState<string[]>([]);
  const [startedCount, setStartedCount] = useState<number>();
  const [taskPage, setTaskPage] = useState(1);
  const course = useQuery({ queryKey: ["courses", courseId], enabled: Boolean(courseId), queryFn: async () => requireData(await getCourse({ path: { course_id: courseId } })) });
  const account = useQuery({ queryKey: ["provider-accounts", course.data?.provider_account_id], enabled: Boolean(course.data?.provider_account_id), queryFn: async () => requireData(await getProviderAccount({ path: { account_id: course.data!.provider_account_id } })) });
  const isChaoxing = account.data?.provider_id === "chaoxing";
  const readOnly = courseReadOnly(course.data);
  const tasks = useQuery({
    queryKey: ["courses", courseId, "tasks", isChaoxing ? "all" : taskPage],
    enabled: Boolean(courseId && account.data),
    queryFn: () => loadCourseTasks(courseId, isChaoxing, taskPage),
  });
  const progress = useQuery({ queryKey: ["courses", courseId, "progress"], enabled: Boolean(courseId), retry: false, queryFn: async () => requireData(await getCourseProgress({ path: { course_id: courseId } })) });
  const courseTasks = useMemo(() => {
    const visible = (tasks.data?.items ?? []).filter((task) => task.remote_state !== "removed");
    if (!isChaoxing) return visible;
    return visible.map((task, index) => ({ task, index })).sort((left, right) => {
      if (left.task.source_type !== "chapter" || right.task.source_type !== "chapter") {
        return left.index - right.index;
      }
      return left.task.title.localeCompare(right.task.title, "zh-CN", { numeric: true })
        || left.task.id.localeCompare(right.task.id);
    }).map(({ task }) => task);
  }, [isChaoxing, tasks.data?.items]);
  const directTasks = useMemo(
    () => courseTasks.filter((task) => selectableTask(task, account.data?.provider_id, readOnly)),
    [account.data?.provider_id, courseTasks, readOnly],
  );
  const allKnowledgeSelected = directTasks.length > 0
    && directTasks.every((task) => selected.includes(task.id));
  const startSelected = useMutation({
    mutationFn: async () => {
      const chosen = directTasks.filter((task) => selected.includes(task.id));
      for (const task of chosen) {
        const capability = directCapability(task);
        if (!capability) continue;
        requireData(await executeTask({ path: { task_id: task.id }, headers: { "Idempotency-Key": crypto.randomUUID() }, body: { requested_capabilities: [capability] } }));
      }
      return chosen.length;
    },
    onSuccess: (count) => { setStartedCount(count); setSelected([]); },
  });
  const error = course.error ?? account.error ?? tasks.error ?? progress.error ?? startSelected.error;

  if (course.isLoading) return <PageShell title="课程详情" description="正在读取课程。"><TableSkeleton /></PageShell>;
  if (!course.data) return <PageShell title="课程详情" description="课程不存在或当前账号无法访问。">{error ? <QueryError error={error} /> : null}</PageShell>;

  const allTasks = courseTasks;
  return <PageShell title={course.data.title} description={`${providerName(account.data?.provider_id ?? "")} · ${[course.data.term, course.data.teacher].filter(Boolean).join(" · ") || "课程任务"}`} actions={<Link className={buttonVariants({ variant: "outline" })} to={`/provider-accounts/${course.data.provider_account_id}`}>返回账号</Link>}>
    {error ? <QueryError error={error} /> : null}
    {startedCount ? <Alert><AlertTitle>已开始执行</AlertTitle><AlertDescription>{startedCount} 个任务已加入执行队列，可在“执行记录”中查看进度。</AlertDescription></Alert> : null}
    {readOnly ? <Alert><AlertTitle>只读课程</AlertTitle><AlertDescription>这门 WELearn 新课程只同步目录和完成状态，不会创建批量执行或写入平台。</AlertDescription></Alert> : null}
    <div className="grid gap-4 sm:grid-cols-3">
      <Summary label={isChaoxing ? "知识点与独立任务" : "全部任务"} value={`${isChaoxing ? courseTasks.length : progress.data?.progress.countable_task_count ?? tasks.data?.total ?? 0} 个`} />
      <Summary label="待完成" value={`${progress.data?.progress.remaining_task_count ?? allTasks.filter((task) => task.remote_state !== "completed").length} 个`} />
      <Summary label="最近同步" value={formatTimestamp(course.data.last_seen_at)} />
    </div>
    {progress.data ? <Card><CardHeader><CardTitle>课程进度</CardTitle></CardHeader><CardContent className="flex flex-wrap gap-2"><Badge variant="secondary">剩余 {progress.data.progress.remaining_task_count}</Badge><Badge variant="secondary">需要人工处理 {progress.data.progress.human_required_task_count}</Badge>{progress.data.progress.duration ? <Badge variant="outline">学习 {Math.round(progress.data.progress.duration.observed_seconds / 60)} 分钟</Badge> : null}</CardContent></Card> : null}
    <Card><CardHeader className="gap-3 sm:flex-row sm:items-center sm:justify-between"><div><CardTitle>课程任务</CardTitle><p className="mt-1 text-sm text-muted-foreground">普通学习任务可以勾选后批量开始；作业、考试和需要填写内容的任务请直接进入处理。</p></div><Button disabled={!selected.length || startSelected.isPending} onClick={() => startSelected.mutate()}><Play className="size-4" />{startSelected.isPending ? "正在开始…" : `开始所选任务${selected.length ? `（${selected.length}）` : ""}`}</Button></CardHeader><CardContent>
      {tasks.isLoading ? <TableSkeleton /> : allTasks.length ? <div className="space-y-6">{groupTasks(allTasks).map(([type, grouped]) => <section key={type}><div className="mb-2 flex items-center justify-between gap-2"><div className="flex items-center gap-2"><h2 className="font-semibold">{taskTypeLabels[type]}</h2><Badge variant="secondary">{grouped.length}</Badge></div>{isChaoxing && type === "chapter" && directTasks.length ? <Button size="sm" variant="outline" onClick={() => setSelected(allKnowledgeSelected ? [] : directTasks.map((task) => task.id))}>{allKnowledgeSelected ? "取消全选" : "全选知识点"}</Button> : null}</div><div className="divide-y rounded-xl border">{grouped.map((task) => {
        const selectable = selectableTask(task, account.data?.provider_id, readOnly);
        const checked = selected.includes(task.id);
        return <div key={task.id} className="flex items-center gap-3 p-3">
          {selectable ? <input aria-label={`选择 ${task.title}`} type="checkbox" checked={checked} onChange={(event) => setSelected((current) => event.target.checked ? [...current, task.id] : current.filter((id) => id !== task.id))} /> : <CheckSquare2 className="size-4 shrink-0 text-muted-foreground/40" />}
          <div className="min-w-0 flex-1"><Link className="block truncate font-medium hover:text-primary" to={`/tasks/${task.id}`}>{task.title}</Link><p className="text-xs text-muted-foreground">{remoteStateLabel(task.remote_state)}{task.due_at ? ` · 截止 ${formatTimestamp(task.due_at)}` : ""}</p></div>
          <Link className={buttonVariants({ variant: readOnly || task.remote_state === "completed" ? "outline" : "default", size: "sm" })} to={`/tasks/${task.id}`}>{readOnly ? "查看详情" : taskActionLabel(task)}<ChevronRight className="size-4" /></Link>
        </div>;
      })}</div></section>)}{!isChaoxing && (tasks.data?.total ?? 0) > 200 ? <div className="flex items-center justify-between border-t pt-4"><span className="text-sm text-muted-foreground">第 {taskPage} / {Math.ceil((tasks.data?.total ?? 0) / 200)} 页</span><div className="flex gap-2"><Button variant="outline" disabled={taskPage <= 1} onClick={() => { setSelected([]); setTaskPage((page) => Math.max(1, page - 1)); }}>上一页</Button><Button variant="outline" disabled={taskPage >= Math.ceil((tasks.data?.total ?? 0) / 200)} onClick={() => { setSelected([]); setTaskPage((page) => page + 1); }}>下一页</Button></div></div> : null}</div> : <p className="rounded-lg border border-dashed p-8 text-center text-sm text-muted-foreground">尚未发现任务，请返回账号页面重新同步。</p>}
    </CardContent></Card>
  </PageShell>;
}

function directCapability(task: Task): DirectCapability | undefined {
  if (task.capabilities.includes("resource_execution")) return "resource_execution";
  if (task.capabilities.includes("duration_report")) return "duration_report";
  if (task.capabilities.includes("practice") && !task.capabilities.includes("submission_execute")) return "practice";
  return undefined;
}

function selectableTask(task: Task, providerId: string | undefined, readOnly: boolean): boolean {
  if (readOnly || !directCapability(task) || task.assessment_class === "formal") return false;
  if (providerId === "chaoxing") return task.source_type === "chapter";
  return task.remote_state !== "completed";
}

function courseReadOnly(course: Course | undefined): boolean {
  return Boolean(course?.metadata && typeof course.metadata === "object"
    && (course.metadata as Record<string, unknown>).read_only === true);
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
