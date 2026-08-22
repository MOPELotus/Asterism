import { useList } from "@refinedev/core";
import { Plus } from "lucide-react";
import { Link } from "react-router";

import type { ProviderAccountResponse } from "@/api/generated/types.gen.ts";
import { PageShell } from "@/components/page-shell.tsx";
import { QueryError, TableSkeleton } from "@/components/query-feedback.tsx";
import { StateBadge } from "@/components/state-badge.tsx";
import { Card, CardContent } from "@/components/ui/card.tsx";
import { buttonVariants } from "@/components/ui/button.tsx";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table.tsx";
import { formatTimestamp } from "@/lib/format.ts";
import { providerName } from "@/lib/learning-display.ts";

export function ProviderAccountsPage() {
  const accounts = useList<ProviderAccountResponse>({ resource: "provider-accounts", pagination: { pageSize: 100 } });

  return (
    <PageShell title="学习平台" description="选择账号后查看该账号的课程、任务和完成情况。" actions={<Link className={buttonVariants({ variant: "default" })} to="/provider-accounts/create"><Plus className="size-4" />添加账号</Link>}>
      {accounts.query.error ? <QueryError error={accounts.query.error} /> : null}
      {accounts.query.isLoading ? <TableSkeleton /> : (
        <Card><CardContent className="p-0">
          <Table>
            <TableHeader><TableRow>
              <TableHead>账号</TableHead><TableHead>学习平台</TableHead>
              <TableHead>登录状态</TableHead><TableHead>最近更新</TableHead>
            </TableRow></TableHeader>
            <TableBody>
              {accounts.result.data?.map((account) => (
                <TableRow key={account.id}>
                  <TableCell><Link className="font-medium text-primary hover:underline" to={`/provider-accounts/${account.id}`}>{account.display_name}</Link></TableCell>
                  <TableCell>{providerName(account.provider_id)}</TableCell>
                  <TableCell><StateBadge state={account.auth_state.state} /></TableCell>
                  <TableCell>{formatTimestamp(account.updated_at)}</TableCell>
                </TableRow>
              ))}
              {!accounts.result.data?.length ? <TableRow><TableCell colSpan={4} className="h-24 text-center text-muted-foreground">还没有添加学习平台账号</TableCell></TableRow> : null}
            </TableBody>
          </Table>
        </CardContent></Card>
      )}
    </PageShell>
  );
}
