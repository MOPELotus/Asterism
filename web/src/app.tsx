import { Authenticated, Refine } from "@refinedev/core";
import routerProvider, { CatchAllNavigate } from "@refinedev/react-router";
import { lazy, Suspense } from "react";
import { BrowserRouter, Navigate, Outlet, Route, Routes } from "react-router";

import { authProvider } from "@/auth-provider.ts";
import { TableSkeleton } from "@/components/query-feedback.tsx";
import { dataProvider } from "@/data-provider.ts";
import { AppLayout } from "@/layouts/app-layout.tsx";
import { LoginPage } from "@/pages/login-page.tsx";

const CreditsPage = lazy(() => import("@/pages/credits-page.tsx").then((module) => ({ default: module.CreditsPage })));
const CoursesPage = lazy(() => import("@/pages/courses-page.tsx").then((module) => ({ default: module.CoursesPage })));
const CourseDetailPage = lazy(() => import("@/pages/course-detail-page.tsx").then((module) => ({ default: module.CourseDetailPage })));
const AuditPage = lazy(() => import("@/pages/audit-page.tsx").then((module) => ({ default: module.AuditPage })));
const DashboardPage = lazy(() => import("@/pages/dashboard-page.tsx").then((module) => ({ default: module.DashboardPage })));
const ExecutionDetailPage = lazy(() => import("@/pages/execution-detail-page.tsx").then((module) => ({ default: module.ExecutionDetailPage })));
const ExecutionsPage = lazy(() => import("@/pages/executions-page.tsx").then((module) => ({ default: module.ExecutionsPage })));
const NotFoundPage = lazy(() => import("@/pages/not-found-page.tsx").then((module) => ({ default: module.NotFoundPage })));
const ProviderAccountsPage = lazy(() => import("@/pages/provider-accounts-page.tsx").then((module) => ({ default: module.ProviderAccountsPage })));
const ProviderAccountCreatePage = lazy(() => import("@/pages/provider-account-create-page.tsx").then((module) => ({ default: module.ProviderAccountCreatePage })));
const ProviderAccountDetailPage = lazy(() => import("@/pages/provider-account-detail-page.tsx").then((module) => ({ default: module.ProviderAccountDetailPage })));
const RuntimeSettingsPage = lazy(() => import("@/pages/runtime-settings-page.tsx").then((module) => ({ default: module.RuntimeSettingsPage })));
const ProtocolObservationsPage = lazy(() => import("@/pages/protocol-observations-page.tsx").then((module) => ({ default: module.ProtocolObservationsPage })));
const ServiceTokensPage = lazy(() => import("@/pages/service-tokens-page.tsx").then((module) => ({ default: module.ServiceTokensPage })));
const TasksPage = lazy(() => import("@/pages/tasks-page.tsx").then((module) => ({ default: module.TasksPage })));
const TaskDetailPage = lazy(() => import("@/pages/task-detail-page.tsx").then((module) => ({ default: module.TaskDetailPage })));
const AnswerWorkflowPage = lazy(() => import("@/pages/answer-workflow-page.tsx").then((module) => ({ default: module.AnswerWorkflowPage })));
const UsersPage = lazy(() => import("@/pages/users-page.tsx").then((module) => ({ default: module.UsersPage })));
const AiConfigPage = lazy(() => import("@/pages/ai-config-page.tsx").then((module) => ({ default: module.AiConfigPage })));

export function App() {
  return (
    <BrowserRouter>
      <Refine
        authProvider={authProvider}
        dataProvider={dataProvider}
        routerProvider={routerProvider}
        resources={[
          { name: "providers", list: "/" },
          { name: "provider-accounts", list: "/provider-accounts", create: "/provider-accounts/create", show: "/provider-accounts/:id" },
          { name: "courses", list: "/courses", show: "/courses/:id" },
          { name: "tasks", list: "/tasks", show: "/tasks/:id" },
          { name: "executions", list: "/executions", show: "/executions/:id" },
          { name: "admin-users", list: "/admin/users" },
        ]}
        options={{
          disableTelemetry: true,
          syncWithLocation: true,
          title: { text: "Asterism" },
          warnWhenUnsavedChanges: true,
        }}
      >
        <Suspense fallback={<div className="mx-auto max-w-4xl p-8"><TableSkeleton /></div>}>
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
            <Route path="provider-accounts/create" element={<ProviderAccountCreatePage />} />
            <Route path="provider-accounts/:accountId" element={<ProviderAccountDetailPage />} />
            <Route path="courses" element={<CoursesPage />} />
            <Route path="courses/:courseId" element={<CourseDetailPage />} />
            <Route path="tasks" element={<TasksPage />} />
            <Route path="tasks/:taskId" element={<TaskDetailPage />} />
            <Route path="tasks/:taskId/question-snapshots/:snapshotId" element={<AnswerWorkflowPage />} />
            <Route path="executions" element={<ExecutionsPage />} />
            <Route path="executions/:executionId" element={<ExecutionDetailPage />} />
            <Route path="credits" element={<CreditsPage />} />
            <Route path="admin/users" element={<UsersPage />} />
            <Route path="admin/audit" element={<AuditPage />} />
            <Route path="admin/service-tokens" element={<ServiceTokensPage />} />
            <Route path="admin/runtime-settings" element={<RuntimeSettingsPage />} />
            <Route path="admin/protocol-observations" element={<ProtocolObservationsPage />} />
            <Route path="admin/ai-config" element={<AiConfigPage />} />
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
        </Suspense>
      </Refine>
    </BrowserRouter>
  );
}
