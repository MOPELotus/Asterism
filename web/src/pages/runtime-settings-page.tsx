import { useList } from "@refinedev/core";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Save, ShieldAlert } from "lucide-react";
import { useEffect, useMemo, useState } from "react";

import {
  getProviderAccountRuntimeSettings,
  getProviderRuntimeSettings,
  getTaskRuntimeSettings,
  putProviderAccountRuntimeSettings,
  putProviderRuntimeSettings,
  putTaskRuntimeSettings,
} from "@/api/generated/sdk.gen.ts";
import type {
  ProviderAccountResponse,
  ProviderMetadata,
  ProviderRuntimeSettingsResponse,
  ProviderSettingDefinition,
  ProviderSettingValue,
  Task,
} from "@/api/generated/types.gen.ts";
import { requireData } from "@/api/result.ts";
import { PageShell } from "@/components/page-shell.tsx";
import { QueryError, TableSkeleton } from "@/components/query-feedback.tsx";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert.tsx";
import { Badge } from "@/components/ui/badge.tsx";
import { Button } from "@/components/ui/button.tsx";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card.tsx";
import { Input } from "@/components/ui/input.tsx";
import { Label } from "@/components/ui/label.tsx";

type SettingsScope = "provider" | "provider_account" | "task";
type DraftValues = Record<string, ProviderSettingValue>;

const selectClassName = "h-9 w-full rounded-md border border-input bg-background px-3 text-sm outline-none focus:ring-2 focus:ring-ring";

export function RuntimeSettingsPage() {
  const [scope, setScope] = useState<SettingsScope>("provider");
  const [providerId, setProviderId] = useState("");
  const [accountId, setAccountId] = useState("");
  const [taskId, setTaskId] = useState("");
  const [draft, setDraft] = useState<DraftValues>({});
  const [confirmed, setConfirmed] = useState(false);
  const [saved, setSaved] = useState(false);

  const providers = useList<ProviderMetadata>({ resource: "providers", pagination: { pageSize: 100 } });
  const accounts = useList<ProviderAccountResponse>({ resource: "provider-accounts", pagination: { pageSize: 100 } });
  const tasks = useList<Task>({ resource: "tasks", pagination: { pageSize: 500 } });
  const queryClient = useQueryClient();

  useEffect(() => {
    if (!providerId && providers.result.data?.[0]) setProviderId(providers.result.data[0].id);
  }, [providerId, providers.result.data]);
  useEffect(() => {
    if (!accountId && accounts.result.data?.[0]) setAccountId(accounts.result.data[0].id);
  }, [accountId, accounts.result.data]);
  useEffect(() => {
    if (!taskId && tasks.result.data?.[0]) setTaskId(tasks.result.data[0].id);
  }, [taskId, tasks.result.data]);

  const selectedAccount = accounts.result.data?.find((account) => account.id === accountId);
  const selectedTask = tasks.result.data?.find((task) => task.id === taskId);
  const taskAccount = accounts.result.data?.find((account) => account.id === selectedTask?.provider_account_id);
  const targetId = scope === "provider" ? providerId : scope === "provider_account" ? accountId : taskId;
  const targetProviderId = scope === "provider" ? providerId : scope === "provider_account" ? selectedAccount?.provider_id : taskAccount?.provider_id;
  const settingsKey = ["runtime-settings", scope, targetId];

  const settings = useQuery({
    queryKey: settingsKey,
    enabled: Boolean(targetId && targetProviderId),
    queryFn: async (): Promise<ProviderRuntimeSettingsResponse> => {
      if (scope === "provider") return requireData(await getProviderRuntimeSettings({ path: { provider_id: targetId } }));
      if (scope === "provider_account") return requireData(await getProviderAccountRuntimeSettings({ path: { account_id: targetId } }));
      return requireData(await getTaskRuntimeSettings({ path: { task_id: targetId } }));
    },
  });

  useEffect(() => {
    const layer = settings.data ? targetLayer(settings.data, scope) : null;
    setDraft(layer?.patch.values ?? {});
    setConfirmed(false);
    setSaved(false);
  }, [scope, settings.data]);

  const definitions = useMemo(
    () => settings.data?.schema.definitions.filter((definition) => definition.scopes.includes(scope)) ?? [],
    [scope, settings.data],
  );

  const save = useMutation({
    mutationFn: async () => {
      if (!settings.data) throw new Error("运行设置尚未加载");
      const body = {
        expected_revision: targetLayer(settings.data, scope)?.revision ?? 0,
        schema_version: settings.data.schema.version,
        values: draft,
      };
      if (scope === "provider") return requireData(await putProviderRuntimeSettings({ path: { provider_id: targetId }, body }));
      if (scope === "provider_account") return requireData(await putProviderAccountRuntimeSettings({ path: { account_id: targetId }, body }));
      return requireData(await putTaskRuntimeSettings({ path: { task_id: targetId }, body }));
    },
    onSuccess: (data) => {
      queryClient.setQueryData(settingsKey, data);
      setConfirmed(false);
      setSaved(true);
    },
  });

  const listError = providers.query.error ?? accounts.query.error ?? tasks.query.error;

  return (
    <PageShell title="运行设置" description="Master 管理 Provider 默认值，并可在账号或单任务范围内覆盖；未覆盖字段继续继承上层。">
      {listError || settings.error || save.error ? <QueryError error={listError ?? settings.error ?? save.error} /> : null}
      <Alert className="border-amber-300 bg-amber-50 dark:border-amber-800 dark:bg-amber-950/30">
        <ShieldAlert className="size-4" />
        <AlertTitle>共享技术设置</AlertTitle>
        <AlertDescription>并发、速度和巡查周期会影响远端请求行为。保存前请核对作用范围；后端仍会执行 schema 范围和安全上限校验。</AlertDescription>
      </Alert>

      <Card><CardHeader><CardTitle>设置目标</CardTitle></CardHeader><CardContent className="grid gap-4 md:grid-cols-2">
        <Field label="作用范围"><select className={selectClassName} value={scope} onChange={(event) => setScope(event.target.value as SettingsScope)}><option value="provider">Provider 默认</option><option value="provider_account">平台账号覆盖</option><option value="task">单任务覆盖</option></select></Field>
        {scope === "provider" ? <Field label="Provider"><select className={selectClassName} value={providerId} onChange={(event) => setProviderId(event.target.value)}>{providers.result.data?.map((provider) => <option key={provider.id} value={provider.id}>{provider.display_name} ({provider.id})</option>)}</select></Field> : null}
        {scope === "provider_account" ? <Field label="平台账号"><select className={selectClassName} value={accountId} onChange={(event) => setAccountId(event.target.value)}>{accounts.result.data?.map((account) => <option key={account.id} value={account.id}>{account.display_name} · {account.provider_id}</option>)}</select></Field> : null}
        {scope === "task" ? <Field label="任务"><select className={selectClassName} value={taskId} onChange={(event) => setTaskId(event.target.value)}>{tasks.result.data?.map((task) => <option key={task.id} value={task.id}>{task.title}</option>)}</select></Field> : null}
      </CardContent></Card>

      {settings.isLoading ? <TableSkeleton /> : settings.data ? (
        <Card><CardHeader className="flex-row items-center justify-between"><div><CardTitle>{settings.data.provider_id} · schema v{settings.data.schema.version}</CardTitle><p className="mt-1 text-sm text-muted-foreground">当前目标 revision {targetLayer(settings.data, scope)?.revision ?? 0}</p></div><Badge variant="outline">{scopeLabel(scope)}</Badge></CardHeader>
          <CardContent className="space-y-5">
            {definitions.map((definition) => <SettingField key={definition.key} definition={definition} resolved={settings.data.resolved.values[definition.key] ?? definition.default} source={settings.data.sources[definition.key] ?? "schema_default"} value={draft[definition.key]} onChange={(value) => { setSaved(false); setDraft((current) => { const next = { ...current }; if (value) next[definition.key] = value; else delete next[definition.key]; return next; }); }} />)}
            {!definitions.length ? <p className="text-sm text-muted-foreground">此 Provider 在当前范围没有可配置字段。</p> : null}
            <div className="flex flex-col gap-3 border-t pt-5 sm:flex-row sm:items-center sm:justify-between">
              <label className="flex items-start gap-2 text-sm"><input type="checkbox" className="mt-1" checked={confirmed} onChange={(event) => setConfirmed(event.target.checked)} /><span>我已确认目标范围和最终解析值，保存该 revision。</span></label>
              <div className="flex items-center gap-3">{saved ? <span className="text-sm text-emerald-600">已保存并刷新 revision</span> : null}<Button disabled={!confirmed || save.isPending || !definitions.length} onClick={() => save.mutate()}><Save className="size-4" />{save.isPending ? "保存中" : "保存设置"}</Button></div>
            </div>
          </CardContent>
        </Card>
      ) : null}
    </PageShell>
  );
}

function SettingField({ definition, resolved, source, value, onChange }: { definition: ProviderSettingDefinition; resolved: ProviderSettingValue; source: string; value?: ProviderSettingValue; onChange: (value: ProviderSettingValue | undefined) => void }) {
  const enabled = value !== undefined;
  const visible = value ?? resolved;
  return <div className="grid gap-3 rounded-lg border p-4 lg:grid-cols-[minmax(16rem,1fr)_minmax(14rem,22rem)]">
    <div><div className="flex flex-wrap items-center gap-2"><Label className="text-base">{definition.display_name}</Label><Badge variant="outline">{definition.key}</Badge>{definition.core_behavior ? <Badge variant="secondary">Core · {definition.core_behavior}</Badge> : null}</div><p className="mt-2 text-sm text-muted-foreground">{definition.description}</p><p className="mt-2 text-xs text-muted-foreground">最终值 {displayValue(resolved)} · 来源 {source}</p></div>
    <div className="space-y-3"><label className="flex items-center gap-2 text-sm"><input type="checkbox" checked={enabled} onChange={(event) => onChange(event.target.checked ? resolved : undefined)} />在当前范围覆盖</label><SettingInput definition={definition} value={visible} disabled={!enabled} onChange={onChange} /></div>
  </div>;
}

function SettingInput({ definition, value, disabled, onChange }: { definition: ProviderSettingDefinition; value: ProviderSettingValue; disabled: boolean; onChange: (value: ProviderSettingValue) => void }) {
  if (definition.kind.type === "boolean") return <select className={selectClassName} disabled={disabled} value={String(value.value)} onChange={(event) => onChange({ type: "boolean", value: event.target.value === "true" })}><option value="true">启用</option><option value="false">停用</option></select>;
  if (definition.kind.type === "choice") return <select className={selectClassName} disabled={disabled} value={String(value.value)} onChange={(event) => onChange({ type: "choice", value: event.target.value })}>{definition.kind.options.map((option) => <option key={option} value={option}>{option}</option>)}</select>;
  const numericValue = typeof value.value === "number" ? value.value : 0;
  const numericKind = definition.kind;
  if (numericKind.type === "integer") return <Input type="number" disabled={disabled} value={numericValue} min={numericKind.minimum} max={numericKind.maximum} step={numericKind.step} onChange={(event) => onChange({ type: "integer", value: Number(event.target.value) })} />;
  if (numericKind.type === "decimal_millis") return <Input type="number" disabled={disabled} value={numericValue} min={numericKind.minimum} max={numericKind.maximum} step={numericKind.step} onChange={(event) => onChange({ type: "decimal_millis", value: Number(event.target.value) })} />;
  return <Input type="number" disabled={disabled} value={numericValue} min={numericKind.minimum} max={numericKind.maximum} step={numericKind.step} onChange={(event) => onChange({ type: "duration_seconds", value: Number(event.target.value) })} />;
}

function targetLayer(settings: ProviderRuntimeSettingsResponse, scope: SettingsScope) {
  return scope === "provider" ? settings.overrides.provider : scope === "provider_account" ? settings.overrides.provider_account : settings.overrides.task;
}

function displayValue(value: ProviderSettingValue): string {
  if (value.type === "decimal_millis") return `${value.value / 1000}×`;
  if (value.type === "duration_seconds") return `${value.value} 秒`;
  return String(value.value);
}

function scopeLabel(scope: SettingsScope): string {
  return scope === "provider" ? "Provider 默认" : scope === "provider_account" ? "账号覆盖" : "任务覆盖";
}

function Field({ label, children }: { label: string; children: React.ReactNode }) {
  return <div className="space-y-2"><Label>{label}</Label>{children}</div>;
}
