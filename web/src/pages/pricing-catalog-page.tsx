import { useEffect, useState } from "react";

import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert.tsx";
import { Button } from "@/components/ui/button.tsx";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card.tsx";

const endpoint = "/api/v1/admin/pricing-catalog";

export function PricingCatalogPage() {
  const [value, setValue] = useState(`{
  "revision": "catalog-2026-08",
  "catalog": {
    "default_amount": 0,
    "capability_amounts": {},
    "reason": "task execution"
  }
}`);
  const [status, setStatus] = useState<string>();
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    void fetch(endpoint, { credentials: "same-origin" })
      .then(async (response) => {
        if (!response.ok) throw new Error(`读取失败（${response.status}）`);
        const current = await response.json() as Record<string, unknown> | null;
        if (current) {
          setValue(JSON.stringify({
            revision: current.revision,
            catalog: current.catalog,
            effective_from: current.effective_from,
            expires_at: current.expires_at,
          }, null, 2));
        }
      })
      .catch((error: unknown) => setStatus(error instanceof Error ? error.message : "读取失败"))
      .finally(() => setLoading(false));
  }, []);

  async function save() {
    setStatus(undefined);
    let body: unknown;
    try {
      body = JSON.parse(value);
    } catch {
      setStatus("JSON 格式不正确");
      return;
    }
    const response = await fetch(endpoint, {
      method: "PUT",
      credentials: "same-origin",
      headers: { "content-type": "application/json", "x-request-id": crypto.randomUUID() },
      body: JSON.stringify(body),
    });
    if (!response.ok) {
      setStatus((await response.text()) || `保存失败（${response.status}）`);
      return;
    }
    setStatus("定价版本已保存；新执行会使用当前生效版本生成报价并预留点数。已有执行不会回写。");
  }

  return <div className="mx-auto max-w-5xl space-y-6">
    <div><h1 className="text-2xl font-semibold">点数定价目录</h1><p className="mt-1 text-sm text-muted-foreground">仅具备定价管理权限的管理员可见。API key、用户余额和历史报价不会写入这里。</p></div>
    {status ? <Alert><AlertTitle>定价状态</AlertTitle><AlertDescription>{status}</AlertDescription></Alert> : null}
    <Card><CardHeader><CardTitle>部署级定价版本（JSON）</CardTitle></CardHeader><CardContent className="space-y-3">
      <p className="text-sm text-muted-foreground">支持 <code>default_amount</code>、按 capability 的 <code>capability_amounts</code>、<code>reason</code> 和生效时间。金额为部署内部点数，不是人民币。</p>
      <textarea className="min-h-[28rem] w-full rounded-md border bg-muted/20 p-3 font-mono text-xs" value={value} disabled={loading} onChange={(event) => setValue(event.target.value)} spellCheck={false} />
      <Button onClick={() => void save()} disabled={loading || !value.trim()}>保存定价版本</Button>
    </CardContent></Card>
  </div>;
}
