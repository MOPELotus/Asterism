import { usePermissions } from "@refinedev/core";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { KeyRound, RefreshCw, ShieldX } from "lucide-react";
import { useState } from "react";

import { createServiceToken, listServiceTokens, revokeServiceToken } from "@/api/generated/sdk.gen.ts";
import type { CreateServiceTokenResponse, ServiceScope } from "@/api/generated/types.gen.ts";
import { ensureSuccess, requireData } from "@/api/result.ts";
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

const scopes: ServiceScope[] = [
  "system_read", "provider_read", "provider_manage", "task_read", "task_execute",
  "credit_read", "credit_manage", "audit_read", "service_token_manage",
  "qq_identity_assert", "task_command_proxy", "notification_delivery_report", "binding_verify",
];

export function ServiceTokensPage() {
  const access = usePermissions<string[]>({});
  const allowed = access.data?.includes("manage_system") ?? false;
  const queryClient = useQueryClient();
  const tokens = useQuery({
    queryKey: ["service-tokens"],
    enabled: allowed,
    queryFn: async () => requireData(await listServiceTokens({ query: { limit: 200, offset: 0 } })),
  });
  const [issued, setIssued] = useState<CreateServiceTokenResponse | null>(null);
  const revoke = useMutation({
    mutationFn: async (tokenId: string) => ensureSuccess(await revokeServiceToken({ path: { token_id: tokenId } })),
    onSuccess: async () => queryClient.invalidateQueries({ queryKey: ["service-tokens"] }),
  });

  if (access.isLoading) return <TableSkeleton />;
  if (!allowed) return <PageShell title="无权访问" description="Service Token 管理仅对具有 manage_system 权限的 Web Session 开放。"><Alert><AlertTitle>权限不足</AlertTitle><AlertDescription>owner-bound 委托令牌仍会由后端执行独立的 owner 隔离。</AlertDescription></Alert></PageShell>;

  return <PageShell title="Service Tokens" description="创建 scoped 集成令牌、查看无密钥元数据并撤销访问。明文只展示一次。" actions={<Button variant="outline" onClick={() => tokens.refetch()}><RefreshCw className="size-4" />刷新</Button>}>
    {tokens.error || revoke.error ? <QueryError error={tokens.error ?? revoke.error} /> : null}
    {issued ? <Alert className="border-amber-300 bg-amber-50 dark:border-amber-800 dark:bg-amber-950/30"><KeyRound className="size-4" /><AlertTitle>立即保存令牌，关闭后无法再次读取</AlertTitle><AlertDescription><code className="mt-2 block break-all rounded bg-background p-3 text-xs">{issued.token}</code><Button className="mt-3" size="sm" variant="outline" onClick={() => setIssued(null)}>我已保存并关闭</Button></AlertDescription></Alert> : null}
    <CreateTokenCard onIssued={async (result) => { setIssued(result); await queryClient.invalidateQueries({ queryKey: ["service-tokens"] }); }} />
    <Card><CardHeader><CardTitle>令牌元数据 · {tokens.data?.total ?? 0}</CardTitle></CardHeader><CardContent className="p-0">
      {tokens.isLoading ? <div className="p-5"><TableSkeleton /></div> : <Table><TableHeader><TableRow><TableHead>名称</TableHead><TableHead>Scopes</TableHead><TableHead>状态</TableHead><TableHead>过期</TableHead><TableHead>最近使用</TableHead><TableHead className="text-right">操作</TableHead></TableRow></TableHeader><TableBody>
        {tokens.data?.items.map((token) => {
          const state = token.revoked_at ? "revoked" : token.expires_at && new Date(token.expires_at).getTime() <= Date.now() ? "expired" : "active";
          return <TableRow key={token.id}><TableCell><div className="font-medium">{token.name}</div><div className="font-mono text-xs text-muted-foreground">{shortId(token.id)}</div></TableCell><TableCell><div className="max-w-md text-xs">{token.scopes.join(", ")}</div></TableCell><TableCell><StateBadge state={state} /></TableCell><TableCell>{token.expires_at ? formatTimestamp(token.expires_at) : "永不过期"}</TableCell><TableCell>{token.last_used_at ? formatTimestamp(token.last_used_at) : "—"}</TableCell><TableCell className="text-right"><Button size="sm" variant="outline" disabled={Boolean(token.revoked_at) || revoke.isPending} onClick={() => { if (window.confirm(`撤销 ${token.name}？此操作不能恢复。`)) revoke.mutate(token.id); }}><ShieldX className="size-4" />撤销</Button></TableCell></TableRow>;
        })}
        {!tokens.data?.items.length ? <TableRow><TableCell colSpan={6} className="h-20 text-center text-muted-foreground">暂无 Service Token</TableCell></TableRow> : null}
      </TableBody></Table>}
    </CardContent></Card>
  </PageShell>;
}

function CreateTokenCard({ onIssued }: { onIssued: (result: CreateServiceTokenResponse) => Promise<void> }) {
  const [name, setName] = useState("");
  const [expiresDays, setExpiresDays] = useState("30");
  const [selectedScopes, setSelectedScopes] = useState<ServiceScope[]>(["system_read"]);
  const create = useMutation({
    mutationFn: async () => requireData(await createServiceToken({ body: { name: name.trim(), scopes: selectedScopes, expires_in_seconds: expiresDays ? Number(expiresDays) * 86_400 : undefined } })),
    onSuccess: async (result) => { setName(""); await onIssued(result); },
  });
  return <Card><CardHeader><CardTitle>创建令牌</CardTitle></CardHeader><CardContent className="space-y-4">
    {create.error ? <QueryError error={create.error} /> : null}
    <div className="grid gap-4 md:grid-cols-2"><Field label="名称"><Input value={name} maxLength={128} onChange={(event) => setName(event.target.value)} /></Field><Field label="有效天数（留空为永不过期）"><Input type="number" min={1} value={expiresDays} onChange={(event) => setExpiresDays(event.target.value)} /></Field></div>
    <fieldset><legend className="mb-2 text-sm font-medium">Scopes</legend><div className="grid gap-2 sm:grid-cols-2 lg:grid-cols-3">{scopes.map((scope) => <label key={scope} className="flex items-center gap-2 rounded-md border px-3 py-2 text-xs"><input type="checkbox" checked={selectedScopes.includes(scope)} onChange={() => setSelectedScopes(selectedScopes.includes(scope) ? selectedScopes.filter((item) => item !== scope) : [...selectedScopes, scope])} />{scope}</label>)}</div></fieldset>
    <Button disabled={create.isPending || !name.trim() || selectedScopes.length === 0 || (expiresDays !== "" && (!Number.isSafeInteger(Number(expiresDays)) || Number(expiresDays) <= 0))} onClick={() => create.mutate()}><KeyRound className="size-4" />{create.isPending ? "创建中" : "创建令牌"}</Button>
  </CardContent></Card>;
}

function Field({ label, children }: { label: string; children: React.ReactNode }) { return <div className="space-y-2"><Label>{label}</Label>{children}</div>; }
