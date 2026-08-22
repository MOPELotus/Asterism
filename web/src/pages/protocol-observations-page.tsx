import { usePermissions } from "@refinedev/core";
import { useQuery } from "@tanstack/react-query";
import { Filter, RefreshCw } from "lucide-react";
import { useState } from "react";

import { listProtocolObservations } from "@/api/generated/sdk.gen.ts";
import type { ProtocolObservation } from "@/api/generated/types.gen.ts";
import { requireData } from "@/api/result.ts";
import { PageShell } from "@/components/page-shell.tsx";
import { QueryError, TableSkeleton } from "@/components/query-feedback.tsx";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert.tsx";
import { Badge } from "@/components/ui/badge.tsx";
import { Button } from "@/components/ui/button.tsx";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card.tsx";
import { Input } from "@/components/ui/input.tsx";
import { Label } from "@/components/ui/label.tsx";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table.tsx";
import { formatTimestamp, shortId } from "@/lib/format.ts";

type ObservationKind = ProtocolObservation["kind"];
const kinds: Array<{ value: ObservationKind | ""; label: string }> = [
  { value: "", label: "全部类型" },
  { value: "unknown_question_kind", label: "未知题型" },
  { value: "unknown_result_shape", label: "未知结果" },
  { value: "unknown_task_type", label: "未知任务" },
  { value: "field_drift", label: "字段漂移" },
  { value: "endpoint_version_drift", label: "端点版本漂移" },
  { value: "other", label: "其他" },
];

export function ProtocolObservationsPage() {
  const permissions = usePermissions<string[]>({});
  const canManageSystem = permissions.data?.includes("manage_system") ?? false;
  const [providerDraft, setProviderDraft] = useState("");
  const [kindDraft, setKindDraft] = useState<ObservationKind | "">("");
  const [filters, setFilters] = useState<{ provider_id?: string; kind?: ObservationKind }>({});
  const observations = useQuery({
    queryKey: ["protocol-observations", filters],
    enabled: canManageSystem,
    queryFn: async () => requireData(await listProtocolObservations({ query: { ...filters, limit: 200, offset: 0 } })),
  });

  if (permissions.isLoading) return <TableSkeleton />;
  if (!canManageSystem) return <PageShell title="无权访问" description="协议观察收件箱只向 Master 开放。"><Alert><AlertTitle>权限不足</AlertTitle><AlertDescription>该页面可能包含用于协议补漏的脱敏结构信息。</AlertDescription></Alert></PageShell>;

  return <PageShell title="协议观察" description="集中查看真实账号运行时捕获的未知题型、结果形状和协议漂移；内容已在后端脱敏聚合。" actions={<Button variant="outline" onClick={() => observations.refetch()}><RefreshCw className="size-4" />刷新</Button>}>
    {observations.error ? <QueryError error={observations.error} /> : null}
    <Card><CardHeader><CardTitle>过滤</CardTitle></CardHeader><CardContent className="grid gap-4 md:grid-cols-[minmax(0,1fr)_minmax(0,1fr)_auto] md:items-end">
      <div className="space-y-2"><Label htmlFor="observation-provider">Provider ID</Label><Input id="observation-provider" value={providerDraft} onChange={(event) => setProviderDraft(event.target.value)} placeholder="chaoxing / welearn / uai / cidaren" /></div>
      <div className="space-y-2"><Label htmlFor="observation-kind">类型</Label><select id="observation-kind" className="h-9 w-full rounded-md border border-input bg-background px-3 text-sm" value={kindDraft} onChange={(event) => setKindDraft(event.target.value as ObservationKind | "")}>{kinds.map((item) => <option key={item.value || "all"} value={item.value}>{item.label}</option>)}</select></div>
      <Button onClick={() => setFilters({ ...(providerDraft.trim() ? { provider_id: providerDraft.trim().toLowerCase() } : {}), ...(kindDraft ? { kind: kindDraft } : {}) })}><Filter className="size-4" />应用</Button>
    </CardContent></Card>
    <Card><CardHeader><CardTitle>观察聚合 · {observations.data?.total ?? 0}</CardTitle></CardHeader><CardContent className="p-0">
      {observations.isLoading ? <div className="p-5"><TableSkeleton /></div> : <Table><TableHeader><TableRow><TableHead>最近出现</TableHead><TableHead>Provider / Surface</TableHead><TableHead>类型</TableHead><TableHead>次数</TableHead><TableHead>关联执行</TableHead><TableHead>脱敏结构</TableHead></TableRow></TableHeader><TableBody>
        {observations.data?.items.map((item) => <TableRow key={item.id}><TableCell className="whitespace-nowrap"><div>{formatTimestamp(item.last_seen_at)}</div><div className="text-xs text-muted-foreground">首次 {formatTimestamp(item.first_seen_at)}</div></TableCell><TableCell><div className="font-medium">{item.provider_id}</div><div className="text-xs text-muted-foreground">{item.surface}</div></TableCell><TableCell><Badge variant="outline">{item.kind}</Badge></TableCell><TableCell>{item.occurrence_count}</TableCell><TableCell className="font-mono text-xs">{item.last_execution_id ? shortId(item.last_execution_id) : "—"}</TableCell><TableCell><pre className="max-h-40 max-w-xl overflow-auto whitespace-pre-wrap text-xs">{JSON.stringify(item.shape_sanitized, null, 2)}</pre></TableCell></TableRow>)}
        {!observations.data?.items.length ? <TableRow><TableCell colSpan={6} className="h-20 text-center text-muted-foreground">暂无匹配的协议观察</TableCell></TableRow> : null}
      </TableBody></Table>}
    </CardContent></Card>
  </PageShell>;
}
