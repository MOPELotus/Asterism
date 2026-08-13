use serde_json::{Map, Value, json};

const JSON_SUCCESS_SCHEMAS: &[(&str, &str)] = &[
    ("health", "HealthResponse"),
    ("systemHealth", "HealthResponse"),
    ("bootstrapMaster", "LoginResponse"),
    ("login", "LoginResponse"),
    ("currentIdentity", "IdentityResponse"),
    ("createAuthBootstrapSession", "AuthBootstrapCreateResponse"),
    ("getAuthBootstrapSession", "AuthBootstrapSession"),
    ("cancelAuthBootstrapSession", "AuthBootstrapSession"),
    ("claimAuthBootstrapSession", "AuthBootstrapClaimResponse"),
    (
        "submitAuthBootstrapCredential",
        "AuthBootstrapCredentialResponse",
    ),
    ("recordAuthBootstrapEvent", "AuthBootstrapEventResponse"),
    ("pollAuthBootstrapStream", "AuthBootstrapSession"),
    ("listProviders", "ProviderMetadataListResponse"),
    ("listProviderAccounts", "ProviderAccountListResponse"),
    ("createProviderAccount", "ProviderAccountResponse"),
    ("getProviderAccount", "ProviderAccountResponse"),
    ("updateProviderAccount", "ProviderAccountResponse"),
    ("scanProviderAccount", "ProviderScanReport"),
    (
        "replaceProviderAccountCredentials",
        "PutProviderCredentialsResponse",
    ),
    (
        "beginProviderAccountAuthSession",
        "AuthSessionBeginResponse",
    ),
    ("getLatestProviderAccountAuthSession", "AuthSession"),
    ("getProviderAccountAuthSession", "AuthSession"),
    ("cancelProviderAccountAuthSession", "AuthSession"),
    (
        "submitProviderAccountAuthSessionCredentials",
        "PutAuthSessionCredentialsResponse",
    ),
    (
        "getProviderAccountExternalOauthPending",
        "ExternalOauthPendingResponse",
    ),
    (
        "submitProviderAccountExternalOauthCallback",
        "PutAuthSessionCredentialsResponse",
    ),
    ("getProviderAccountScanSchedule", "ScanScheduleResponse"),
    (
        "configureProviderAccountScanSchedule",
        "ScanScheduleResponse",
    ),
    (
        "getProviderRuntimeSettingsSchema",
        "ProviderRuntimeSettingsSchemaResponse",
    ),
    (
        "getProviderRuntimeSettings",
        "ProviderRuntimeSettingsResponse",
    ),
    (
        "putProviderRuntimeSettings",
        "ProviderRuntimeSettingsResponse",
    ),
    (
        "getProviderAccountRuntimeSettings",
        "ProviderRuntimeSettingsResponse",
    ),
    (
        "putProviderAccountRuntimeSettings",
        "ProviderRuntimeSettingsResponse",
    ),
    ("getTaskRuntimeSettings", "ProviderRuntimeSettingsResponse"),
    ("putTaskRuntimeSettings", "ProviderRuntimeSettingsResponse"),
    ("listTasks", "TaskPageResponse"),
    ("getTask", "Task"),
    ("getTaskDetail", "TaskDetailResponse"),
    (
        "getTaskBrowserSessionSpec",
        "TaskBrowserSessionSpecResponse",
    ),
    ("getTaskProgress", "TaskProgressResponse"),
    ("getTaskDuration", "TaskDurationResponse"),
    ("getTaskQuestions", "TaskQuestionsResponse"),
    ("getTaskQuestionSnapshot", "TaskQuestionsResponse"),
    (
        "resolveProviderAnswerCandidates",
        "AnswerCandidatesResponse",
    ),
    ("listAnswerCandidates", "AnswerCandidatesResponse"),
    ("createManualAnswerCandidate", "AnswerCandidateResponse"),
    (
        "importLocalAnswerCandidates",
        "LocalAnswerCacheImportResponse",
    ),
    ("resolveAnswerCandidates", "AnswerResolutionPlan"),
    ("buildSubmissionDraft", "SubmissionDraft"),
    ("getSubmissionDraft", "SubmissionDraft"),
    ("getSubmissionResult", "SubmissionResult"),
    ("executeTask", "ExecuteTaskResponse"),
    ("approveTask", "TaskLifecycleResponse"),
    ("cancelTask", "TaskLifecycleResponse"),
    ("delayTask", "TaskLifecycleResponse"),
    ("ignoreTask", "TaskLifecycleResponse"),
    ("listAdminUsers", "UserProfilePageResponse"),
    ("createAdminUser", "UserProfile"),
    ("getAdminUser", "UserProfile"),
    ("updateAdminUser", "UserProfile"),
    ("grantUserCredits", "CreditGrantResponse"),
    ("listAuditRecords", "AuditPageResponse"),
    ("getOwnCreditAccount", "CreditAccount"),
    ("listOwnCreditTransactions", "CreditTransactionPageResponse"),
    ("listOwnCreditReservations", "CreditReservationPageResponse"),
    ("listExecutions", "ExecutionPageResponse"),
    ("getExecution", "ExecutionDetailResponse"),
    ("listExecutionLogs", "ExecutionLogPageResponse"),
    ("createServiceToken", "CreateServiceTokenResponse"),
    ("listServiceTokens", "ServiceTokenPageResponse"),
];

pub(super) fn schema_for(operation_id: &str) -> Option<&'static str> {
    JSON_SUCCESS_SCHEMAS
        .iter()
        .find_map(|(operation, schema)| (*operation == operation_id).then_some(*schema))
}

pub(super) fn register(schemas: &mut Map<String, Value>) {
    for (name, schema) in schemas_for_client() {
        schemas.insert(name.to_owned(), schema);
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "wire schemas remain auditable beside their operation mapping"
)]
fn schemas_for_client() -> Vec<(&'static str, Value)> {
    vec![
        (
            "HealthResponse",
            object(
                &[
                    "service",
                    "version",
                    "status",
                    "database",
                    "schema_version",
                    "registered_providers",
                    "outbox_pending",
                    "outbox_dead_letter",
                    "secret_store_configured",
                ],
                json!({
                    "service": string(),
                    "version": string(),
                    "status": string(),
                    "database": string(),
                    "schema_version": integer(),
                    "registered_providers": unsigned_integer(),
                    "outbox_pending": unsigned_integer(),
                    "outbox_dead_letter": unsigned_integer(),
                    "secret_store_configured": {"type": "boolean"}
                }),
            ),
        ),
        (
            "Role",
            json!({"type": "string", "enum": ["master", "operator", "user"]}),
        ),
        (
            "Permission",
            string_enum(&[
                "read_providers",
                "read_own_tasks",
                "manage_users",
                "manage_providers",
                "manage_credits",
                "grant_credits",
                "manage_pricing",
                "manage_system",
                "manage_own_accounts",
                "read_own_credits",
                "execute_own_tasks",
                "execute_any_task",
                "view_own_audit",
                "view_any_audit",
            ]),
        ),
        (
            "UserSummary",
            object(
                &["id", "username", "roles"],
                json!({
                    "id": uuid(),
                    "username": string(),
                    "roles": {"type": "array", "items": schema_ref("Role")}
                }),
            ),
        ),
        (
            "UserStatus",
            json!({"type": "string", "enum": ["active", "suspended", "disabled"]}),
        ),
        (
            "UserProfile",
            object(
                &[
                    "id",
                    "username",
                    "status",
                    "roles",
                    "permissions",
                    "created_at",
                    "updated_at",
                ],
                json!({
                    "id": uuid(),
                    "username": string(),
                    "status": schema_ref("UserStatus"),
                    "roles": {"type": "array", "uniqueItems": true, "items": schema_ref("Role")},
                    "permissions": {"type": "array", "uniqueItems": true, "items": schema_ref("Permission")},
                    "created_at": timestamp(),
                    "updated_at": timestamp()
                }),
            ),
        ),
        ("UserProfilePageResponse", page_response("UserProfile")),
        (
            "AuditRecord",
            object(
                &[
                    "id",
                    "occurred_at",
                    "actor_type",
                    "actor_id",
                    "action",
                    "resource_type",
                    "resource_id",
                    "request_id",
                    "correlation_id",
                    "outcome",
                    "metadata_sanitized",
                ],
                json!({
                    "id": uuid(),
                    "occurred_at": timestamp(),
                    "actor_type": string(),
                    "actor_id": nullable_string(),
                    "action": string(),
                    "resource_type": string(),
                    "resource_id": nullable_string(),
                    "request_id": nullable_string(),
                    "correlation_id": nullable_string(),
                    "outcome": string(),
                    "metadata_sanitized": {}
                }),
            ),
        ),
        ("AuditPageResponse", page_response("AuditRecord")),
        (
            "LoginResponse",
            object(
                &["user", "expires_at"],
                json!({"user": schema_ref("UserSummary"), "expires_at": timestamp()}),
            ),
        ),
        (
            "IdentityResponse",
            object(
                &[
                    "identity_type",
                    "user_id",
                    "service_token_id",
                    "scopes",
                    "permissions",
                ],
                json!({
                    "identity_type": {"type": "string", "enum": ["web_session", "service_token"]},
                    "user_id": nullable_uuid(),
                    "service_token_id": nullable_uuid(),
                    "scopes": {"type": "array", "uniqueItems": true, "items": schema_ref("ServiceScope")},
                    "permissions": {"type": "array", "uniqueItems": true, "items": schema_ref("Permission")}
                }),
            ),
        ),
        (
            "ServiceToken",
            object(
                &[
                    "id",
                    "owner_user_id",
                    "name",
                    "scopes",
                    "created_at",
                    "expires_at",
                    "revoked_at",
                    "last_used_at",
                ],
                json!({
                    "id": uuid(),
                    "owner_user_id": nullable_uuid(),
                    "name": string(),
                    "scopes": {"type": "array", "uniqueItems": true, "items": schema_ref("ServiceScope")},
                    "created_at": timestamp(),
                    "expires_at": nullable_timestamp(),
                    "revoked_at": nullable_timestamp(),
                    "last_used_at": nullable_timestamp()
                }),
            ),
        ),
        (
            "CreateServiceTokenResponse",
            object(
                &["token", "metadata"],
                json!({
                    "token": {"type": "string", "minLength": 1, "readOnly": true},
                    "metadata": schema_ref("ServiceToken")
                }),
            ),
        ),
        ("ServiceTokenPageResponse", page_response("ServiceToken")),
        (
            "VerificationLevel",
            string_enum(&[
                "development",
                "experimental",
                "community_verified",
                "verified",
                "broken",
            ]),
        ),
        (
            "ProviderCapability",
            string_enum(&[
                "authentication",
                "course_inventory",
                "task_inventory",
                "task_detail",
                "task_progress_read",
                "resource_execution",
                "execution_verify",
                "question_inventory",
                "question_parse",
                "answer_resolve",
                "submission_build",
                "submission_execute",
                "submission_verify",
                "duration_read",
                "duration_report",
                "discussion",
                "practice",
                "browser_bridge",
            ]),
        ),
        (
            "AuthMethod",
            string_enum(&[
                "password",
                "qr_code",
                "external_browser_oauth",
                "assisted_session",
                "imported_cookie",
                "imported_token",
            ]),
        ),
        (
            "SessionKind",
            string_enum(&[
                "cookie",
                "bearer_token",
                "jwt",
                "composite",
                "provider_specific",
            ]),
        ),
        (
            "ProviderMetadata",
            object(
                &[
                    "id",
                    "display_name",
                    "implementation_version",
                    "verification",
                    "scan_min_interval_seconds",
                    "capture_recipe_version",
                    "capabilities",
                    "auth_methods",
                    "session_kinds",
                ],
                json!({
                    "id": provider_id(),
                    "display_name": string(),
                    "implementation_version": string(),
                    "verification": schema_ref("VerificationLevel"),
                    "scan_min_interval_seconds": nullable_unsigned_integer(),
                    "capture_recipe_version": nullable_unsigned_integer(),
                    "capabilities": {"type": "array", "uniqueItems": true, "items": schema_ref("ProviderCapability")},
                    "auth_methods": {"type": "array", "uniqueItems": true, "items": schema_ref("AuthMethod")},
                    "session_kinds": {"type": "array", "uniqueItems": true, "items": schema_ref("SessionKind")}
                }),
            ),
        ),
        (
            "ProviderMetadataListResponse",
            list_response("ProviderMetadata"),
        ),
        ("AuthState", auth_state_schema()),
        (
            "ProviderAccountResponse",
            object(
                &[
                    "id",
                    "owner_id",
                    "provider_id",
                    "display_name",
                    "tenant",
                    "auth_state",
                    "network_profile_id",
                    "credential_count",
                    "created_at",
                    "updated_at",
                ],
                json!({
                    "id": uuid(),
                    "owner_id": uuid(),
                    "provider_id": provider_id(),
                    "display_name": string(),
                    "tenant": nullable_string(),
                    "auth_state": schema_ref("AuthState"),
                    "network_profile_id": nullable_string(),
                    "credential_count": unsigned_integer(),
                    "created_at": timestamp(),
                    "updated_at": timestamp()
                }),
            ),
        ),
        (
            "ProviderAccountListResponse",
            list_response("ProviderAccountResponse"),
        ),
        ("ProviderScanReport", provider_scan_report_schema()),
        (
            "SessionStatus",
            object(
                &["valid", "kind", "expires_at", "account_hint"],
                json!({
                    "valid": {"type": "boolean"},
                    "kind": schema_ref("SessionKind"),
                    "expires_at": nullable_timestamp(),
                    "account_hint": nullable_string()
                }),
            ),
        ),
        ("AuthBootstrapSession", auth_bootstrap_session_schema()),
        ("CaptureValueSource", capture_value_source_schema()),
        ("CaptureReadiness", capture_readiness_schema()),
        (
            "CaptureCredentialOutput",
            object(
                &["purpose", "required", "sources"],
                json!({
                    "purpose": capture_secret_purpose(),
                    "required": {"type": "boolean"},
                    "sources": {
                        "type": "array",
                        "minItems": 1,
                        "maxItems": 8,
                        "items": schema_ref("CaptureValueSource")
                    }
                }),
            ),
        ),
        (
            "CaptureRecipe",
            object(
                &[
                    "version",
                    "start_url",
                    "navigation_origins",
                    "read_origins",
                    "poll_interval_millis",
                    "auth_method",
                    "session_kind",
                    "readiness",
                    "outputs",
                ],
                json!({
                    "version": {"type": "integer", "format": "int64", "minimum": 1},
                    "start_url": {"type": "string", "format": "uri", "pattern": "^https://"},
                    "navigation_origins": {
                        "type": "array",
                        "minItems": 1,
                        "maxItems": 8,
                        "uniqueItems": true,
                        "items": {"type": "string", "format": "uri", "pattern": "^https://[^/]+$"}
                    },
                    "read_origins": {
                        "type": "array",
                        "minItems": 1,
                        "maxItems": 8,
                        "uniqueItems": true,
                        "items": {"type": "string", "format": "uri", "pattern": "^https://[^/]+$"}
                    },
                    "poll_interval_millis": {"type": "integer", "format": "int64", "minimum": 100, "maximum": 5000},
                    "auth_method": schema_ref("AuthMethod"),
                    "session_kind": schema_ref("SessionKind"),
                    "readiness": schema_ref("CaptureReadiness"),
                    "outputs": {
                        "type": "array",
                        "minItems": 1,
                        "maxItems": 16,
                        "items": schema_ref("CaptureCredentialOutput")
                    }
                }),
            ),
        ),
        (
            "AuthBootstrapCreateResponse",
            object(
                &["session", "pairing_token"],
                json!({
                    "session": schema_ref("AuthBootstrapSession"),
                    "pairing_token": {"type": "string", "minLength": 1, "writeOnly": true}
                }),
            ),
        ),
        (
            "AuthBootstrapClaimResponse",
            object(
                &["session", "access_token", "recipe"],
                json!({
                    "session": schema_ref("AuthBootstrapSession"),
                    "access_token": {"type": "string", "minLength": 1, "writeOnly": true},
                    "recipe": schema_ref("CaptureRecipe")
                }),
            ),
        ),
        (
            "AuthBootstrapClientEvent",
            object(
                &["session_id", "sequence", "kind", "received_at"],
                json!({
                    "session_id": uuid(),
                    "sequence": {"type": "integer", "format": "int64", "minimum": 1},
                    "kind": auth_bootstrap_event_kind_schema(),
                    "received_at": timestamp()
                }),
            ),
        ),
        (
            "AuthBootstrapEventResponse",
            object(
                &["event", "duplicate"],
                json!({
                    "event": schema_ref("AuthBootstrapClientEvent"),
                    "duplicate": {"type": "boolean"}
                }),
            ),
        ),
        (
            "AuthBootstrapCredentialResponse",
            object(
                &[
                    "session",
                    "provider_account_id",
                    "credential_count",
                    "status",
                ],
                json!({
                    "session": schema_ref("AuthBootstrapSession"),
                    "provider_account_id": uuid(),
                    "credential_count": {"type": "integer", "format": "int64", "minimum": 1, "maximum": 16},
                    "status": schema_ref("SessionStatus")
                }),
            ),
        ),
        (
            "AuthSession",
            object(
                &[
                    "id",
                    "owner_user_id",
                    "provider_account_id",
                    "method",
                    "state",
                    "created_at",
                    "updated_at",
                    "expires_at",
                    "revision",
                ],
                json!({
                    "id": uuid(),
                    "owner_user_id": uuid(),
                    "provider_account_id": uuid(),
                    "method": schema_ref("AuthMethod"),
                    "state": schema_ref("AuthState"),
                    "created_at": timestamp(),
                    "updated_at": timestamp(),
                    "expires_at": timestamp(),
                    "revision": unsigned_integer()
                }),
            ),
        ),
        (
            "ExternalOauthAuthorization",
            object(
                &["authorization_url"],
                json!({"authorization_url": {"type": "string", "format": "uri", "maxLength": 4096}}),
            ),
        ),
        (
            "AuthChallenge",
            object(
                &[
                    "session_id",
                    "method",
                    "waiting_for",
                    "user_action",
                    "expires_at",
                    "external_oauth",
                ],
                json!({
                    "session_id": uuid(),
                    "method": schema_ref("AuthMethod"),
                    "waiting_for": string_enum(&["credential_input", "qr_scan", "qr_confirm", "browser_callback", "sms_code", "session_import"]),
                    "user_action": nullable_string(),
                    "expires_at": nullable_timestamp(),
                    "external_oauth": nullable_schema_ref("ExternalOauthAuthorization")
                }),
            ),
        ),
        (
            "AuthSessionBeginResponse",
            object(
                &["session", "challenge"],
                json!({"session": schema_ref("AuthSession"), "challenge": schema_ref("AuthChallenge")}),
            ),
        ),
        (
            "ExternalOauthPendingResponse",
            object(
                &[
                    "auth_session_id",
                    "state",
                    "expires_at",
                    "consumed_at",
                    "revision",
                ],
                json!({
                    "auth_session_id": uuid(),
                    "state": string_enum(&["pending", "completing", "succeeded", "failed", "ambiguous", "expired", "cancelled"]),
                    "expires_at": timestamp(),
                    "consumed_at": nullable_timestamp(),
                    "revision": unsigned_integer()
                }),
            ),
        ),
        (
            "PutProviderCredentialsResponse",
            object(
                &["credential_count", "status"],
                json!({"credential_count": unsigned_integer(), "status": schema_ref("SessionStatus")}),
            ),
        ),
        (
            "PutAuthSessionCredentialsResponse",
            object(
                &["session", "credential_count", "status"],
                json!({
                    "session": schema_ref("AuthSession"),
                    "credential_count": unsigned_integer(),
                    "status": schema_ref("SessionStatus")
                }),
            ),
        ),
        (
            "ScanScheduleResponse",
            object(
                &[
                    "id",
                    "provider_account_id",
                    "desired_interval_seconds",
                    "effective_interval_seconds",
                    "provider_min_interval_seconds",
                    "next_run_at",
                    "enabled",
                    "created_at",
                    "updated_at",
                ],
                json!({
                    "id": uuid(),
                    "provider_account_id": uuid(),
                    "desired_interval_seconds": unsigned_integer(),
                    "effective_interval_seconds": unsigned_integer(),
                    "provider_min_interval_seconds": nullable_unsigned_integer(),
                    "next_run_at": timestamp(),
                    "enabled": {"type": "boolean"},
                    "created_at": timestamp(),
                    "updated_at": timestamp()
                }),
            ),
        ),
        ("ProviderSettingValue", provider_setting_value_schema()),
        ("ProviderSettingKind", provider_setting_kind_schema()),
        (
            "ProviderSettingDefinition",
            provider_setting_definition_schema(),
        ),
        (
            "ProviderRuntimeSettingsSchema",
            object(
                &["version", "definitions"],
                json!({
                    "version": {"type": "integer", "minimum": 1},
                    "definitions": {"type": "array", "items": schema_ref("ProviderSettingDefinition")}
                }),
            ),
        ),
        (
            "ProviderRuntimeSettingsSchemaResponse",
            object(
                &["provider_id", "schema"],
                json!({
                    "provider_id": provider_id(),
                    "schema": schema_ref("ProviderRuntimeSettingsSchema")
                }),
            ),
        ),
        ("ProviderRuntimeSettingsPatch", runtime_settings_values()),
        ("ResolvedProviderRuntimeSettings", runtime_settings_values()),
        (
            "ProviderRuntimeSettingsOverride",
            object(
                &["id", "patch", "revision", "created_at", "updated_at"],
                json!({
                    "id": uuid(),
                    "patch": schema_ref("ProviderRuntimeSettingsPatch"),
                    "revision": unsigned_integer(),
                    "created_at": timestamp(),
                    "updated_at": timestamp()
                }),
            ),
        ),
        (
            "ProviderRuntimeSettingsLayers",
            object(
                &["provider", "provider_account", "task"],
                json!({
                    "provider": nullable_schema_ref("ProviderRuntimeSettingsOverride"),
                    "provider_account": nullable_schema_ref("ProviderRuntimeSettingsOverride"),
                    "task": nullable_schema_ref("ProviderRuntimeSettingsOverride")
                }),
            ),
        ),
        (
            "ProviderRuntimeSettingsResponse",
            provider_runtime_settings_response_schema(),
        ),
        ("Task", task_schema()),
        ("TaskPageResponse", page_response("Task")),
        ("RemoteTask", remote_task_schema()),
        (
            "RemoteTaskDetail",
            object(
                &["task", "normalized_detail"],
                json!({"task": schema_ref("RemoteTask"), "normalized_detail": {}}),
            ),
        ),
        (
            "RemoteProgress",
            object(
                &["remote_state", "percent", "duration_seconds", "updated_at"],
                json!({
                    "remote_state": remote_state(),
                    "percent": {"type": ["integer", "null"], "minimum": 0, "maximum": 100},
                    "duration_seconds": nullable_unsigned_integer(),
                    "updated_at": timestamp()
                }),
            ),
        ),
        (
            "RemoteDuration",
            object(
                &["duration_seconds", "updated_at"],
                json!({"duration_seconds": unsigned_integer(), "updated_at": timestamp()}),
            ),
        ),
        (
            "TaskDetailResponse",
            provider_task_read_response("detail", "RemoteTaskDetail"),
        ),
        (
            "BrowserSessionSpec",
            object(
                &["version", "isolation_key", "allowed_origins", "headless"],
                json!({
                    "version": {"type": "integer", "minimum": 1, "maximum": u32::MAX},
                    "isolation_key": {"type": "string", "minLength": 1, "maxLength": 128},
                    "allowed_origins": {
                        "type": "array",
                        "minItems": 1,
                        "maxItems": 8,
                        "uniqueItems": true,
                        "items": {"type": "string", "format": "uri", "maxLength": 256}
                    },
                    "headless": {"type": "boolean"}
                }),
            ),
        ),
        (
            "TaskBrowserSessionSpecResponse",
            provider_task_read_response("spec", "BrowserSessionSpec"),
        ),
        (
            "TaskProgressResponse",
            provider_task_read_response("progress", "RemoteProgress"),
        ),
        (
            "TaskDurationResponse",
            provider_task_read_response("duration", "RemoteDuration"),
        ),
        ("QuestionAttachment", question_attachment_schema()),
        ("QuestionOption", question_option_schema()),
        ("Question", question_schema()),
        ("TaskQuestionsResponse", task_questions_response_schema()),
        ("AnswerCandidate", answer_candidate_schema()),
        (
            "AnswerCandidateResponse",
            object(
                &["id", "candidate", "created_at"],
                json!({
                    "id": uuid(),
                    "candidate": schema_ref("AnswerCandidate"),
                    "created_at": timestamp()
                }),
            ),
        ),
        (
            "AnswerCandidatesResponse",
            answer_candidates_response_schema(),
        ),
        (
            "LocalAnswerCacheImportResponse",
            local_answer_cache_import_response_schema(),
        ),
        (
            "AnswerResolutionDecision",
            answer_resolution_decision_schema(),
        ),
        ("AnswerResolutionPlan", answer_resolution_plan_schema()),
        ("SelectedAnswer", selected_answer_schema()),
        (
            "SubmissionPayloadPreview",
            submission_payload_preview_schema(),
        ),
        ("SubmissionDraft", submission_draft_schema()),
        ("SubmissionReceipt", submission_receipt_schema()),
        (
            "SubmissionVerificationSnapshot",
            submission_verification_schema(),
        ),
        ("SubmissionResult", submission_result_schema()),
        ("CreditAccount", credit_account_schema()),
        ("PriceQuote", price_quote_schema()),
        ("CreditReservation", credit_reservation_schema()),
        ("CreditTransaction", credit_transaction_schema()),
        (
            "CreditGrantResponse",
            object(
                &["account", "transaction", "created"],
                json!({
                    "account": schema_ref("CreditAccount"),
                    "transaction": schema_ref("CreditTransaction"),
                    "created": {"type": "boolean"}
                }),
            ),
        ),
        (
            "CreditReservationDetailResponse",
            object(
                &["reservation", "quote"],
                json!({
                    "reservation": schema_ref("CreditReservation"),
                    "quote": schema_ref("PriceQuote")
                }),
            ),
        ),
        (
            "CreditTransactionPageResponse",
            page_response("CreditTransaction"),
        ),
        (
            "CreditReservationPageResponse",
            page_response("CreditReservationDetailResponse"),
        ),
        ("Execution", execution_schema()),
        ("ExecutionAttempt", execution_attempt_schema()),
        ("ExecutionProgress", execution_progress_schema()),
        ("ExecutionLogEvent", execution_log_schema()),
        (
            "ExecuteTaskResponse",
            object(
                &["execution", "created"],
                json!({"execution": schema_ref("Execution"), "created": {"type": "boolean"}}),
            ),
        ),
        (
            "TaskLifecycleResponse",
            object(
                &[
                    "task_id",
                    "action",
                    "task_state",
                    "affected_execution_id",
                    "delayed_until",
                    "created",
                ],
                json!({
                    "task_id": uuid(),
                    "action": {"type": "string", "enum": ["approve", "cancel", "delay", "ignore"]},
                    "task_state": {"type": "string", "enum": ["discovered", "ready", "waiting_approval", "scheduled", "credit_blocked", "human_required", "running", "recovering", "retry_waiting", "succeeded", "failed", "cancelled", "ignored"]},
                    "affected_execution_id": {"oneOf": [uuid(), {"type": "null"}]},
                    "delayed_until": {"oneOf": [timestamp(), {"type": "null"}]},
                    "created": {"type": "boolean"}
                }),
            ),
        ),
        ("ExecutionPageResponse", page_response("Execution")),
        (
            "ExecutionDetailResponse",
            object(
                &["execution", "progress", "attempts"],
                json!({
                    "execution": schema_ref("Execution"),
                    "progress": {"oneOf": [schema_ref("ExecutionProgress"), {"type": "null"}]},
                    "attempts": {"type": "array", "items": schema_ref("ExecutionAttempt")}
                }),
            ),
        ),
        (
            "ExecutionLogPageResponse",
            page_response("ExecutionLogEvent"),
        ),
    ]
}

fn provider_scan_report_schema() -> Value {
    object(
        &[
            "courses_seen",
            "tasks_created",
            "tasks_updated",
            "tasks_unchanged",
            "task_changes",
        ],
        json!({
            "courses_seen": unsigned_integer(),
            "tasks_created": unsigned_integer(),
            "tasks_updated": unsigned_integer(),
            "tasks_unchanged": unsigned_integer(),
            "task_changes": {
                "type": "array",
                "items": object(&["task_id", "changes"], json!({
                    "task_id": uuid(),
                    "changes": {
                        "type": "array",
                        "items": string_enum(&["created", "metadata_changed", "content_changed", "deadline_changed", "opened", "closed", "reopened", "completed_externally", "removed"])
                    }
                }))
            }
        }),
    )
}

fn credit_account_schema() -> Value {
    object(
        &["user_id", "available", "reserved"],
        json!({
            "user_id": uuid(),
            "available": unsigned_integer(),
            "reserved": unsigned_integer()
        }),
    )
}

fn price_quote_schema() -> Value {
    object(
        &[
            "id",
            "task_id",
            "amount",
            "pricing_revision",
            "reason",
            "created_at",
        ],
        json!({
            "id": uuid(),
            "task_id": uuid(),
            "amount": unsigned_integer(),
            "pricing_revision": string(),
            "reason": string(),
            "created_at": timestamp()
        }),
    )
}

fn credit_reservation_schema() -> Value {
    object(
        &[
            "id",
            "user_id",
            "quote_id",
            "execution_id",
            "amount",
            "state",
            "created_at",
            "updated_at",
        ],
        json!({
            "id": uuid(),
            "user_id": uuid(),
            "quote_id": uuid(),
            "execution_id": uuid(),
            "amount": unsigned_integer(),
            "state": string_enum(&["reserved", "committed", "released"]),
            "created_at": timestamp(),
            "updated_at": timestamp()
        }),
    )
}

fn credit_transaction_schema() -> Value {
    object(
        &[
            "id",
            "user_id",
            "amount",
            "transaction_type",
            "task_id",
            "execution_id",
            "operator_id",
            "reason",
            "created_at",
        ],
        json!({
            "id": uuid(),
            "user_id": uuid(),
            "amount": integer(),
            "transaction_type": string_enum(&["master_grant", "task_execution", "task_refund", "manual_adjustment"]),
            "task_id": nullable_uuid(),
            "execution_id": nullable_uuid(),
            "operator_id": nullable_uuid(),
            "reason": string(),
            "created_at": timestamp()
        }),
    )
}

fn question_attachment_schema() -> Value {
    object(
        &["kind", "remote_id", "label", "metadata_sanitized"],
        json!({
            "kind": string_enum(&["image", "audio", "video", "file", "formula", "other"]),
            "remote_id": nullable_string(),
            "label": nullable_string(),
            "metadata_sanitized": {}
        }),
    )
}

fn question_option_schema() -> Value {
    object(
        &["id", "content", "attachments", "metadata_sanitized"],
        json!({
            "id": string(),
            "content": nullable_string(),
            "attachments": {"type": "array", "items": schema_ref("QuestionAttachment")},
            "metadata_sanitized": {}
        }),
    )
}

fn question_schema() -> Value {
    object(
        &[
            "id",
            "task_id",
            "remote_question_id",
            "kind",
            "stem",
            "options",
            "attachments",
            "metadata_sanitized",
            "position",
        ],
        json!({
            "id": uuid(),
            "task_id": uuid(),
            "remote_question_id": nullable_string(),
            "kind": string_enum(&["single_choice", "multiple_choice", "true_false", "fill_blank", "short_answer", "matching", "ordering", "composite", "unknown"]),
            "stem": string(),
            "options": {"type": "array", "items": schema_ref("QuestionOption")},
            "attachments": {"type": "array", "items": schema_ref("QuestionAttachment")},
            "metadata_sanitized": {},
            "position": {"type": "integer", "minimum": 1}
        }),
    )
}

fn task_questions_response_schema() -> Value {
    object(
        &[
            "snapshot_id",
            "task_id",
            "provider_id",
            "provider_version",
            "captured_at",
            "questions",
        ],
        json!({
            "snapshot_id": uuid(),
            "task_id": uuid(),
            "provider_id": provider_id(),
            "provider_version": string(),
            "captured_at": timestamp(),
            "questions": {"type": "array", "items": schema_ref("Question")}
        }),
    )
}

fn answer_candidate_schema() -> Value {
    object(
        &[
            "question_id",
            "source",
            "answer",
            "confidence",
            "explanation",
            "provenance_sanitized",
        ],
        json!({
            "question_id": uuid(),
            "source": answer_source(),
            "answer": schema_ref("NormalizedAnswer"),
            "confidence": {"type": ["integer", "null"], "minimum": 0, "maximum": 10_000},
            "explanation": nullable_string(),
            "provenance_sanitized": {}
        }),
    )
}

fn answer_candidates_response_schema() -> Value {
    object(
        &[
            "task_id",
            "question_snapshot_id",
            "provider_id",
            "provider_version",
            "candidates",
        ],
        json!({
            "task_id": uuid(),
            "question_snapshot_id": uuid(),
            "provider_id": provider_id(),
            "provider_version": string(),
            "candidates": {"type": "array", "items": schema_ref("AnswerCandidateResponse")}
        }),
    )
}

fn local_answer_cache_import_response_schema() -> Value {
    object(
        &["task_id", "question_snapshot_id", "candidates"],
        json!({
            "task_id": uuid(),
            "question_snapshot_id": uuid(),
            "candidates": {"type": "array", "items": schema_ref("AnswerCandidateResponse")}
        }),
    )
}

fn answer_resolution_decision_schema() -> Value {
    object(
        &[
            "question_id",
            "status",
            "considered_candidate_ids",
            "selected_candidate_id",
            "selected_answer",
        ],
        json!({
            "question_id": uuid(),
            "status": string_enum(&["selected", "conflict", "missing"]),
            "considered_candidate_ids": {"type": "array", "items": uuid()},
            "selected_candidate_id": nullable_uuid(),
            "selected_answer": nullable_schema_ref("NormalizedAnswer")
        }),
    )
}

fn answer_resolution_plan_schema() -> Value {
    object(
        &["task_id", "question_snapshot_id", "decisions"],
        json!({
            "task_id": uuid(),
            "question_snapshot_id": uuid(),
            "decisions": {"type": "array", "items": schema_ref("AnswerResolutionDecision")}
        }),
    )
}

fn selected_answer_schema() -> Value {
    object(
        &[
            "candidate_id",
            "question_id",
            "answer",
            "source",
            "confidence",
        ],
        json!({
            "candidate_id": uuid(),
            "question_id": uuid(),
            "answer": schema_ref("NormalizedAnswer"),
            "source": answer_source(),
            "confidence": {"type": ["integer", "null"], "minimum": 0, "maximum": 10_000}
        }),
    )
}

fn submission_payload_preview_schema() -> Value {
    object(
        &["encoding", "format", "fields"],
        json!({
            "encoding": string_enum(&["form", "json", "query", "provider_specific"]),
            "format": string(),
            "fields": {
                "type": "array",
                "items": object(&["question_id", "field_name"], json!({
                    "question_id": uuid(),
                    "field_name": string()
                }))
            }
        }),
    )
}

fn submission_draft_schema() -> Value {
    object(
        &[
            "id",
            "task_id",
            "question_snapshot_id",
            "provider_id",
            "provider_version",
            "items",
            "payload_preview",
            "created_at",
        ],
        json!({
            "id": uuid(),
            "task_id": uuid(),
            "question_snapshot_id": uuid(),
            "provider_id": provider_id(),
            "provider_version": string(),
            "items": {
                "type": "array",
                "items": object(&["question", "selected"], json!({
                    "question": schema_ref("Question"),
                    "selected": schema_ref("SelectedAnswer")
                }))
            },
            "payload_preview": schema_ref("SubmissionPayloadPreview"),
            "created_at": timestamp()
        }),
    )
}

fn submission_receipt_schema() -> Value {
    object(
        &[
            "remote_status",
            "message_sanitized",
            "provider_trace_id",
            "received_at",
        ],
        json!({
            "remote_status": string(),
            "message_sanitized": nullable_string(),
            "provider_trace_id": nullable_string(),
            "received_at": timestamp()
        }),
    )
}

fn submission_verification_schema() -> Value {
    object(
        &[
            "status",
            "remote_state",
            "score",
            "progress_percent",
            "questions",
            "verified_at",
        ],
        json!({
            "status": string_enum(&["confirmed", "rejected", "pending", "inconclusive"]),
            "remote_state": nullable_string_enum(&["unknown", "not_open", "pending", "in_progress", "completed", "expired", "removed"]),
            "score": {
                "oneOf": [
                    object(&["earned_milli_points", "possible_milli_points"], json!({
                        "earned_milli_points": unsigned_integer(),
                        "possible_milli_points": unsigned_integer()
                    })),
                    {"type": "null"}
                ]
            },
            "progress_percent": {"type": ["integer", "null"], "minimum": 0, "maximum": 100},
            "questions": {
                "type": "array",
                "items": object(&["question_id", "status"], json!({
                    "question_id": uuid(),
                    "status": string_enum(&["confirmed", "rejected", "unverified"])
                }))
            },
            "verified_at": timestamp()
        }),
    )
}

fn submission_result_schema() -> Value {
    object(
        &[
            "id",
            "submission_draft_id",
            "execution_id",
            "execution_attempt_id",
            "task_id",
            "question_snapshot_id",
            "provider_id",
            "provider_version",
            "status",
            "receipt",
            "verification",
            "created_at",
        ],
        json!({
            "id": uuid(),
            "submission_draft_id": uuid(),
            "execution_id": uuid(),
            "execution_attempt_id": uuid(),
            "task_id": uuid(),
            "question_snapshot_id": uuid(),
            "provider_id": provider_id(),
            "provider_version": string(),
            "status": string_enum(&["confirmed", "rejected", "execution_failed", "inconclusive"]),
            "receipt": nullable_schema_ref("SubmissionReceipt"),
            "verification": schema_ref("SubmissionVerificationSnapshot"),
            "created_at": timestamp()
        }),
    )
}

fn answer_source() -> Value {
    string_enum(&[
        "manual",
        "local_cache",
        "provider_native",
        "external_bank",
        "other",
    ])
}

fn provider_setting_value_schema() -> Value {
    json!({
        "oneOf": [
            object(&["type", "value"], json!({"type": {"const": "boolean"}, "value": {"type": "boolean"}})),
            object(&["type", "value"], json!({"type": {"const": "integer"}, "value": integer()})),
            object(&["type", "value"], json!({"type": {"const": "decimal_millis"}, "value": integer()})),
            object(&["type", "value"], json!({"type": {"const": "duration_seconds"}, "value": unsigned_integer()})),
            object(&["type", "value"], json!({"type": {"const": "choice"}, "value": string()}))
        ]
    })
}

fn provider_setting_kind_schema() -> Value {
    json!({
        "oneOf": [
            object(&["type"], json!({"type": {"const": "boolean"}})),
            bounded_numeric_setting_kind("integer"),
            bounded_numeric_setting_kind("decimal_millis"),
            bounded_numeric_setting_kind("duration_seconds"),
            object(&["type", "options"], json!({
                "type": {"const": "choice"},
                "options": {"type": "array", "uniqueItems": true, "items": string()}
            }))
        ]
    })
}

fn bounded_numeric_setting_kind(kind: &str) -> Value {
    object(
        &["type", "minimum", "maximum", "step"],
        json!({
            "type": {"const": kind},
            "minimum": integer(),
            "maximum": integer(),
            "step": unsigned_integer()
        }),
    )
}

fn provider_setting_definition_schema() -> Value {
    object(
        &[
            "key",
            "display_name",
            "description",
            "kind",
            "default",
            "scopes",
        ],
        json!({
            "key": string(),
            "display_name": string(),
            "description": string(),
            "kind": schema_ref("ProviderSettingKind"),
            "default": schema_ref("ProviderSettingValue"),
            "scopes": {"type": "array", "uniqueItems": true, "items": string_enum(&["provider", "provider_account", "task"])},
            "core_behavior": string_enum(&["provider_execution_concurrency", "account_execution_concurrency", "account_scan_interval"])
        }),
    )
}

fn runtime_settings_values() -> Value {
    object(
        &["schema_version", "values"],
        json!({
            "schema_version": {"type": "integer", "minimum": 1},
            "values": {"type": "object", "additionalProperties": schema_ref("ProviderSettingValue")}
        }),
    )
}

fn provider_runtime_settings_response_schema() -> Value {
    object(
        &[
            "provider_id",
            "provider_account_id",
            "task_id",
            "target_scope",
            "schema",
            "overrides",
            "resolved",
            "sources",
        ],
        json!({
            "provider_id": provider_id(),
            "provider_account_id": nullable_uuid(),
            "task_id": nullable_uuid(),
            "target_scope": string_enum(&["provider", "provider_account", "task"]),
            "schema": schema_ref("ProviderRuntimeSettingsSchema"),
            "overrides": schema_ref("ProviderRuntimeSettingsLayers"),
            "resolved": schema_ref("ResolvedProviderRuntimeSettings"),
            "sources": {"type": "object", "additionalProperties": string_enum(&["schema_default", "provider", "provider_account", "task"])}
        }),
    )
}

fn remote_task_schema() -> Value {
    object(
        &[
            "remote_id",
            "course_remote_id",
            "title",
            "source_type",
            "assessment_class",
            "remote_state",
            "opens_at",
            "due_at",
            "closes_at",
            "capabilities",
            "fingerprint",
            "normalized",
            "raw_sanitized",
        ],
        json!({
            "remote_id": string(),
            "course_remote_id": nullable_string(),
            "title": string(),
            "source_type": source_type(),
            "assessment_class": assessment_class(),
            "remote_state": remote_state(),
            "opens_at": nullable_timestamp(),
            "due_at": nullable_timestamp(),
            "closes_at": nullable_timestamp(),
            "capabilities": {"type": "array", "items": task_capability()},
            "fingerprint": string(),
            "normalized": {},
            "raw_sanitized": {}
        }),
    )
}

fn provider_task_read_response(field: &str, value_schema: &str) -> Value {
    let properties = Map::from_iter([
        ("task_id".to_owned(), uuid()),
        ("provider_id".to_owned(), provider_id()),
        ("provider_version".to_owned(), string()),
        (field.to_owned(), schema_ref(value_schema)),
    ]);
    object(
        &["task_id", "provider_id", "provider_version", field],
        Value::Object(properties),
    )
}

fn task_schema() -> Value {
    object(
        &[
            "id",
            "provider_account_id",
            "course_id",
            "remote_id",
            "source_type",
            "assessment_class",
            "title",
            "remote_state",
            "orchestration_state",
            "opens_at",
            "due_at",
            "closes_at",
            "discovered_at",
            "updated_at",
            "latest_snapshot_id",
            "capabilities",
        ],
        json!({
            "id": uuid(),
            "provider_account_id": uuid(),
            "course_id": nullable_uuid(),
            "remote_id": string(),
            "source_type": source_type(),
            "assessment_class": assessment_class(),
            "title": string(),
            "remote_state": remote_state(),
            "orchestration_state": string_enum(&["discovered", "ready", "waiting_approval", "scheduled", "credit_blocked", "human_required", "running", "recovering", "retry_waiting", "succeeded", "failed", "cancelled", "ignored"]),
            "opens_at": nullable_timestamp(),
            "due_at": nullable_timestamp(),
            "closes_at": nullable_timestamp(),
            "discovered_at": timestamp(),
            "updated_at": timestamp(),
            "latest_snapshot_id": nullable_uuid(),
            "capabilities": {"type": "array", "items": task_capability()}
        }),
    )
}

fn execution_schema() -> Value {
    object(
        &[
            "id",
            "task_id",
            "requested_capabilities",
            "submission_draft_id",
            "requested_by",
            "request_source",
            "quote_id",
            "state",
            "scheduled_at",
            "started_at",
            "finished_at",
            "created_at",
        ],
        json!({
            "id": uuid(),
            "task_id": uuid(),
            "requested_capabilities": {
                "type": "array",
                "minItems": 1,
                "maxItems": 5,
                "uniqueItems": true,
                "items": task_capability()
            },
            "submission_draft_id": nullable_uuid(),
            "requested_by": nullable_uuid(),
            "request_source": string_enum(&["scheduler", "web_ui", "yunzai", "cli", "system"]),
            "quote_id": nullable_uuid(),
            "state": string_enum(&["requested", "scheduled", "running", "recovering", "retry_waiting", "human_required", "succeeded", "failed", "cancelled"]),
            "scheduled_at": nullable_timestamp(),
            "started_at": nullable_timestamp(),
            "finished_at": nullable_timestamp(),
            "created_at": timestamp()
        }),
    )
}

fn execution_attempt_schema() -> Value {
    object(
        &[
            "id",
            "execution_id",
            "attempt_no",
            "started_at",
            "finished_at",
            "result",
            "error_class",
            "provider_trace_id",
        ],
        json!({
            "id": uuid(),
            "execution_id": uuid(),
            "attempt_no": unsigned_integer(),
            "started_at": timestamp(),
            "finished_at": nullable_timestamp(),
            "result": nullable_string_enum(&["succeeded", "failed", "cancelled"]),
            "error_class": nullable_string_enum(&["authentication", "authorization", "rate_limited", "network", "provider_unavailable", "protocol_drift", "invalid_remote_state", "unsupported_task", "human_required", "internal"]),
            "provider_trace_id": nullable_string()
        }),
    )
}

fn execution_progress_schema() -> Value {
    object(
        &[
            "execution_id",
            "percent",
            "stage",
            "status_text",
            "current_item",
            "completed_items",
            "total_items",
            "updated_at",
        ],
        json!({
            "execution_id": uuid(),
            "percent": {"type": ["integer", "null"], "minimum": 0, "maximum": 100},
            "stage": execution_stage(),
            "status_text": nullable_string(),
            "current_item": nullable_string(),
            "completed_items": nullable_unsigned_integer(),
            "total_items": nullable_unsigned_integer(),
            "updated_at": timestamp()
        }),
    )
}

fn execution_log_schema() -> Value {
    object(
        &[
            "execution_id",
            "attempt_id",
            "timestamp",
            "level",
            "stage",
            "message",
            "provider_trace_id",
            "metadata_sanitized",
        ],
        json!({
            "execution_id": uuid(),
            "attempt_id": nullable_uuid(),
            "timestamp": timestamp(),
            "level": string_enum(&["trace", "debug", "info", "warn", "error"]),
            "stage": execution_stage(),
            "message": string(),
            "provider_trace_id": nullable_string(),
            "metadata_sanitized": {}
        }),
    )
}

fn auth_state_schema() -> Value {
    let unit_states = [
        "idle",
        "starting",
        "exchanging_credential",
        "validating_credential",
        "authenticated",
        "refreshing",
        "expired",
        "auth_failed",
        "provider_unavailable",
        "client_update_required",
        "cancelled",
    ];
    json!({
        "oneOf": [
            object(&["state"], json!({"state": string_enum(&unit_states)})),
            object(&["state", "detail"], json!({
                "state": {"const": "waiting_user"},
                "detail": string_enum(&["credential_input", "qr_scan", "qr_confirm", "browser_callback", "sms_code", "session_import"])
            })),
            object(&["state", "detail"], json!({
                "state": {"const": "human_required"},
                "detail": string_enum(&["auth_required", "session_expired", "qr_required", "sms_verification", "image_captcha", "browser_callback_required", "session_import_required", "user_confirmation", "browser_required", "unsupported_task", "remote_changed", "manual_intervention"])
            }))
        ]
    })
}

fn auth_bootstrap_session_schema() -> Value {
    object(
        &[
            "id",
            "owner_user_id",
            "provider_id",
            "provider_account_id",
            "purpose",
            "required_recipe_version",
            "state",
            "created_at",
            "updated_at",
            "expires_at",
            "claimed_at",
            "revision",
        ],
        json!({
            "id": uuid(),
            "owner_user_id": uuid(),
            "provider_id": provider_id(),
            "provider_account_id": nullable_uuid(),
            "purpose": string_enum(&["add_account", "reauthenticate", "repair_session"]),
            "required_recipe_version": {"type": "integer", "format": "int64", "minimum": 1},
            "state": string_enum(&["awaiting_claim", "claimed", "completed", "failed", "expired", "cancelled"]),
            "created_at": timestamp(),
            "updated_at": timestamp(),
            "expires_at": timestamp(),
            "claimed_at": nullable_timestamp(),
            "revision": {"type": "integer", "format": "int64", "minimum": 1}
        }),
    )
}

fn capture_secret_purpose() -> Value {
    string_enum(&[
        "provider_username",
        "provider_password",
        "provider_cookie",
        "provider_access_token",
        "provider_refresh_token",
        "provider_composite_session",
    ])
}

fn capture_scalar_source_schema() -> Value {
    json!({
        "oneOf": [
            object(&["type", "origin", "name"], json!({"type": {"const": "request_header"}, "origin": string(), "name": string()})),
            object(&["type", "origin", "key"], json!({"type": {"const": "local_storage"}, "origin": string(), "key": string()})),
            object(&["type", "origin", "key"], json!({"type": {"const": "session_storage"}, "origin": string(), "key": string()}))
        ]
    })
}

fn capture_value_source_schema() -> Value {
    json!({
        "oneOf": [
            object(&["type", "origin", "name"], json!({"type": {"const": "request_header"}, "origin": string(), "name": string()})),
            object(&["type", "origin", "key"], json!({"type": {"const": "local_storage"}, "origin": string(), "key": string()})),
            object(&["type", "origin", "key"], json!({"type": {"const": "session_storage"}, "origin": string(), "key": string()})),
            object(&["type", "origin"], json!({"type": {"const": "cookie_header"}, "origin": string()})),
            object(&["type", "fields"], json!({
                "type": {"const": "json_object"},
                "fields": {
                    "type": "array",
                    "minItems": 1,
                    "maxItems": 16,
                    "items": object(&["name", "sources"], json!({
                        "name": string(),
                        "sources": {"type": "array", "minItems": 1, "maxItems": 8, "items": capture_scalar_source_schema()}
                    }))
                }
            }))
        ]
    })
}

fn capture_readiness_schema() -> Value {
    json!({
        "oneOf": [
            object(&["type"], json!({"type": {"const": "outputs_complete"}})),
            object(
                &["type", "origin", "method", "path_and_query"],
                json!({
                    "type": {"const": "request_observed"},
                    "origin": {"type": "string", "format": "uri", "pattern": "^https://[^/]+$"},
                    "method": string_enum(&["GET", "POST"]),
                    "path_and_query": {"type": "string", "pattern": "^/", "maxLength": 2048}
                }),
            ),
            object(
                &["type", "origin", "method", "path_and_query", "status", "mime_type"],
                json!({
                    "type": {"const": "response_observed"},
                    "origin": {"type": "string", "format": "uri", "pattern": "^https://[^/]+$"},
                    "method": string_enum(&["GET", "POST"]),
                    "path_and_query": {"type": "string", "pattern": "^/", "maxLength": 2048},
                    "status": {"type": "integer", "minimum": 200, "maximum": 299},
                    "mime_type": {"type": "string", "minLength": 3, "maxLength": 128}
                }),
            )
        ]
    })
}

fn auth_bootstrap_event_kind_schema() -> Value {
    json!({
        "oneOf": [
            object(&["type"], json!({"type": {"const": "client_ready"}})),
            object(&["type", "stage"], json!({"type": {"const": "stage_changed"}, "stage": string()})),
            object(&["type", "stage", "percent"], json!({"type": {"const": "progress"}, "stage": string(), "percent": {"type": "integer", "minimum": 0, "maximum": 100}})),
            object(&["type"], json!({"type": {"const": "credential_detected"}})),
            object(&["type"], json!({"type": {"const": "validating"}})),
            object(&["type"], json!({"type": {"const": "authenticated"}})),
            object(&["type", "code"], json!({"type": {"const": "failed"}, "code": string()}))
        ]
    })
}

fn execution_stage() -> Value {
    string_enum(&[
        "preparing",
        "refreshing_remote_state",
        "authenticating",
        "scanning",
        "fetching_detail",
        "executing",
        "submitting",
        "reporting_duration",
        "verifying",
        "finalizing",
        "completed",
    ])
}

fn list_response(item_schema: &str) -> Value {
    object(
        &["total", "items"],
        json!({
            "total": unsigned_integer(),
            "items": {"type": "array", "items": schema_ref(item_schema)}
        }),
    )
}

fn page_response(item_schema: &str) -> Value {
    object(
        &["total", "limit", "offset", "items"],
        json!({
            "total": unsigned_integer(),
            "limit": unsigned_integer(),
            "offset": unsigned_integer(),
            "items": {"type": "array", "items": schema_ref(item_schema)}
        }),
    )
}

fn object(required: &[&str], properties: Value) -> Value {
    let mut schema = json!({
        "type": "object",
        "required": required,
        "properties": {},
        "additionalProperties": false
    });
    schema["properties"] = properties;
    schema
}

fn schema_ref(name: &str) -> Value {
    json!({"$ref": format!("#/components/schemas/{name}")})
}

fn nullable_schema_ref(name: &str) -> Value {
    json!({"oneOf": [schema_ref(name), {"type": "null"}]})
}

fn source_type() -> Value {
    string_enum(&[
        "chapter",
        "work",
        "exam",
        "resource",
        "practice",
        "discussion",
        "other",
    ])
}

fn assessment_class() -> Value {
    string_enum(&["routine", "unknown", "formal"])
}

fn remote_state() -> Value {
    string_enum(&[
        "unknown",
        "not_open",
        "pending",
        "in_progress",
        "completed",
        "expired",
        "removed",
    ])
}

fn task_capability() -> Value {
    string_enum(&[
        "progress_read",
        "resource_execution",
        "execution_verify",
        "question_inventory",
        "question_parse",
        "answer_resolve",
        "submission_build",
        "submission_execute",
        "submission_verify",
        "duration_read",
        "duration_report",
        "discussion",
        "practice",
        "browser_bridge",
    ])
}

fn string() -> Value {
    json!({"type": "string"})
}

fn nullable_string() -> Value {
    json!({"type": ["string", "null"]})
}

fn string_enum(values: &[&str]) -> Value {
    json!({"type": "string", "enum": values})
}

fn nullable_string_enum(values: &[&str]) -> Value {
    json!({"type": ["string", "null"], "enum": values.iter().copied().map(Some).chain(std::iter::once(None)).collect::<Vec<_>>()})
}

fn integer() -> Value {
    json!({"type": "integer", "format": "int64"})
}

fn unsigned_integer() -> Value {
    json!({"type": "integer", "format": "int64", "minimum": 0})
}

fn nullable_unsigned_integer() -> Value {
    json!({"type": ["integer", "null"], "format": "int64", "minimum": 0})
}

fn uuid() -> Value {
    json!({"type": "string", "format": "uuid"})
}

fn nullable_uuid() -> Value {
    json!({"type": ["string", "null"], "format": "uuid"})
}

fn timestamp() -> Value {
    json!({"type": "string", "format": "date-time"})
}

fn nullable_timestamp() -> Value {
    json!({"type": ["string", "null"], "format": "date-time"})
}

fn provider_id() -> Value {
    json!({"type": "string", "pattern": "^[a-z0-9-]{1,64}$"})
}
