import { usePermissions } from "@refinedev/core";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Coins, Save, UserPlus } from "lucide-react";
import { useEffect, useState } from "react";

import {
  createAdminUser,
  grantUserCredits,
  listAdminUsers,
  updateAdminUser,
} from "@/api/generated/sdk.gen.ts";
import type { Permission, Role, UserProfile, UserStatus } from "@/api/generated/types.gen.ts";
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

const roles: Role[] = ["master", "operator", "user"];
const permissions: Permission[] = [
  "read_providers", "read_own_tasks", "manage_users", "manage_providers",
  "manage_credits", "grant_credits", "manage_pricing", "manage_system",
  "manage_own_accounts", "read_own_credits", "execute_own_tasks", "execute_any_task",
  "view_own_audit", "view_any_audit",
];
const selectClassName = "h-9 w-full rounded-md border border-input bg-background px-3 text-sm outline-none focus:ring-2 focus:ring-ring";

export function UsersPage() {
  const access = usePermissions<string[]>({});
  const canManageUsers = access.data?.includes("manage_users") ?? false;
  const canGrantCredits = access.data?.includes("grant_credits") ?? false;
  const queryClient = useQueryClient();
  const users = useQuery({
    queryKey: ["admin-users"],
    enabled: canManageUsers,
    queryFn: async () => requireData(await listAdminUsers({ query: { limit: 200, offset: 0 } })),
  });
  const [selectedId, setSelectedId] = useState("");
  const selected = users.data?.items.find((user) => user.id === selectedId) ?? users.data?.items[0];

  useEffect(() => {
    if (!selectedId && users.data?.items[0]) setSelectedId(users.data.items[0].id);
  }, [selectedId, users.data]);

  if (access.isLoading) return <TableSkeleton />;
  if (!canManageUsers) {
    return <AccessDenied description="用户列表和权限编辑仅对具有 manage_users 权限的 Web Session 开放。" />;
  }

  return (
    <PageShell title="用户管理" description="管理 password-free 用户资料、角色和显式权限；注册不会自动授予点数。">
      {users.error ? <QueryError error={users.error} /> : null}
      <CreateUserCard onCreated={async (user) => {
        setSelectedId(user.id);
        await queryClient.invalidateQueries({ queryKey: ["admin-users"] });
      }} />
      <div className="grid gap-6 2xl:grid-cols-[minmax(32rem,1.3fr)_minmax(24rem,1fr)]">
        <Card>
          <CardHeader><CardTitle>用户</CardTitle></CardHeader>
          <CardContent className="p-0">
            {users.isLoading ? <div className="p-5"><TableSkeleton /></div> : (
              <Table>
                <TableHeader><TableRow><TableHead>用户名</TableHead><TableHead>状态</TableHead><TableHead>角色</TableHead><TableHead>更新时间</TableHead></TableRow></TableHeader>
                <TableBody>
                  {users.data?.items.map((user) => (
                    <TableRow key={user.id} className="cursor-pointer" data-state={selected?.id === user.id ? "selected" : undefined} onClick={() => setSelectedId(user.id)}>
                      <TableCell><div className="font-medium">{user.username}</div><div className="font-mono text-xs text-muted-foreground">{shortId(user.id)}</div></TableCell>
                      <TableCell><StateBadge state={user.status} /></TableCell>
                      <TableCell>{user.roles.join(", ")}</TableCell>
                      <TableCell>{formatTimestamp(user.updated_at)}</TableCell>
                    </TableRow>
                  ))}
                  {!users.data?.items.length ? <TableRow><TableCell colSpan={4} className="h-20 text-center text-muted-foreground">暂无用户</TableCell></TableRow> : null}
                </TableBody>
              </Table>
            )}
          </CardContent>
        </Card>
        <div className="space-y-6">
          {selected ? <EditUserCard user={selected} onUpdated={async () => queryClient.invalidateQueries({ queryKey: ["admin-users"] })} /> : null}
          {selected && canGrantCredits ? <CreditGrantCard user={selected} /> : null}
        </div>
      </div>
    </PageShell>
  );
}

function CreateUserCard({ onCreated }: { onCreated: (user: UserProfile) => Promise<void> }) {
  const [username, setUsername] = useState("");
  const [password, setPassword] = useState("");
  const [role, setRole] = useState<Role>("user");
  const create = useMutation({
    mutationFn: async () => requireData(await createAdminUser({ body: { username: username.trim(), password, roles: [role], permissions: [] } })),
    onSuccess: async (user) => {
      setUsername(""); setPassword(""); setRole("user");
      await onCreated(user);
    },
  });
  return <Card><CardHeader><CardTitle>创建用户</CardTitle></CardHeader><CardContent className="grid gap-4 lg:grid-cols-2 2xl:grid-cols-[minmax(0,1fr)_minmax(0,1fr)_10rem_auto] 2xl:items-end">
    <Field label="用户名"><Input value={username} maxLength={64} onChange={(event) => setUsername(event.target.value)} /></Field>
    <Field label="初始密码"><Input type="password" value={password} maxLength={1024} autoComplete="new-password" onChange={(event) => setPassword(event.target.value)} /></Field>
    <Field label="初始角色"><select className={selectClassName} value={role} onChange={(event) => setRole(event.target.value as Role)}>{roles.map((item) => <option key={item} value={item}>{item}</option>)}</select></Field>
    <Button disabled={create.isPending || !username.trim() || password.length < 8} onClick={() => create.mutate()}><UserPlus className="size-4" />{create.isPending ? "创建中" : "创建"}</Button>
    {create.error ? <div className="lg:col-span-2 2xl:col-span-4"><QueryError error={create.error} /></div> : null}
  </CardContent></Card>;
}

function EditUserCard({ user, onUpdated }: { user: UserProfile; onUpdated: () => Promise<unknown> }) {
  const [status, setStatus] = useState<UserStatus>(user.status);
  const [selectedRoles, setSelectedRoles] = useState<Role[]>(user.roles);
  const [selectedPermissions, setSelectedPermissions] = useState<Permission[]>(user.permissions);
  useEffect(() => {
    setStatus(user.status); setSelectedRoles(user.roles); setSelectedPermissions(user.permissions);
  }, [user]);
  const update = useMutation({
    mutationFn: async () => requireData(await updateAdminUser({ path: { user_id: user.id }, body: { expected_updated_at: user.updated_at, status, roles: selectedRoles, permissions: selectedPermissions } })),
    onSuccess: onUpdated,
  });
  return <Card><CardHeader><CardTitle>编辑 · {user.username}</CardTitle></CardHeader><CardContent className="space-y-5">
    {update.error ? <QueryError error={update.error} /> : null}
    <Field label="状态"><select className={selectClassName} value={status} onChange={(event) => setStatus(event.target.value as UserStatus)}><option value="active">active</option><option value="suspended">suspended</option><option value="disabled">disabled</option></select></Field>
    <ChoiceGrid label="角色" values={roles} selected={selectedRoles} onToggle={(value) => setSelectedRoles(toggle(selectedRoles, value))} />
    <ChoiceGrid label="显式权限" values={permissions} selected={selectedPermissions} onToggle={(value) => setSelectedPermissions(toggle(selectedPermissions, value))} />
    <div className="flex items-center justify-between border-t pt-4"><span className="text-xs text-muted-foreground">revision {formatTimestamp(user.updated_at)}</span><Button disabled={update.isPending || selectedRoles.length === 0} onClick={() => update.mutate()}><Save className="size-4" />{update.isPending ? "保存中" : "保存"}</Button></div>
  </CardContent></Card>;
}

function CreditGrantCard({ user }: { user: UserProfile }) {
  const [amount, setAmount] = useState("");
  const [reason, setReason] = useState("");
  const [idempotencyKey, setIdempotencyKey] = useState(() => crypto.randomUUID());
  const grant = useMutation({
    mutationFn: async () => requireData(await grantUserCredits({ path: { user_id: user.id }, headers: { "Idempotency-Key": idempotencyKey }, body: { amount: Number(amount), reason: reason.trim() } })),
    onSuccess: () => { setAmount(""); setReason(""); setIdempotencyKey(crypto.randomUUID()); },
  });
  return <Card><CardHeader><CardTitle>授予点数 · {user.username}</CardTitle></CardHeader><CardContent className="space-y-4">
    <p className="text-sm text-muted-foreground">提交后会形成不可变账本、审计和幂等回执。失败重试会复用当前操作键。</p>
    {grant.error ? <QueryError error={grant.error} /> : null}
    {grant.data ? <Alert><Coins className="size-4" /><AlertTitle>授予完成</AlertTitle><AlertDescription>当前授予后快照：可用 {grant.data.account.available}，流水 {shortId(grant.data.transaction.id)}。</AlertDescription></Alert> : null}
    <div className="grid gap-4 sm:grid-cols-2"><Field label="金额"><Input type="number" min={1} step={1} value={amount} onChange={(event) => setAmount(event.target.value)} /></Field><Field label="原因"><Input value={reason} maxLength={256} onChange={(event) => setReason(event.target.value)} /></Field></div>
    <Button disabled={grant.isPending || !Number.isSafeInteger(Number(amount)) || Number(amount) <= 0 || !reason.trim()} onClick={() => grant.mutate()}><Coins className="size-4" />{grant.isPending ? "授予中" : "授予点数"}</Button>
  </CardContent></Card>;
}

function ChoiceGrid<T extends string>({ label, values, selected, onToggle }: { label: string; values: T[]; selected: T[]; onToggle: (value: T) => void }) {
  return <fieldset><legend className="mb-2 text-sm font-medium">{label}</legend><div className="grid gap-2 sm:grid-cols-2">{values.map((value) => <label key={value} className="flex items-center gap-2 rounded-md border px-3 py-2 text-xs"><input type="checkbox" checked={selected.includes(value)} onChange={() => onToggle(value)} />{value}</label>)}</div></fieldset>;
}

function Field({ label, children }: { label: string; children: React.ReactNode }) { return <div className="space-y-2"><Label>{label}</Label>{children}</div>; }
function toggle<T>(values: T[], value: T): T[] { return values.includes(value) ? values.filter((item) => item !== value) : [...values, value]; }
function AccessDenied({ description }: { description: string }) { return <PageShell title="无权访问" description={description}><Alert><AlertTitle>权限不足</AlertTitle><AlertDescription>后端仍会执行同一权限校验。</AlertDescription></Alert></PageShell>; }
