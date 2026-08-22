import { useQuery } from "@tanstack/react-query";
import { ExternalLink } from "lucide-react";
import { Link, useParams } from "react-router";

import { getCourse, getCourseProgress, getProviderAccount } from "@/api/generated/sdk.gen.ts";
import { requireData } from "@/api/result.ts";
import { PageShell } from "@/components/page-shell.tsx";
import { QueryError, TableSkeleton } from "@/components/query-feedback.tsx";
import { Badge } from "@/components/ui/badge.tsx";
import { buttonVariants } from "@/components/ui/button.tsx";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card.tsx";
import { formatTimestamp } from "@/lib/format.ts";

export function CourseDetailPage() {
  const { courseId = "" } = useParams();
  const course = useQuery({ queryKey: ["courses", courseId], enabled: Boolean(courseId), queryFn: async () => requireData(await getCourse({ path: { course_id: courseId } })) });
  const account = useQuery({ queryKey: ["provider-accounts", course.data?.provider_account_id], enabled: Boolean(course.data?.provider_account_id), queryFn: async () => requireData(await getProviderAccount({ path: { account_id: course.data!.provider_account_id } })) });
  const progress = useQuery({ queryKey: ["courses", courseId, "progress"], enabled: Boolean(courseId), retry: false, queryFn: async () => requireData(await getCourseProgress({ path: { course_id: courseId } })) });
  const error = course.error ?? account.error ?? progress.error;
  if (course.isLoading) return <PageShell title="课程详情" description="正在读取课程。"><TableSkeleton /></PageShell>;
  if (!course.data) return <PageShell title="课程详情" description="课程不存在或当前身份不可访问。">{error ? <QueryError error={error} /> : null}</PageShell>;
  const isWelearn = account.data?.provider_id === "welearn";
  const accountHref = isWelearn ? `/provider-accounts/${course.data.provider_account_id}?courseId=${encodeURIComponent(course.data.id)}&remoteCourseId=${encodeURIComponent(course.data.remote_id)}` : `/provider-accounts/${course.data.provider_account_id}`;
  return <PageShell title={course.data.title} description={`${course.data.term ?? "未标注学期"} · ${course.data.teacher ?? "未标注教师"}`} actions={<Link className={buttonVariants({ variant: "outline" })} to={accountHref}><ExternalLink className="size-4" />{isWelearn ? "预填 WELearn 批执行" : "查看平台账号"}</Link>}>
    {error ? <QueryError error={error} /> : null}
    <div className="grid gap-4 md:grid-cols-3"><Summary label="本地 Course UUID" value={course.data.id} /><Summary label="规范化远端 Course ID" value={course.data.remote_id} /><Summary label="最近发现" value={formatTimestamp(course.data.last_seen_at)} /></div>
    {progress.data ? <Card><CardHeader><CardTitle>聚合进度</CardTitle></CardHeader><CardContent className="flex flex-wrap gap-2">{progress.data.progress.required ? <Badge variant="outline">required {progress.data.progress.required.completed_required_task_count}/{progress.data.progress.required.required_task_count}</Badge> : null}<Badge variant="secondary">remaining {progress.data.progress.remaining_task_count}</Badge><Badge variant="secondary">failed {progress.data.progress.failed_task_count}</Badge><Badge variant="secondary">human {progress.data.progress.human_required_task_count}</Badge>{progress.data.progress.duration ? <Badge variant="secondary">duration {progress.data.progress.duration.observed_seconds}s</Badge> : null}</CardContent></Card> : null}
    <Card><CardHeader><CardTitle>Provider 元数据</CardTitle></CardHeader><CardContent><pre className="max-h-96 overflow-auto rounded-md bg-muted p-3 text-xs">{JSON.stringify(course.data.metadata, null, 2)}</pre></CardContent></Card>
  </PageShell>;
}

function Summary({ label, value }: { label: string; value: string }) { return <Card><CardHeader className="pb-2"><CardTitle className="text-sm text-muted-foreground">{label}</CardTitle></CardHeader><CardContent className="break-all font-mono text-sm">{value}</CardContent></Card>; }
