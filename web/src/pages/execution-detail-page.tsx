import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useEffect, useState } from "react";
import { Link, useParams } from "react-router";

import { getExecution, listExecutionLogs, streamExecution } from "@/api/generated/sdk.gen.ts";
import type { ExecutionDetailResponse } from "@/api/generated/types.gen.ts";
import { requireData } from "@/api/result.ts";
import { PageShell } from "@/components/page-shell.tsx";
import { QueryError, TableSkeleton } from "@/components/query-feedback.tsx";
import { StateBadge } from "@/components/state-badge.tsx";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card.tsx";
import { buttonVariants } from "@/components/ui/button.tsx";
import { formatTimestamp, shortId } from "@/lib/format.ts";

export function ExecutionDetailPage() {
  const { executionId = "" } = useParams();
  const queryClient = useQueryClient();
  const [streamState, setStreamState] = useState<"connecting" | "live" | "reconnecting">("connecting");
  const detail = useQuery({
    queryKey: ["executions", executionId],
    enabled: Boolean(executionId),
    queryFn: async () => requireData(await getExecution({ path: { execution_id: executionId } })),
  });
  const logs = useQuery({
    queryKey: ["executions", executionId, "logs"],
    enabled: Boolean(executionId),
    queryFn: async () => requireData(await listExecutionLogs({ path: { execution_id: executionId }, query: { limit: 500, offset: 0 } })),
  });

  useEffect(() => {
    if (!executionId) return;
    const controller = new AbortController();
    let active = true;

    async function consume() {
      setStreamState("connecting");
      const result = await streamExecution({
        path: { execution_id: executionId },
        signal: controller.signal,
        sseMaxRetryAttempts: 8,
        onSseError: () => { if (active) setStreamState("reconnecting"); },
        onSseEvent: (event) => {
          if (!active) return;
          setStreamState("live");
          if (event.event === "snapshot" && isExecutionDetail(event.data)) {
            queryClient.setQueryData(["executions", executionId], event.data);
            return;
          }
          void queryClient.invalidateQueries({ queryKey: ["executions", executionId] });
          if (event.event === "execution_log" || event.event === "resync") {
            void queryClient.invalidateQueries({ queryKey: ["executions", executionId, "logs"] });
          }
        },
      });
      for await (const _event of result.stream) {
        if (!active) break;
      }
    }

    void consume().catch(() => { if (active) setStreamState("reconnecting"); });
    return () => { active = false; controller.abort(); };
  }, [executionId, queryClient]);

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
          {detail.data.next_question_snapshot_id ? <Card><CardHeader><CardTitle>连续题目已就绪</CardTitle></CardHeader><CardContent className="flex flex-wrap items-center gap-3"><p className="text-sm text-muted-foreground">本次提交已物化下一道不可变题目快照，可继续审核并提交。</p><Link className={buttonVariants()} to={`/tasks/${detail.data.execution.task_id}/question-snapshots/${detail.data.next_question_snapshot_id}`}>进入下一题</Link></CardContent></Card> : null}
        </>
      ) : null}
      <Card><CardHeader className="flex-row items-center justify-between"><CardTitle>运行日志</CardTitle><div className="flex items-center gap-2 text-xs text-muted-foreground"><span className={`size-2 rounded-full ${streamState === "live" ? "bg-emerald-500" : streamState === "reconnecting" ? "bg-amber-500" : "bg-slate-400"}`} />{streamState === "live" ? "Core SSE 实时" : streamState === "reconnecting" ? "正在重连并重同步" : "正在连接"}</div></CardHeader><CardContent>
        <div className="max-h-[32rem] overflow-auto rounded-lg bg-slate-950 p-4 font-mono text-xs text-slate-100">
          {logs.data?.items.map((event, index) => <div key={`${event.timestamp}-${index}`} className="grid gap-2 border-b border-slate-800 py-2 sm:grid-cols-[10rem_5rem_9rem_1fr]"><span className="text-slate-400">{formatTimestamp(event.timestamp)}</span><span>{event.level}</span><span className="text-cyan-300">{event.stage}</span><span className="whitespace-pre-wrap break-words">{event.message}</span></div>)}
          {!logs.data?.items.length ? <div className="text-slate-400">暂无日志；页面会自动刷新。</div> : null}
        </div>
      </CardContent></Card>
    </PageShell>
  );
}

function isExecutionDetail(value: unknown): value is ExecutionDetailResponse {
  if (!value || typeof value !== "object" || !("execution" in value)) return false;
  const execution = value.execution;
  return Boolean(execution && typeof execution === "object" && "id" in execution && typeof execution.id === "string");
}

function DetailCard({ label, children }: { label: string; children: React.ReactNode }) {
  return <Card><CardHeader className="pb-2"><CardTitle className="text-sm text-muted-foreground">{label}</CardTitle></CardHeader><CardContent className="font-medium">{children}</CardContent></Card>;
}
