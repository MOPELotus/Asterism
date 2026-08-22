import { useList } from "@refinedev/core";
import { useMutation } from "@tanstack/react-query";
import { type FormEvent, useEffect, useState } from "react";
import { useNavigate } from "react-router";

import { createProviderAccount } from "@/api/generated/sdk.gen.ts";
import type { ProviderMetadata } from "@/api/generated/types.gen.ts";
import { requireData } from "@/api/result.ts";
import { PageShell } from "@/components/page-shell.tsx";
import { QueryError } from "@/components/query-feedback.tsx";
import { Button } from "@/components/ui/button.tsx";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card.tsx";
import { Input } from "@/components/ui/input.tsx";
import { Label } from "@/components/ui/label.tsx";

const selectClassName = "h-9 w-full rounded-md border border-input bg-background px-3 text-sm outline-none focus:ring-2 focus:ring-ring";

export function ProviderAccountCreatePage() {
  const providers = useList<ProviderMetadata>({ resource: "providers", pagination: { pageSize: 100 } });
  const [providerId, setProviderId] = useState("");
  const [displayName, setDisplayName] = useState("");
  const navigate = useNavigate();

  useEffect(() => {
    if (!providerId && providers.result.data?.[0]) setProviderId(providers.result.data[0].id);
  }, [providerId, providers.result.data]);

  const create = useMutation({
    mutationFn: async () => requireData(await createProviderAccount({ body: { provider_id: providerId, display_name: displayName.trim(), tenant: null } })),
    onSuccess: (account) => navigate(`/provider-accounts/${account.id}`, { replace: true }),
  });

  function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    create.mutate();
  }

  return <PageShell title="添加平台账号" description="选择学习平台并填写一个便于识别的名称，创建后继续完成平台登录。">
    {providers.query.error || create.error ? <QueryError error={providers.query.error ?? create.error} /> : null}
    <Card className="max-w-2xl"><CardHeader><CardTitle>账号信息</CardTitle></CardHeader><CardContent>
      <form className="space-y-4" onSubmit={submit}>
        <div className="space-y-2"><Label htmlFor="provider">学习平台</Label><select id="provider" className={selectClassName} value={providerId} onChange={(event) => setProviderId(event.target.value)}>{providers.result.data?.map((provider) => <option key={provider.id} value={provider.id}>{provider.display_name}</option>)}</select></div>
        <div className="space-y-2"><Label htmlFor="display-name">显示名称</Label><Input id="display-name" required maxLength={128} value={displayName} onChange={(event) => setDisplayName(event.target.value)} /></div>
        <Button type="submit" disabled={create.isPending || !providerId || !displayName.trim()}>{create.isPending ? "创建中…" : "创建并继续认证"}</Button>
      </form>
    </CardContent></Card>
  </PageShell>;
}
