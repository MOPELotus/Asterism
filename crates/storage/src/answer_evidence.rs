use std::{fmt::Display, str::FromStr};

use asterism_domain::{
    AnswerCandidate, AnswerEvidenceClass, AnswerSource, CorpusProjectionEligibility,
    ExecutionAttemptId, GlobalAnswerCorpusEntryId, GlobalCorpusQuestionAsset, GlobalSemanticAnswer,
    PrivateAnswerEvidence, QuestionContentFingerprint, UnmatchedEvidenceReason, UserId,
};
use async_trait::async_trait;
use serde_json::json;
use sha2::{Digest, Sha256};
use sqlx::{Row, Sqlite, Transaction};

use crate::auth_session::{decode_timestamp, encode_timestamp};
use crate::{
    AnswerEvidenceClassCounts, AnswerEvidenceProjectionState, AnswerEvidenceRecord,
    AnswerEvidenceRecordOutcome, AnswerEvidenceRepository, Database, GlobalAnswerCorpusEvidence,
    StorageError,
};

const MAX_GLOBAL_ANSWERS_PER_QUESTION: usize = 1_024;

#[derive(Clone, Debug)]
pub struct SqliteAnswerEvidenceRepository {
    database: Database,
}

impl SqliteAnswerEvidenceRepository {
    pub const fn new(database: Database) -> Self {
        Self { database }
    }
}

#[async_trait]
impl AnswerEvidenceRepository for SqliteAnswerEvidenceRepository {
    async fn record_answer_evidence(
        &self,
        evidence: &PrivateAnswerEvidence,
    ) -> Result<AnswerEvidenceRecordOutcome, StorageError> {
        let mut transaction = self.database.pool().begin_with("BEGIN IMMEDIATE").await?;
        let outcome = record_answer_evidence_in_transaction(&mut transaction, evidence).await?;
        transaction.commit().await?;
        Ok(outcome)
    }

    async fn list_global_answer_corpus_evidence(
        &self,
        question_content_fingerprint: &QuestionContentFingerprint,
    ) -> Result<Vec<GlobalAnswerCorpusEvidence>, StorageError> {
        let rows = sqlx::query(
            "SELECT id, question_content_fingerprint, question_asset_json, \
                    semantic_answer_json, official_evidence_count, \
                    verified_historical_evidence_count, negative_evidence_count, \
                    first_seen_at, last_seen_at, last_verified_at \
             FROM global_answer_corpus_entries \
             WHERE question_content_fingerprint = ? \
             ORDER BY official_evidence_count DESC, \
                      verified_historical_evidence_count DESC, \
                      negative_evidence_count ASC, id \
             LIMIT ?",
        )
        .bind(question_content_fingerprint.as_str())
        .bind(i64::try_from(MAX_GLOBAL_ANSWERS_PER_QUESTION + 1).expect("bound fits i64"))
        .fetch_all(self.database.pool())
        .await?;
        if rows.len() > MAX_GLOBAL_ANSWERS_PER_QUESTION {
            return Err(invalid_evidence("global answer set exceeds its bound"));
        }
        rows.iter().map(decode_global_evidence).collect()
    }

    async fn count_owned_execution_attempt_evidence(
        &self,
        owner_id: UserId,
        execution_attempt_id: ExecutionAttemptId,
    ) -> Result<Option<AnswerEvidenceClassCounts>, StorageError> {
        let row = sqlx::query(
            "SELECT COUNT(DISTINCT attempt.id) AS owned_attempts, \
                    COALESCE(SUM(CASE WHEN evidence.evidence_class = 'official' THEN 1 ELSE 0 END), 0) AS official, \
                    COALESCE(SUM(CASE WHEN evidence.evidence_class = 'verified_historical' THEN 1 ELSE 0 END), 0) AS verified_historical, \
                    COALESCE(SUM(CASE WHEN evidence.evidence_class = 'negative' THEN 1 ELSE 0 END), 0) AS negative \
             FROM execution_attempts AS attempt \
             INNER JOIN executions AS execution ON execution.id = attempt.execution_id \
             INNER JOIN tasks AS task ON task.id = execution.task_id \
             INNER JOIN provider_accounts AS account ON account.id = task.provider_account_id \
             LEFT JOIN private_answer_evidence AS evidence \
                    ON evidence.execution_attempt_id = attempt.id \
                   AND evidence.owner_user_id = account.owner_user_id \
             WHERE attempt.id = ? AND account.owner_user_id = ?",
        )
        .bind(execution_attempt_id.to_string())
        .bind(owner_id.to_string())
        .fetch_one(self.database.pool())
        .await?;
        if row.try_get::<i64, _>("owned_attempts")? == 0 {
            return Ok(None);
        }
        Ok(Some(AnswerEvidenceClassCounts {
            official: u64::try_from(row.try_get::<i64, _>("official")?)
                .map_err(|_| invalid_evidence("official evidence count is invalid"))?,
            verified_historical: u64::try_from(row.try_get::<i64, _>("verified_historical")?)
                .map_err(|_| invalid_evidence("verified evidence count is invalid"))?,
            negative: u64::try_from(row.try_get::<i64, _>("negative")?)
                .map_err(|_| invalid_evidence("negative evidence count is invalid"))?,
        }))
    }
}

pub(crate) async fn record_answer_evidence_in_transaction(
    transaction: &mut Transaction<'_, Sqlite>,
    evidence: &PrivateAnswerEvidence,
) -> Result<AnswerEvidenceRecordOutcome, StorageError> {
    evidence
        .validate()
        .map_err(|error| invalid_evidence(error.to_string()))?;
    let encoded = EncodedPrivateEvidence::new(evidence)?;
    validate_private_bindings(transaction, evidence).await?;
    if let Some(record) = find_record_by_digest(transaction, &encoded.digest).await? {
        return Ok(AnswerEvidenceRecordOutcome::Duplicate(record));
    }

    insert_private_evidence(transaction, evidence, &encoded).await?;
    let record = match evidence.projection {
        CorpusProjectionEligibility::Exact => {
            let (question, answer) = evidence
                .global_projection()
                .map_err(|error| invalid_evidence(error.to_string()))?;
            let corpus_entry_id =
                project_global_evidence(transaction, evidence, &question, &answer).await?;
            sqlx::query(
                "INSERT INTO global_answer_corpus_projections \
                 (private_evidence_id, corpus_entry_id, projected_at) VALUES (?, ?, ?)",
            )
            .bind(evidence.id.to_string())
            .bind(corpus_entry_id.to_string())
            .bind(encode_timestamp(evidence.verified_at))
            .execute(&mut **transaction)
            .await?;
            AnswerEvidenceRecord {
                private_evidence_id: evidence.id,
                corpus_entry_id: Some(corpus_entry_id),
                projection_state: AnswerEvidenceProjectionState::Projected,
            }
        }
        CorpusProjectionEligibility::Unmatched(reason) => AnswerEvidenceRecord {
            private_evidence_id: evidence.id,
            corpus_entry_id: None,
            projection_state: AnswerEvidenceProjectionState::Unmatched(reason),
        },
    };
    Ok(AnswerEvidenceRecordOutcome::Inserted(record))
}

struct EncodedPrivateEvidence {
    digest: [u8; 32],
    question_json: String,
    answer_json: String,
    provenance_json: String,
}

impl EncodedPrivateEvidence {
    fn new(evidence: &PrivateAnswerEvidence) -> Result<Self, StorageError> {
        let question_json = serde_json::to_string(&evidence.question)?;
        let answer_json = serde_json::to_string(&evidence.answer)?;
        let provenance_json = serde_json::to_string(&evidence.provenance_sanitized)?;
        let stable_fact = json!({
            "owner_user_id": evidence.owner_user_id,
            "provider_id": evidence.provider_id,
            "provider_account_id": evidence.provider_account_id,
            "course_id": evidence.course_id,
            "task_id": evidence.task_id,
            "question_snapshot_id": evidence.question_snapshot_id,
            "question_id": evidence.question_id,
            "execution_attempt_id": evidence.execution_attempt_id,
            "provider_attempt_digest": evidence.provider_attempt_digest,
            "source_candidate_id": evidence.source_candidate_id,
            "question_content_fingerprint": evidence.question_content_fingerprint,
            "answer": evidence.answer,
            "answer_source": evidence.answer_source,
            "evidence_class": evidence.evidence_class,
            "result_digest": evidence.result_digest,
            "projection": evidence.projection,
        });
        let digest = Sha256::digest(serde_json::to_vec(&stable_fact)?).into();
        Ok(Self {
            digest,
            question_json,
            answer_json,
            provenance_json,
        })
    }
}

async fn validate_private_bindings(
    transaction: &mut Transaction<'_, Sqlite>,
    evidence: &PrivateAnswerEvidence,
) -> Result<(), StorageError> {
    let row = sqlx::query(
        "SELECT account.owner_user_id, account.provider_id, task.course_id, \
                item.question_json, item.content_fingerprint \
         FROM question_snapshot_items AS item \
         INNER JOIN question_snapshots AS snapshot ON snapshot.id = item.snapshot_id \
         INNER JOIN tasks AS task ON task.id = snapshot.task_id \
         INNER JOIN provider_accounts AS account ON account.id = task.provider_account_id \
         WHERE account.id = ? AND task.id = ? AND snapshot.id = ? AND item.question_id = ?",
    )
    .bind(evidence.provider_account_id.to_string())
    .bind(evidence.task_id.to_string())
    .bind(evidence.question_snapshot_id.to_string())
    .bind(evidence.question_id.to_string())
    .fetch_optional(&mut **transaction)
    .await?
    .ok_or_else(|| invalid_evidence("question ownership binding is invalid"))?;

    let stored_question: asterism_domain::Question =
        serde_json::from_str(row.try_get("question_json")?)?;
    let stored_course = row
        .try_get::<Option<&str>, _>("course_id")?
        .map(parse_id)
        .transpose()?;
    if row.try_get::<&str, _>("owner_user_id")? != evidence.owner_user_id.to_string()
        || row.try_get::<&str, _>("provider_id")? != evidence.provider_id.as_str()
        || stored_course != evidence.course_id
        || stored_question != evidence.question
        || row.try_get::<Option<&str>, _>("content_fingerprint")?
            != Some(evidence.question_content_fingerprint.as_str())
    {
        return Err(invalid_evidence("question ownership binding is invalid"));
    }

    if let Some(candidate_id) = evidence.source_candidate_id {
        let candidate_json: String = sqlx::query_scalar(
            "SELECT candidate_json FROM answer_candidates \
             WHERE question_snapshot_id = ? AND id = ? AND question_id = ?",
        )
        .bind(evidence.question_snapshot_id.to_string())
        .bind(candidate_id.to_string())
        .bind(evidence.question_id.to_string())
        .fetch_optional(&mut **transaction)
        .await?
        .ok_or_else(|| invalid_evidence("answer candidate binding is invalid"))?;
        let candidate: AnswerCandidate = serde_json::from_str(&candidate_json)?;
        if candidate.question_id != evidence.question_id
            || candidate.answer != evidence.answer
            || candidate.source != evidence.answer_source
            || candidate.validate().is_err()
        {
            return Err(invalid_evidence("answer candidate binding is invalid"));
        }
    }

    if let Some(attempt_id) = evidence.execution_attempt_id {
        let attempt_task: String = sqlx::query_scalar(
            "SELECT execution.task_id FROM execution_attempts AS attempt \
             INNER JOIN executions AS execution ON execution.id = attempt.execution_id \
             WHERE attempt.id = ?",
        )
        .bind(attempt_id.to_string())
        .fetch_optional(&mut **transaction)
        .await?
        .ok_or_else(|| invalid_evidence("execution attempt binding is invalid"))?;
        if attempt_task != evidence.task_id.to_string() {
            return Err(invalid_evidence("execution attempt binding is invalid"));
        }
    }
    Ok(())
}

async fn insert_private_evidence(
    transaction: &mut Transaction<'_, Sqlite>,
    evidence: &PrivateAnswerEvidence,
    encoded: &EncodedPrivateEvidence,
) -> Result<(), StorageError> {
    let (projection_state, unmatched_reason) = encode_projection(evidence.projection);
    sqlx::query(
        "INSERT INTO private_answer_evidence \
         (id, evidence_digest, owner_user_id, provider_id, provider_account_id, course_id, \
          task_id, question_snapshot_id, question_id, execution_attempt_id, \
          provider_attempt_digest, \
          source_candidate_id, question_json, question_content_fingerprint, answer_json, \
          answer_source, evidence_class, result_digest, provenance_sanitized_json, \
          projection_state, unmatched_reason, observed_at, verified_at, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(evidence.id.to_string())
    .bind(encoded.digest.to_vec())
    .bind(evidence.owner_user_id.to_string())
    .bind(evidence.provider_id.as_str())
    .bind(evidence.provider_account_id.to_string())
    .bind(evidence.course_id.map(|id| id.to_string()))
    .bind(evidence.task_id.to_string())
    .bind(evidence.question_snapshot_id.to_string())
    .bind(evidence.question_id.to_string())
    .bind(evidence.execution_attempt_id.map(|id| id.to_string()))
    .bind(
        evidence
            .provider_attempt_digest
            .map(|digest| digest.to_vec()),
    )
    .bind(evidence.source_candidate_id.map(|id| id.to_string()))
    .bind(&encoded.question_json)
    .bind(evidence.question_content_fingerprint.as_str())
    .bind(&encoded.answer_json)
    .bind(encode_answer_source(evidence.answer_source))
    .bind(encode_evidence_class(evidence.evidence_class))
    .bind(evidence.result_digest.map(|digest| digest.to_vec()))
    .bind(&encoded.provenance_json)
    .bind(projection_state)
    .bind(unmatched_reason)
    .bind(encode_timestamp(evidence.observed_at))
    .bind(encode_timestamp(evidence.verified_at))
    .bind(encode_timestamp(evidence.verified_at))
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

async fn find_record_by_digest(
    transaction: &mut Transaction<'_, Sqlite>,
    digest: &[u8; 32],
) -> Result<Option<AnswerEvidenceRecord>, StorageError> {
    let row = sqlx::query(
        "SELECT private.id, private.projection_state, private.unmatched_reason, \
                projection.corpus_entry_id \
         FROM private_answer_evidence AS private \
         LEFT JOIN global_answer_corpus_projections AS projection \
            ON projection.private_evidence_id = private.id \
         WHERE private.evidence_digest = ?",
    )
    .bind(digest.to_vec())
    .fetch_optional(&mut **transaction)
    .await?;
    row.as_ref().map(decode_private_record).transpose()
}

async fn project_global_evidence(
    transaction: &mut Transaction<'_, Sqlite>,
    evidence: &PrivateAnswerEvidence,
    question: &GlobalCorpusQuestionAsset,
    answer: &GlobalSemanticAnswer,
) -> Result<GlobalAnswerCorpusEntryId, StorageError> {
    let question_json = serde_json::to_string(question)?;
    let answer_json = serde_json::to_string(answer)?;
    let answer_digest: [u8; 32] = Sha256::digest(answer_json.as_bytes()).into();
    let proposed_id = GlobalAnswerCorpusEntryId::new();
    let (official, historical, negative) = evidence_count_delta(evidence.evidence_class);
    let seen_at = encode_timestamp(evidence.observed_at);
    let verified_at = encode_timestamp(evidence.verified_at);
    let insert = sqlx::query(
        "INSERT INTO global_answer_corpus_entries \
         (id, question_content_fingerprint, question_asset_json, semantic_answer_digest, \
          semantic_answer_json, official_evidence_count, verified_historical_evidence_count, \
          negative_evidence_count, first_seen_at, last_seen_at, last_verified_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
         ON CONFLICT(question_content_fingerprint, semantic_answer_digest) DO NOTHING",
    )
    .bind(proposed_id.to_string())
    .bind(evidence.question_content_fingerprint.as_str())
    .bind(&question_json)
    .bind(answer_digest.to_vec())
    .bind(&answer_json)
    .bind(official)
    .bind(historical)
    .bind(negative)
    .bind(&seen_at)
    .bind(&seen_at)
    .bind(&verified_at)
    .execute(&mut **transaction)
    .await?;
    if insert.rows_affected() == 1 {
        return Ok(proposed_id);
    }

    let row = sqlx::query(
        "SELECT id, question_asset_json, semantic_answer_json \
         FROM global_answer_corpus_entries \
         WHERE question_content_fingerprint = ? AND semantic_answer_digest = ?",
    )
    .bind(evidence.question_content_fingerprint.as_str())
    .bind(answer_digest.to_vec())
    .fetch_one(&mut **transaction)
    .await?;
    let existing_question: GlobalCorpusQuestionAsset =
        serde_json::from_str(row.try_get("question_asset_json")?)?;
    let existing_answer: GlobalSemanticAnswer =
        serde_json::from_str(row.try_get("semantic_answer_json")?)?;
    if existing_question != *question || existing_answer != *answer {
        return Err(invalid_evidence("global semantic identity collision"));
    }
    let entry_id: GlobalAnswerCorpusEntryId = parse_id(row.try_get("id")?)?;
    sqlx::query(
        "UPDATE global_answer_corpus_entries \
         SET official_evidence_count = official_evidence_count + ?, \
             verified_historical_evidence_count = verified_historical_evidence_count + ?, \
             negative_evidence_count = negative_evidence_count + ?, \
             last_seen_at = max(last_seen_at, ?), \
             last_verified_at = max(COALESCE(last_verified_at, ?), ?) \
         WHERE id = ?",
    )
    .bind(official)
    .bind(historical)
    .bind(negative)
    .bind(&seen_at)
    .bind(&verified_at)
    .bind(&verified_at)
    .bind(entry_id.to_string())
    .execute(&mut **transaction)
    .await?;
    Ok(entry_id)
}

fn decode_private_record(
    row: &sqlx::sqlite::SqliteRow,
) -> Result<AnswerEvidenceRecord, StorageError> {
    let state = match row.try_get::<&str, _>("projection_state")? {
        "projected" => AnswerEvidenceProjectionState::Projected,
        "unmatched" => AnswerEvidenceProjectionState::Unmatched(decode_unmatched_reason(
            row.try_get::<Option<&str>, _>("unmatched_reason")?
                .ok_or_else(|| invalid_evidence("missing unmatched reason"))?,
        )?),
        _ => return Err(invalid_evidence("invalid projection state")),
    };
    let corpus_entry_id = row
        .try_get::<Option<&str>, _>("corpus_entry_id")?
        .map(parse_id)
        .transpose()?;
    if matches!(state, AnswerEvidenceProjectionState::Projected) != corpus_entry_id.is_some() {
        return Err(invalid_evidence("projection record is inconsistent"));
    }
    Ok(AnswerEvidenceRecord {
        private_evidence_id: parse_id(row.try_get("id")?)?,
        corpus_entry_id,
        projection_state: state,
    })
}

fn decode_global_evidence(
    row: &sqlx::sqlite::SqliteRow,
) -> Result<GlobalAnswerCorpusEvidence, StorageError> {
    Ok(GlobalAnswerCorpusEvidence {
        corpus_entry_id: parse_id(row.try_get("id")?)?,
        question_content_fingerprint: parse_id(row.try_get("question_content_fingerprint")?)?,
        question: serde_json::from_str(row.try_get("question_asset_json")?)?,
        answer: serde_json::from_str(row.try_get("semantic_answer_json")?)?,
        official_evidence_count: decode_count(row.try_get("official_evidence_count")?)?,
        verified_historical_evidence_count: decode_count(
            row.try_get("verified_historical_evidence_count")?,
        )?,
        negative_evidence_count: decode_count(row.try_get("negative_evidence_count")?)?,
        first_seen_at: decode_timestamp(row.try_get("first_seen_at")?)?,
        last_seen_at: decode_timestamp(row.try_get("last_seen_at")?)?,
        last_verified_at: row
            .try_get::<Option<&str>, _>("last_verified_at")?
            .map(decode_timestamp)
            .transpose()?,
    })
}

const fn evidence_count_delta(class: AnswerEvidenceClass) -> (i64, i64, i64) {
    match class {
        AnswerEvidenceClass::Official => (1, 0, 0),
        AnswerEvidenceClass::VerifiedHistorical => (0, 1, 0),
        AnswerEvidenceClass::Negative => (0, 0, 1),
    }
}

const fn encode_projection(
    projection: CorpusProjectionEligibility,
) -> (&'static str, Option<&'static str>) {
    match projection {
        CorpusProjectionEligibility::Exact => ("projected", None),
        CorpusProjectionEligibility::Unmatched(reason) => {
            ("unmatched", Some(encode_unmatched_reason(reason)))
        }
    }
}

const fn encode_unmatched_reason(reason: UnmatchedEvidenceReason) -> &'static str {
    match reason {
        UnmatchedEvidenceReason::IncompleteQuestion => "incomplete_question",
        UnmatchedEvidenceReason::MissingSharedContext => "missing_shared_context",
        UnmatchedEvidenceReason::AmbiguousSemanticIdentity => "ambiguous_semantic_identity",
        UnmatchedEvidenceReason::UnsupportedHierarchy => "unsupported_hierarchy",
    }
}

fn decode_unmatched_reason(value: &str) -> Result<UnmatchedEvidenceReason, StorageError> {
    match value {
        "incomplete_question" => Ok(UnmatchedEvidenceReason::IncompleteQuestion),
        "missing_shared_context" => Ok(UnmatchedEvidenceReason::MissingSharedContext),
        "ambiguous_semantic_identity" => Ok(UnmatchedEvidenceReason::AmbiguousSemanticIdentity),
        "unsupported_hierarchy" => Ok(UnmatchedEvidenceReason::UnsupportedHierarchy),
        _ => Err(invalid_evidence("invalid unmatched reason")),
    }
}

const fn encode_answer_source(source: AnswerSource) -> &'static str {
    match source {
        AnswerSource::Manual => "manual",
        AnswerSource::LocalCache => "local_cache",
        AnswerSource::ProviderNative => "provider_native",
        AnswerSource::Ai => "ai",
        AnswerSource::ExternalBank => "external_bank",
        AnswerSource::Other => "other",
    }
}

const fn encode_evidence_class(class: AnswerEvidenceClass) -> &'static str {
    match class {
        AnswerEvidenceClass::Official => "official",
        AnswerEvidenceClass::VerifiedHistorical => "verified_historical",
        AnswerEvidenceClass::Negative => "negative",
    }
}

fn parse_id<T>(value: &str) -> Result<T, StorageError>
where
    T: FromStr,
    T::Err: Display,
{
    value
        .parse()
        .map_err(|error: T::Err| invalid_evidence(error.to_string()))
}

fn decode_count(value: i64) -> Result<u64, StorageError> {
    u64::try_from(value).map_err(|_| invalid_evidence("negative evidence count"))
}

fn invalid_evidence(message: impl Into<String>) -> StorageError {
    StorageError::InvalidData(format!("Answer Evidence is invalid: {}", message.into()))
}

#[cfg(test)]
mod tests {
    use asterism_domain::{
        AnswerCandidateId, AnswerConfidence, CourseId, ExecutionAttemptId, ExecutionId,
        NormalizedAnswer, PrivateAnswerEvidenceId, ProviderAccountId, ProviderId, Question,
        QuestionId, QuestionKind, QuestionOption, QuestionSnapshotId, TaskId, Timestamp, UserId,
    };
    use chrono::{Duration, Utc};

    use crate::{
        AnswerCandidateRecord, AnswerCandidateRepository, QuestionSnapshot,
        QuestionSnapshotRepository, SqliteQuestionSnapshotRepository,
    };

    use super::*;

    struct Fixture {
        owner: UserId,
        account: ProviderAccountId,
        task: TaskId,
        snapshot: QuestionSnapshot,
        candidate: AnswerCandidateRecord,
        attempt: ExecutionAttemptId,
        now: Timestamp,
    }

    async fn insert_identity_task(
        database: &Database,
        username: &str,
        provider: &ProviderId,
        now: Timestamp,
    ) -> (UserId, ProviderAccountId, TaskId) {
        let owner = UserId::new();
        let account = ProviderAccountId::new();
        let task = TaskId::new();
        let timestamp = encode_timestamp(now);
        sqlx::query(
            "INSERT INTO users \
             (id, username, password_hash, status, roles_json, permissions_json, created_at, updated_at) \
             VALUES (?, ?, '$argon2id$test', 'active', '[\"user\"]', '[]', ?, ?)",
        )
        .bind(owner.to_string())
        .bind(username)
        .bind(&timestamp)
        .bind(&timestamp)
        .execute(database.pool())
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO provider_accounts \
             (id, owner_user_id, provider_id, display_name, auth_state_json, created_at, updated_at) \
             VALUES (?, ?, ?, 'evidence', '\"authenticated\"', ?, ?)",
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
             VALUES (?, ?, ?, 'v1:test', 'work', 'routine', 'Evidence', \
                     'completed', 'succeeded', ?, ?, '[]')",
        )
        .bind(task.to_string())
        .bind(account.to_string())
        .bind(format!("remote-{username}"))
        .bind(&timestamp)
        .bind(&timestamp)
        .execute(database.pool())
        .await
        .unwrap();
        (owner, account, task)
    }

    async fn insert_attempt(
        database: &Database,
        owner: UserId,
        task: TaskId,
        now: Timestamp,
    ) -> ExecutionAttemptId {
        let execution = ExecutionId::new();
        let attempt = ExecutionAttemptId::new();
        let timestamp = encode_timestamp(now);
        sqlx::query(
            "INSERT INTO executions \
             (id, task_id, requested_by, request_source, state, started_at, created_at) \
             VALUES (?, ?, ?, 'manual', 'running', ?, ?)",
        )
        .bind(execution.to_string())
        .bind(task.to_string())
        .bind(owner.to_string())
        .bind(&timestamp)
        .bind(&timestamp)
        .execute(database.pool())
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO execution_attempts (id, execution_id, attempt_no, started_at) \
             VALUES (?, ?, 1, ?)",
        )
        .bind(attempt.to_string())
        .bind(execution.to_string())
        .bind(&timestamp)
        .execute(database.pool())
        .await
        .unwrap();
        attempt
    }

    impl Fixture {
        async fn insert(database: &Database, username: &str, kind: QuestionKind) -> Self {
            let provider = ProviderId::new("provider-alpha").unwrap();
            let now = Utc::now();
            let (owner, account, task) =
                insert_identity_task(database, username, &provider, now).await;

            let (options, answer) = match kind {
                QuestionKind::Composite => (
                    Vec::new(),
                    NormalizedAnswer::Composite(vec![NormalizedAnswer::Boolean(true)]),
                ),
                _ => (
                    vec![
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
                    NormalizedAnswer::Selections(vec!["B".to_owned()]),
                ),
            };
            let question = Question {
                id: QuestionId::new(),
                task_id: task,
                remote_question_id: Some(format!("question-{username}")),
                kind,
                stem: "Which value is correct?".to_owned(),
                options,
                attachments: Vec::new(),
                metadata_sanitized: json!({}),
                position: 1,
            };
            let snapshot = QuestionSnapshot {
                id: QuestionSnapshotId::new(),
                task_id: task,
                provider_id: provider.clone(),
                provider_version: "1.0-test".to_owned(),
                captured_at: now,
                questions: vec![question.clone()],
                groups: Vec::new(),
            };
            let candidate = AnswerCandidateRecord {
                id: AnswerCandidateId::new(),
                question_snapshot_id: snapshot.id,
                candidate: AnswerCandidate {
                    question_id: question.id,
                    source: AnswerSource::Manual,
                    answer,
                    confidence: Some(AnswerConfidence::try_new(10_000).unwrap()),
                    explanation: None,
                    provenance_sanitized: json!({"surface": "result_page"}),
                },
                created_at: now,
            };
            let question_repository = SqliteQuestionSnapshotRepository::new(database.clone());
            question_repository
                .save_question_snapshot(&snapshot)
                .await
                .unwrap();
            question_repository
                .save_answer_candidate_batch(std::slice::from_ref(&candidate))
                .await
                .unwrap();

            let attempt = insert_attempt(database, owner, task, now).await;
            Self {
                owner,
                account,
                task,
                snapshot,
                candidate,
                attempt,
                now,
            }
        }

        fn evidence(&self, projection: CorpusProjectionEligibility) -> PrivateAnswerEvidence {
            let question = self.snapshot.questions[0].clone();
            PrivateAnswerEvidence {
                id: PrivateAnswerEvidenceId::new(),
                owner_user_id: self.owner,
                provider_id: self.snapshot.provider_id.clone(),
                provider_account_id: self.account,
                course_id: None::<CourseId>,
                task_id: self.task,
                question_snapshot_id: self.snapshot.id,
                question_id: question.id,
                execution_attempt_id: Some(self.attempt),
                provider_attempt_digest: None,
                source_candidate_id: Some(self.candidate.id),
                question,
                question_content_fingerprint: self.snapshot.questions[0]
                    .content_fingerprint()
                    .unwrap(),
                answer: self.candidate.candidate.answer.clone(),
                answer_source: self.candidate.candidate.source,
                evidence_class: AnswerEvidenceClass::VerifiedHistorical,
                result_digest: Some([7; 32]),
                provenance_sanitized: json!({"surface": "result_page"}),
                projection,
                observed_at: self.now,
                verified_at: self.now + Duration::seconds(1),
            }
        }
    }

    #[tokio::test]
    async fn exact_evidence_is_idempotent_and_deidentified_across_owners() {
        let database = Database::connect("sqlite::memory:").await.unwrap();
        database.migrate().await.unwrap();
        let first =
            Fixture::insert(&database, "evidence-owner-one", QuestionKind::SingleChoice).await;
        let second =
            Fixture::insert(&database, "evidence-owner-two", QuestionKind::SingleChoice).await;
        let repository = SqliteAnswerEvidenceRepository::new(database.clone());

        let evidence = first.evidence(CorpusProjectionEligibility::Exact);
        let inserted = repository.record_answer_evidence(&evidence).await.unwrap();
        let mut replay = evidence.clone();
        replay.id = PrivateAnswerEvidenceId::new();
        replay.observed_at += Duration::minutes(1);
        replay.verified_at += Duration::minutes(1);
        replay.provenance_sanitized = json!({"surface": "same_result_replayed"});
        let duplicate = repository.record_answer_evidence(&replay).await.unwrap();
        assert!(matches!(inserted, AnswerEvidenceRecordOutcome::Inserted(_)));
        assert!(matches!(
            duplicate,
            AnswerEvidenceRecordOutcome::Duplicate(_)
        ));

        repository
            .record_answer_evidence(&second.evidence(CorpusProjectionEligibility::Exact))
            .await
            .unwrap();
        let global = repository
            .list_global_answer_corpus_evidence(&evidence.question_content_fingerprint)
            .await
            .unwrap();
        assert_eq!(global.len(), 1);
        assert_eq!(global[0].verified_historical_evidence_count, 2);
        assert_eq!(
            global[0].answer,
            GlobalSemanticAnswer::Selections(vec!["Beta".to_owned()])
        );

        let global_payloads: String = sqlx::query_scalar(
            "SELECT question_asset_json || semantic_answer_json FROM global_answer_corpus_entries",
        )
        .fetch_one(database.pool())
        .await
        .unwrap();
        for private_identity in [
            first.owner.to_string(),
            first.account.to_string(),
            second.owner.to_string(),
            second.account.to_string(),
            "evidence-owner-one".to_owned(),
            "evidence-owner-two".to_owned(),
        ] {
            assert!(!global_payloads.contains(&private_identity));
        }
    }

    #[tokio::test]
    async fn negative_and_unmatched_evidence_are_preserved_without_guessing() {
        let database = Database::connect("sqlite::memory:").await.unwrap();
        database.migrate().await.unwrap();
        let exact = Fixture::insert(&database, "negative-owner", QuestionKind::SingleChoice).await;
        let unmatched =
            Fixture::insert(&database, "unmatched-owner", QuestionKind::Composite).await;
        let repository = SqliteAnswerEvidenceRepository::new(database.clone());

        let mut negative = exact.evidence(CorpusProjectionEligibility::Exact);
        negative.evidence_class = AnswerEvidenceClass::Negative;
        repository.record_answer_evidence(&negative).await.unwrap();
        let global = repository
            .list_global_answer_corpus_evidence(&negative.question_content_fingerprint)
            .await
            .unwrap();
        assert_eq!(global[0].negative_evidence_count, 1);
        assert_eq!(global[0].verified_historical_evidence_count, 0);

        let unmatched = unmatched.evidence(CorpusProjectionEligibility::Unmatched(
            UnmatchedEvidenceReason::UnsupportedHierarchy,
        ));
        let outcome = repository.record_answer_evidence(&unmatched).await.unwrap();
        assert!(matches!(
            outcome,
            AnswerEvidenceRecordOutcome::Inserted(AnswerEvidenceRecord {
                corpus_entry_id: None,
                projection_state: AnswerEvidenceProjectionState::Unmatched(
                    UnmatchedEvidenceReason::UnsupportedHierarchy
                ),
                ..
            })
        ));
        let private_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM private_answer_evidence")
            .fetch_one(database.pool())
            .await
            .unwrap();
        let global_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM global_answer_corpus_entries")
                .fetch_one(database.pool())
                .await
                .unwrap();
        assert_eq!(private_count, 2);
        assert_eq!(global_count, 1);
    }

    #[tokio::test]
    async fn foreign_owner_and_attempt_bindings_fail_without_partial_writes() {
        let database = Database::connect("sqlite::memory:").await.unwrap();
        database.migrate().await.unwrap();
        let first =
            Fixture::insert(&database, "binding-owner-one", QuestionKind::SingleChoice).await;
        let second =
            Fixture::insert(&database, "binding-owner-two", QuestionKind::SingleChoice).await;
        let repository = SqliteAnswerEvidenceRepository::new(database.clone());

        let mut foreign_owner = first.evidence(CorpusProjectionEligibility::Exact);
        foreign_owner.owner_user_id = second.owner;
        assert!(
            repository
                .record_answer_evidence(&foreign_owner)
                .await
                .is_err()
        );

        let mut foreign_attempt = first.evidence(CorpusProjectionEligibility::Exact);
        foreign_attempt.execution_attempt_id = Some(second.attempt);
        assert!(
            repository
                .record_answer_evidence(&foreign_attempt)
                .await
                .is_err()
        );

        let private_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM private_answer_evidence")
            .fetch_one(database.pool())
            .await
            .unwrap();
        let global_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM global_answer_corpus_entries")
                .fetch_one(database.pool())
                .await
                .unwrap();
        assert_eq!((private_count, global_count), (0, 0));
    }

    #[tokio::test]
    async fn provider_history_attempt_does_not_require_a_local_execution_attempt() {
        let database = Database::connect("sqlite::memory:").await.unwrap();
        database.migrate().await.unwrap();
        let fixture = Fixture::insert(
            &database,
            "bootstrap-history-owner",
            QuestionKind::SingleChoice,
        )
        .await;
        let repository = SqliteAnswerEvidenceRepository::new(database.clone());
        let mut evidence = fixture.evidence(CorpusProjectionEligibility::Exact);
        evidence.execution_attempt_id = None;
        evidence.provider_attempt_digest = Some([9; 32]);
        let outcome = repository.record_answer_evidence(&evidence).await.unwrap();
        assert!(matches!(outcome, AnswerEvidenceRecordOutcome::Inserted(_)));
        let stored_digest: Vec<u8> = sqlx::query_scalar(
            "SELECT provider_attempt_digest FROM private_answer_evidence WHERE id = ?",
        )
        .bind(evidence.id.to_string())
        .fetch_one(database.pool())
        .await
        .unwrap();
        assert_eq!(stored_digest, vec![9; 32]);
    }
}
