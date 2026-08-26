import { useLogin } from "@refinedev/core";
import { LockKeyhole } from "lucide-react";
import { type FormEvent, useState } from "react";
import { useSearchParams } from "react-router";

import { Alert, AlertDescription } from "@/components/ui/alert.tsx";
import { Button } from "@/components/ui/button.tsx";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card.tsx";
import { Input } from "@/components/ui/input.tsx";
import { Label } from "@/components/ui/label.tsx";

type LoginVariables = { username: string; password: string };

export function LoginPage() {
  const login = useLogin<LoginVariables>();
  const [searchParams] = useSearchParams();
  const [username, setUsername] = useState("");
  const [password, setPassword] = useState("");

  function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    login.mutate({ username, password });
  }

  return (
    <main className="grid min-h-screen place-items-center bg-[radial-gradient(circle_at_top_left,oklch(0.91_0.055_245),transparent_42%)] p-4">
      <Card className="w-full max-w-md border-border/80 shadow-xl shadow-primary/5">
        <CardHeader className="space-y-4">
          <div className="grid size-12 place-items-center rounded-2xl bg-primary text-primary-foreground">
            <LockKeyhole className="size-6" />
          </div>
          <div>
            <CardTitle className="text-2xl">登录 Asterism</CardTitle>
            <CardDescription className="mt-1">使用本机服务端会话进入统一管理页面。</CardDescription>
          </div>
        </CardHeader>
        <CardContent>
          <form className="space-y-4" onSubmit={submit}>
            {searchParams.get("passwordChanged") === "1" ? (
              <Alert>
                <AlertDescription>密码已保存，旧会话已撤销。请使用新密码登录。</AlertDescription>
              </Alert>
            ) : null}
            <div className="space-y-2">
              <Label htmlFor="username">用户名</Label>
              <Input
                id="username"
                name="username"
                autoComplete="username"
                value={username}
                onChange={(event) => setUsername(event.target.value)}
                disabled={login.isPending}
              />
            </div>
            <div className="space-y-2">
              <Label htmlFor="password">密码</Label>
              <Input
                id="password"
                name="password"
                type="password"
                autoComplete="current-password"
                value={password}
                onChange={(event) => setPassword(event.target.value)}
                disabled={login.isPending}
              />
            </div>
            {login.error ? (
              <Alert className="border-red-200 bg-red-50 dark:border-red-900 dark:bg-red-950/40">
                <AlertDescription>{login.error.message}</AlertDescription>
              </Alert>
            ) : null}
            <Button type="submit" className="w-full" disabled={login.isPending}>
              {login.isPending ? "正在登录…" : "登录"}
            </Button>
          </form>
        </CardContent>
      </Card>
    </main>
  );
}
