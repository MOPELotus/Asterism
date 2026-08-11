import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert.tsx";
import { Skeleton } from "@/components/ui/skeleton.tsx";

export function QueryError({ error }: { error: unknown }) {
  return (
    <Alert className="border-red-200 bg-red-50 dark:border-red-900 dark:bg-red-950/40">
      <AlertTitle>读取失败</AlertTitle>
      <AlertDescription>{error instanceof Error ? error.message : "发生未知错误"}</AlertDescription>
    </Alert>
  );
}

export function TableSkeleton() {
  return (
    <div className="space-y-3 rounded-xl border bg-card p-5">
      {[0, 1, 2, 3].map((row) => (
        <Skeleton key={row} className="h-10 w-full" />
      ))}
    </div>
  );
}
