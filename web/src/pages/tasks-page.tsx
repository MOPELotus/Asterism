import { useList } from "@refinedev/core";
import { Link } from "react-router";

import type { Task } from "@/api/generated/types.gen.ts";
import { PageShell } from "@/components/page-shell.tsx";
import { QueryError, TableSkeleton } from "@/components/query-feedback.tsx";
import { StateBadge } from "@/components/state-badge.tsx";
import { Card, CardContent } from "@/components/ui/card.tsx";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table.tsx";
import { formatTimestamp, shortId } from "@/lib/format.ts";

export function TasksPage() {
  const tasks = useList<Task>({ resource: "tasks", pagination: { pageSize: 100 } });

  return (
    <PageShell title="任务" description="汇总所有 Provider 发现的标准化学习任务。">
      {tasks.query.error ? <QueryError error={tasks.query.error} /> : null}
      {tasks.query.isLoading ? <TableSkeleton /> : (
        <Card><CardContent className="p-0">
          <Table>
            <TableHeader><TableRow>
              <TableHead>任务</TableHead><TableHead>类型</TableHead><TableHead>远端状态</TableHead>
              <TableHead>编排状态</TableHead><TableHead>截止时间</TableHead><TableHead>更新时间</TableHead>
            </TableRow></TableHeader>
            <TableBody>
              {tasks.result.data?.map((task) => (
                <TableRow key={task.id}>
                  <TableCell className="max-w-md"><Link className="block truncate font-medium text-primary hover:underline" to={`/tasks/${task.id}`}>{task.title}</Link><div className="font-mono text-xs text-muted-foreground">{shortId(task.id)}</div></TableCell>
                  <TableCell>{task.source_type}</TableCell>
                  <TableCell><StateBadge state={task.remote_state} /></TableCell>
                  <TableCell><StateBadge state={task.orchestration_state} /></TableCell>
                  <TableCell>{formatTimestamp(task.due_at)}</TableCell>
                  <TableCell>{formatTimestamp(task.updated_at)}</TableCell>
                </TableRow>
              ))}
              {!tasks.result.data?.length ? <TableRow><TableCell colSpan={6} className="h-24 text-center text-muted-foreground">暂无任务</TableCell></TableRow> : null}
            </TableBody>
          </Table>
        </CardContent></Card>
      )}
    </PageShell>
  );
}
