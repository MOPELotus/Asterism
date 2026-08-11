import { Link } from "react-router";

import { buttonVariants } from "@/components/ui/button.tsx";

export function NotFoundPage() {
  return <div className="grid min-h-[60vh] place-items-center text-center"><div><p className="text-sm font-medium text-primary">404</p><h1 className="mt-2 text-3xl font-semibold">页面不存在</h1><p className="mt-2 text-muted-foreground">请求的管理页面尚不存在或已移动。</p><Link className={`${buttonVariants({ variant: "default" })} mt-6`} to="/">返回概览</Link></div></div>;
}
