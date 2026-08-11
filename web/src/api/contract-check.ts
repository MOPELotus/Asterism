import type {
  BuildSubmissionDraftResponse,
  GetExecutionResponse,
  GetOwnCreditAccountResponse,
  GetProviderRuntimeSettingsResponse,
  GetSubmissionResultResponse,
  GetTaskQuestionsResponse,
  ListAnswerCandidatesResponse,
  ListProvidersResponse,
  ListTasksResponse,
  LoginError,
  LoginResponse2,
  NormalizedAnswer,
  ProviderMetadata,
  ProviderSettingValue,
  ResolveAnswerCandidatesResponse,
  ScanProviderAccountResponse,
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

export type ScanReportContractIsTyped = Assert<
  ScanProviderAccountResponse extends {
    courses_seen: number;
    task_changes: Array<{ changes: Array<string>; task_id: string }>;
    tasks_created: number;
  }
    ? true
    : false
>;

export type CreditAccountContractIsTyped = Assert<
  GetOwnCreditAccountResponse extends { available: number; reserved: number; user_id: string }
    ? true
    : false
>;

export type QuestionContractIsTyped = Assert<
  GetTaskQuestionsResponse extends {
    captured_at: string;
    questions: Array<{ id: string; kind: string; position: number; task_id: string }>;
    snapshot_id: string;
  }
    ? true
    : false
>;

export type CandidateContractIsTyped = Assert<
  ListAnswerCandidatesResponse extends {
    candidates: Array<{
      candidate: { answer: NormalizedAnswer; question_id: string; source: string };
      id: string;
    }>;
    question_snapshot_id: string;
  }
    ? true
    : false
>;

export type ResolutionContractIsTyped = Assert<
  ResolveAnswerCandidatesResponse extends {
    decisions: Array<{
      considered_candidate_ids: Array<string>;
      selected_answer: NormalizedAnswer | null;
      status: "selected" | "conflict" | "missing";
    }>;
  }
    ? true
    : false
>;

export type SubmissionDraftContractIsTyped = Assert<
  BuildSubmissionDraftResponse extends {
    id: string;
    items: Array<{ question: { id: string }; selected: { candidate_id: string } }>;
    payload_preview: { encoding: "form" | "json" | "query" | "provider_specific" };
  }
    ? true
    : false
>;

export type SubmissionResultContractIsTyped = Assert<
  GetSubmissionResultResponse extends {
    receipt: { received_at: string; remote_status: string } | null;
    status: "confirmed" | "rejected" | "execution_failed" | "inconclusive";
    verification: {
      status: "confirmed" | "rejected" | "pending" | "inconclusive";
      verified_at: string;
    };
  }
    ? true
    : false
>;
