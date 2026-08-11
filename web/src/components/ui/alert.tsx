import type { HTMLAttributes } from "react";

import { cn } from "@/lib/utils.ts";

export function Alert({ className, ...props }: HTMLAttributes<HTMLDivElement>) {
  return <div role="alert" className={cn("rounded-lg border bg-card p-4 text-sm", className)} {...props} />;
}

export function AlertTitle({ className, ...props }: HTMLAttributes<HTMLHeadingElement>) {
  return <h5 className={cn("mb-1 font-medium", className)} {...props} />;
}

export function AlertDescription({ className, ...props }: HTMLAttributes<HTMLDivElement>) {
  return <div className={cn("text-muted-foreground", className)} {...props} />;
}
