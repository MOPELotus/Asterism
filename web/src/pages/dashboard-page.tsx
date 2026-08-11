import { useList } from "@refinedev/core";
import { Activity, CircleDollarSign, ListTodo, PlugZap } from "lucide-react";

import type {
  Execution,
  ProviderAccountResponse,
  ProviderMetadata,
  Task,
} from "@/api/generated/types.gen.ts";
import { PageShell } from "@/components/page-shell.tsx";
import { QueryError, TableSkeleton } from "@/components/query-feedback.tsx";
import { StateBadge } from "@/components/state-badge.tsx";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card.tsx";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table.tsx";
import { formatTimestamp, shortId } from "@/lib/format.ts";

export function DashboardPage() {
  const providers = useList<ProviderMetadata>({ resource: "providers", pagination: { pageSize: 100 } });
  const accounts = useList<ProviderAccountResponse>({ resource: "provider-accounts", pagination: { pageSize: 100 } });
  const tasks = useList<Task>({ resource: "tasks", pagination: { pageSize: 100 } });
  const executions = useList<Execution>({ resource: "executions", pagination: { pageSize: 8 } });

  const queries = [providers.query, accounts.query, tasks.query, executions.query];
  const error = queries.find((query) => query.error)?.error;

  return (
    <PageShell title="概览" description="查看平台接入、任务发现与最近执行的当前状态。">
      {error ? <QueryError error={error} /> : null}
      <div className="grid gap-4 sm:grid-cols-2 xl:grid-cols-4">
        <MetricCard title="Provider" value={providers.result.total ?? 0} icon={PlugZap} />
        <MetricCard title="平台账号" value={accounts.result.total ?? 0} icon={CircleDollarSign} />
        <MetricCard title="任务" value={tasks.result.total ?? 0} icon={ListTodo} />
        <MetricCard title="执行" value={executions.result.total ?? 0} icon={Activity} />
      </div>

      <Card>
        <CardHeader>
          <CardTitle>最近执行</CardTitle>
        </CardHeader>
        <CardContent>
          {executions.query.isLoading ? (
            <TableSkeleton />
          ) : (
            <Table>
              <TableHeader>
                <TableRow>
                  <TableHead>执行</TableHead>
                  <TableHead>任务</TableHead>
                  <TableHead>状态</TableHead>
                  <TableHead>创建时间</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {executions.result.data?.map((execution) => (
                  <TableRow key={execution.id}>
                    <TableCell className="font-mono text-xs">{shortId(execution.id)}</TableCell>
                    <TableCell className="font-mono text-xs">{shortId(execution.task_id)}</TableCell>
                    <TableCell><StateBadge state={execution.state} /></TableCell>
                    <TableCell>{formatTimestamp(execution.created_at)}</TableCell>
                  </TableRow>
                ))}
                {!executions.result.data?.length ? (
                  <TableRow><TableCell colSpan={4} className="h-24 text-center text-muted-foreground">暂无执行记录</TableCell></TableRow>
                ) : null}
              </TableBody>
            </Table>
          )}
        </CardContent>
      </Card>
    </PageShell>
  );
}

function MetricCard({ title, value, icon: Icon }: { title: string; value: number; icon: typeof Activity }) {
  return (
    <Card>
      <CardHeader className="flex flex-row items-center justify-between space-y-0 pb-2">
        <CardTitle className="text-sm font-medium text-muted-foreground">{title}</CardTitle>
        <Icon className="size-4 text-primary" />
      </CardHeader>
      <CardContent><div className="text-3xl font-semibold">{value}</div></CardContent>
    </Card>
  );
}
