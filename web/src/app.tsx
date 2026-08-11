import { Authenticated, Refine } from "@refinedev/core";
import routerProvider, { CatchAllNavigate } from "@refinedev/react-router";
import { BrowserRouter, Navigate, Outlet, Route, Routes } from "react-router";

import { authProvider } from "@/auth-provider.ts";
import { TableSkeleton } from "@/components/query-feedback.tsx";
import { dataProvider } from "@/data-provider.ts";
import { AppLayout } from "@/layouts/app-layout.tsx";
import { CreditsPage } from "@/pages/credits-page.tsx";
import { DashboardPage } from "@/pages/dashboard-page.tsx";
import { ExecutionDetailPage } from "@/pages/execution-detail-page.tsx";
import { ExecutionsPage } from "@/pages/executions-page.tsx";
import { LoginPage } from "@/pages/login-page.tsx";
import { NotFoundPage } from "@/pages/not-found-page.tsx";
import { ProviderAccountsPage } from "@/pages/provider-accounts-page.tsx";
import { RuntimeSettingsPage } from "@/pages/runtime-settings-page.tsx";
import { TasksPage } from "@/pages/tasks-page.tsx";

export function App() {
  return (
    <BrowserRouter>
      <Refine
        authProvider={authProvider}
        dataProvider={dataProvider}
        routerProvider={routerProvider}
        resources={[
          { name: "providers", list: "/" },
          { name: "provider-accounts", list: "/provider-accounts" },
          { name: "tasks", list: "/tasks" },
          { name: "executions", list: "/executions", show: "/executions/:id" },
        ]}
        options={{
          disableTelemetry: true,
          syncWithLocation: true,
          title: { text: "Asterism" },
          warnWhenUnsavedChanges: true,
        }}
      >
        <Routes>
          <Route
            element={
              <Authenticated
                key="authenticated-routes"
                fallback={<CatchAllNavigate to="/login" />}
                loading={<div className="mx-auto max-w-4xl p-8"><TableSkeleton /></div>}
              >
                <AppLayout />
              </Authenticated>
            }
          >
            <Route index element={<DashboardPage />} />
            <Route path="provider-accounts" element={<ProviderAccountsPage />} />
            <Route path="tasks" element={<TasksPage />} />
            <Route path="executions" element={<ExecutionsPage />} />
            <Route path="executions/:executionId" element={<ExecutionDetailPage />} />
            <Route path="credits" element={<CreditsPage />} />
            <Route path="admin/runtime-settings" element={<RuntimeSettingsPage />} />
            <Route path="*" element={<NotFoundPage />} />
          </Route>

          <Route
            element={
              <Authenticated key="unauthenticated-routes" fallback={<Outlet />}>
                <Navigate to="/" replace />
              </Authenticated>
            }
          >
            <Route path="login" element={<LoginPage />} />
          </Route>
        </Routes>
      </Refine>
    </BrowserRouter>
  );
}
