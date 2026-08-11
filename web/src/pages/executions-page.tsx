import { useList } from "@refinedev/core";
import { Link } from "react-router";

import type { Execution } from "@/api/generated/types.gen.ts";
import { PageShell } from "@/components/page-shell.tsx";
import { QueryError, TableSkeleton } from "@/components/query-feedback.tsx";
import { StateBadge } from "@/components/state-badge.tsx";
import { buttonVariants } from "@/components/ui/button.tsx";
import { Card, CardContent } from "@/components/ui/card.tsx";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table.tsx";
import { formatTimestamp, shortId } from "@/lib/format.ts";

export function ExecutionsPage() {
  const executions = useList<Execution>({ resource: "executions", pagination: { pageSize: 100 } });

  return (
    <PageShell title="执行" description="检查任务执行状态、尝试历史与 Core 日志。">
      {executions.query.error ? <QueryError error={executions.query.error} /> : null}
      {executions.query.isLoading ? <TableSkeleton /> : (
        <Card><CardContent className="p-0">
          <Table>
            <TableHeader><TableRow>
              <TableHead>执行</TableHead><TableHead>任务</TableHead><TableHead>来源</TableHead>
              <TableHead>状态</TableHead><TableHead>开始时间</TableHead><TableHead className="text-right">详情</TableHead>
            </TableRow></TableHeader>
            <TableBody>
              {executions.result.data?.map((execution) => (
                <TableRow key={execution.id}>
                  <TableCell className="font-mono text-xs">{shortId(execution.id)}</TableCell>
                  <TableCell className="font-mono text-xs">{shortId(execution.task_id)}</TableCell>
                  <TableCell>{execution.request_source}</TableCell>
                  <TableCell><StateBadge state={execution.state} /></TableCell>
                  <TableCell>{formatTimestamp(execution.started_at ?? execution.created_at)}</TableCell>
                  <TableCell className="text-right"><Link className={buttonVariants({ variant: "outline", size: "sm" })} to={`/executions/${execution.id}`}>查看</Link></TableCell>
                </TableRow>
              ))}
              {!executions.result.data?.length ? <TableRow><TableCell colSpan={6} className="h-24 text-center text-muted-foreground">暂无执行记录</TableCell></TableRow> : null}
            </TableBody>
          </Table>
        </CardContent></Card>
      )}
    </PageShell>
  );
}
