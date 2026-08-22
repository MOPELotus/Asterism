import { Badge } from "@/components/ui/badge.tsx";

const successStates = new Set(["authenticated", "completed", "succeeded", "confirmed", "verified"]);
const warningStates = new Set([
  "pending",
  "in_progress",
  "running",
  "recovering",
  "retry_waiting",
  "human_required",
  "waiting_approval",
  "development",
  "experimental",
]);
const destructiveStates = new Set([
  "failed",
  "auth_failed",
  "expired",
  "removed",
  "cancelled",
  "broken",
  "rejected",
]);

export function StateBadge({ state }: { state: string }) {
  const variant = successStates.has(state)
    ? "success"
    : warningStates.has(state)
      ? "warning"
      : destructiveStates.has(state)
        ? "destructive"
        : "outline";
  return <Badge variant={variant}>{humanize(state)}</Badge>;
}

export function humanize(value: string): string {
  return stateLabels[value] ?? value.replaceAll("_", " ");
}

const stateLabels: Record<string, string> = {
  authenticated: "已认证",
  auth_failed: "认证失败",
  pending: "等待中",
  in_progress: "进行中",
  running: "运行中",
  recovering: "恢复中",
  retry_waiting: "等待重试",
  human_required: "需要人工操作",
  waiting_approval: "等待确认",
  completed: "已完成",
  succeeded: "已成功",
  confirmed: "已确认",
  verified: "已验证",
  failed: "失败",
  expired: "已过期",
  removed: "已删除",
  cancelled: "已取消",
  broken: "异常",
  rejected: "已拒绝",
  development: "开发验证中",
  experimental: "实验性",
};
