use std::str::FromStr;

use asterism_domain::{
    AnswerResolutionStatus, NormalizedAnswer, Question, QuestionId, QuestionKind, QuestionOption,
    TaskId,
};
use asterism_engine::{
    ConservativeAnswerResolverService, ImportLocalAnswerCandidatesCommand, LocalAnswerCacheService,
    ResolveAnswerCandidatesCommand,
};
use asterism_storage::{
    QuestionSnapshot, QuestionSnapshotRepository, SqliteQuestionSnapshotRepository,
};
use axum::{
    Json,
    extract::State as AxumState,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::{
    ApiError, ApiState,
    ai::{AiAnswerProfile, AiAnswerRoute, generate_ai_records},
};

const PROTOCOL: &str = "asterism.cidaren.answer-bridge.v1";
const MAX_EXAM_BYTES: usize = 512 * 1024;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct BridgeRequest {
    protocol: String,
    kind: String,
    execution_id: String,
    task_id: String,
    remote_task_id: Option<String>,
    route: Option<String>,
    timeout_seconds: Option<u64>,
    remote_id: Option<String>,
    mode: Option<Value>,
    exam: Option<Value>,
    submitted: Option<Value>,
    source: Option<String>,
    outcome: Option<Value>,
}

#[derive(Debug, Serialize)]
struct BridgeResponse {
    protocol: &'static str,
    answer_available: bool,
    donor_value: Option<Value>,
    source: &'static str,
}

pub(super) async fn handle(
    AxumState(state): AxumState<ApiState>,
    headers: HeaderMap,
    Json(request): Json<BridgeRequest>,
) -> Result<Response, ApiError> {
    if request.protocol != PROTOCOL
        || !matches!(
            request.kind.as_str(),
            "resolve_answer" | "answer_observation"
        )
    {
        return Err(ApiError::bad_request(
            "cidaren_bridge_protocol",
            "unsupported Cidaren answer bridge request",
        ));
    }
    let expected = bridge_ticket().ok_or_else(|| {
        ApiError::service_unavailable(
            "cidaren_bridge_unconfigured",
            "Cidaren answer bridge ticket is not configured",
        )
    })?;
    let supplied = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "));
    if supplied != Some(expected.as_str()) {
        return Err(ApiError::unauthorized());
    }
    let task_id = TaskId::from_str(&request.task_id).map_err(|_| {
        ApiError::bad_request(
            "cidaren_bridge_task_invalid",
            "Cidaren task binding is invalid",
        )
    })?;
    let execution_id =
        asterism_domain::ExecutionId::from_str(&request.execution_id).map_err(|_| {
            ApiError::bad_request(
                "cidaren_bridge_execution_invalid",
                "Cidaren execution binding is invalid",
            )
        })?;
    if request
        .timeout_seconds
        .is_some_and(|value| value == 0 || value > 300)
    {
        return Err(ApiError::bad_request(
            "cidaren_bridge_timeout_invalid",
            "Cidaren answer bridge timeout is invalid",
        ));
    }
    let binding: Option<(Option<String>, String)> = sqlx::query_as(
        "SELECT execution.requested_by, task.remote_id FROM executions AS execution INNER JOIN tasks AS task ON task.id = execution.task_id INNER JOIN provider_accounts AS account ON account.id = task.provider_account_id WHERE execution.id = ? AND execution.task_id = ? AND account.provider_id = 'cidaren'",
    ).bind(execution_id.to_string()).bind(task_id.to_string()).fetch_optional(state.database.pool()).await.map_err(ApiError::internal)?;
    let (owner, stored_remote_task_id) =
        binding.ok_or_else(|| ApiError::not_found("cidaren_bridge_binding_not_found"))?;
    if request
        .remote_task_id
        .as_deref()
        .is_some_and(|value| value != stored_remote_task_id)
    {
        return Err(ApiError::not_found("cidaren_bridge_binding_not_found"));
    }
    let owner = owner.ok_or_else(|| ApiError::not_found("cidaren_bridge_binding_not_found"))?;
    let owner = asterism_domain::UserId::from_str(&owner).map_err(ApiError::internal)?;
    if request.kind == "answer_observation" {
        let correctness = request
            .outcome
            .as_ref()
            .and_then(|value| value.get("correctness"))
            .and_then(Value::as_str)
            .filter(|value| matches!(*value, "correct" | "wrong" | "mixed" | "unknown"))
            .unwrap_or("unknown");
        let source = request
            .source
            .as_deref()
            .filter(|value| matches!(*value, "supplied" | "bridge" | "donor"))
            .unwrap_or("donor");
        sqlx::query(
            "INSERT INTO execution_logs (id, execution_id, timestamp, level, stage, message, metadata_sanitized_json) VALUES (?, ?, ?, 'info', 'answer_observation', 'Cidaren answer result observed', ?)",
        )
        .bind(uuid::Uuid::now_v7().to_string())
        .bind(execution_id.to_string())
        .bind(chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Micros, true))
        .bind(serde_json::to_string(&json!({"remote_id": request.remote_id, "correctness": correctness, "source": source, "submitted_shape": request.submitted.as_ref().map(value_shape)})).map_err(ApiError::internal)?)
        .execute(state.database.pool())
        .await
        .map_err(ApiError::internal)?;
        return Ok(StatusCode::NO_CONTENT.into_response());
    }
    let exam = request.exam.ok_or_else(|| {
        ApiError::bad_request(
            "cidaren_bridge_question_missing",
            "Cidaren question payload is missing",
        )
    })?;
    let bytes = serde_json::to_vec(&exam).map_err(ApiError::internal)?;
    if bytes.len() > MAX_EXAM_BYTES {
        return Err(ApiError::bad_request(
            "cidaren_bridge_question_oversized",
            "Cidaren question payload is oversized",
        ));
    }
    let question =
        asterism_provider_cidaren::parse_attempt_question(&exam, &stored_remote_task_id, 1)
            .and_then(|parsed| parsed.to_question(task_id))
            .unwrap_or(question_from_exam(
                task_id,
                request.remote_id.as_deref(),
                question_kind(request.mode.as_ref(), &exam),
                &exam,
            )?);
    let snapshot = QuestionSnapshot {
        id: asterism_domain::QuestionSnapshotId::new(),
        task_id,
        provider_id: asterism_domain::ProviderId::new("cidaren").map_err(ApiError::internal)?,
        provider_version: "cidaren-answer-bridge-v1".to_owned(),
        captured_at: chrono::Utc::now(),
        questions: vec![question.clone()],
        groups: Vec::new(),
    };
    question.validate().map_err(|_| {
        ApiError::bad_request(
            "cidaren_bridge_question_invalid",
            "Cidaren question could not be normalized",
        )
    })?;
    let repository = SqliteQuestionSnapshotRepository::new(state.database.clone());
    repository
        .save_question_snapshot(&snapshot)
        .await
        .map_err(ApiError::internal)?;
    LocalAnswerCacheService::new(repository.clone())
        .import(ImportLocalAnswerCandidatesCommand {
            owner_id: owner,
            task_id,
            question_snapshot_id: snapshot.id,
        })
        .await
        .map_err(ApiError::internal)?;
    let cached_plan = ConservativeAnswerResolverService::new(repository.clone())
        .resolve(ResolveAnswerCandidatesCommand {
            owner_id: owner,
            task_id,
            question_snapshot_id: snapshot.id,
        })
        .await
        .map_err(ApiError::internal)?;
    if let Some(answer) = cached_plan
        .decisions
        .first()
        .filter(|decision| decision.status == AnswerResolutionStatus::Selected)
        .and_then(|decision| decision.selected_answer.as_ref())
    {
        let donor_value = donor_value(&question, answer);
        return Ok(Json(BridgeResponse {
            protocol: PROTOCOL,
            answer_available: donor_value.is_some(),
            donor_value,
            source: "local_cache",
        })
        .into_response());
    }
    let route = match request.route.as_deref().unwrap_or("untimed") {
        "timed" => AiAnswerRoute::Timed,
        "escalation" => AiAnswerRoute::Escalation,
        _ => AiAnswerRoute::Untimed,
    };
    generate_ai_records(
        &state,
        owner,
        task_id,
        &snapshot,
        &[&snapshot.questions[0]],
        AiAnswerProfile::Economy,
        route,
        false,
    )
    .await?;
    let resolved = ConservativeAnswerResolverService::new(repository)
        .resolve(ResolveAnswerCandidatesCommand {
            owner_id: owner,
            task_id,
            question_snapshot_id: snapshot.id,
        })
        .await
        .map_err(ApiError::internal)?;
    let Some(answer) = resolved
        .decisions
        .first()
        .filter(|decision| decision.status == AnswerResolutionStatus::Selected)
        .and_then(|decision| decision.selected_answer.as_ref())
    else {
        return Ok(Json(BridgeResponse {
            protocol: PROTOCOL,
            answer_available: false,
            donor_value: None,
            source: "ai",
        })
        .into_response());
    };
    let donor_value = donor_value(&question, answer);
    Ok(Json(BridgeResponse {
        protocol: PROTOCOL,
        answer_available: donor_value.is_some(),
        donor_value,
        source: "ai",
    })
    .into_response())
}

fn value_shape(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn bridge_ticket() -> Option<String> {
    std::env::var("ASTERISM_CIDAREN_ANSWER_BRIDGE_TICKET")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            let deployment_secret = std::env::var("ASTERISM_SECRET_KEYS").ok()?;
            let digest = Sha256::digest(
                format!("asterism.cidaren.answer-bridge.v1\0{deployment_secret}").as_bytes(),
            );
            Some(digest.iter().map(|byte| format!("{byte:02x}")).collect())
        })
}

fn question_kind(mode: Option<&Value>, exam: &Value) -> QuestionKind {
    let numeric = mode
        .and_then(Value::as_i64)
        .or_else(|| exam.get("topic_mode").and_then(Value::as_i64));
    if numeric.is_some_and(|value| matches!(value, 11 | 13 | 15..=18 | 21..=22 | 41..=44)) {
        return QuestionKind::SingleChoice;
    }
    if numeric == Some(31) {
        return QuestionKind::Matching;
    }
    if numeric.is_some_and(|value| matches!(value, 32 | 51..=54 | 73)) {
        return QuestionKind::FillBlank;
    }
    let text = mode
        .and_then(Value::as_str)
        .or_else(|| exam.get("topic_mode").and_then(Value::as_str))
        .unwrap_or("")
        .to_ascii_lowercase();
    if text == "1" || text.contains("single") || text.contains("choice") {
        QuestionKind::SingleChoice
    } else if text == "2" || text.contains("multiple") {
        QuestionKind::MultipleChoice
    } else if text == "3" || text.contains("judge") || text.contains("true") {
        QuestionKind::TrueFalse
    } else if text.contains("match") {
        QuestionKind::Matching
    } else if text.contains("order") || text.contains("sequence") {
        QuestionKind::Ordering
    } else if text.contains("fill") || text.contains("blank") {
        QuestionKind::FillBlank
    } else {
        QuestionKind::Unknown
    }
}

fn question_from_exam(
    task_id: TaskId,
    remote_id: Option<&str>,
    kind: QuestionKind,
    exam: &Value,
) -> Result<Question, ApiError> {
    let stem = ["topic_title", "question", "word", "title"]
        .iter()
        .find_map(|key| exam.get(*key).and_then(Value::as_str))
        .unwrap_or_default()
        .to_owned();
    let raw_options = exam
        .get("options")
        .or_else(|| exam.get("option"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let options = raw_options
        .into_iter()
        .enumerate()
        .map(|(index, value)| {
            let (id, content) = if let Some(object) = value.as_object() {
                (
                    object
                        .get("answer_tag")
                        .or_else(|| object.get("id"))
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                        .unwrap_or_else(|| format!("option-{}", index + 1)),
                    object
                        .get("content")
                        .or_else(|| object.get("text"))
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                )
            } else {
                (
                    format!("option-{}", index + 1),
                    value.as_str().map(str::to_owned),
                )
            };
            QuestionOption {
                id,
                content,
                attachments: Vec::new(),
                metadata_sanitized: json!({}),
            }
        })
        .collect();
    Ok(Question {
        id: QuestionId::new(),
        task_id,
        remote_question_id: remote_id.map(str::to_owned),
        kind,
        stem,
        options,
        attachments: Vec::new(),
        metadata_sanitized: json!({"cidaren_bridge": true}),
        position: 1,
    })
}

fn donor_value(question: &Question, answer: &NormalizedAnswer) -> Option<Value> {
    match answer {
        NormalizedAnswer::Selections(values) => Some(Value::Array(
            values
                .iter()
                .filter_map(|value| {
                    question
                        .options
                        .iter()
                        .find(|option| option.id == *value)
                        .map(|option| Value::String(option.id.clone()))
                })
                .collect(),
        )),
        NormalizedAnswer::Boolean(value) => Some(Value::Bool(*value)),
        NormalizedAnswer::Texts(values) => Some(Value::Array(
            values.iter().cloned().map(Value::String).collect(),
        )),
        NormalizedAnswer::Ordering(values) => Some(Value::Array(
            values
                .iter()
                .filter_map(|value| {
                    question
                        .options
                        .iter()
                        .find(|option| option.id == *value)
                        .map(|option| Value::String(option.id.clone()))
                })
                .collect(),
        )),
        NormalizedAnswer::Pairs(values) => Some(Value::Array(
            values
                .iter()
                .map(|pair| json!({"left": pair.left, "right": pair.right}))
                .collect(),
        )),
        NormalizedAnswer::Composite(values) => Some(Value::Array(
            values
                .iter()
                .filter_map(|value| donor_value(question, value))
                .collect(),
        )),
        NormalizedAnswer::Skip | NormalizedAnswer::Unknown => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn donor_value_uses_current_option_tags_and_preserves_complex_shapes() {
        let question = Question {
            id: QuestionId::new(),
            task_id: TaskId::new(),
            remote_question_id: Some("question-1".to_owned()),
            kind: QuestionKind::Matching,
            stem: "match".to_owned(),
            options: vec![QuestionOption {
                id: "tag-b".to_owned(),
                content: Some("content".to_owned()),
                attachments: Vec::new(),
                metadata_sanitized: json!({}),
            }],
            attachments: Vec::new(),
            metadata_sanitized: json!({}),
            position: 1,
        };
        assert_eq!(
            donor_value(
                &question,
                &NormalizedAnswer::Selections(vec!["tag-b".to_owned()])
            ),
            Some(json!(["tag-b"]))
        );
        assert_eq!(
            donor_value(
                &question,
                &NormalizedAnswer::Pairs(vec![asterism_domain::AnswerPair {
                    left: "left".to_owned(),
                    right: "right".to_owned(),
                }])
            ),
            Some(json!([{"left":"left","right":"right"}]))
        );
    }

    #[test]
    fn numeric_cidaren_modes_have_conservative_fallback_kinds() {
        assert_eq!(
            question_kind(Some(&json!(31)), &json!({})),
            QuestionKind::Matching
        );
        assert_eq!(
            question_kind(Some(&json!(73)), &json!({})),
            QuestionKind::FillBlank
        );
        assert_eq!(
            question_kind(Some(&json!(999)), &json!({})),
            QuestionKind::Unknown
        );
    }
}
