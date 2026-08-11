import { useList } from "@refinedev/core";

import type { ProviderAccountResponse } from "@/api/generated/types.gen.ts";
import { PageShell } from "@/components/page-shell.tsx";
import { QueryError, TableSkeleton } from "@/components/query-feedback.tsx";
import { StateBadge } from "@/components/state-badge.tsx";
import { Card, CardContent } from "@/components/ui/card.tsx";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table.tsx";
import { formatTimestamp, shortId } from "@/lib/format.ts";

export function ProviderAccountsPage() {
  const accounts = useList<ProviderAccountResponse>({ resource: "provider-accounts", pagination: { pageSize: 100 } });

  return (
    <PageShell title="平台账号" description="管理当前身份可见的平台账号与认证状态。">
      {accounts.query.error ? <QueryError error={accounts.query.error} /> : null}
      {accounts.query.isLoading ? <TableSkeleton /> : (
        <Card><CardContent className="p-0">
          <Table>
            <TableHeader><TableRow>
              <TableHead>显示名称</TableHead><TableHead>Provider</TableHead><TableHead>租户</TableHead>
              <TableHead>认证</TableHead><TableHead>凭据</TableHead><TableHead>更新时间</TableHead>
            </TableRow></TableHeader>
            <TableBody>
              {accounts.result.data?.map((account) => (
                <TableRow key={account.id}>
                  <TableCell><div className="font-medium">{account.display_name}</div><div className="font-mono text-xs text-muted-foreground">{shortId(account.id)}</div></TableCell>
                  <TableCell>{account.provider_id}</TableCell>
                  <TableCell>{account.tenant ?? "—"}</TableCell>
                  <TableCell><StateBadge state={account.auth_state.state} /></TableCell>
                  <TableCell>{account.credential_count}</TableCell>
                  <TableCell>{formatTimestamp(account.updated_at)}</TableCell>
                </TableRow>
              ))}
              {!accounts.result.data?.length ? <TableRow><TableCell colSpan={6} className="h-24 text-center text-muted-foreground">暂无平台账号</TableCell></TableRow> : null}
            </TableBody>
          </Table>
        </CardContent></Card>
      )}
    </PageShell>
  );
}
