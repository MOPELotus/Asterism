import { useQuery } from "@tanstack/react-query";
import { useParams } from "react-router";

import { getExecution, listExecutionLogs } from "@/api/generated/sdk.gen.ts";
import { requireData } from "@/api/result.ts";
import { PageShell } from "@/components/page-shell.tsx";
import { QueryError, TableSkeleton } from "@/components/query-feedback.tsx";
import { StateBadge } from "@/components/state-badge.tsx";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card.tsx";
import { formatTimestamp, shortId } from "@/lib/format.ts";

export function ExecutionDetailPage() {
  const { executionId = "" } = useParams();
  const detail = useQuery({
    queryKey: ["executions", executionId],
    enabled: Boolean(executionId),
    refetchInterval: 5_000,
    queryFn: async () => requireData(await getExecution({ path: { execution_id: executionId } })),
  });
  const logs = useQuery({
    queryKey: ["executions", executionId, "logs"],
    enabled: Boolean(executionId),
    refetchInterval: 3_000,
    queryFn: async () => requireData(await listExecutionLogs({ path: { execution_id: executionId }, query: { limit: 500, offset: 0 } })),
  });

  return (
    <PageShell title="执行详情" description={`执行 ${shortId(executionId)} 的状态、尝试与 Core 日志。`}>
      {detail.error || logs.error ? <QueryError error={detail.error ?? logs.error} /> : null}
      {detail.isLoading ? <TableSkeleton /> : detail.data ? (
        <>
          <div className="grid gap-4 sm:grid-cols-2 xl:grid-cols-4">
            <DetailCard label="状态"><StateBadge state={detail.data.execution.state} /></DetailCard>
            <DetailCard label="阶段">{detail.data.progress?.stage ?? "—"}</DetailCard>
            <DetailCard label="进度">{detail.data.progress?.percent == null ? "—" : `${detail.data.progress.percent}%`}</DetailCard>
            <DetailCard label="当前项目">{detail.data.progress?.current_item ?? "—"}</DetailCard>
          </div>
          <Card><CardHeader><CardTitle>尝试历史</CardTitle></CardHeader><CardContent className="space-y-3">
            {detail.data.attempts.map((attempt) => <div key={attempt.id} className="grid gap-2 rounded-lg border p-4 text-sm sm:grid-cols-5"><span>#{attempt.attempt_no}</span><span>{attempt.result ? <StateBadge state={attempt.result} /> : "进行中"}</span><span>{attempt.error_class ?? "无错误"}</span><span className="font-mono text-xs">{attempt.provider_trace_id ?? "无 trace"}</span><span>{formatTimestamp(attempt.started_at)}</span></div>)}
            {!detail.data.attempts.length ? <p className="text-sm text-muted-foreground">尚未产生执行尝试。</p> : null}
          </CardContent></Card>
        </>
      ) : null}
      <Card><CardHeader><CardTitle>运行日志</CardTitle></CardHeader><CardContent>
        <div className="max-h-[32rem] overflow-auto rounded-lg bg-slate-950 p-4 font-mono text-xs text-slate-100">
          {logs.data?.items.map((event, index) => <div key={`${event.timestamp}-${index}`} className="grid gap-2 border-b border-slate-800 py-2 sm:grid-cols-[10rem_5rem_9rem_1fr]"><span className="text-slate-400">{formatTimestamp(event.timestamp)}</span><span>{event.level}</span><span className="text-cyan-300">{event.stage}</span><span className="whitespace-pre-wrap break-words">{event.message}</span></div>)}
          {!logs.data?.items.length ? <div className="text-slate-400">暂无日志；页面会自动刷新。</div> : null}
        </div>
      </CardContent></Card>
    </PageShell>
  );
}

function DetailCard({ label, children }: { label: string; children: React.ReactNode }) {
  return <Card><CardHeader className="pb-2"><CardTitle className="text-sm text-muted-foreground">{label}</CardTitle></CardHeader><CardContent className="font-medium">{children}</CardContent></Card>;
}
