import { useQuery } from "@tanstack/react-query";
import { CircleDollarSign, LockKeyhole } from "lucide-react";

import { getOwnCreditAccount, getRechargeContact, listOwnCreditReservations, listOwnCreditTransactions } from "@/api/generated/sdk.gen.ts";
import { requireData } from "@/api/result.ts";
import { PageShell } from "@/components/page-shell.tsx";
import { QueryError, TableSkeleton } from "@/components/query-feedback.tsx";
import { StateBadge } from "@/components/state-badge.tsx";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card.tsx";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table.tsx";
import { formatTimestamp, shortId } from "@/lib/format.ts";

export function CreditsPage() {
  const account = useQuery({ queryKey: ["credits", "account"], queryFn: async () => requireData(await getOwnCreditAccount()) });
  const reservations = useQuery({ queryKey: ["credits", "reservations"], queryFn: async () => requireData(await listOwnCreditReservations({ query: { limit: 50, offset: 0 } })) });
  const transactions = useQuery({ queryKey: ["credits", "transactions"], queryFn: async () => requireData(await listOwnCreditTransactions({ query: { limit: 50, offset: 0 } })) });
  const recharge = useQuery({ queryKey: ["credits", "recharge-contact"], queryFn: async () => requireData(await getRechargeContact()) });
  const error = account.error ?? reservations.error ?? transactions.error ?? recharge.error;

  return (
    <PageShell title="点数" description="查看可用点数、执行预留与不可变账本记录。">
      {error ? <QueryError error={error} /> : null}
      <div className="grid gap-4 sm:grid-cols-2">
        <BalanceCard title="可用" value={account.data?.available ?? 0} icon={CircleDollarSign} />
        <BalanceCard title="已预留" value={account.data?.reserved ?? 0} icon={LockKeyhole} />
      </div>
      {recharge.data?.contact ? <Card><CardHeader><CardTitle>充值联系</CardTitle></CardHeader><CardContent className="whitespace-pre-wrap text-sm">{recharge.data.contact}</CardContent></Card> : null}
      <Card>
        <CardHeader><CardTitle>当前预留</CardTitle></CardHeader>
        <CardContent className="p-0">
          {reservations.isLoading ? <div className="p-5"><TableSkeleton /></div> : (
            <Table><TableHeader><TableRow><TableHead>预留</TableHead><TableHead>执行</TableHead><TableHead>金额</TableHead><TableHead>状态</TableHead><TableHead>创建时间</TableHead></TableRow></TableHeader>
              <TableBody>{reservations.data?.items.map(({ reservation }) => <TableRow key={reservation.id}><TableCell className="font-mono text-xs">{shortId(reservation.id)}</TableCell><TableCell className="font-mono text-xs">{shortId(reservation.execution_id)}</TableCell><TableCell>{reservation.amount}</TableCell><TableCell><StateBadge state={reservation.state} /></TableCell><TableCell>{formatTimestamp(reservation.created_at)}</TableCell></TableRow>)}
              {!reservations.data?.items.length ? <TableRow><TableCell colSpan={5} className="h-20 text-center text-muted-foreground">暂无点数预留</TableCell></TableRow> : null}</TableBody></Table>
          )}
        </CardContent>
      </Card>
      <Card>
        <CardHeader><CardTitle>点数流水</CardTitle></CardHeader>
        <CardContent className="p-0">
          {transactions.isLoading ? <div className="p-5"><TableSkeleton /></div> : (
            <Table><TableHeader><TableRow><TableHead>时间</TableHead><TableHead>类型</TableHead><TableHead>金额</TableHead><TableHead>原因</TableHead><TableHead>任务</TableHead></TableRow></TableHeader>
              <TableBody>{transactions.data?.items.map((transaction) => <TableRow key={transaction.id}><TableCell>{formatTimestamp(transaction.created_at)}</TableCell><TableCell>{transaction.transaction_type}</TableCell><TableCell className={transaction.amount >= 0 ? "text-emerald-600" : "text-red-600"}>{transaction.amount >= 0 ? "+" : ""}{transaction.amount}</TableCell><TableCell>{transaction.reason}</TableCell><TableCell className="font-mono text-xs">{transaction.task_id ? shortId(transaction.task_id) : "—"}</TableCell></TableRow>)}
              {!transactions.data?.items.length ? <TableRow><TableCell colSpan={5} className="h-20 text-center text-muted-foreground">暂无点数流水</TableCell></TableRow> : null}</TableBody></Table>
          )}
        </CardContent>
      </Card>
    </PageShell>
  );
}

function BalanceCard({ title, value, icon: Icon }: { title: string; value: number; icon: typeof CircleDollarSign }) {
  return <Card><CardHeader className="flex flex-row items-center justify-between space-y-0 pb-2"><CardTitle className="text-sm text-muted-foreground">{title}</CardTitle><Icon className="size-4 text-primary" /></CardHeader><CardContent><div className="text-3xl font-semibold">{value}</div></CardContent></Card>;
}
