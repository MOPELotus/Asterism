import { useList } from "@refinedev/core";
import { Link, useSearchParams } from "react-router";

import type { Course } from "@/api/generated/types.gen.ts";
import { PageShell } from "@/components/page-shell.tsx";
import { QueryError, TableSkeleton } from "@/components/query-feedback.tsx";
import { Card, CardContent } from "@/components/ui/card.tsx";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table.tsx";
import { formatTimestamp } from "@/lib/format.ts";

export function CoursesPage() {
  const [searchParams] = useSearchParams();
  const accountId = searchParams.get("provider_account_id");
  const courses = useList<Course>({ resource: "courses", pagination: { pageSize: 100 }, filters: accountId ? [{ field: "provider_account_id", operator: "eq", value: accountId }] : undefined });

  return <PageShell title="课程" description={accountId ? "当前账号已同步的课程。" : "全部学习平台账号已同步的课程。"}>
    {courses.query.error ? <QueryError error={courses.query.error} /> : null}
    {courses.query.isLoading ? <TableSkeleton /> : <Card><CardContent className="p-0"><Table>
      <TableHeader><TableRow><TableHead>课程</TableHead><TableHead>学期</TableHead><TableHead>教师</TableHead><TableHead>远端状态</TableHead><TableHead>最近发现</TableHead></TableRow></TableHeader>
      <TableBody>{courses.result.data?.map((course) => <TableRow key={course.id}>
        <TableCell className="max-w-lg"><Link className="block truncate font-medium text-primary hover:underline" to={`/courses/${course.id}`}>{course.title}</Link></TableCell>
        <TableCell>{course.term ?? "—"}</TableCell><TableCell>{course.teacher ?? "—"}</TableCell><TableCell>{course.remote_status ?? "—"}</TableCell><TableCell>{formatTimestamp(course.last_seen_at)}</TableCell>
      </TableRow>)}{!courses.result.data?.length ? <TableRow><TableCell colSpan={5} className="h-24 text-center text-muted-foreground">尚未发现课程；先在平台账号页完成认证并立即巡查。</TableCell></TableRow> : null}</TableBody>
    </Table></CardContent></Card>}
  </PageShell>;
}
