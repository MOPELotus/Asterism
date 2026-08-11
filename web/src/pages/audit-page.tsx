import { usePermissions } from "@refinedev/core";
import { useQuery } from "@tanstack/react-query";
import { Filter, RefreshCw } from "lucide-react";
import { useState } from "react";

import { listAuditRecords } from "@/api/generated/sdk.gen.ts";
import { requireData } from "@/api/result.ts";
import { PageShell } from "@/components/page-shell.tsx";
import { QueryError, TableSkeleton } from "@/components/query-feedback.tsx";
import { StateBadge } from "@/components/state-badge.tsx";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert.tsx";
import { Button } from "@/components/ui/button.tsx";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card.tsx";
import { Input } from "@/components/ui/input.tsx";
import { Label } from "@/components/ui/label.tsx";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table.tsx";
import { formatTimestamp, shortId } from "@/lib/format.ts";

type Filters = { action?: string; resource_type?: string; outcome?: string };

export function AuditPage() {
  const access = usePermissions<string[]>({});
  const canRead = access.data?.some((permission) => permission === "view_any_audit" || permission === "view_own_audit") ?? false;
  const [draft, setDraft] = useState({ action: "", resource_type: "", outcome: "" });
  const [filters, setFilters] = useState<Filters>({});
  const records = useQuery({
    queryKey: ["audit", filters],
    enabled: canRead,
    queryFn: async () => requireData(await listAuditRecords({ query: { ...filters, limit: 100, offset: 0 } })),
  });

  if (access.isLoading) return <TableSkeleton />;
  if (!canRead) return <PageShell title="无权访问" description="审计记录需要 view_own_audit 或 view_any_audit 权限。"><Alert><AlertTitle>权限不足</AlertTitle><AlertDescription>普通用户不会获得全局审计数据。</AlertDescription></Alert></PageShell>;

  return <PageShell title="审计" description="读取不可变且已脱敏的操作记录；实际数据范围由当前身份权限在后端裁剪。" actions={<Button variant="outline" onClick={() => records.refetch()}><RefreshCw className="size-4" />刷新</Button>}>
    {records.error ? <QueryError error={records.error} /> : null}
    <Card><CardHeader><CardTitle>过滤</CardTitle></CardHeader><CardContent className="grid gap-4 lg:grid-cols-2 2xl:grid-cols-[minmax(0,1fr)_minmax(0,1fr)_minmax(0,1fr)_auto] 2xl:items-end">
      <FilterField label="Action"><Input value={draft.action} maxLength={128} onChange={(event) => setDraft((value) => ({ ...value, action: event.target.value }))} /></FilterField>
      <FilterField label="Resource type"><Input value={draft.resource_type} maxLength={128} onChange={(event) => setDraft((value) => ({ ...value, resource_type: event.target.value }))} /></FilterField>
      <FilterField label="Outcome"><Input value={draft.outcome} maxLength={128} onChange={(event) => setDraft((value) => ({ ...value, outcome: event.target.value }))} /></FilterField>
      <Button onClick={() => setFilters(compactFilters(draft))}><Filter className="size-4" />应用</Button>
    </CardContent></Card>
    <Card><CardHeader><CardTitle>最近记录 · {records.data?.total ?? 0}</CardTitle></CardHeader><CardContent className="p-0">
      {records.isLoading ? <div className="p-5"><TableSkeleton /></div> : <Table><TableHeader><TableRow><TableHead>时间</TableHead><TableHead>Actor</TableHead><TableHead>Action</TableHead><TableHead>Resource</TableHead><TableHead>结果</TableHead><TableHead>脱敏元数据</TableHead></TableRow></TableHeader><TableBody>
        {records.data?.items.map((record) => <TableRow key={record.id}>
          <TableCell className="whitespace-nowrap">{formatTimestamp(record.occurred_at)}</TableCell>
          <TableCell><div>{record.actor_type}</div><div className="font-mono text-xs text-muted-foreground">{record.actor_id ? shortId(record.actor_id) : "system"}</div></TableCell>
          <TableCell className="font-medium">{record.action}</TableCell>
          <TableCell><div>{record.resource_type}</div><div className="font-mono text-xs text-muted-foreground">{record.resource_id ? shortId(record.resource_id) : "—"}</div></TableCell>
          <TableCell><StateBadge state={record.outcome} /></TableCell>
          <TableCell><pre className="max-w-sm overflow-auto whitespace-pre-wrap text-xs text-muted-foreground">{JSON.stringify(record.metadata_sanitized)}</pre></TableCell>
        </TableRow>)}
        {!records.data?.items.length ? <TableRow><TableCell colSpan={6} className="h-20 text-center text-muted-foreground">没有匹配的审计记录</TableCell></TableRow> : null}
      </TableBody></Table>}
    </CardContent></Card>
  </PageShell>;
}

function compactFilters(value: { action: string; resource_type: string; outcome: string }): Filters {
  return Object.fromEntries(Object.entries(value).map(([key, item]) => [key, item.trim()]).filter(([, item]) => item)) as Filters;
}

function FilterField({ label, children }: { label: string; children: React.ReactNode }) { return <div className="space-y-2"><Label>{label}</Label>{children}</div>; }
