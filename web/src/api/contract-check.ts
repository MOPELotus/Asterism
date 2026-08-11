import type {
  GetExecutionResponse,
  GetProviderRuntimeSettingsResponse,
  ListProvidersResponse,
  ListTasksResponse,
  LoginError,
  LoginResponse2,
  ProviderMetadata,
  ProviderSettingValue,
  Task,
} from "./generated/index.ts";

type Assert<T extends true> = T;

export type LoginContractIsTyped = Assert<
  LoginResponse2 extends {
    expires_at: string;
    user: { id: string; roles: Array<"master" | "operator" | "user">; username: string };
  }
    ? true
    : false
>;

export type ErrorContractIsTyped = Assert<
  LoginError extends { error: { code: string; message: string } } ? true : false
>;

export type ProviderListContractIsTyped = Assert<
  ListProvidersResponse extends { items: Array<ProviderMetadata>; total: number } ? true : false
>;

export type TaskListContractIsTyped = Assert<
  ListTasksResponse extends {
    items: Array<Task>;
    limit: number;
    offset: number;
    total: number;
  }
    ? true
    : false
>;

export type ExecutionDetailContractIsTyped = Assert<
  GetExecutionResponse extends {
    attempts: Array<{ attempt_no: number }>;
    execution: { id: string; state: string; task_id: string };
    progress: { stage: string; updated_at: string } | null;
  }
    ? true
    : false
>;

export type RuntimeSettingsContractIsTyped = Assert<
  GetProviderRuntimeSettingsResponse extends {
    provider_id: string;
    resolved: { schema_version: number; values: Record<string, ProviderSettingValue> };
    target_scope: "provider" | "provider_account" | "task";
  }
    ? true
    : false
>;
