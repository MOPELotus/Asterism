use std::{collections::BTreeSet, str::FromStr};

use asterism_domain::{
    AnswerHistoryImportId, ProviderAccountId, ProviderId, QuestionContentFingerprint,
    SubmissionScore, TaskId, UserId,
};
use asterism_provider_api::AnswerHistoryRetakeFacts;
use async_trait::async_trait;
use serde_json::json;
use sha2::{Digest, Sha256};
use sqlx::{Row, Sqlite, Transaction};

use crate::answer_evidence::record_answer_evidence_in_transaction;
use crate::auth_session::{decode_timestamp, encode_timestamp};
use crate::question::{
    save_answer_candidate_batch_in_transaction, save_question_snapshot_in_transaction,
};
use crate::{
    AnswerEvidenceRecordOutcome, AnswerHistoryImportRecord, AnswerHistoryIngestOutcome,
    AnswerHistoryIngestRequest, AnswerHistoryIngestionRepository, AnswerHistoryTaskFact, Database,
    StorageError,
};

#[derive(Clone, Debug)]
pub struct SqliteAnswerHistoryIngestionRepository {
    database: Database,
}

impl SqliteAnswerHistoryIngestionRepository {
    pub const fn new(database: Database) -> Self {
        Self { database }
    }
}

#[async_trait]
impl AnswerHistoryIngestionRepository for SqliteAnswerHistoryIngestionRepository {
    async fn ingest_answer_history_task(
        &self,
        request: AnswerHistoryIngestRequest<'_>,
    ) -> Result<AnswerHistoryIngestOutcome, StorageError> {
        let validated = validate_request(&request)?;
        let mut transaction = self.database.pool().begin_with("BEGIN IMMEDIATE").await?;
        let existing = sqlx::query(
            "SELECT id, question_snapshot_id, candidate_count, evidence_count, \
                    content_digest, score_json, retake_json, provenance_sanitized_json, \
                    observed_at, imported_at \
             FROM answer_history_imports \
             WHERE provider_account_id = ? AND task_id = ? \
               AND provider_attempt_digest = ? AND result_digest = ?",
        )
        .bind(request.provider_account_id.to_string())
        .bind(request.snapshot.task_id.to_string())
        .bind(request.provider_attempt_digest.to_vec())
        .bind(request.result_digest.to_vec())
        .fetch_optional(&mut *transaction)
        .await?;
        if let Some(row) = existing {
            let record =
                validate_existing_import(&mut transaction, &row, &request, &validated).await?;
            transaction.commit().await?;
            return Ok(AnswerHistoryIngestOutcome::Duplicate(record));
        }

        save_question_snapshot_in_transaction(&mut transaction, request.snapshot).await?;
        if !request.candidates.is_empty() {
            save_answer_candidate_batch_in_transaction(&mut transaction, request.candidates)
                .await?;
        }
        for evidence in request.evidence {
            if !matches!(
                record_answer_evidence_in_transaction(&mut transaction, evidence).await?,
                AnswerEvidenceRecordOutcome::Inserted(_)
            ) {
                return Err(invalid_import());
            }
        }
        let record = AnswerHistoryImportRecord {
            import_id: AnswerHistoryImportId::new(),
            question_snapshot_id: request.snapshot.id,
            candidate_count: validated.candidate_count,
            evidence_count: validated.evidence_count,
        };
        sqlx::query(
            "INSERT INTO answer_history_imports \
             (id, owner_user_id, provider_id, provider_account_id, task_id, \
              provider_attempt_digest, result_digest, content_digest, question_snapshot_id, \
              candidate_count, evidence_count, score_json, retake_json, \
              provenance_sanitized_json, observed_at, imported_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(record.import_id.to_string())
        .bind(request.owner_user_id.to_string())
        .bind(request.snapshot.provider_id.as_str())
        .bind(request.provider_account_id.to_string())
        .bind(request.snapshot.task_id.to_string())
        .bind(request.provider_attempt_digest.to_vec())
        .bind(request.result_digest.to_vec())
        .bind(validated.content_digest.to_vec())
        .bind(request.snapshot.id.to_string())
        .bind(i64::from(record.candidate_count))
        .bind(i64::from(record.evidence_count))
        .bind(
            request
                .score
                .map(|score| serde_json::to_string(&score))
                .transpose()?,
        )
        .bind(request.retake.map(serde_json::to_string).transpose()?)
        .bind(serde_json::to_string(request.provenance_sanitized)?)
        .bind(encode_timestamp(request.observed_at))
        .bind(encode_timestamp(request.imported_at))
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(AnswerHistoryIngestOutcome::Inserted(record))
    }

    async fn find_latest_owned_answer_history_task_fact(
        &self,
        owner_user_id: UserId,
        task_id: TaskId,
    ) -> Result<Option<AnswerHistoryTaskFact>, StorageError> {
        let row = sqlx::query(
            "SELECT id, owner_user_id, provider_id, provider_account_id, task_id, \
                    provider_attempt_digest, result_digest, score_json, retake_json, \
                    provenance_sanitized_json, observed_at, imported_at \
             FROM answer_history_imports WHERE owner_user_id = ? AND task_id = ? \
             ORDER BY observed_at DESC, imported_at DESC, id DESC LIMIT 1",
        )
        .bind(owner_user_id.to_string())
        .bind(task_id.to_string())
        .fetch_optional(self.database.pool())
        .await?;
        row.as_ref().map(decode_task_fact).transpose()
    }
}

async fn validate_existing_import(
    transaction: &mut Transaction<'_, Sqlite>,
    row: &sqlx::sqlite::SqliteRow,
    request: &AnswerHistoryIngestRequest<'_>,
    validated: &ValidatedImport,
) -> Result<AnswerHistoryImportRecord, StorageError> {
    let record = decode_record(row)?;
    let stored_digest: Vec<u8> = row.try_get("content_digest")?;
    if record.candidate_count != validated.candidate_count
        || record.evidence_count != validated.evidence_count
    {
        return Err(invalid_import());
    }
    if stored_digest.as_slice() == validated.content_digest {
        return Ok(record);
    }

    let stored_retake_json = row.try_get::<Option<String>, _>("retake_json")?;
    let stored_retake = stored_retake_json
        .as_deref()
        .map(serde_json::from_str::<AnswerHistoryRetakeFacts>)
        .transpose()?;
    let legacy_retake_policy = request.retake.is_some()
        && stored_retake.as_ref() == request.retake
        && request.retake.is_some_and(|retake| {
            retake.score_policy == asterism_domain::RetakeScorePolicy::Unknown
        })
        && stored_digest.as_slice() == legacy_retake_policy_content_digest(request)?;
    if legacy_retake_policy {
        let updated = sqlx::query(
            "UPDATE answer_history_imports SET content_digest = ?, retake_json = ? \
             WHERE id = ? AND content_digest = ?",
        )
        .bind(validated.content_digest.to_vec())
        .bind(request.retake.map(serde_json::to_string).transpose()?)
        .bind(record.import_id.to_string())
        .bind(stored_digest)
        .execute(&mut **transaction)
        .await?;
        if updated.rows_affected() != 1 {
            return Err(invalid_import());
        }
        return Ok(record);
    }

    let observed_at = decode_timestamp(row.try_get("observed_at")?)?;
    let imported_at = decode_timestamp(row.try_get("imported_at")?)?;
    let legacy_unknown = row.try_get::<Option<String>, _>("score_json")?.is_none()
        && row.try_get::<Option<String>, _>("retake_json")?.is_none()
        && row.try_get::<&str, _>("provenance_sanitized_json")? == "{}"
        && observed_at == imported_at;
    if !legacy_unknown || stored_digest.as_slice() != legacy_content_digest(request)? {
        return Err(invalid_import());
    }

    let updated = sqlx::query(
        "UPDATE answer_history_imports SET content_digest = ?, score_json = ?, \
                retake_json = ?, provenance_sanitized_json = ?, observed_at = ? \
         WHERE id = ? AND content_digest = ?",
    )
    .bind(validated.content_digest.to_vec())
    .bind(
        request
            .score
            .map(|score| serde_json::to_string(&score))
            .transpose()?,
    )
    .bind(request.retake.map(serde_json::to_string).transpose()?)
    .bind(serde_json::to_string(request.provenance_sanitized)?)
    .bind(encode_timestamp(request.observed_at))
    .bind(record.import_id.to_string())
    .bind(stored_digest)
    .execute(&mut **transaction)
    .await?;
    if updated.rows_affected() != 1 {
        return Err(invalid_import());
    }
    Ok(record)
}

struct ValidatedImport {
    content_digest: [u8; 32],
    candidate_count: u32,
    evidence_count: u32,
}

fn validate_request(
    request: &AnswerHistoryIngestRequest<'_>,
) -> Result<ValidatedImport, StorageError> {
    if request.provider_attempt_digest == [0; 32]
        || request.result_digest == [0; 32]
        || request.score.is_some_and(|score| score.validate().is_err())
        || request
            .retake
            .is_some_and(|retake| retake.validate().is_err())
        || !valid_private_provenance(request.provenance_sanitized)
        || request.observed_at > request.imported_at
    {
        return Err(invalid_import());
    }
    let candidate_count = u32::try_from(request.candidates.len()).map_err(|_| invalid_import())?;
    let evidence_count = u32::try_from(request.evidence.len()).map_err(|_| invalid_import())?;
    let question_ids = request
        .snapshot
        .questions
        .iter()
        .map(|question| question.id)
        .collect::<BTreeSet<_>>();
    if question_ids.len() != request.snapshot.questions.len() {
        return Err(invalid_import());
    }
    let candidate_ids = request
        .candidates
        .iter()
        .map(|candidate| candidate.id)
        .collect::<BTreeSet<_>>();
    if candidate_ids.len() != request.candidates.len()
        || request.candidates.iter().any(|candidate| {
            candidate.question_snapshot_id != request.snapshot.id
                || !question_ids.contains(&candidate.candidate.question_id)
        })
    {
        return Err(invalid_import());
    }
    let mut used_candidates = BTreeSet::new();
    for evidence in request.evidence {
        if evidence.owner_user_id != request.owner_user_id
            || evidence.provider_id != request.snapshot.provider_id
            || evidence.provider_account_id != request.provider_account_id
            || evidence.task_id != request.snapshot.task_id
            || evidence.question_snapshot_id != request.snapshot.id
            || evidence.execution_attempt_id.is_some()
            || evidence.provider_attempt_digest != Some(request.provider_attempt_digest)
            || evidence.result_digest != Some(request.result_digest)
            || !question_ids.contains(&evidence.question_id)
            || request
                .snapshot
                .questions
                .iter()
                .find(|question| question.id == evidence.question_id)
                != Some(&evidence.question)
            || evidence
                .source_candidate_id
                .is_none_or(|candidate_id| !candidate_ids.contains(&candidate_id))
        {
            return Err(invalid_import());
        }
        used_candidates.insert(evidence.source_candidate_id.expect("checked Some"));
    }
    if !used_candidates.is_subset(&candidate_ids) {
        return Err(invalid_import());
    }
    Ok(ValidatedImport {
        content_digest: content_digest(request)?,
        candidate_count,
        evidence_count,
    })
}

fn content_digest(request: &AnswerHistoryIngestRequest<'_>) -> Result<[u8; 32], StorageError> {
    let mut material = semantic_content_material(request)?;
    let values = material.as_object_mut().ok_or_else(invalid_import)?;
    values.insert("score".to_owned(), json!(request.score));
    values.insert("retake".to_owned(), json!(request.retake));
    values.insert(
        "provenance".to_owned(),
        request.provenance_sanitized.clone(),
    );
    values.insert("observed_at".to_owned(), json!(request.observed_at));
    digest_material(&material)
}

fn legacy_content_digest(
    request: &AnswerHistoryIngestRequest<'_>,
) -> Result<[u8; 32], StorageError> {
    digest_material(&semantic_content_material(request)?)
}

fn legacy_retake_policy_content_digest(
    request: &AnswerHistoryIngestRequest<'_>,
) -> Result<[u8; 32], StorageError> {
    let mut material = semantic_content_material(request)?;
    let values = material.as_object_mut().ok_or_else(invalid_import)?;
    values.insert("score".to_owned(), json!(request.score));
    values.insert(
        "retake".to_owned(),
        request.retake.map_or(serde_json::Value::Null, |retake| {
            json!({
                "allowed": retake.allowed,
                "remaining_attempts": retake.remaining_attempts,
                "closes_at": retake.closes_at,
                "metadata_sanitized": retake.metadata_sanitized,
            })
        }),
    );
    values.insert(
        "provenance".to_owned(),
        request.provenance_sanitized.clone(),
    );
    values.insert("observed_at".to_owned(), json!(request.observed_at));
    digest_material(&material)
}

fn semantic_content_material(
    request: &AnswerHistoryIngestRequest<'_>,
) -> Result<serde_json::Value, StorageError> {
    let positions = request
        .snapshot
        .questions
        .iter()
        .map(|question| (question.id, question.position))
        .collect::<std::collections::BTreeMap<_, _>>();
    let mut questions = request
        .snapshot
        .questions
        .iter()
        .map(|question| {
            Ok((
                question.position,
                question
                    .content_fingerprint()
                    .map_err(|_| invalid_import())?,
            ))
        })
        .collect::<Result<Vec<(u32, QuestionContentFingerprint)>, StorageError>>()?;
    questions.sort_by_key(|(position, _)| *position);
    let mut candidates = request
        .candidates
        .iter()
        .map(|candidate| {
            let position = positions
                .get(&candidate.candidate.question_id)
                .copied()
                .ok_or_else(invalid_import)?;
            Ok(json!({
                "position": position,
                "source": candidate.candidate.source,
                "answer": candidate.candidate.answer,
                "confidence": candidate.candidate.confidence,
                "explanation": candidate.candidate.explanation,
                "provenance": candidate.candidate.provenance_sanitized,
            }))
        })
        .collect::<Result<Vec<_>, StorageError>>()?;
    sort_json(&mut candidates)?;
    let mut evidence = request
        .evidence
        .iter()
        .map(|evidence| {
            let position = positions
                .get(&evidence.question_id)
                .copied()
                .ok_or_else(invalid_import)?;
            Ok(json!({
                "position": position,
                "answer": evidence.answer,
                "answer_source": evidence.answer_source,
                "evidence_class": evidence.evidence_class,
                "projection": evidence.projection,
                "provenance": evidence.provenance_sanitized,
            }))
        })
        .collect::<Result<Vec<_>, StorageError>>()?;
    sort_json(&mut evidence)?;
    Ok(json!({
        "questions": questions,
        "candidates": candidates,
        "evidence": evidence,
    }))
}

fn digest_material(material: &serde_json::Value) -> Result<[u8; 32], StorageError> {
    Ok(Sha256::digest(serde_json::to_vec(material)?).into())
}

fn sort_json(values: &mut [serde_json::Value]) -> Result<(), StorageError> {
    let mut encoded = values
        .iter()
        .map(serde_json::to_string)
        .collect::<Result<Vec<_>, _>>()?;
    encoded.sort_unstable();
    for (target, encoded) in values.iter_mut().zip(encoded) {
        *target = serde_json::from_str(&encoded)?;
    }
    Ok(())
}

fn decode_record(row: &sqlx::sqlite::SqliteRow) -> Result<AnswerHistoryImportRecord, StorageError> {
    Ok(AnswerHistoryImportRecord {
        import_id: AnswerHistoryImportId::from_str(row.try_get("id")?)
            .map_err(|_| invalid_import())?,
        question_snapshot_id: asterism_domain::QuestionSnapshotId::from_str(
            row.try_get("question_snapshot_id")?,
        )
        .map_err(|_| invalid_import())?,
        candidate_count: u32::try_from(row.try_get::<i64, _>("candidate_count")?)
            .map_err(|_| invalid_import())?,
        evidence_count: u32::try_from(row.try_get::<i64, _>("evidence_count")?)
            .map_err(|_| invalid_import())?,
    })
}

fn decode_task_fact(row: &sqlx::sqlite::SqliteRow) -> Result<AnswerHistoryTaskFact, StorageError> {
    let score = row
        .try_get::<Option<String>, _>("score_json")?
        .map(|value| serde_json::from_str::<SubmissionScore>(&value))
        .transpose()?;
    if score.is_some_and(|score| score.validate().is_err()) {
        return Err(invalid_import());
    }
    let retake = row
        .try_get::<Option<String>, _>("retake_json")?
        .map(|value| serde_json::from_str::<AnswerHistoryRetakeFacts>(&value))
        .transpose()?;
    if retake
        .as_ref()
        .is_some_and(|retake| retake.validate().is_err())
    {
        return Err(invalid_import());
    }
    let provenance_sanitized =
        serde_json::from_str(row.try_get::<&str, _>("provenance_sanitized_json")?)?;
    if !valid_private_provenance(&provenance_sanitized) {
        return Err(invalid_import());
    }
    Ok(AnswerHistoryTaskFact {
        import_id: AnswerHistoryImportId::from_str(row.try_get("id")?)
            .map_err(|_| invalid_import())?,
        owner_user_id: UserId::from_str(row.try_get("owner_user_id")?)
            .map_err(|_| invalid_import())?,
        provider_id: ProviderId::new(row.try_get::<String, _>("provider_id")?)
            .map_err(|_| invalid_import())?,
        provider_account_id: ProviderAccountId::from_str(row.try_get("provider_account_id")?)
            .map_err(|_| invalid_import())?,
        task_id: TaskId::from_str(row.try_get("task_id")?).map_err(|_| invalid_import())?,
        provider_attempt_digest: decode_digest(row.try_get("provider_attempt_digest")?)?,
        result_digest: decode_digest(row.try_get("result_digest")?)?,
        score,
        retake,
        provenance_sanitized,
        observed_at: decode_timestamp(row.try_get("observed_at")?)?,
        imported_at: decode_timestamp(row.try_get("imported_at")?)?,
    })
}

fn decode_digest(value: Vec<u8>) -> Result<[u8; 32], StorageError> {
    value.try_into().map_err(|_| invalid_import())
}

fn valid_private_provenance(value: &serde_json::Value) -> bool {
    serde_json::to_vec(value)
        .ok()
        .is_some_and(|bytes| !bytes.is_empty() && bytes.len() <= 256 * 1_024)
        && !contains_sensitive_key(value)
}

fn contains_sensitive_key(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Object(values) => values.iter().any(|(key, value)| {
            let normalized = key
                .chars()
                .filter(char::is_ascii_alphanumeric)
                .flat_map(char::to_lowercase)
                .collect::<String>();
            ["cookie", "authorization", "password", "token", "secret"]
                .iter()
                .any(|needle| normalized.contains(needle))
                || contains_sensitive_key(value)
        }),
        serde_json::Value::Array(values) => values.iter().any(contains_sensitive_key),
        serde_json::Value::Null
        | serde_json::Value::Bool(_)
        | serde_json::Value::Number(_)
        | serde_json::Value::String(_) => false,
    }
}

fn invalid_import() -> StorageError {
    StorageError::InvalidData(
        "answer history import is invalid, cross-bound or conflicts with an existing result"
            .to_owned(),
    )
}

#[cfg(test)]
mod tests {
    use asterism_domain::{
        AnswerCandidate, AnswerCandidateId, AnswerEvidenceClass, AnswerSource,
        CorpusProjectionEligibility, NormalizedAnswer, PrivateAnswerEvidence,
        PrivateAnswerEvidenceId, ProviderAccountId, ProviderId, Question, QuestionId, QuestionKind,
        QuestionOption, QuestionSnapshotId, TaskId, UserId,
    };
    use asterism_provider_api::AnswerHistoryRetakeFacts;
    use chrono::{Duration, Utc};

    use crate::{AnswerCandidateRecord, QuestionSnapshot};

    use super::*;

    struct Fixture {
        database: Database,
        owner: UserId,
        account: ProviderAccountId,
        provider: ProviderId,
        task: TaskId,
        now: asterism_domain::Timestamp,
    }

    struct ImportBundle {
        snapshot: QuestionSnapshot,
        candidates: Vec<AnswerCandidateRecord>,
        evidence: Vec<PrivateAnswerEvidence>,
        retake: AnswerHistoryRetakeFacts,
        provenance_sanitized: serde_json::Value,
    }

    impl Fixture {
        async fn new() -> Self {
            let database = Database::connect("sqlite::memory:").await.unwrap();
            database.migrate().await.unwrap();
            let owner = UserId::new();
            let account = ProviderAccountId::new();
            let provider = ProviderId::new("provider-alpha").unwrap();
            let task = TaskId::new();
            let now = Utc::now();
            let timestamp = encode_timestamp(now);
            sqlx::query(
                "INSERT INTO users \
                 (id, username, password_hash, status, roles_json, permissions_json, created_at, updated_at) \
                 VALUES (?, 'history-import-owner', '$argon2id$test', 'active', '[\"user\"]', '[]', ?, ?)",
            )
            .bind(owner.to_string())
            .bind(&timestamp)
            .bind(&timestamp)
            .execute(database.pool())
            .await
            .unwrap();
            sqlx::query(
                "INSERT INTO provider_accounts \
                 (id, owner_user_id, provider_id, display_name, auth_state_json, created_at, updated_at) \
                 VALUES (?, ?, ?, 'history', '\"authenticated\"', ?, ?)",
            )
            .bind(account.to_string())
            .bind(owner.to_string())
            .bind(provider.as_str())
            .bind(&timestamp)
            .bind(&timestamp)
            .execute(database.pool())
            .await
            .unwrap();
            sqlx::query(
                "INSERT INTO tasks \
                 (id, provider_account_id, remote_id, remote_fingerprint, source_type, \
                  assessment_class, title, remote_state, orchestration_state, discovered_at, \
                  updated_at, capabilities_json) \
                 VALUES (?, ?, 'history-work-1', 'v1:history', 'work', 'routine', 'History', \
                         'completed', 'succeeded', ?, ?, '[]')",
            )
            .bind(task.to_string())
            .bind(account.to_string())
            .bind(&timestamp)
            .bind(&timestamp)
            .execute(database.pool())
            .await
            .unwrap();
            Self {
                database,
                owner,
                account,
                provider,
                task,
                now,
            }
        }

        fn bundle(&self, attempt_digest: [u8; 32], result_digest: [u8; 32]) -> ImportBundle {
            let question = Question {
                id: QuestionId::new(),
                task_id: self.task,
                remote_question_id: Some("history-question-1".to_owned()),
                kind: QuestionKind::SingleChoice,
                stem: "Which value is official?".to_owned(),
                options: vec![
                    QuestionOption {
                        id: "A".to_owned(),
                        content: Some("Alpha".to_owned()),
                        attachments: Vec::new(),
                        metadata_sanitized: json!({}),
                    },
                    QuestionOption {
                        id: "B".to_owned(),
                        content: Some("Beta".to_owned()),
                        attachments: Vec::new(),
                        metadata_sanitized: json!({}),
                    },
                ],
                attachments: Vec::new(),
                metadata_sanitized: json!({}),
                position: 1,
            };
            let snapshot = QuestionSnapshot {
                id: QuestionSnapshotId::new(),
                task_id: self.task,
                provider_id: self.provider.clone(),
                provider_version: "history-v1".to_owned(),
                captured_at: self.now,
                questions: vec![question.clone()],
                groups: Vec::new(),
            };
            let submitted = self.candidate(
                snapshot.id,
                question.id,
                NormalizedAnswer::Selections(vec!["B".to_owned()]),
            );
            let official = self.candidate(
                snapshot.id,
                question.id,
                NormalizedAnswer::Selections(vec!["A".to_owned()]),
            );
            let evidence = vec![
                self.evidence(
                    &snapshot,
                    &submitted,
                    AnswerEvidenceClass::Negative,
                    attempt_digest,
                    result_digest,
                ),
                self.evidence(
                    &snapshot,
                    &official,
                    AnswerEvidenceClass::Official,
                    attempt_digest,
                    result_digest,
                ),
            ];
            ImportBundle {
                snapshot,
                candidates: vec![submitted, official],
                evidence,
                retake: AnswerHistoryRetakeFacts {
                    allowed: true,
                    remaining_attempts: Some(2),
                    closes_at: Some(self.now + Duration::hours(1)),
                    score_policy: asterism_domain::RetakeScorePolicy::HighestScore,
                    metadata_sanitized: json!({"action": "redo"}),
                },
                provenance_sanitized: json!({"surface": "history_result"}),
            }
        }

        fn candidate(
            &self,
            snapshot_id: QuestionSnapshotId,
            question_id: QuestionId,
            answer: NormalizedAnswer,
        ) -> AnswerCandidateRecord {
            AnswerCandidateRecord {
                id: AnswerCandidateId::new(),
                question_snapshot_id: snapshot_id,
                candidate: AnswerCandidate {
                    question_id,
                    source: AnswerSource::ProviderNative,
                    answer,
                    confidence: None,
                    explanation: None,
                    provenance_sanitized: json!({"surface": "history_result"}),
                },
                created_at: self.now,
            }
        }

        fn evidence(
            &self,
            snapshot: &QuestionSnapshot,
            candidate: &AnswerCandidateRecord,
            evidence_class: AnswerEvidenceClass,
            attempt_digest: [u8; 32],
            result_digest: [u8; 32],
        ) -> PrivateAnswerEvidence {
            let question = snapshot.questions[0].clone();
            PrivateAnswerEvidence {
                id: PrivateAnswerEvidenceId::new(),
                owner_user_id: self.owner,
                provider_id: self.provider.clone(),
                provider_account_id: self.account,
                course_id: None,
                task_id: self.task,
                question_snapshot_id: snapshot.id,
                question_id: question.id,
                execution_attempt_id: None,
                provider_attempt_digest: Some(attempt_digest),
                source_candidate_id: Some(candidate.id),
                question_content_fingerprint: question.content_fingerprint().unwrap(),
                question,
                answer: candidate.candidate.answer.clone(),
                answer_source: candidate.candidate.source,
                evidence_class,
                result_digest: Some(result_digest),
                provenance_sanitized: json!({"surface": "history_result"}),
                projection: CorpusProjectionEligibility::Exact,
                observed_at: self.now,
                verified_at: self.now + Duration::seconds(1),
            }
        }

        fn request<'a>(
            &'a self,
            bundle: &'a ImportBundle,
            attempt_digest: [u8; 32],
            result_digest: [u8; 32],
        ) -> AnswerHistoryIngestRequest<'a> {
            AnswerHistoryIngestRequest {
                owner_user_id: self.owner,
                provider_account_id: self.account,
                provider_attempt_digest: attempt_digest,
                result_digest,
                snapshot: &bundle.snapshot,
                candidates: &bundle.candidates,
                evidence: &bundle.evidence,
                score: Some(SubmissionScore {
                    earned_milli_points: 80,
                    possible_milli_points: 100,
                }),
                retake: Some(&bundle.retake),
                provenance_sanitized: &bundle.provenance_sanitized,
                observed_at: self.now,
                imported_at: self.now + Duration::seconds(2),
            }
        }
    }

    #[tokio::test]
    async fn history_import_is_atomic_idempotent_and_content_bound() {
        let fixture = Fixture::new().await;
        let repository = SqliteAnswerHistoryIngestionRepository::new(fixture.database.clone());
        let attempt = [7; 32];
        let result = [8; 32];
        let first = fixture.bundle(attempt, result);
        let inserted = repository
            .ingest_answer_history_task(fixture.request(&first, attempt, result))
            .await
            .unwrap();
        let AnswerHistoryIngestOutcome::Inserted(inserted_record) = inserted else {
            panic!("first import must insert")
        };

        let replay = fixture.bundle(attempt, result);
        let duplicate = repository
            .ingest_answer_history_task(fixture.request(&replay, attempt, result))
            .await
            .unwrap();
        assert_eq!(
            duplicate,
            AnswerHistoryIngestOutcome::Duplicate(inserted_record.clone())
        );
        let fact = repository
            .find_latest_owned_answer_history_task_fact(fixture.owner, fixture.task)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(fact.provider_attempt_digest, attempt);
        assert_eq!(fact.result_digest, result);
        assert_eq!(fact.score.unwrap().earned_milli_points, 80);
        assert_eq!(fact.retake.unwrap().remaining_attempts, Some(2));
        assert!(
            repository
                .find_latest_owned_answer_history_task_fact(UserId::new(), fixture.task)
                .await
                .unwrap()
                .is_none()
        );
        let legacy_digest =
            legacy_content_digest(&fixture.request(&replay, attempt, result)).unwrap();
        sqlx::query(
            "UPDATE answer_history_imports SET content_digest = ?, score_json = NULL, \
                    retake_json = NULL, provenance_sanitized_json = '{}', \
                    observed_at = imported_at",
        )
        .bind(legacy_digest.to_vec())
        .execute(fixture.database.pool())
        .await
        .unwrap();
        let upgrade_replay = fixture.bundle(attempt, result);
        assert_eq!(
            repository
                .ingest_answer_history_task(fixture.request(&upgrade_replay, attempt, result))
                .await
                .unwrap(),
            AnswerHistoryIngestOutcome::Duplicate(inserted_record.clone())
        );
        let upgraded = repository
            .find_latest_owned_answer_history_task_fact(fixture.owner, fixture.task)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(upgraded.retake.unwrap().remaining_attempts, Some(2));
        assert_counts(&fixture.database, (1, 2, 2, 2, 1)).await;

        let mut changed = fixture.bundle(attempt, result);
        changed.candidates[0].candidate.explanation = Some("changed parser output".to_owned());
        assert!(
            repository
                .ingest_answer_history_task(fixture.request(&changed, attempt, result))
                .await
                .is_err()
        );
        assert_counts(&fixture.database, (1, 2, 2, 2, 1)).await;
    }

    #[tokio::test]
    async fn score_and_retake_facts_persist_without_answer_candidates() {
        let fixture = Fixture::new().await;
        let repository = SqliteAnswerHistoryIngestionRepository::new(fixture.database.clone());
        let attempt = [21; 32];
        let result = [22; 32];
        let mut bundle = fixture.bundle(attempt, result);
        bundle.candidates.clear();
        bundle.evidence.clear();
        let outcome = repository
            .ingest_answer_history_task(fixture.request(&bundle, attempt, result))
            .await
            .unwrap();
        assert!(matches!(outcome, AnswerHistoryIngestOutcome::Inserted(_)));
        assert_counts(&fixture.database, (1, 0, 0, 0, 1)).await;
        let fact = repository
            .find_latest_owned_answer_history_task_fact(fixture.owner, fixture.task)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(fact.score.unwrap().earned_milli_points, 80);
        assert!(fact.retake.unwrap().allowed);
    }

    #[tokio::test]
    async fn legacy_retake_without_score_policy_upgrades_on_exact_replay() {
        let fixture = Fixture::new().await;
        let repository = SqliteAnswerHistoryIngestionRepository::new(fixture.database.clone());
        let attempt = [23; 32];
        let result = [24; 32];
        let mut bundle = fixture.bundle(attempt, result);
        bundle.retake.score_policy = asterism_domain::RetakeScorePolicy::Unknown;
        let inserted = repository
            .ingest_answer_history_task(fixture.request(&bundle, attempt, result))
            .await
            .unwrap();
        let legacy_digest =
            legacy_retake_policy_content_digest(&fixture.request(&bundle, attempt, result))
                .unwrap();
        let legacy_retake = serde_json::json!({
            "allowed": bundle.retake.allowed,
            "remaining_attempts": bundle.retake.remaining_attempts,
            "closes_at": bundle.retake.closes_at,
            "metadata_sanitized": bundle.retake.metadata_sanitized,
        });
        sqlx::query(
            "UPDATE answer_history_imports SET content_digest = ?, retake_json = ? WHERE task_id = ?",
        )
        .bind(legacy_digest.to_vec())
        .bind(serde_json::to_string(&legacy_retake).unwrap())
        .bind(fixture.task.to_string())
        .execute(fixture.database.pool())
        .await
        .unwrap();

        assert_eq!(
            repository
                .ingest_answer_history_task(fixture.request(&bundle, attempt, result))
                .await
                .unwrap(),
            match inserted {
                AnswerHistoryIngestOutcome::Inserted(record) => {
                    AnswerHistoryIngestOutcome::Duplicate(record)
                }
                AnswerHistoryIngestOutcome::Duplicate(_) => unreachable!(),
            }
        );
        let upgraded_json: String =
            sqlx::query_scalar("SELECT retake_json FROM answer_history_imports WHERE task_id = ?")
                .bind(fixture.task.to_string())
                .fetch_one(fixture.database.pool())
                .await
                .unwrap();
        assert!(upgraded_json.contains("\"score_policy\":\"unknown\""));

        let mut changed = fixture.bundle(attempt, result);
        changed.retake.score_policy = asterism_domain::RetakeScorePolicy::HighestScore;
        assert!(
            repository
                .ingest_answer_history_task(fixture.request(&changed, attempt, result))
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn late_evidence_binding_failure_rolls_back_snapshot_and_candidates() {
        let fixture = Fixture::new().await;
        let repository = SqliteAnswerHistoryIngestionRepository::new(fixture.database.clone());
        let mut bundle = fixture.bundle([3; 32], [4; 32]);
        bundle.evidence[0].answer = NormalizedAnswer::Selections(vec!["A".to_owned()]);
        assert!(
            repository
                .ingest_answer_history_task(fixture.request(&bundle, [3; 32], [4; 32]))
                .await
                .is_err()
        );
        assert_counts(&fixture.database, (0, 0, 0, 0, 0)).await;
    }

    #[tokio::test]
    async fn unverified_submitted_candidates_remain_private_without_corpus_projection() {
        let fixture = Fixture::new().await;
        let repository = SqliteAnswerHistoryIngestionRepository::new(fixture.database.clone());
        let mut bundle = fixture.bundle([5; 32], [6; 32]);
        bundle.evidence.clear();
        let outcome = repository
            .ingest_answer_history_task(fixture.request(&bundle, [5; 32], [6; 32]))
            .await
            .unwrap();
        let AnswerHistoryIngestOutcome::Inserted(record) = outcome else {
            panic!("first unverified import must insert")
        };
        assert_eq!(record.candidate_count, 2);
        assert_eq!(record.evidence_count, 0);
        assert_counts(&fixture.database, (1, 2, 0, 0, 1)).await;
    }

    async fn assert_counts(database: &Database, expected: (i64, i64, i64, i64, i64)) {
        let actual = (
            count(database, "question_snapshots").await,
            count(database, "answer_candidates").await,
            count(database, "private_answer_evidence").await,
            count(database, "global_answer_corpus_entries").await,
            count(database, "answer_history_imports").await,
        );
        assert_eq!(actual, expected);
    }

    async fn count(database: &Database, table: &str) -> i64 {
        sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {table}"))
            .fetch_one(database.pool())
            .await
            .unwrap()
    }
}
