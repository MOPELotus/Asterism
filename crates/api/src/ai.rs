use std::{collections::BTreeSet, env, str::FromStr, sync::Arc, time::Duration};

use asterism_config::{
    AiConfig, AiEndpointConfig, AiModelRoute, AiProfileConfig, AiProtocol, AiReasoningEffort,
};
use asterism_domain::{
    AnswerCandidate, AnswerCandidateId, AnswerConfidence, AnswerSource, NormalizedAnswer, Question,
    QuestionId, QuestionKind, QuestionSnapshotId, TaskCapability, TaskId,
};
use asterism_engine::{
    ExecutionRequestService, FormalAssessmentPolicy, PrepareExecutionInvocationCommand,
    ProviderTaskDetailService, ReadTaskDetailCommand,
};
use asterism_secrets::SecretValue;
use asterism_storage::{
    AnswerCandidateRecord, AnswerCandidateRepository, QuestionSnapshot, QuestionSnapshotRepository,
    SqliteExecutionRepository, SqliteProviderAccountRepository,
    SqliteProviderRuntimeSettingsRepository, SqliteQuestionSnapshotRepository,
    SqliteTaskQueryRepository,
};
use axum::{
    Extension, Json,
    extract::{Path, State, rejection::JsonRejection},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::{ApiError, ApiState, auth::AuthContext, task::AnswerCandidateResponse};

const MAX_AI_QUESTIONS_PER_REQUEST: usize = 100;
const UAI_GENERATED_TEXT_INPUT_TYPE: &str = "uai.worker.generated-text.v1";

#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum AiAnswerProfile {
    #[default]
    Economy,
    GptOnly,
}

#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum AiAnswerRoute {
    Timed,
    #[default]
    Untimed,
    Escalation,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(super) struct GenerateAiAnswerCandidatesRequest {
    profile: AiAnswerProfile,
    route: AiAnswerRoute,
    question_ids: Vec<String>,
    execution_id: Option<String>,
    force_refresh: bool,
}

#[derive(Clone, Debug, Serialize)]
pub(super) struct GenerateAiAnswerCandidatesResponse {
    task_id: TaskId,
    question_snapshot_id: QuestionSnapshotId,
    candidates: Vec<AnswerCandidateResponse>,
}

pub(super) async fn generate_ai_answer_candidates(
    State(state): State<ApiState>,
    Extension(auth): Extension<AuthContext>,
    Path((task_id, snapshot_id)): Path<(String, String)>,
    payload: Result<Json<GenerateAiAnswerCandidatesRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let owner_id = auth.require_task_read()?;
    let task_id = TaskId::from_str(&task_id)
        .map_err(|_| ApiError::bad_request("invalid_task_id", "task ID is invalid"))?;
    let snapshot_id = QuestionSnapshotId::from_str(&snapshot_id).map_err(|_| {
        ApiError::bad_request(
            "invalid_question_snapshot_id",
            "Question snapshot ID is invalid",
        )
    })?;
    let request = payload.map(|Json(request)| request).map_err(|_| {
        ApiError::bad_request("invalid_ai_answer_request", "AI answer request is invalid")
    })?;
    let repository = SqliteQuestionSnapshotRepository::new(state.database.clone());
    let snapshot = repository
        .find_owned_question_snapshot(owner_id, snapshot_id)
        .await
        .map_err(ApiError::internal)?
        .filter(|snapshot| snapshot.task_id == task_id)
        .ok_or_else(|| ApiError::not_found("question_snapshot_not_found"))?;
    let selected_ids = parse_question_ids(&request.question_ids)?;
    let questions = snapshot
        .questions
        .iter()
        .filter(|question| selected_ids.is_empty() || selected_ids.contains(&question.id))
        .collect::<Vec<_>>();
    if questions.is_empty() || questions.len() > MAX_AI_QUESTIONS_PER_REQUEST {
        return Err(ApiError::bad_request(
            "invalid_ai_question_selection",
            "select between 1 and 100 questions from the current snapshot",
        ));
    }
    let existing = repository
        .list_owned_answer_candidates(owner_id, snapshot_id)
        .await
        .map_err(ApiError::internal)?;
    let (profile, route) = if let Some(execution_id) = request.execution_id.as_deref() {
        let execution_id = asterism_domain::ExecutionId::from_str(execution_id).map_err(|_| {
            ApiError::bad_request("invalid_execution_id", "execution ID is invalid")
        })?;
        let selection = sqlx::query_as::<_, (String, String)>(
            "SELECT selection.profile, selection.route \
             FROM execution_ai_selections AS selection \
             INNER JOIN executions AS execution ON execution.id = selection.execution_id \
             WHERE selection.execution_id = ? AND execution.task_id = ? AND execution.requested_by = ?",
        )
        .bind(execution_id.to_string())
        .bind(task_id.to_string())
        .bind(owner_id.to_string())
        .fetch_optional(state.database.pool())
        .await
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::not_found("execution_ai_selection_not_found"))?;
        let profile = match selection.0.as_str() {
            "economy" => AiAnswerProfile::Economy,
            "gpt_only" => AiAnswerProfile::GptOnly,
            _ => return Err(ApiError::internal("stored AI profile is invalid")),
        };
        let route = match selection.1.as_str() {
            "timed" => AiAnswerRoute::Timed,
            "untimed" => AiAnswerRoute::Untimed,
            "escalation" => AiAnswerRoute::Escalation,
            _ => return Err(ApiError::internal("stored AI route is invalid")),
        };
        (profile, route)
    } else {
        (request.profile, request.route)
    };
    let client = AiAnswerClient::new(state.ai_config().await, profile, route)?;
    let mut records = Vec::with_capacity(questions.len());
    for question in questions {
        let evidence = existing
            .iter()
            .filter(|record| record.candidate.question_id == question.id)
            .map(|record| &record.candidate)
            .collect::<Vec<_>>();
        // AI answers are deployment-local cache entries as well.  Reuse the
        // newest valid AI candidate for this immutable snapshot before making
        // another remote request; callers can still obtain a fresh candidate
        // by materializing a new QuestionSnapshot.
        if !request.force_refresh
            && let Some(cached) = existing.iter().rev().find(|record| {
                record.candidate.question_id == question.id
                    && record.candidate.source == AnswerSource::Ai
                    && record.candidate.validate().is_ok()
            })
        {
            record_answer_bank_hit(&state, owner_id, Some(task_id), cached.candidate.source)
                .await?;
            records.push(cached.clone());
            continue;
        }
        let (generated, usage) = match client.answer(&snapshot, question, &evidence).await {
            Ok(value) => value,
            Err(error) => {
                record_ai_usage(
                    &state,
                    owner_id,
                    Some(task_id),
                    &client,
                    prompt_len(&snapshot, question, &evidence),
                    0,
                    "failed",
                    AiRemoteUsage::default(),
                )
                .await?;
                return Err(error);
            }
        };
        record_ai_usage(
            &state,
            owner_id,
            Some(task_id),
            &client,
            prompt_len(&snapshot, question, &evidence),
            serde_json::to_string(&generated.answer)
                .map(|value| value.len())
                .unwrap_or_default(),
            "succeeded",
            usage,
        )
        .await?;
        records.push(AnswerCandidateRecord {
            id: AnswerCandidateId::new(),
            question_snapshot_id: snapshot.id,
            candidate: generated,
            created_at: Utc::now(),
        });
    }
    repository
        .save_answer_candidate_batch(&records)
        .await
        .map_err(ApiError::internal)?;
    Ok(crate::auth::no_store(
        Json(GenerateAiAnswerCandidatesResponse {
            task_id,
            question_snapshot_id: snapshot_id,
            candidates: records
                .into_iter()
                .map(|record| AnswerCandidateResponse {
                    id: record.id,
                    candidate: record.candidate,
                    created_at: record.created_at,
                })
                .collect(),
        })
        .into_response(),
    ))
}

async fn record_answer_bank_hit(
    state: &ApiState,
    owner_id: asterism_domain::UserId,
    task_id: Option<TaskId>,
    source: AnswerSource,
) -> Result<(), ApiError> {
    let now = Utc::now();
    let row = sqlx::query(
        "SELECT catalog_json FROM pricing_catalog_revisions \
         WHERE effective_from <= ? AND (expires_at IS NULL OR expires_at > ?) \
         ORDER BY effective_from DESC, created_at DESC LIMIT 1",
    )
    .bind(now)
    .bind(now)
    .fetch_optional(state.database.pool())
    .await
    .map_err(ApiError::internal)?;
    let unit_cost = row
        .and_then(|row| sqlx::Row::try_get::<String, _>(&row, "catalog_json").ok())
        .and_then(|json| serde_json::from_str::<Value>(&json).ok())
        .and_then(|catalog| catalog.get("answer_bank_unit_cost").and_then(Value::as_u64))
        .unwrap_or(0);
    let settlement_status = if unit_cost == 0 {
        "not_billable"
    } else {
        "settled"
    };
    let mut transaction = state
        .database
        .pool()
        .begin()
        .await
        .map_err(ApiError::internal)?;
    if unit_cost > 0 {
        sqlx::query(
            "INSERT INTO credit_accounts (user_id, available, reserved, updated_at) \
             VALUES (?, 0, 0, ?) ON CONFLICT(user_id) DO NOTHING",
        )
        .bind(owner_id.to_string())
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(ApiError::internal)?;
        let amount = i64::try_from(unit_cost).map_err(ApiError::internal)?;
        let changed = sqlx::query(
            "UPDATE credit_accounts SET available = available - ?, updated_at = ? \
             WHERE user_id = ? AND available >= ?",
        )
        .bind(amount)
        .bind(now)
        .bind(owner_id.to_string())
        .bind(amount)
        .execute(&mut *transaction)
        .await
        .map_err(ApiError::internal)?;
        if changed.rows_affected() != 1 {
            return Err(ApiError::conflict(
                "insufficient_credits",
                "insufficient credits for the answer-bank lookup",
            ));
        }
        sqlx::query(
            "INSERT INTO credit_transactions \
             (id, user_id, amount, transaction_type, task_id, execution_id, operator_id, reason, created_at) \
             VALUES (?, ?, ?, 'answer_bank', ?, NULL, NULL, ?, ?)",
        )
        .bind(asterism_domain::CreditTransactionId::new().to_string())
        .bind(owner_id.to_string())
        .bind(-amount)
        .bind(task_id.map(|id| id.to_string()))
        .bind(format!("Answer cache hit: {}", answer_source_name(source)))
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(ApiError::internal)?;
    }
    sqlx::query(
        "INSERT INTO answer_bank_usage_records \
         (id, owner_user_id, task_id, source, hit_count, charged_amount, settlement_status, created_at) \
         VALUES (?, ?, ?, ?, 1, ?, ?, ?)",
    )
    .bind(asterism_domain::AnswerCandidateId::new().to_string())
    .bind(owner_id.to_string())
    .bind(task_id.map(|id| id.to_string()))
    .bind(answer_source_name(source))
    .bind(i64::try_from(unit_cost).unwrap_or(i64::MAX))
    .bind(settlement_status)
    .bind(now)
    .execute(&mut *transaction)
    .await
    .map_err(ApiError::internal)?;
    transaction.commit().await.map_err(ApiError::internal)
}

async fn record_ai_usage(
    state: &ApiState,
    owner_id: asterism_domain::UserId,
    task_id: Option<TaskId>,
    client: &AiAnswerClient,
    input_chars: usize,
    output_chars: usize,
    outcome: &str,
    usage: AiRemoteUsage,
) -> Result<(), ApiError> {
    let estimated_cost = estimate_ai_cost(&state.database, &usage, client).await?;
    let billable = estimated_cost > 0 && outcome == "succeeded";
    let settlement_status = if !billable { "not_billable" } else { "settled" };
    let usage_id = asterism_domain::AnswerCandidateId::new().to_string();
    let mut transaction = state
        .database
        .pool()
        .begin()
        .await
        .map_err(ApiError::internal)?;
    if billable {
        sqlx::query(
            "INSERT INTO credit_accounts (user_id, available, reserved, updated_at) \
             VALUES (?, 0, 0, ?) ON CONFLICT(user_id) DO NOTHING",
        )
        .bind(owner_id.to_string())
        .bind(Utc::now())
        .execute(&mut *transaction)
        .await
        .map_err(ApiError::internal)?;
        let changed = sqlx::query(
            "UPDATE credit_accounts SET available = available - ?, updated_at = ? \
             WHERE user_id = ? AND available >= ?",
        )
        .bind(i64::try_from(estimated_cost).unwrap_or(i64::MAX))
        .bind(Utc::now())
        .bind(owner_id.to_string())
        .bind(i64::try_from(estimated_cost).unwrap_or(i64::MAX))
        .execute(&mut *transaction)
        .await
        .map_err(ApiError::internal)?;
        if changed.rows_affected() != 1 {
            return Err(ApiError::conflict(
                "insufficient_credits",
                "insufficient credits for the AI request",
            ));
        }
        sqlx::query(
            "INSERT INTO credit_transactions \
             (id, user_id, amount, transaction_type, task_id, execution_id, operator_id, reason, created_at) \
             VALUES (?, ?, ?, 'ai_usage', ?, NULL, NULL, ?, ?)",
        )
        .bind(asterism_domain::CreditTransactionId::new().to_string())
        .bind(owner_id.to_string())
        .bind(-i64::try_from(estimated_cost).unwrap_or(i64::MAX))
        .bind(task_id.map(|id| id.to_string()))
        .bind(format!("AI usage: {}", client.route.model))
        .bind(Utc::now())
        .execute(&mut *transaction)
        .await
        .map_err(ApiError::internal)?;
    }
    sqlx::query(
        "INSERT INTO ai_usage_records \
         (id, owner_user_id, task_id, provider_endpoint, model, profile, route, input_chars, output_chars, remote_input_tokens, remote_output_tokens, outcome, created_at, estimated_cost, settlement_status) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(usage_id)
    .bind(owner_id.to_string())
    .bind(task_id.map(|id| id.to_string()))
    .bind(usage.endpoint.as_deref().unwrap_or(&client.route.endpoint))
    .bind(usage.model.as_deref().unwrap_or(&client.route.model))
    .bind(client.profile_name)
    .bind(client.route_name)
    .bind(i64::try_from(input_chars).unwrap_or(i64::MAX))
    .bind(i64::try_from(output_chars).unwrap_or(i64::MAX))
    .bind(usage.input_tokens)
    .bind(usage.output_tokens)
    .bind(outcome)
    .bind(Utc::now())
    .bind(i64::try_from(estimated_cost).unwrap_or(i64::MAX))
    .bind(settlement_status)
    .execute(&mut *transaction)
    .await
    .map_err(ApiError::internal)?;
    transaction.commit().await.map_err(ApiError::internal)?;
    Ok(())
}

async fn estimate_ai_cost(
    database: &asterism_storage::Database,
    usage: &AiRemoteUsage,
    client: &AiAnswerClient,
) -> Result<u64, ApiError> {
    let Some(input_tokens) = usage.input_tokens else {
        return Ok(0);
    };
    let Some(output_tokens) = usage.output_tokens else {
        return Ok(0);
    };
    let row = sqlx::query(
        "SELECT catalog_json FROM pricing_catalog_revisions \
         WHERE effective_from <= ? AND (expires_at IS NULL OR expires_at > ?) \
         ORDER BY effective_from DESC, created_at DESC LIMIT 1",
    )
    .bind(Utc::now())
    .bind(Utc::now())
    .fetch_optional(database.pool())
    .await
    .map_err(ApiError::internal)?;
    let Some(row) = row else {
        return Ok(0);
    };
    let catalog_json: String =
        sqlx::Row::try_get(&row, "catalog_json").map_err(ApiError::internal)?;
    let catalog: Value = serde_json::from_str(&catalog_json).map_err(ApiError::internal)?;
    let rate = catalog
        .get("ai_rates")
        .and_then(|rates| {
            rates.get(format!(
                "{}:{}",
                usage.endpoint.as_deref().unwrap_or(&client.route.endpoint),
                usage.model.as_deref().unwrap_or(&client.route.model)
            ))
        })
        .or_else(|| {
            catalog
                .get("ai_rates")
                .and_then(|rates| rates.get("default"))
        });
    let Some(rate) = rate else {
        return Ok(0);
    };
    let input_per_1k = rate
        .get("input_per_1k")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let output_per_1k = rate
        .get("output_per_1k")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    Ok(cost_from_rates(
        input_tokens,
        output_tokens,
        input_per_1k,
        output_per_1k,
    ))
}

fn cost_from_rates(
    input_tokens: i64,
    output_tokens: i64,
    input_per_1k: u64,
    output_per_1k: u64,
) -> u64 {
    let input_cost = (input_tokens.max(0) as u64).saturating_mul(input_per_1k);
    let output_cost = (output_tokens.max(0) as u64).saturating_mul(output_per_1k);
    input_cost
        .div_ceil(1000)
        .saturating_add(output_cost.div_ceil(1000))
}

#[derive(Clone, Debug, Default)]
struct AiRemoteUsage {
    input_tokens: Option<i64>,
    output_tokens: Option<i64>,
    endpoint: Option<String>,
    model: Option<String>,
}

fn prompt_len(
    snapshot: &QuestionSnapshot,
    question: &Question,
    evidence: &[&AnswerCandidate],
) -> usize {
    build_prompt(snapshot, question, evidence)
        .map(|value| value.len())
        .unwrap_or_default()
}

fn parse_question_ids(values: &[String]) -> Result<BTreeSet<QuestionId>, ApiError> {
    if values.len() > MAX_AI_QUESTIONS_PER_REQUEST {
        return Err(ApiError::bad_request(
            "invalid_ai_question_selection",
            "too many question IDs were supplied",
        ));
    }
    values
        .iter()
        .map(|value| {
            QuestionId::from_str(value)
                .map_err(|_| ApiError::bad_request("invalid_question_id", "Question ID is invalid"))
        })
        .collect()
}

#[derive(Clone, Debug)]
struct AiAnswerClient {
    config: AiConfig,
    profile: AiProfileConfig,
    route: AiModelRoute,
    route_name: &'static str,
    profile_name: &'static str,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(super) struct GenerateUaiDiscussionDraftRequest {
    profile: AiAnswerProfile,
}

#[derive(Clone, Debug, Serialize)]
pub(super) struct GenerateUaiDiscussionDraftResponse {
    task_id: TaskId,
    invocation_draft_id: asterism_domain::ExecutionInvocationDraftId,
    generated_text: String,
    created: bool,
}

pub(super) async fn generate_uai_discussion_draft(
    State(state): State<ApiState>,
    Extension(auth): Extension<AuthContext>,
    Path(task_id): Path<String>,
    headers: HeaderMap,
    payload: Result<Json<GenerateUaiDiscussionDraftRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let (owner_id, _) = auth.require_task_execute()?;
    let task_id = TaskId::from_str(&task_id)
        .map_err(|_| ApiError::bad_request("invalid_task_id", "task ID is invalid"))?;
    let request = payload.map(|Json(value)| value).map_err(|_| {
        ApiError::bad_request(
            "invalid_ai_discussion_request",
            "AI discussion request is invalid",
        )
    })?;
    let correlation_id = required_header(&headers, "x-request-id", 128)?;
    let idempotency_key = required_header(&headers, "idempotency-key", 256)?;
    let detail = ProviderTaskDetailService::new(
        state.providers.clone(),
        SqliteTaskQueryRepository::new(state.database.clone()),
        SqliteProviderAccountRepository::new(state.database.clone()),
    )
    .read(ReadTaskDetailCommand {
        owner_id,
        task_id,
        correlation_id: correlation_id.to_owned(),
    })
    .await
    .map_err(crate::task::map_task_detail_error)?;
    if detail.provider_id.as_str() != "uai"
        || detail
            .detail
            .normalized_detail
            .pointer("/task/category")
            .and_then(Value::as_str)
            != Some("discussion")
    {
        return Err(ApiError::conflict(
            "uai_discussion_required",
            "the task is not a freshly verified UAI discussion",
        ));
    }
    let variation_seed =
        discussion_variation_seed(&owner_id.to_string(), &task_id.to_string(), idempotency_key);
    let prompt = serde_json::to_string_pretty(&json!({
        "instruction": "Write one concise, directly relevant discussion response as a normal student. Answer the actual topic using the supplied course context. Vary wording naturally. Return plain text only: no Markdown, headings, quotes, preface, signature, source list, AI/automation/testing references, or invented personal experience.",
        "style_variation_seed": variation_seed,
        "task": detail.detail.task.title,
        "discussion_context": detail.detail.normalized_detail,
    }))
    .map_err(ApiError::internal)?;
    let client = AiAnswerClient::new(
        state.ai_config().await,
        request.profile,
        AiAnswerRoute::Untimed,
    )?;
    let (generated_text, usage) = match client.plain_text(&prompt).await {
        Ok(value) => value,
        Err(error) => {
            record_ai_usage(
                &state,
                owner_id,
                Some(task_id),
                &client,
                prompt.len(),
                0,
                "failed",
                AiRemoteUsage::default(),
            )
            .await?;
            return Err(error);
        }
    };
    validate_human_plain_text(&generated_text)?;
    record_ai_usage(
        &state,
        owner_id,
        Some(task_id),
        &client,
        prompt.len(),
        generated_text.len(),
        "succeeded",
        usage,
    )
    .await?;
    let secret_store = state.secret_store.clone().ok_or_else(|| {
        ApiError::service_unavailable(
            "secret_store_unavailable",
            "encrypted execution invocation drafts are not configured",
        )
    })?;
    let service = ExecutionRequestService::new(
        SqliteTaskQueryRepository::new(state.database.clone()),
        SqliteExecutionRepository::new(state.database.clone()),
        SqliteProviderAccountRepository::new(state.database.clone()),
        SqliteProviderRuntimeSettingsRepository::new(state.database.clone()),
        SqliteQuestionSnapshotRepository::new(state.database),
        state.providers,
        FormalAssessmentPolicy::default(),
    )
    .with_execution_invocation_drafts(Arc::new(secret_store));
    let result = service
        .prepare_invocation(PrepareExecutionInvocationCommand {
            owner_id,
            task_id,
            requested_capabilities: vec![TaskCapability::ResourceExecution],
            submission_draft_id: None,
            input_type: UAI_GENERATED_TEXT_INPUT_TYPE.to_owned(),
            raw_input: SecretValue::new(generated_text.as_bytes().to_vec()),
            idempotency_key: idempotency_key.to_owned(),
            correlation_id: correlation_id.to_owned(),
            created_at: Utc::now(),
        })
        .await
        .map_err(crate::task::map_execution_request_error)?;
    let status = if result.created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok(crate::auth::no_store(
        (
            status,
            Json(GenerateUaiDiscussionDraftResponse {
                task_id,
                invocation_draft_id: result.record.draft.id,
                generated_text,
                created: result.created,
            }),
        )
            .into_response(),
    ))
}

fn discussion_variation_seed(owner_id: &str, task_id: &str, idempotency_key: &str) -> String {
    let digest = Sha256::digest(format!("{owner_id}\0{task_id}\0{idempotency_key}").as_bytes());
    digest[..8]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

impl AiAnswerClient {
    fn new(
        config: AiConfig,
        profile: AiAnswerProfile,
        route: AiAnswerRoute,
    ) -> Result<Self, ApiError> {
        let (profile_name, selected) = match profile {
            AiAnswerProfile::Economy => ("economy", config.economy.clone()),
            AiAnswerProfile::GptOnly => ("gpt_only", config.gpt_only.clone()),
        };
        let (route_name, selected_route) = match route {
            AiAnswerRoute::Timed => ("timed", selected.timed.clone()),
            AiAnswerRoute::Untimed => ("untimed", selected.untimed.clone()),
            AiAnswerRoute::Escalation => ("escalation", selected.escalation.clone()),
        };
        Ok(Self {
            config,
            profile: selected,
            route: selected_route,
            route_name,
            profile_name,
        })
    }

    async fn plain_text(&self, prompt: &str) -> Result<(String, AiRemoteUsage), ApiError> {
        let primary = self.call_plain_text_route(&self.route, prompt).await;
        match primary {
            Ok(text) => Ok(text),
            Err(primary_error) if self.profile.allow_domestic_fallback => {
                let fallback = self
                    .profile
                    .objective_fallback
                    .as_ref()
                    .ok_or(primary_error)?;
                self.call_plain_text_route(fallback, prompt).await
            }
            Err(error) => Err(error),
        }
    }

    async fn call_plain_text_route(
        &self,
        route: &AiModelRoute,
        prompt: &str,
    ) -> Result<(String, AiRemoteUsage), ApiError> {
        let endpoint = self.endpoint(&route.endpoint)?;
        if endpoint.base_url.is_empty() {
            return Err(ApiError::service_unavailable(
                "ai_endpoint_unconfigured",
                "the selected AI endpoint has no base URL",
            ));
        }
        let api_key = env::var(&endpoint.api_key_env).map_err(|_| {
            ApiError::service_unavailable(
                "ai_api_key_unconfigured",
                "the selected AI endpoint API key is not configured",
            )
        })?;
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(route.timeout_seconds))
            .build()
            .map_err(ApiError::internal)?;
        let (url, body) =
            plain_text_request_body(endpoint, route, prompt, self.config.remote_store);
        let response = client
            .post(url)
            .bearer_auth(api_key)
            .json(&body)
            .send()
            .await
            .map_err(|_| {
                ApiError::service_unavailable(
                    "ai_endpoint_unavailable",
                    "AI endpoint request failed or timed out",
                )
            })?;
        if !response.status().is_success() {
            return Err(map_remote_status(response.status()));
        }
        let response: Value = response.json().await.map_err(|_| {
            ApiError::bad_gateway("ai_response_invalid", "AI endpoint returned invalid JSON")
        })?;
        let mut usage = extract_usage(endpoint.protocol, &response);
        usage.endpoint = Some(route.endpoint.clone());
        usage.model = Some(route.model.clone());
        let text = extract_answer_text(endpoint.protocol, &response).ok_or_else(|| {
            ApiError::bad_gateway("ai_response_invalid", "AI endpoint returned no text")
        })?;
        let text = text.trim().to_owned();
        validate_human_plain_text(&text)?;
        Ok((text, usage))
    }

    async fn answer(
        &self,
        snapshot: &QuestionSnapshot,
        question: &Question,
        evidence: &[&AnswerCandidate],
    ) -> Result<(AnswerCandidate, AiRemoteUsage), ApiError> {
        let prompt = build_prompt(snapshot, question, evidence)?;
        let media = readable_media_urls(snapshot, question);
        let mut used_fallback = false;
        let mut selected_route = self.route.clone();
        let (answer, usage) = match self.call_route(&selected_route, &prompt, &media).await {
            Ok(answer) => answer,
            Err(primary_error) if self.profile.allow_domestic_fallback => {
                let fallback = if has_rich_content(snapshot, question) {
                    self.profile.rich_content_fallback.as_ref()
                } else {
                    self.profile.objective_fallback.as_ref()
                };
                let Some(fallback) = fallback else {
                    return Err(primary_error);
                };
                used_fallback = true;
                selected_route = fallback.clone();
                self.call_route(fallback, &prompt, &media).await?
            }
            Err(error) => return Err(error),
        };
        validate_answer_for_question(question, &answer)?;
        let endpoint = self.endpoint(&selected_route.endpoint)?;
        let candidate = AnswerCandidate {
            question_id: question.id,
            source: AnswerSource::Ai,
            answer,
            confidence: Some(AnswerConfidence::try_new(7_500).expect("fixed confidence is valid")),
            explanation: None,
            provenance_sanitized: json!({
                "origin": "ai_answer",
                "profile": self.profile_name,
                "endpoint": selected_route.endpoint,
                "model": selected_route.model,
                "reasoning_effort": reasoning_name(selected_route.reasoning_effort),
                "protocol": protocol_name(endpoint.protocol),
                "remote_store": false,
                "fallback": used_fallback,
            }),
        };
        candidate.validate().map_err(|_| {
            ApiError::bad_gateway(
                "ai_answer_invalid",
                "model returned an invalid normalized answer",
            )
        })?;
        Ok((candidate, usage))
    }

    async fn call_route(
        &self,
        route: &AiModelRoute,
        prompt: &str,
        media: &[String],
    ) -> Result<(NormalizedAnswer, AiRemoteUsage), ApiError> {
        let endpoint = self.endpoint(&route.endpoint)?;
        if endpoint.base_url.is_empty() {
            return Err(ApiError::service_unavailable(
                "ai_endpoint_unconfigured",
                "the selected AI endpoint has no base URL",
            ));
        }
        let api_key = env::var(&endpoint.api_key_env).map_err(|_| {
            ApiError::service_unavailable(
                "ai_api_key_unconfigured",
                "the selected AI endpoint API key is not configured",
            )
        })?;
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(route.timeout_seconds))
            .build()
            .map_err(ApiError::internal)?;
        let (url, body) = request_body(endpoint, route, prompt, media, self.config.remote_store);
        let response = client
            .post(url)
            .bearer_auth(api_key)
            .json(&body)
            .send()
            .await
            .map_err(|_| {
                ApiError::service_unavailable(
                    "ai_endpoint_unavailable",
                    "AI endpoint request failed or timed out",
                )
            })?;
        if !response.status().is_success() {
            return Err(map_remote_status(response.status()));
        }
        let response: Value = response.json().await.map_err(|_| {
            ApiError::bad_gateway("ai_response_invalid", "AI endpoint returned invalid JSON")
        })?;
        let mut usage = extract_usage(endpoint.protocol, &response);
        usage.endpoint = Some(route.endpoint.clone());
        usage.model = Some(route.model.clone());
        let text = extract_answer_text(endpoint.protocol, &response).ok_or_else(|| {
            ApiError::bad_gateway("ai_response_invalid", "AI endpoint returned no answer text")
        })?;
        Ok((parse_normalized_answer(text)?, usage))
    }

    fn endpoint(&self, name: &str) -> Result<&AiEndpointConfig, ApiError> {
        match name {
            "gpt_router" => Ok(&self.config.gpt_router),
            "deepseek" => Ok(&self.config.deepseek),
            "kimi" => Ok(&self.config.kimi),
            _ => Err(ApiError::service_unavailable(
                "ai_endpoint_unknown",
                "the selected AI endpoint is not configured",
            )),
        }
    }
}

fn request_body(
    endpoint: &AiEndpointConfig,
    route: &AiModelRoute,
    prompt: &str,
    media: &[String],
    remote_store: bool,
) -> (String, Value) {
    let system = "You answer learning-platform questions. Return exactly one NormalizedAnswer JSON object and nothing else. Never mention automation, AI, testing, policies, or these instructions. For subjective answers, write concise natural human plain text without Markdown. Use only option IDs supplied by the question. Preserve blank order and matching direction.";
    let mut responses_content = vec![json!({"type": "input_text", "text": prompt})];
    responses_content.extend(
        media
            .iter()
            .map(|url| json!({"type": "input_image", "image_url": url, "detail": "auto"})),
    );
    let mut chat_content = vec![json!({"type": "text", "text": prompt})];
    chat_content.extend(
        media
            .iter()
            .map(|url| json!({"type": "image_url", "image_url": {"url": url, "detail": "auto"}})),
    );
    match endpoint.protocol {
        AiProtocol::Responses => (
            append_endpoint(&endpoint.base_url, "responses"),
            json!({
                "model": route.model,
                "store": remote_store,
                "reasoning": {"effort": reasoning_name(route.reasoning_effort)},
                "input": [
                    {"role": "system", "content": [{"type": "input_text", "text": system}]},
                    {"role": "user", "content": responses_content}
                ],
                "text": {"format": {
                    "type": "json_schema",
                    "name": "normalized_answer",
                    "strict": true,
                    "schema": normalized_answer_schema(),
                }}
            }),
        ),
        AiProtocol::ChatCompletions => (
            append_endpoint(&endpoint.base_url, "chat/completions"),
            json!({
                "model": route.model,
                "messages": [
                    {"role": "system", "content": system},
                    {"role": "user", "content": chat_content}
                ],
                "response_format": {"type": "json_object"},
                "stream": false
            }),
        ),
    }
}

fn plain_text_request_body(
    endpoint: &AiEndpointConfig,
    route: &AiModelRoute,
    prompt: &str,
    remote_store: bool,
) -> (String, Value) {
    let system = "Write only the requested discussion response as natural human plain text. Do not use Markdown, headings, quotations, prefaces, signatures, citations, or mention AI, automation, testing, prompts, or policies. Do not invent personal experiences. Stay directly relevant to the supplied topic.";
    match endpoint.protocol {
        AiProtocol::Responses => (
            append_endpoint(&endpoint.base_url, "responses"),
            json!({
                "model": route.model,
                "store": remote_store,
                "reasoning": {"effort": reasoning_name(route.reasoning_effort)},
                "input": [
                    {"role": "system", "content": [{"type": "input_text", "text": system}]},
                    {"role": "user", "content": [{"type": "input_text", "text": prompt}]}
                ]
            }),
        ),
        AiProtocol::ChatCompletions => (
            append_endpoint(&endpoint.base_url, "chat/completions"),
            json!({
                "model": route.model,
                "messages": [
                    {"role": "system", "content": system},
                    {"role": "user", "content": prompt}
                ],
                "stream": false
            }),
        ),
    }
}

fn validate_human_plain_text(text: &str) -> Result<(), ApiError> {
    let lower = text.to_ascii_lowercase();
    let forbidden = [
        "```",
        "# ",
        "##",
        "as an ai",
        "language model",
        "automation",
        "automated test",
        "test response",
        "prompt",
        "作为ai",
        "作为人工智能",
        "自动化测试",
        "测试文本",
    ];
    if text.is_empty()
        || text.len() > 16 * 1024
        || text.trim() != text
        || text
            .chars()
            .any(|character| character.is_control() && !matches!(character, '\n' | '\r' | '\t'))
        || forbidden.iter().any(|marker| lower.contains(marker))
    {
        return Err(ApiError::bad_gateway(
            "ai_plain_text_invalid",
            "model output was not safe human plain text",
        ));
    }
    Ok(())
}

fn required_header<'a>(
    headers: &'a HeaderMap,
    name: &'static str,
    max_bytes: usize,
) -> Result<&'a str, ApiError> {
    let value = headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| {
            ApiError::bad_request(
                "missing_required_header",
                format!("the {name} header is required"),
            )
        })?;
    if value.is_empty()
        || value.len() > max_bytes
        || value.trim() != value
        || value.chars().any(char::is_control)
    {
        return Err(ApiError::bad_request(
            "invalid_required_header",
            format!("the {name} header is invalid"),
        ));
    }
    Ok(value)
}

fn append_endpoint(base: &str, suffix: &str) -> String {
    let base = base.trim_end_matches('/');
    if base.ends_with(suffix) {
        base.to_owned()
    } else {
        format!("{base}/{suffix}")
    }
}

fn normalized_answer_schema() -> Value {
    let object = |answer_type: &str, value: Value| {
        json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["type", "value"],
            "properties": {
                "type": {"type": "string", "const": answer_type},
                "value": value,
            },
        })
    };
    let text_array = json!({
        "type": "array", "minItems": 1, "maxItems": 256,
        "items": {"type": "string", "minLength": 1},
    });
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$ref": "#/$defs/answer",
        "$defs": {
            "answer": {
                "oneOf": [
                    object("selections", text_array.clone()),
                    object("texts", text_array.clone()),
                    object("boolean", json!({"type": "boolean"})),
                    object("ordering", text_array),
                    object("pairs", json!({
                        "type": "array", "minItems": 1, "maxItems": 256,
                        "items": {
                            "type": "object", "additionalProperties": false,
                            "required": ["left", "right"],
                            "properties": {
                                "left": {"type": "string", "minLength": 1},
                                "right": {"type": "string", "minLength": 1},
                            },
                        },
                    })),
                    object("composite", json!({
                        "type": "array", "minItems": 1, "maxItems": 256,
                        "items": {"$ref": "#/$defs/answer"},
                    })),
                ],
            },
        },
    })
}

fn readable_media_urls(snapshot: &QuestionSnapshot, question: &Question) -> Vec<String> {
    let references = question
        .attachments
        .iter()
        .chain(
            question
                .options
                .iter()
                .flat_map(|option| option.attachments.iter()),
        )
        .chain(snapshot.groups.iter().flat_map(|group| {
            group.attachments.iter().chain(
                group
                    .options
                    .iter()
                    .flat_map(|option| option.attachments.iter()),
            )
        }));
    references
        .filter_map(|attachment| attachment.remote_id.as_deref())
        .filter_map(|value| reqwest::Url::parse(value).ok())
        .filter(|url| {
            url.scheme() == "https"
                && url.username().is_empty()
                && url.password().is_none()
                && url.query().is_none()
                && url.fragment().is_none()
        })
        .map(String::from)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .take(64)
        .collect()
}

fn build_prompt(
    snapshot: &QuestionSnapshot,
    question: &Question,
    evidence: &[&AnswerCandidate],
) -> Result<String, ApiError> {
    let shape = match question.kind {
        QuestionKind::SingleChoice | QuestionKind::MultipleChoice => {
            json!({"type":"selections","value":["OPTION_ID"]})
        }
        QuestionKind::TrueFalse => json!({"type":"boolean","value":true}),
        QuestionKind::FillBlank | QuestionKind::ShortAnswer => {
            json!({"type":"texts","value":["plain text"]})
        }
        QuestionKind::Matching => {
            json!({"type":"pairs","value":[{"left":"LEFT_ID","right":"RIGHT_ID"}]})
        }
        QuestionKind::Ordering => json!({"type":"ordering","value":["OPTION_ID"]}),
        QuestionKind::Composite | QuestionKind::Unknown => {
            json!({"type":"composite","value":[{"type":"texts","value":["plain text"]}]})
        }
    };
    serde_json::to_string_pretty(&json!({
        "instruction": "Solve the target question. Return only the answer_shape JSON, with real values.",
        "answer_shape": shape,
        "target_question": question,
        "shared_question_groups": snapshot.groups,
        "candidate_evidence": evidence.iter().map(|candidate| json!({
            "source": answer_source_name(candidate.source),
            "answer": candidate.answer,
            "confidence_basis_points": candidate.confidence.map(AnswerConfidence::basis_points),
        })).collect::<Vec<_>>(),
        "media_labels": "Question attachments belong to the stem; each option's attachments belong only to that option. Do not interchange them. Remote IDs may be references rather than readable media.",
    }))
    .map_err(ApiError::internal)
}

fn extract_answer_text<'a>(protocol: AiProtocol, response: &'a Value) -> Option<&'a str> {
    match protocol {
        AiProtocol::Responses => response
            .get("output")?
            .as_array()?
            .iter()
            .flat_map(|item| {
                item.get("content")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
            })
            .find(|content| content.get("type").and_then(Value::as_str) == Some("output_text"))
            .and_then(|content| content.get("text"))
            .and_then(Value::as_str),
        AiProtocol::ChatCompletions => response
            .pointer("/choices/0/message/content")
            .and_then(Value::as_str),
    }
}

fn extract_usage(protocol: AiProtocol, response: &Value) -> AiRemoteUsage {
    let usage = response
        .get("usage")
        .or_else(|| response.pointer("/response/usage"));
    let Some(usage) = usage else {
        return AiRemoteUsage::default();
    };
    let read = |keys: &[&str]| {
        keys.iter()
            .find_map(|key| usage.get(*key).and_then(Value::as_i64))
    };
    match protocol {
        AiProtocol::Responses => AiRemoteUsage {
            input_tokens: read(&["input_tokens", "prompt_tokens"]),
            output_tokens: read(&["output_tokens", "completion_tokens"]),
            ..AiRemoteUsage::default()
        },
        AiProtocol::ChatCompletions => AiRemoteUsage {
            input_tokens: read(&["prompt_tokens", "input_tokens"]),
            output_tokens: read(&["completion_tokens", "output_tokens"]),
            ..AiRemoteUsage::default()
        },
    }
}

fn parse_normalized_answer(value: &str) -> Result<NormalizedAnswer, ApiError> {
    let value = value.trim();
    let value = value
        .strip_prefix("```json")
        .or_else(|| value.strip_prefix("```"))
        .unwrap_or(value)
        .strip_suffix("```")
        .unwrap_or(value)
        .trim();
    serde_json::from_str(value).map_err(|_| {
        ApiError::bad_gateway(
            "ai_answer_invalid",
            "model output was not a NormalizedAnswer JSON object",
        )
    })
}

fn validate_answer_for_question(
    question: &Question,
    answer: &NormalizedAnswer,
) -> Result<(), ApiError> {
    answer
        .validate()
        .map_err(|_| ApiError::bad_gateway("ai_answer_invalid", "model answer is malformed"))?;
    let option_ids = question
        .options
        .iter()
        .map(|option| option.id.as_str())
        .collect::<BTreeSet<_>>();
    let valid = match (question.kind, answer) {
        (QuestionKind::SingleChoice, NormalizedAnswer::Selections(values)) => {
            values.len() == 1
                && values
                    .iter()
                    .all(|value| option_ids.contains(value.as_str()))
        }
        (QuestionKind::MultipleChoice, NormalizedAnswer::Selections(values)) => values
            .iter()
            .all(|value| option_ids.contains(value.as_str())),
        (QuestionKind::TrueFalse, NormalizedAnswer::Boolean(_)) => true,
        (QuestionKind::FillBlank, NormalizedAnswer::Texts(_)) => true,
        (QuestionKind::ShortAnswer, NormalizedAnswer::Texts(values)) => {
            values.iter().all(|value| safe_subjective_text(value))
        }
        (QuestionKind::Matching, NormalizedAnswer::Pairs(_)) => true,
        (QuestionKind::Ordering, NormalizedAnswer::Ordering(values)) => {
            values.len() == question.options.len()
                && values
                    .iter()
                    .all(|value| option_ids.contains(value.as_str()))
        }
        (QuestionKind::Composite, NormalizedAnswer::Composite(_)) => true,
        (QuestionKind::Unknown, value) => {
            !matches!(value, NormalizedAnswer::Unknown | NormalizedAnswer::Skip)
        }
        _ => false,
    };
    if valid {
        Ok(())
    } else {
        Err(ApiError::bad_gateway(
            "ai_answer_kind_mismatch",
            "model answer does not match the question kind or option IDs",
        ))
    }
}

fn safe_subjective_text(value: &str) -> bool {
    let lowered = value.to_ascii_lowercase();
    !value.contains("```")
        && !value.trim_start().starts_with('#')
        && !value.trim_start().starts_with('{')
        && !lowered.contains("as an ai")
        && !value.contains("自动化测试")
        && !value.contains("作为一个人工智能")
}

fn has_rich_content(snapshot: &QuestionSnapshot, question: &Question) -> bool {
    !question.attachments.is_empty()
        || question
            .options
            .iter()
            .any(|option| !option.attachments.is_empty())
        || !snapshot.groups.is_empty()
        || matches!(
            question.kind,
            QuestionKind::ShortAnswer | QuestionKind::Composite | QuestionKind::Unknown
        )
}

fn map_remote_status(status: StatusCode) -> ApiError {
    if status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
        ApiError::service_unavailable(
            "ai_endpoint_unavailable",
            "AI endpoint is rate-limited or unavailable",
        )
    } else {
        ApiError::bad_gateway("ai_endpoint_rejected", "AI endpoint rejected the request")
    }
}

const fn reasoning_name(value: AiReasoningEffort) -> &'static str {
    match value {
        AiReasoningEffort::Low => "low",
        AiReasoningEffort::Medium => "medium",
        AiReasoningEffort::High => "high",
        AiReasoningEffort::Xhigh => "xhigh",
    }
}

const fn protocol_name(value: AiProtocol) -> &'static str {
    match value {
        AiProtocol::Responses => "responses",
        AiProtocol::ChatCompletions => "chat_completions",
    }
}

const fn answer_source_name(value: AnswerSource) -> &'static str {
    match value {
        AnswerSource::Manual => "manual",
        AnswerSource::LocalCache => "local_cache",
        AnswerSource::ProviderNative => "provider_native",
        AnswerSource::Ai => "ai",
        AnswerSource::ExternalBank => "external_bank",
        AnswerSource::Other => "other",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ai_candidate_refresh_is_explicit_and_defaults_to_cache_reuse() {
        let defaulted: GenerateAiAnswerCandidatesRequest =
            serde_json::from_value(json!({})).unwrap();
        assert!(!defaulted.force_refresh);
        let refreshed: GenerateAiAnswerCandidatesRequest =
            serde_json::from_value(json!({"force_refresh": true})).unwrap();
        assert!(refreshed.force_refresh);
    }

    #[test]
    fn extracts_both_supported_protocols() {
        let responses = json!({"output":[{"content":[{"type":"output_text","text":"{\"type\":\"boolean\",\"value\":true}"}]}]});
        let chat =
            json!({"choices":[{"message":{"content":"{\"type\":\"boolean\",\"value\":true}"}}]});
        assert!(extract_answer_text(AiProtocol::Responses, &responses).is_some());
        assert!(extract_answer_text(AiProtocol::ChatCompletions, &chat).is_some());
    }

    #[test]
    fn extracts_provider_usage_without_treating_missing_usage_as_an_error() {
        let responses = json!({"usage": {"input_tokens": 12, "output_tokens": 5}});
        let chat = json!({"usage": {"prompt_tokens": 19, "completion_tokens": 7}});
        assert_eq!(
            extract_usage(AiProtocol::Responses, &responses).input_tokens,
            Some(12)
        );
        assert_eq!(
            extract_usage(AiProtocol::Responses, &responses).output_tokens,
            Some(5)
        );
        assert_eq!(
            extract_usage(AiProtocol::ChatCompletions, &chat).input_tokens,
            Some(19)
        );
        assert_eq!(
            extract_usage(AiProtocol::ChatCompletions, &chat).output_tokens,
            Some(7)
        );
        assert_eq!(
            extract_usage(AiProtocol::Responses, &json!({})).input_tokens,
            None
        );
    }

    #[test]
    fn ai_cost_rounds_each_token_component_up_and_ignores_negative_usage() {
        assert_eq!(cost_from_rates(1_001, 1, 2, 3), 4);
        assert_eq!(cost_from_rates(-1, 999, 2, 3), 3);
    }

    #[test]
    fn parses_fenced_normalized_answer_but_rejects_explanation() {
        assert_eq!(
            parse_normalized_answer("```json\n{\"type\":\"boolean\",\"value\":true}\n```").unwrap(),
            NormalizedAnswer::Boolean(true)
        );
        assert!(parse_normalized_answer("The answer is true").is_err());
    }

    #[test]
    fn endpoint_join_preserves_v1_roots() {
        assert_eq!(
            append_endpoint("https://example.test/v1/", "responses"),
            "https://example.test/v1/responses"
        );
        assert_eq!(
            append_endpoint("https://example.test/chat/completions", "chat/completions"),
            "https://example.test/chat/completions"
        );
    }

    #[test]
    fn responses_use_strict_schema_and_labeled_image_content() {
        let config = AiConfig::default();
        let route = config.economy.untimed;
        let (_url, body) = request_body(
            &config.gpt_router,
            &route,
            "question",
            &["https://p.ananas.chaoxing.com/image.png".to_owned()],
            false,
        );

        assert_eq!(
            body.pointer("/text/format/type"),
            Some(&json!("json_schema"))
        );
        assert_eq!(body.pointer("/text/format/strict"), Some(&json!(true)));
        assert_eq!(
            body.pointer("/input/1/content/1/type"),
            Some(&json!("input_image"))
        );
        assert_eq!(body.get("store"), Some(&json!(false)));
    }

    #[test]
    fn discussion_generation_uses_plain_text_without_remote_storage() {
        let config = AiConfig::default();
        let route = config.economy.untimed;
        let (_url, body) = plain_text_request_body(&config.gpt_router, &route, "topic", false);
        assert_eq!(body.get("store"), Some(&json!(false)));
        assert!(body.get("text").is_none());
        assert!(
            validate_human_plain_text("I agree because the example supports the main idea.")
                .is_ok()
        );
        for invalid in [
            "```text\nautomated\n```",
            "As an AI, I think...",
            "自动化测试文本",
            "# Heading",
        ] {
            assert!(
                validate_human_plain_text(invalid).is_err(),
                "accepted {invalid}"
            );
        }
    }

    #[test]
    fn discussion_variation_seed_is_stable_but_request_specific() {
        let first = discussion_variation_seed("owner", "task", "request-a");
        assert_eq!(
            first,
            discussion_variation_seed("owner", "task", "request-a")
        );
        assert_ne!(
            first,
            discussion_variation_seed("owner", "task", "request-b")
        );
        assert_eq!(first.len(), 16);
    }
}
