import { useEffect, useState } from "react";

import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert.tsx";
import { Button } from "@/components/ui/button.tsx";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card.tsx";

const endpoint = "/api/v1/admin/ai-config";

export function AiConfigPage() {
  const [value, setValue] = useState("");
  const [status, setStatus] = useState<string>();
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    void fetch(endpoint, { credentials: "same-origin" })
      .then(async (response) => {
        if (!response.ok) throw new Error(`读取失败（${response.status}）`);
        setValue(JSON.stringify(await response.json(), null, 2));
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
    setValue(JSON.stringify(await response.json(), null, 2));
    setStatus("已保存；API key 仍只从部署环境变量读取。");
  }

  return <div className="mx-auto max-w-5xl space-y-6 p-6">
    <div><h1 className="text-2xl font-semibold">AI 组合与端点</h1><p className="mt-1 text-sm text-muted-foreground">仅管理员可见。这里保存站点、模型、协议、思考等级和两个预设组合；密钥不会写入配置。</p></div>
    {status ? <Alert><AlertTitle>配置状态</AlertTitle><AlertDescription>{status}</AlertDescription></Alert> : null}
    <Card><CardHeader><CardTitle>部署级 AiConfig（JSON）</CardTitle></CardHeader><CardContent className="space-y-3">
      <textarea className="min-h-[32rem] w-full rounded-md border bg-muted/20 p-3 font-mono text-xs" value={value} disabled={loading} onChange={(event) => setValue(event.target.value)} spellCheck={false} />
      <Button onClick={() => void save()} disabled={loading || !value.trim()}>保存配置</Button>
    </CardContent></Card>
  </div>;
}
