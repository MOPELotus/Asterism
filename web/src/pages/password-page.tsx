import { useMutation } from "@tanstack/react-query";
import { KeyRound } from "lucide-react";
import { type FormEvent, useState } from "react";
import { useNavigate } from "react-router";

import { changeOrSetPassword } from "@/api/generated/sdk.gen.ts";
import { ensureSuccess } from "@/api/result.ts";
import { PageShell } from "@/components/page-shell.tsx";
import { QueryError } from "@/components/query-feedback.tsx";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert.tsx";
import { Button } from "@/components/ui/button.tsx";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card.tsx";
import { Input } from "@/components/ui/input.tsx";
import { Label } from "@/components/ui/label.tsx";

export function PasswordPage() {
  const navigate = useNavigate();
  const [currentPassword, setCurrentPassword] = useState("");
  const [newPassword, setNewPassword] = useState("");
  const [confirmation, setConfirmation] = useState("");
  const passwordsMatch = newPassword === confirmation;
  const change = useMutation({
    mutationFn: async () => ensureSuccess(await changeOrSetPassword({
      body: {
        ...(currentPassword ? { current_password: currentPassword } : {}),
        new_password: newPassword,
      },
    })),
    onSuccess: () => navigate("/login?passwordChanged=1", { replace: true }),
  });

  function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (newPassword.length >= 8 && passwordsMatch) change.mutate();
  }

  return (
    <PageShell title="设置登录密码" description="QQ 创建的新账号可直接设置首个密码；已有密码的账号需要先验证当前密码。">
      <Card className="max-w-2xl">
        <CardHeader><CardTitle className="flex items-center gap-2"><KeyRound className="size-5" />密码</CardTitle></CardHeader>
        <CardContent>
          <form className="space-y-5" onSubmit={submit}>
            <Alert>
              <AlertTitle>修改后需要重新登录</AlertTitle>
              <AlertDescription>成功后会撤销此账号的全部旧 Web 会话。QQ 首次设置密码时，“当前密码”留空即可。</AlertDescription>
            </Alert>
            {change.error ? <QueryError error={change.error} /> : null}
            <PasswordField label="当前密码" autoComplete="current-password" value={currentPassword} onChange={setCurrentPassword} hint="仅 QQ 新注册账号首次设置时可留空。" />
            <PasswordField label="新密码" autoComplete="new-password" value={newPassword} onChange={setNewPassword} hint="至少 8 个字符。" />
            <PasswordField label="确认新密码" autoComplete="new-password" value={confirmation} onChange={setConfirmation} />
            {!passwordsMatch && confirmation ? <p className="text-sm text-destructive">两次输入的新密码不一致。</p> : null}
            <Button type="submit" disabled={change.isPending || newPassword.length < 8 || !passwordsMatch}>
              <KeyRound className="size-4" />{change.isPending ? "正在保存" : "保存密码"}
            </Button>
          </form>
        </CardContent>
      </Card>
    </PageShell>
  );
}

function PasswordField({ label, autoComplete, value, onChange, hint }: { label: string; autoComplete: string; value: string; onChange: (value: string) => void; hint?: string }) {
  return <div className="space-y-2"><Label>{label}</Label><Input type="password" maxLength={1024} autoComplete={autoComplete} value={value} onChange={(event) => onChange(event.target.value)} />{hint ? <p className="text-xs text-muted-foreground">{hint}</p> : null}</div>;
}
