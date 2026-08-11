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
  return value.replaceAll("_", " ");
}
