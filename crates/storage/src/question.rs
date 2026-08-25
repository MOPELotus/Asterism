use std::{
    collections::{BTreeMap, BTreeSet},
    str::FromStr,
};

use asterism_domain::{
    AnswerCandidate, AnswerCandidateId, AnswerEvidenceClass, AnswerSource,
    CorpusProjectionEligibility, CourseId, ExecutionAttemptId, ExecutionId, PrivateAnswerEvidence,
    PrivateAnswerEvidenceId, ProviderAccountId, ProviderId, Question, QuestionContentFingerprint,
    QuestionGroup, QuestionGroupChild, QuestionGroupId, QuestionId, QuestionSemanticFingerprint,
    QuestionSetView, QuestionSnapshotId, SelectedAnswer, SubmissionAnswerCoverage, SubmissionDraft,
    SubmissionDraftId, SubmissionDraftItem, SubmissionPayloadPreview,
    SubmissionQuestionVerificationStatus, SubmissionReceipt, SubmissionResult, SubmissionResultId,
    SubmissionResultStatus, SubmissionVerificationSnapshot, TaskId, Timestamp,
    UnmatchedEvidenceReason, UserId, validate_question_groups,
};
use async_trait::async_trait;
use chrono::{DateTime, SecondsFormat, Utc};
use serde_json::json;
use sha2::{Digest, Sha256};
use sqlx::{Row, Sqlite, Transaction, sqlite::SqliteRow};

use crate::answer_evidence::record_answer_evidence_in_transaction;
use crate::{
    AnswerCacheRepository, AnswerCandidateRecord, AnswerCandidateRepository, Database,
    PriorAnswerEvidence, QuestionSnapshot, QuestionSnapshotRepository, StorageError,
    SubmissionDraftRepository, SubmissionResultRepository,
};

const MAX_QUESTIONS_PER_SNAPSHOT: usize = 5_000;
const MAX_QUESTION_SNAPSHOT_BYTES: usize = 16 * 1_024 * 1_024;
const MAX_QUESTION_GROUP_BYTES: usize = 16 * 1_024 * 1_024;
const MAX_PROVIDER_VERSION_BYTES: usize = 128;
const MAX_CANDIDATES_PER_SNAPSHOT: usize = 20_000;
const MAX_ANSWER_CANDIDATE_BYTES: usize = 16 * 1_024 * 1_024;
const MAX_PRIOR_ANSWER_EVIDENCE: usize = 20_000;
const MAX_SUBMISSION_DRAFT_ITEMS: usize = 5_000;
const MAX_SUBMISSION_PREVIEW_BYTES: usize = 8 * 1_024 * 1_024;
const MAX_SUBMISSION_RECEIPT_BYTES: usize = 64 * 1_024;
const MAX_SUBMISSION_VERIFICATION_BYTES: usize = 8 * 1_024 * 1_024;

#[derive(Clone, Debug)]
pub struct SqliteQuestionSnapshotRepository {
    database: Database,
}

impl SqliteQuestionSnapshotRepository {
    pub const fn new(database: Database) -> Self {
        Self { database }
    }
}

#[async_trait]
impl QuestionSnapshotRepository for SqliteQuestionSnapshotRepository {
    async fn save_question_snapshot(
        &self,
        snapshot: &QuestionSnapshot,
    ) -> Result<(), StorageError> {
        let mut transaction = self.database.pool().begin().await?;
        save_question_snapshot_in_transaction(&mut transaction, snapshot).await?;
        transaction.commit().await?;
        Ok(())
    }

    async fn find_owned_question_snapshot(
        &self,
        owner_id: UserId,
        question_snapshot_id: QuestionSnapshotId,
    ) -> Result<Option<QuestionSnapshot>, StorageError> {
        let row = sqlx::query(
            "SELECT snapshot.id, snapshot.task_id, snapshot.provider_id, \
                    snapshot.provider_version, snapshot.captured_at, \
                    snapshot.question_count, snapshot.group_count, snapshot.total_bytes \
             FROM question_snapshots AS snapshot \
             INNER JOIN tasks AS task ON task.id = snapshot.task_id \
             INNER JOIN provider_accounts AS account \
                ON account.id = task.provider_account_id \
             WHERE account.owner_user_id = ? AND snapshot.id = ?",
        )
        .bind(owner_id.to_string())
        .bind(question_snapshot_id.to_string())
        .fetch_optional(self.database.pool())
        .await?;
        match row {
            Some(row) => Ok(Some(decode_question_snapshot(&self.database, &row).await?)),
            None => Ok(None),
        }
    }

    async fn find_latest_owned_question_snapshot(
        &self,
        owner_id: UserId,
        task_id: TaskId,
    ) -> Result<Option<QuestionSnapshot>, StorageError> {
        let row = sqlx::query(
            "SELECT snapshot.id, snapshot.task_id, snapshot.provider_id, \
                    snapshot.provider_version, snapshot.captured_at, \
                    snapshot.question_count, snapshot.group_count, snapshot.total_bytes \
             FROM question_snapshots AS snapshot \
             INNER JOIN tasks AS task ON task.id = snapshot.task_id \
             INNER JOIN provider_accounts AS account \
                ON account.id = task.provider_account_id \
             WHERE account.owner_user_id = ? AND snapshot.task_id = ? \
             ORDER BY snapshot.captured_at DESC, snapshot.id DESC LIMIT 1",
        )
        .bind(owner_id.to_string())
        .bind(task_id.to_string())
        .fetch_optional(self.database.pool())
        .await?;
        match row {
            Some(row) => Ok(Some(decode_question_snapshot(&self.database, &row).await?)),
            None => Ok(None),
        }
    }
}

pub(crate) async fn save_question_snapshot_in_transaction(
    transaction: &mut Transaction<'_, Sqlite>,
    snapshot: &QuestionSnapshot,
) -> Result<(), StorageError> {
    let encoded = validate_and_encode(snapshot)?;
    let actual_provider: Option<String> = sqlx::query_scalar(
        "SELECT account.provider_id FROM tasks AS task \
         INNER JOIN provider_accounts AS account ON account.id = task.provider_account_id \
         WHERE task.id = ?",
    )
    .bind(snapshot.task_id.to_string())
    .fetch_optional(&mut **transaction)
    .await?;
    if actual_provider.as_deref() != Some(snapshot.provider_id.as_str()) {
        return Err(StorageError::InvalidData(
            "question snapshot task/provider binding is invalid".to_owned(),
        ));
    }
    sqlx::query(
        "INSERT INTO question_snapshots \
         (id, task_id, provider_id, provider_version, captured_at, question_count, group_count, \
          total_bytes) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(snapshot.id.to_string())
    .bind(snapshot.task_id.to_string())
    .bind(snapshot.provider_id.as_str())
    .bind(&snapshot.provider_version)
    .bind(encode_timestamp(snapshot.captured_at))
    .bind(i64::try_from(encoded.len()).expect("bounded Question count fits i64"))
    .bind(i64::try_from(encoded.groups.len()).expect("bounded Question group count fits i64"))
    .bind(
        i64::try_from(encoded.total_bytes).expect("bounded Question snapshot byte count fits i64"),
    )
    .execute(&mut **transaction)
    .await?;
    for question in encoded.questions {
        sqlx::query(
            "INSERT INTO question_snapshot_items \
             (snapshot_id, question_id, remote_question_id, position, question_json, \
              content_fingerprint, semantic_fingerprint) VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(snapshot.id.to_string())
        .bind(question.id.to_string())
        .bind(question.remote_id)
        .bind(i64::from(question.position))
        .bind(question.json)
        .bind(question.content_fingerprint.as_str())
        .bind(question.semantic_fingerprint.as_str())
        .execute(&mut **transaction)
        .await?;
    }
    for (index, group) in encoded.groups.into_iter().enumerate() {
        sqlx::query(
            "INSERT INTO question_snapshot_groups \
             (snapshot_id, group_id, ordinal, remote_group_id, group_json) \
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(snapshot.id.to_string())
        .bind(group.id.to_string())
        .bind(i64::try_from(index + 1).expect("bounded Question group ordinal fits i64"))
        .bind(group.remote_id)
        .bind(group.json)
        .execute(&mut **transaction)
        .await?;
    }
    Ok(())
}

async fn decode_question_snapshot(
    database: &Database,
    row: &SqliteRow,
) -> Result<QuestionSnapshot, StorageError> {
    let snapshot_id = parse_id::<QuestionSnapshotId>(row.try_get("id")?)?;
    let stored_count = usize::try_from(row.try_get::<i64, _>("question_count")?)
        .map_err(|_| invalid_snapshot())?;
    let stored_group_count =
        usize::try_from(row.try_get::<i64, _>("group_count")?).map_err(|_| invalid_snapshot())?;
    let stored_bytes =
        usize::try_from(row.try_get::<i64, _>("total_bytes")?).map_err(|_| invalid_snapshot())?;
    let item_rows = sqlx::query(
        "SELECT question_id, remote_question_id, position, question_json, content_fingerprint \
         FROM question_snapshot_items WHERE snapshot_id = ? ORDER BY position",
    )
    .bind(snapshot_id.to_string())
    .fetch_all(database.pool())
    .await?;
    if item_rows.len() != stored_count || stored_count > MAX_QUESTIONS_PER_SNAPSHOT {
        return Err(invalid_snapshot());
    }

    let mut total_bytes = 0usize;
    let mut questions = Vec::with_capacity(item_rows.len());
    for item in item_rows {
        let json: &str = item.try_get("question_json")?;
        total_bytes = total_bytes
            .checked_add(json.len())
            .ok_or_else(invalid_snapshot)?;
        let question: Question = serde_json::from_str(json)?;
        let stored_id = parse_id::<QuestionId>(item.try_get("question_id")?)?;
        let stored_remote_id: Option<String> = item.try_get("remote_question_id")?;
        let stored_position =
            u32::try_from(item.try_get::<i64, _>("position")?).map_err(|_| invalid_snapshot())?;
        let stored_fingerprint = item
            .try_get::<Option<&str>, _>("content_fingerprint")?
            .map(QuestionContentFingerprint::from_str)
            .transpose()
            .map_err(|_| invalid_snapshot())?;
        if question.id != stored_id
            || question.remote_question_id != stored_remote_id
            || question.position != stored_position
            || stored_fingerprint.as_ref().is_some_and(|fingerprint| {
                question.content_fingerprint().as_ref() != Ok(fingerprint)
            })
        {
            return Err(invalid_snapshot());
        }
        questions.push(question);
    }
    if total_bytes != stored_bytes || total_bytes > MAX_QUESTION_SNAPSHOT_BYTES {
        return Err(invalid_snapshot());
    }

    let group_rows = sqlx::query(
        "SELECT group_id, ordinal, remote_group_id, group_json \
         FROM question_snapshot_groups WHERE snapshot_id = ? ORDER BY ordinal",
    )
    .bind(snapshot_id.to_string())
    .fetch_all(database.pool())
    .await?;
    if group_rows.len() != stored_group_count {
        return Err(invalid_snapshot());
    }
    let mut group_bytes = 0_usize;
    let mut groups = Vec::with_capacity(group_rows.len());
    for (index, group_row) in group_rows.into_iter().enumerate() {
        let json: &str = group_row.try_get("group_json")?;
        group_bytes = group_bytes
            .checked_add(json.len())
            .filter(|bytes| *bytes <= MAX_QUESTION_GROUP_BYTES)
            .ok_or_else(invalid_snapshot)?;
        let group: QuestionGroup = serde_json::from_str(json)?;
        let stored_id = parse_id::<QuestionGroupId>(group_row.try_get("group_id")?)?;
        let stored_ordinal = usize::try_from(group_row.try_get::<i64, _>("ordinal")?)
            .map_err(|_| invalid_snapshot())?;
        let stored_remote_id: Option<String> = group_row.try_get("remote_group_id")?;
        if group.id != stored_id
            || stored_ordinal != index + 1
            || group.remote_group_id != stored_remote_id
        {
            return Err(invalid_snapshot());
        }
        groups.push(group);
    }

    let snapshot = QuestionSnapshot {
        id: snapshot_id,
        task_id: parse_id::<TaskId>(row.try_get("task_id")?)?,
        provider_id: ProviderId::new(row.try_get::<String, _>("provider_id")?)
            .map_err(|_| invalid_snapshot())?,
        provider_version: row.try_get("provider_version")?,
        captured_at: decode_timestamp(row.try_get("captured_at")?)?,
        questions,
        groups,
    };
    validate_and_encode(&snapshot)?;
    Ok(snapshot)
}

#[async_trait]
impl AnswerCacheRepository for SqliteQuestionSnapshotRepository {
    async fn list_owned_prior_answer_evidence(
        &self,
        owner_id: UserId,
        task_id: TaskId,
        target_question_snapshot_id: QuestionSnapshotId,
    ) -> Result<Vec<PriorAnswerEvidence>, StorageError> {
        let rows = sqlx::query(
            "SELECT DISTINCT target_item.question_id AS target_question_id, \
                    target_item.remote_question_id AS target_remote_question_id, \
                    target_item.position AS target_position, \
                    target_item.question_json AS target_question_json, \
                    source_item.question_id, source_item.remote_question_id, \
                    source_item.position, source_item.question_json, \
                    source_item.content_fingerprint, source_item.semantic_fingerprint, \
                    COALESCE((SELECT json_group_array(json(ordered.question_json)) FROM ( \
                        SELECT question_json FROM question_snapshot_items \
                        WHERE snapshot_id = source_snapshot.id ORDER BY position \
                    ) AS ordered), '[]') AS source_questions_json, \
                    COALESCE((SELECT json_group_array(json(ordered.group_json)) FROM ( \
                        SELECT group_json FROM question_snapshot_groups \
                        WHERE snapshot_id = source_snapshot.id ORDER BY ordinal \
                    ) AS ordered), '[]') AS source_groups_json, \
                    source_snapshot.id AS source_snapshot_id, \
                    candidate.id AS candidate_id, candidate.source, candidate.candidate_json, \
                    candidate.created_at \
             FROM question_snapshots AS target_snapshot \
             INNER JOIN tasks AS task ON task.id = target_snapshot.task_id \
             INNER JOIN provider_accounts AS account ON account.id = task.provider_account_id \
             INNER JOIN question_snapshot_items AS target_item \
                ON target_item.snapshot_id = target_snapshot.id \
             INNER JOIN question_snapshot_items AS source_item \
                ON (source_item.semantic_fingerprint IS NOT NULL \
                    AND source_item.semantic_fingerprint = target_item.semantic_fingerprint) \
                  OR (source_item.semantic_fingerprint IS NULL \
                    AND target_item.semantic_fingerprint IS NULL \
                    AND source_item.content_fingerprint = target_item.content_fingerprint \
                    AND target_snapshot.group_count = 0) \
             INNER JOIN question_snapshots AS source_snapshot \
                ON source_snapshot.id = source_item.snapshot_id \
               AND source_snapshot.provider_id = target_snapshot.provider_id \
             INNER JOIN tasks AS source_task ON source_task.id = source_snapshot.task_id \
             INNER JOIN provider_accounts AS source_account \
                ON source_account.id = source_task.provider_account_id \
             INNER JOIN answer_candidates AS candidate \
                ON candidate.question_snapshot_id = source_item.snapshot_id \
               AND candidate.question_id = source_item.question_id \
             LEFT JOIN private_answer_evidence AS positive \
                ON positive.question_snapshot_id = source_item.snapshot_id \
               AND positive.question_id = source_item.question_id \
               AND positive.source_candidate_id = candidate.id \
               AND positive.provider_id = source_snapshot.provider_id \
               AND positive.evidence_class IN ('official', 'verified_historical') \
             WHERE account.owner_user_id = ? AND target_snapshot.task_id = ? \
               AND target_snapshot.id = ? AND target_item.content_fingerprint IS NOT NULL \
               AND candidate.source <> 'local_cache' \
               AND (source_item.semantic_fingerprint IS NOT NULL OR source_snapshot.group_count = 0) \
               AND (candidate.source = 'provider_native' OR positive.id IS NOT NULL) \
               AND source_snapshot.id <> target_snapshot.id \
               AND NOT EXISTS ( \
                    SELECT 1 FROM private_answer_evidence AS negative \
                    WHERE negative.provider_id = source_snapshot.provider_id \
                      AND negative.question_content_fingerprint = source_item.content_fingerprint \
                      AND (negative.source_candidate_id = candidate.id \
                           OR (positive.id IS NOT NULL AND negative.answer_json = positive.answer_json)) \
                      AND negative.evidence_class = 'negative' \
               ) \
             ORDER BY target_item.position, source_snapshot.captured_at DESC, \
                      source_snapshot.id DESC, candidate.created_at DESC, candidate.id DESC \
             LIMIT ?",
        )
        .bind(owner_id.to_string())
        .bind(task_id.to_string())
        .bind(target_question_snapshot_id.to_string())
        .bind(i64::try_from(MAX_PRIOR_ANSWER_EVIDENCE + 1).expect("evidence bound fits i64"))
        .fetch_all(self.database.pool())
        .await?;
        if rows.len() > MAX_PRIOR_ANSWER_EVIDENCE {
            return Err(invalid_candidates());
        }

        rows.iter().map(decode_prior_answer_evidence).collect()
    }
}

fn decode_prior_answer_evidence(row: &SqliteRow) -> Result<PriorAnswerEvidence, StorageError> {
    let fingerprint = QuestionContentFingerprint::from_str(row.try_get("content_fingerprint")?)
        .map_err(|_| invalid_candidates())?;
    let target_question: Question = serde_json::from_str(row.try_get("target_question_json")?)?;
    let target_question_id = parse_id::<QuestionId>(row.try_get("target_question_id")?)?;
    let target_remote_id: Option<String> = row.try_get("target_remote_question_id")?;
    let target_position = u32::try_from(row.try_get::<i64, _>("target_position")?)
        .map_err(|_| invalid_candidates())?;
    if target_question.id != target_question_id
        || target_question.remote_question_id != target_remote_id
        || target_question.position != target_position
        || target_question.validate().is_err()
        || target_question.content_fingerprint().as_ref() != Ok(&fingerprint)
    {
        return Err(invalid_candidates());
    }

    let question: Question = serde_json::from_str(row.try_get("question_json")?)?;
    let stored_question_id = parse_id::<QuestionId>(row.try_get("question_id")?)?;
    let stored_remote_id: Option<String> = row.try_get("remote_question_id")?;
    let stored_position =
        u32::try_from(row.try_get::<i64, _>("position")?).map_err(|_| invalid_candidates())?;
    if question.id != stored_question_id
        || question.remote_question_id != stored_remote_id
        || question.position != stored_position
        || question.validate().is_err()
        || question.content_fingerprint().as_ref() != Ok(&fingerprint)
    {
        return Err(invalid_candidates());
    }

    let source = decode_answer_source(row.try_get("source")?)?;
    let candidate: AnswerCandidate = serde_json::from_str(row.try_get("candidate_json")?)?;
    if source == AnswerSource::LocalCache
        || candidate.source != source
        || candidate.question_id != question.id
        || candidate.validate().is_err()
    {
        return Err(invalid_candidates());
    }
    let source_snapshot_id = parse_id::<QuestionSnapshotId>(row.try_get("source_snapshot_id")?)?;
    let source_questions: Vec<Question> =
        serde_json::from_str(row.try_get("source_questions_json")?)?;
    let source_groups: Vec<QuestionGroup> =
        serde_json::from_str(row.try_get("source_groups_json")?)?;
    let source_view = QuestionSetView::try_new(question.task_id, &source_questions, &source_groups)
        .map_err(|_| invalid_candidates())?;
    let semantic_fingerprint = row
        .try_get::<Option<&str>, _>("semantic_fingerprint")?
        .map(QuestionSemanticFingerprint::from_str)
        .transpose()
        .map_err(|_| invalid_candidates())?
        .map_or_else(|| source_view.semantic_fingerprint(question.id), Ok)
        .map_err(|_| invalid_candidates())?;
    Ok(PriorAnswerEvidence {
        question_content_fingerprint: fingerprint,
        question_semantic_fingerprint: semantic_fingerprint,
        source_question: question,
        source_questions,
        source_groups,
        source_candidate: AnswerCandidateRecord {
            id: parse_id::<AnswerCandidateId>(row.try_get("candidate_id")?)?,
            question_snapshot_id: source_snapshot_id,
            candidate,
            created_at: decode_timestamp(row.try_get("created_at")?)?,
        },
    })
}

#[async_trait]
impl AnswerCandidateRepository for SqliteQuestionSnapshotRepository {
    async fn save_answer_candidate_batch(
        &self,
        candidates: &[AnswerCandidateRecord],
    ) -> Result<(), StorageError> {
        let mut transaction = self.database.pool().begin().await?;
        save_answer_candidate_batch_in_transaction(&mut transaction, candidates).await?;
        transaction.commit().await?;
        Ok(())
    }

    async fn list_owned_answer_candidates(
        &self,
        owner_id: UserId,
        question_snapshot_id: QuestionSnapshotId,
    ) -> Result<Vec<AnswerCandidateRecord>, StorageError> {
        let rows = sqlx::query(
            "SELECT candidate.id, candidate.source, candidate.candidate_json, \
                    candidate.created_at \
             FROM answer_candidates AS candidate \
             INNER JOIN question_snapshots AS snapshot \
                ON snapshot.id = candidate.question_snapshot_id \
             INNER JOIN question_snapshot_items AS item \
                ON item.snapshot_id = candidate.question_snapshot_id \
               AND item.question_id = candidate.question_id \
             INNER JOIN tasks AS task ON task.id = snapshot.task_id \
             INNER JOIN provider_accounts AS account \
                ON account.id = task.provider_account_id \
             WHERE account.owner_user_id = ? AND candidate.question_snapshot_id = ? \
             ORDER BY item.position, candidate.created_at, candidate.id",
        )
        .bind(owner_id.to_string())
        .bind(question_snapshot_id.to_string())
        .fetch_all(self.database.pool())
        .await?;
        if rows.len() > MAX_CANDIDATES_PER_SNAPSHOT {
            return Err(invalid_candidates());
        }

        let mut total_bytes = 0usize;
        let mut records = Vec::with_capacity(rows.len());
        for row in rows {
            let json: &str = row.try_get("candidate_json")?;
            total_bytes = total_bytes
                .checked_add(json.len())
                .filter(|bytes| *bytes <= MAX_ANSWER_CANDIDATE_BYTES)
                .ok_or_else(invalid_candidates)?;
            let candidate: AnswerCandidate = serde_json::from_str(json)?;
            let source = decode_answer_source(row.try_get("source")?)?;
            if candidate.source != source || candidate.validate().is_err() {
                return Err(invalid_candidates());
            }
            records.push(AnswerCandidateRecord {
                id: parse_id::<AnswerCandidateId>(row.try_get("id")?)?,
                question_snapshot_id,
                candidate,
                created_at: decode_timestamp(row.try_get("created_at")?)?,
            });
        }
        Ok(records)
    }
}

pub(crate) async fn save_answer_candidate_batch_in_transaction(
    transaction: &mut Transaction<'_, Sqlite>,
    candidates: &[AnswerCandidateRecord],
) -> Result<(), StorageError> {
    let encoded = validate_and_encode_candidates(candidates)?;
    let question_ids = sqlx::query_scalar::<_, String>(
        "SELECT question_id FROM question_snapshot_items WHERE snapshot_id = ?",
    )
    .bind(encoded.snapshot_id.to_string())
    .fetch_all(&mut **transaction)
    .await?
    .into_iter()
    .map(|value| parse_id::<QuestionId>(&value))
    .collect::<Result<BTreeSet<_>, _>>()?;
    if encoded
        .candidates
        .iter()
        .any(|candidate| !question_ids.contains(&candidate.question_id))
    {
        return Err(invalid_candidates());
    }

    let totals = sqlx::query(
        "SELECT COUNT(*) AS candidate_count, \
                COALESCE(SUM(length(CAST(candidate_json AS BLOB))), 0) AS total_bytes \
         FROM answer_candidates WHERE question_snapshot_id = ?",
    )
    .bind(encoded.snapshot_id.to_string())
    .fetch_one(&mut **transaction)
    .await?;
    let existing_count = usize::try_from(totals.try_get::<i64, _>("candidate_count")?)
        .map_err(|_| invalid_candidates())?;
    let existing_bytes = usize::try_from(totals.try_get::<i64, _>("total_bytes")?)
        .map_err(|_| invalid_candidates())?;
    if existing_count
        .checked_add(encoded.candidates.len())
        .is_none_or(|count| count > MAX_CANDIDATES_PER_SNAPSHOT)
        || existing_bytes
            .checked_add(encoded.total_bytes)
            .is_none_or(|bytes| bytes > MAX_ANSWER_CANDIDATE_BYTES)
    {
        return Err(invalid_candidates());
    }

    for candidate in encoded.candidates {
        sqlx::query(
            "INSERT INTO answer_candidates \
             (id, question_snapshot_id, question_id, source, candidate_json, created_at) \
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(candidate.id.to_string())
        .bind(encoded.snapshot_id.to_string())
        .bind(candidate.question_id.to_string())
        .bind(encode_answer_source(candidate.source))
        .bind(candidate.json)
        .bind(encode_timestamp(candidate.created_at))
        .execute(&mut **transaction)
        .await?;
    }
    Ok(())
}

#[async_trait]
impl SubmissionDraftRepository for SqliteQuestionSnapshotRepository {
    #[allow(
        clippy::too_many_lines,
        reason = "snapshot partition, candidate identity, immutable coverage and payload preview are verified in one transaction"
    )]
    async fn save_submission_draft(&self, draft: &SubmissionDraft) -> Result<(), StorageError> {
        draft.validate().map_err(|_| invalid_submission_draft())?;
        let preview_json = serde_json::to_string(&draft.payload_preview)?;
        if preview_json.is_empty() || preview_json.len() > MAX_SUBMISSION_PREVIEW_BYTES {
            return Err(invalid_submission_draft());
        }
        let preview_bytes = preview_json.len();
        let unanswered_json =
            serde_json::to_string(&draft.answer_coverage.unanswered_question_ids)?;
        if unanswered_json.len() < 2 || unanswered_json.len() > 200_002 {
            return Err(invalid_submission_draft());
        }

        let mut transaction = self.database.pool().begin().await?;
        let snapshot_binding = sqlx::query(
            "SELECT task_id, provider_id, question_count FROM question_snapshots WHERE id = ?",
        )
        .bind(draft.question_snapshot_id.to_string())
        .fetch_optional(&mut *transaction)
        .await?;
        let Some(snapshot_binding) = snapshot_binding else {
            return Err(invalid_submission_draft());
        };
        if snapshot_binding.try_get::<String, _>("task_id")? != draft.task_id.to_string()
            || snapshot_binding.try_get::<String, _>("provider_id")? != draft.provider_id.as_str()
            || snapshot_binding.try_get::<i64, _>("question_count")?
                != i64::from(draft.answer_coverage.total_question_count)
        {
            return Err(invalid_submission_draft());
        }
        let snapshot_question_ids = sqlx::query_scalar::<_, String>(
            "SELECT question_id FROM question_snapshot_items WHERE snapshot_id = ?",
        )
        .bind(draft.question_snapshot_id.to_string())
        .fetch_all(&mut *transaction)
        .await?
        .into_iter()
        .map(|value| parse_id::<QuestionId>(&value))
        .collect::<Result<BTreeSet<_>, _>>()?;
        let draft_question_ids = draft
            .items
            .iter()
            .map(|item| item.question.id)
            .chain(
                draft
                    .answer_coverage
                    .unanswered_question_ids
                    .iter()
                    .copied(),
            )
            .collect::<BTreeSet<_>>();
        if draft_question_ids != snapshot_question_ids {
            return Err(invalid_submission_draft());
        }

        for item in &draft.items {
            let binding = sqlx::query(
                "SELECT question.question_json, candidate.source, candidate.candidate_json \
                 FROM question_snapshot_items AS question \
                 INNER JOIN answer_candidates AS candidate \
                    ON candidate.question_snapshot_id = question.snapshot_id \
                   AND candidate.question_id = question.question_id \
                 WHERE question.snapshot_id = ? AND question.question_id = ? \
                   AND candidate.id = ?",
            )
            .bind(draft.question_snapshot_id.to_string())
            .bind(item.question.id.to_string())
            .bind(item.selected.candidate_id.to_string())
            .fetch_optional(&mut *transaction)
            .await?;
            let Some(binding) = binding else {
                return Err(invalid_submission_draft());
            };
            let question: Question =
                serde_json::from_str(binding.try_get::<&str, _>("question_json")?)?;
            let candidate: AnswerCandidate =
                serde_json::from_str(binding.try_get::<&str, _>("candidate_json")?)?;
            let source = decode_answer_source(binding.try_get("source")?)?;
            if question != item.question
                || candidate.source != source
                || candidate.question_id != item.selected.question_id
                || candidate.answer != item.selected.answer
                || candidate.source != item.selected.source
                || candidate.confidence != item.selected.confidence
            {
                return Err(invalid_submission_draft());
            }
        }

        sqlx::query(
            "INSERT INTO submission_drafts \
             (id, question_snapshot_id, task_id, provider_id, provider_version, \
              payload_preview_json, preview_bytes, item_count, total_question_count, \
              minimum_coverage_millis, unanswered_question_ids_json, created_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(draft.id.to_string())
        .bind(draft.question_snapshot_id.to_string())
        .bind(draft.task_id.to_string())
        .bind(draft.provider_id.as_str())
        .bind(&draft.provider_version)
        .bind(preview_json)
        .bind(i64::try_from(preview_bytes).expect("bounded draft preview size fits i64"))
        .bind(i64::try_from(draft.items.len()).expect("bounded draft item count fits i64"))
        .bind(i64::from(draft.answer_coverage.total_question_count))
        .bind(i64::from(draft.answer_coverage.minimum_coverage_millis))
        .bind(unanswered_json)
        .bind(encode_timestamp(draft.created_at))
        .execute(&mut *transaction)
        .await?;

        for item in &draft.items {
            sqlx::query(
                "INSERT INTO submission_draft_items \
                 (draft_id, question_snapshot_id, question_id, answer_candidate_id, position) \
                 VALUES (?, ?, ?, ?, ?)",
            )
            .bind(draft.id.to_string())
            .bind(draft.question_snapshot_id.to_string())
            .bind(item.question.id.to_string())
            .bind(item.selected.candidate_id.to_string())
            .bind(i64::from(item.question.position))
            .execute(&mut *transaction)
            .await?;
        }
        transaction.commit().await?;
        Ok(())
    }

    #[allow(
        clippy::too_many_lines,
        reason = "encrypted-free draft reconstruction rechecks every persisted snapshot, candidate, coverage and preview binding"
    )]
    async fn find_owned_submission_draft(
        &self,
        owner_id: UserId,
        submission_draft_id: SubmissionDraftId,
    ) -> Result<Option<SubmissionDraft>, StorageError> {
        let row = sqlx::query(
            "SELECT draft.id, draft.question_snapshot_id, draft.task_id, draft.provider_id, \
                    draft.provider_version, draft.payload_preview_json, draft.preview_bytes, \
                    draft.item_count, draft.total_question_count, \
                    draft.minimum_coverage_millis, draft.unanswered_question_ids_json, \
                    draft.created_at \
             FROM submission_drafts AS draft \
             INNER JOIN question_snapshots AS snapshot \
                ON snapshot.id = draft.question_snapshot_id \
             INNER JOIN tasks AS task ON task.id = snapshot.task_id \
             INNER JOIN provider_accounts AS account \
                ON account.id = task.provider_account_id \
             WHERE account.owner_user_id = ? AND draft.id = ?",
        )
        .bind(owner_id.to_string())
        .bind(submission_draft_id.to_string())
        .fetch_optional(self.database.pool())
        .await?;
        let Some(row) = row else {
            return Ok(None);
        };

        let question_snapshot_id =
            parse_id::<QuestionSnapshotId>(row.try_get("question_snapshot_id")?)?;
        let preview_json: &str = row.try_get("payload_preview_json")?;
        let stored_preview_bytes = usize::try_from(row.try_get::<i64, _>("preview_bytes")?)
            .map_err(|_| invalid_submission_draft())?;
        let stored_item_count = usize::try_from(row.try_get::<i64, _>("item_count")?)
            .map_err(|_| invalid_submission_draft())?;
        if preview_json.len() != stored_preview_bytes
            || stored_preview_bytes == 0
            || stored_preview_bytes > MAX_SUBMISSION_PREVIEW_BYTES
            || stored_item_count == 0
            || stored_item_count > MAX_SUBMISSION_DRAFT_ITEMS
        {
            return Err(invalid_submission_draft());
        }
        let payload_preview: SubmissionPayloadPreview = serde_json::from_str(preview_json)?;
        let unanswered_json = row.try_get::<String, _>("unanswered_question_ids_json")?;
        if unanswered_json.len() < 2 || unanswered_json.len() > 200_002 {
            return Err(invalid_submission_draft());
        }
        let answer_coverage = SubmissionAnswerCoverage {
            total_question_count: u32::try_from(row.try_get::<i64, _>("total_question_count")?)
                .map_err(|_| invalid_submission_draft())?,
            minimum_coverage_millis: u16::try_from(
                row.try_get::<i64, _>("minimum_coverage_millis")?,
            )
            .map_err(|_| invalid_submission_draft())?,
            unanswered_question_ids: serde_json::from_str(&unanswered_json)?,
        };
        let item_rows = sqlx::query(
            "SELECT draft_item.question_id, draft_item.answer_candidate_id, \
                    draft_item.position, question.question_json, candidate.source, \
                    candidate.candidate_json \
             FROM submission_draft_items AS draft_item \
             INNER JOIN question_snapshot_items AS question \
                ON question.snapshot_id = draft_item.question_snapshot_id \
               AND question.question_id = draft_item.question_id \
             INNER JOIN answer_candidates AS candidate \
                ON candidate.question_snapshot_id = draft_item.question_snapshot_id \
               AND candidate.id = draft_item.answer_candidate_id \
               AND candidate.question_id = draft_item.question_id \
             WHERE draft_item.draft_id = ? ORDER BY draft_item.position",
        )
        .bind(submission_draft_id.to_string())
        .fetch_all(self.database.pool())
        .await?;
        if item_rows.len() != stored_item_count {
            return Err(invalid_submission_draft());
        }

        let mut items = Vec::with_capacity(item_rows.len());
        for item_row in item_rows {
            let question: Question =
                serde_json::from_str(item_row.try_get::<&str, _>("question_json")?)?;
            let question_id = parse_id::<QuestionId>(item_row.try_get("question_id")?)?;
            let position = u32::try_from(item_row.try_get::<i64, _>("position")?)
                .map_err(|_| invalid_submission_draft())?;
            let candidate: AnswerCandidate =
                serde_json::from_str(item_row.try_get::<&str, _>("candidate_json")?)?;
            let source = decode_answer_source(item_row.try_get("source")?)?;
            if question.id != question_id
                || question.position != position
                || candidate.question_id != question_id
                || candidate.source != source
                || question.validate().is_err()
                || candidate.validate().is_err()
            {
                return Err(invalid_submission_draft());
            }
            items.push(SubmissionDraftItem {
                question,
                selected: SelectedAnswer {
                    candidate_id: parse_id::<AnswerCandidateId>(
                        item_row.try_get("answer_candidate_id")?,
                    )?,
                    question_id,
                    answer: candidate.answer,
                    source: candidate.source,
                    confidence: candidate.confidence,
                },
            });
        }

        let snapshot_question_ids = sqlx::query_scalar::<_, String>(
            "SELECT question_id FROM question_snapshot_items WHERE snapshot_id = ?",
        )
        .bind(question_snapshot_id.to_string())
        .fetch_all(self.database.pool())
        .await?
        .into_iter()
        .map(|value| parse_id::<QuestionId>(&value))
        .collect::<Result<BTreeSet<_>, _>>()?;
        let draft_question_ids = items
            .iter()
            .map(|item| item.question.id)
            .chain(answer_coverage.unanswered_question_ids.iter().copied())
            .collect::<BTreeSet<_>>();
        if snapshot_question_ids != draft_question_ids {
            return Err(invalid_submission_draft());
        }

        let draft = SubmissionDraft {
            id: parse_id::<SubmissionDraftId>(row.try_get("id")?)?,
            task_id: parse_id::<TaskId>(row.try_get("task_id")?)?,
            question_snapshot_id,
            provider_id: ProviderId::new(row.try_get::<String, _>("provider_id")?)
                .map_err(|_| invalid_submission_draft())?,
            provider_version: row.try_get("provider_version")?,
            answer_coverage,
            items,
            payload_preview,
            created_at: decode_timestamp(row.try_get("created_at")?)?,
        };
        draft.validate().map_err(|_| invalid_submission_draft())?;
        Ok(Some(draft))
    }
}

#[async_trait]
impl SubmissionResultRepository for SqliteQuestionSnapshotRepository {
    async fn save_submission_result(&self, result: &SubmissionResult) -> Result<(), StorageError> {
        result.validate().map_err(|_| invalid_submission_result())?;
        let receipt_json = result
            .receipt
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;
        if receipt_json.as_ref().is_some_and(|receipt| {
            receipt.is_empty() || receipt.len() > MAX_SUBMISSION_RECEIPT_BYTES
        }) {
            return Err(invalid_submission_result());
        }
        let verification_json = serde_json::to_string(&result.verification)?;
        if verification_json.is_empty()
            || verification_json.len() > MAX_SUBMISSION_VERIFICATION_BYTES
        {
            return Err(invalid_submission_result());
        }
        let verification_bytes = verification_json.len();

        let mut transaction = self.database.pool().begin_with("BEGIN IMMEDIATE").await?;
        let draft_binding = sqlx::query(
            "SELECT task_id, question_snapshot_id, provider_id \
             FROM submission_drafts WHERE id = ?",
        )
        .bind(result.submission_draft_id.to_string())
        .fetch_optional(&mut *transaction)
        .await?;
        let Some(draft_binding) = draft_binding else {
            return Err(invalid_submission_result());
        };
        if draft_binding.try_get::<String, _>("task_id")? != result.task_id.to_string()
            || draft_binding.try_get::<String, _>("question_snapshot_id")?
                != result.question_snapshot_id.to_string()
            || draft_binding.try_get::<String, _>("provider_id")? != result.provider_id.as_str()
        {
            return Err(invalid_submission_result());
        }
        let draft_questions = sqlx::query_scalar::<_, String>(
            "SELECT question_id FROM submission_draft_items WHERE draft_id = ?",
        )
        .bind(result.submission_draft_id.to_string())
        .fetch_all(&mut *transaction)
        .await?
        .into_iter()
        .map(|value| parse_id::<QuestionId>(&value))
        .collect::<Result<BTreeSet<_>, _>>()?;
        if result
            .verification
            .questions
            .iter()
            .any(|question| !draft_questions.contains(&question.question_id))
        {
            return Err(invalid_submission_result());
        }
        let attempt_exists: Option<i64> = sqlx::query_scalar(
            "SELECT 1 FROM executions AS execution \
             INNER JOIN execution_attempts AS attempt \
                ON attempt.execution_id = execution.id \
             WHERE execution.id = ? AND execution.task_id = ? AND attempt.id = ?",
        )
        .bind(result.execution_id.to_string())
        .bind(result.task_id.to_string())
        .bind(result.execution_attempt_id.to_string())
        .fetch_optional(&mut *transaction)
        .await?;
        if attempt_exists.is_none() {
            return Err(invalid_submission_result());
        }

        sqlx::query(
            "INSERT INTO submission_results \
             (id, submission_draft_id, execution_id, execution_attempt_id, task_id, \
              question_snapshot_id, provider_id, provider_version, status, receipt_json, \
              receipt_bytes, verification_json, verification_bytes, created_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(result.id.to_string())
        .bind(result.submission_draft_id.to_string())
        .bind(result.execution_id.to_string())
        .bind(result.execution_attempt_id.to_string())
        .bind(result.task_id.to_string())
        .bind(result.question_snapshot_id.to_string())
        .bind(result.provider_id.as_str())
        .bind(&result.provider_version)
        .bind(encode_submission_result_status(result.status))
        .bind(receipt_json.as_deref())
        .bind(
            receipt_json
                .as_ref()
                .map(|receipt| i64::try_from(receipt.len()).expect("bounded receipt fits i64")),
        )
        .bind(verification_json)
        .bind(i64::try_from(verification_bytes).expect("bounded verification fits i64"))
        .bind(encode_timestamp(result.created_at))
        .execute(&mut *transaction)
        .await?;
        project_submission_result_evidence(&mut transaction, result).await?;
        transaction.commit().await?;
        Ok(())
    }

    async fn find_owned_submission_result(
        &self,
        owner_id: UserId,
        submission_result_id: SubmissionResultId,
    ) -> Result<Option<SubmissionResult>, StorageError> {
        let row = submission_result_query()
            .bind(owner_id.to_string())
            .bind(submission_result_id.to_string())
            .fetch_optional(self.database.pool())
            .await?;
        match row {
            Some(row) => Ok(Some(decode_submission_result(&self.database, &row).await?)),
            None => Ok(None),
        }
    }

    async fn find_latest_owned_submission_result(
        &self,
        owner_id: UserId,
        submission_draft_id: SubmissionDraftId,
    ) -> Result<Option<SubmissionResult>, StorageError> {
        let row = sqlx::query(
            "SELECT result.* FROM submission_results AS result \
             INNER JOIN submission_drafts AS draft \
                ON draft.id = result.submission_draft_id \
             INNER JOIN question_snapshots AS snapshot \
                ON snapshot.id = draft.question_snapshot_id \
             INNER JOIN tasks AS task ON task.id = snapshot.task_id \
             INNER JOIN provider_accounts AS account \
                ON account.id = task.provider_account_id \
             WHERE account.owner_user_id = ? AND result.submission_draft_id = ? \
             ORDER BY result.created_at DESC, result.id DESC LIMIT 1",
        )
        .bind(owner_id.to_string())
        .bind(submission_draft_id.to_string())
        .fetch_optional(self.database.pool())
        .await?;
        match row {
            Some(row) => Ok(Some(decode_submission_result(&self.database, &row).await?)),
            None => Ok(None),
        }
    }

    async fn find_previous_owned_submission_score(
        &self,
        owner_id: UserId,
        task_id: TaskId,
        submission_result_id: SubmissionResultId,
    ) -> Result<Option<asterism_domain::SubmissionScore>, StorageError> {
        let verification_json: Option<String> = sqlx::query_scalar(
            "SELECT prior.verification_json \
             FROM submission_results AS current \
             INNER JOIN tasks AS task ON task.id = current.task_id \
             INNER JOIN provider_accounts AS account ON account.id = task.provider_account_id \
             INNER JOIN submission_results AS prior ON prior.task_id = current.task_id \
             WHERE account.owner_user_id = ? AND current.task_id = ? AND current.id = ? \
               AND json_type(prior.verification_json, '$.score') = 'object' \
               AND (prior.created_at < current.created_at \
                    OR (prior.created_at = current.created_at AND prior.id < current.id)) \
             ORDER BY prior.created_at DESC, prior.id DESC LIMIT 1",
        )
        .bind(owner_id.to_string())
        .bind(task_id.to_string())
        .bind(submission_result_id.to_string())
        .fetch_optional(self.database.pool())
        .await?;
        verification_json
            .map(|json| serde_json::from_str::<SubmissionVerificationSnapshot>(&json))
            .transpose()?
            .map(|verification| {
                verification
                    .validate()
                    .map_err(|_| invalid_submission_result())?;
                verification.score.ok_or_else(invalid_submission_result)
            })
            .transpose()
    }
}

async fn project_submission_result_evidence(
    transaction: &mut Transaction<'_, Sqlite>,
    result: &SubmissionResult,
) -> Result<(), StorageError> {
    let verifications = reliable_submission_verifications(result);
    if verifications.is_empty() {
        return Ok(());
    }
    let context = load_submission_evidence_context(transaction, result).await?;
    let grouped_question_ids =
        load_grouped_question_ids(transaction, result.question_snapshot_id, result.task_id).await?;
    let rows = sqlx::query(
        "SELECT item.question_id, item.answer_candidate_id, candidate.candidate_json, \
                question.question_json \
         FROM submission_draft_items AS item \
         INNER JOIN answer_candidates AS candidate \
           ON candidate.question_snapshot_id = item.question_snapshot_id \
          AND candidate.id = item.answer_candidate_id \
          AND candidate.question_id = item.question_id \
         INNER JOIN question_snapshot_items AS question \
           ON question.snapshot_id = item.question_snapshot_id \
          AND question.question_id = item.question_id \
         WHERE item.draft_id = ? ORDER BY item.position",
    )
    .bind(result.submission_draft_id.to_string())
    .fetch_all(&mut **transaction)
    .await?;
    let result_digest = submission_verification_digest(result)?;
    for row in rows {
        let question_id = parse_id::<QuestionId>(row.try_get("question_id")?)?;
        let Some(evidence_class) = verifications.get(&question_id).copied() else {
            continue;
        };
        let candidate_id = parse_id::<AnswerCandidateId>(row.try_get("answer_candidate_id")?)?;
        let candidate: AnswerCandidate = serde_json::from_str(row.try_get("candidate_json")?)?;
        let question: Question = serde_json::from_str(row.try_get("question_json")?)?;
        if candidate.question_id != question_id
            || question.id != question_id
            || question.task_id != result.task_id
        {
            return Err(invalid_submission_result());
        }
        if matches!(candidate.answer, asterism_domain::NormalizedAnswer::Skip) {
            continue;
        }
        let evidence = PrivateAnswerEvidence {
            id: PrivateAnswerEvidenceId::new(),
            owner_user_id: context.owner,
            provider_id: result.provider_id.clone(),
            provider_account_id: context.account,
            course_id: context.course,
            task_id: result.task_id,
            question_snapshot_id: result.question_snapshot_id,
            question_id,
            execution_attempt_id: Some(result.execution_attempt_id),
            provider_attempt_digest: None,
            source_candidate_id: Some(candidate_id),
            question_content_fingerprint: question
                .content_fingerprint()
                .map_err(|_| invalid_submission_result())?,
            question: question.clone(),
            answer: candidate.answer.clone(),
            answer_source: candidate.source,
            evidence_class,
            result_digest: Some(result_digest),
            provenance_sanitized: json!({
                "source": "submission_verify",
                "submission_result_id": result.id,
                "submission_draft_id": result.submission_draft_id,
                "execution_id": result.execution_id,
                "execution_attempt_id": result.execution_attempt_id,
                "result_status": result.status,
                "question_status": if evidence_class == AnswerEvidenceClass::VerifiedHistorical {
                    "confirmed"
                } else {
                    "rejected"
                },
                "remote_state": result.verification.remote_state,
                "score": result.verification.score,
                "progress_percent": result.verification.progress_percent,
                "receipt": result.receipt,
            }),
            projection: if grouped_question_ids.contains(&question_id) {
                CorpusProjectionEligibility::Unmatched(
                    UnmatchedEvidenceReason::MissingSharedContext,
                )
            } else {
                CorpusProjectionEligibility::for_question_answer(&question, &candidate.answer)
            },
            observed_at: result.verification.verified_at,
            verified_at: result.verification.verified_at,
        };
        record_answer_evidence_in_transaction(transaction, &evidence).await?;
    }
    Ok(())
}

async fn load_grouped_question_ids(
    transaction: &mut Transaction<'_, Sqlite>,
    snapshot_id: QuestionSnapshotId,
    task_id: TaskId,
) -> Result<BTreeSet<QuestionId>, StorageError> {
    let questions = sqlx::query_scalar::<_, String>(
        "SELECT question_json FROM question_snapshot_items \
         WHERE snapshot_id = ? ORDER BY position",
    )
    .bind(snapshot_id.to_string())
    .fetch_all(&mut **transaction)
    .await?
    .into_iter()
    .map(|json| serde_json::from_str::<Question>(&json))
    .collect::<Result<Vec<_>, _>>()?;
    let groups = sqlx::query_scalar::<_, String>(
        "SELECT group_json FROM question_snapshot_groups \
         WHERE snapshot_id = ? ORDER BY ordinal",
    )
    .bind(snapshot_id.to_string())
    .fetch_all(&mut **transaction)
    .await?
    .into_iter()
    .map(|json| serde_json::from_str::<QuestionGroup>(&json))
    .collect::<Result<Vec<_>, _>>()?;
    validate_question_groups(task_id, &questions, &groups)
        .map_err(|_| invalid_submission_result())?;
    Ok(groups
        .iter()
        .flat_map(|group| group.children.iter())
        .filter_map(|child| match child {
            QuestionGroupChild::Question(question_id) => Some(*question_id),
            QuestionGroupChild::Group(_) => None,
        })
        .collect())
}

fn reliable_submission_verifications(
    result: &SubmissionResult,
) -> BTreeMap<QuestionId, AnswerEvidenceClass> {
    result
        .verification
        .questions
        .iter()
        .filter_map(|verification| match verification.status {
            SubmissionQuestionVerificationStatus::Confirmed => Some((
                verification.question_id,
                AnswerEvidenceClass::VerifiedHistorical,
            )),
            SubmissionQuestionVerificationStatus::Rejected => {
                Some((verification.question_id, AnswerEvidenceClass::Negative))
            }
            SubmissionQuestionVerificationStatus::Unverified => None,
        })
        .collect()
}

struct SubmissionEvidenceContext {
    owner: UserId,
    account: ProviderAccountId,
    course: Option<CourseId>,
}

async fn load_submission_evidence_context(
    transaction: &mut Transaction<'_, Sqlite>,
    result: &SubmissionResult,
) -> Result<SubmissionEvidenceContext, StorageError> {
    let binding = sqlx::query(
        "SELECT account.owner_user_id, task.provider_account_id, task.course_id \
         FROM submission_drafts AS draft \
         INNER JOIN tasks AS task ON task.id = draft.task_id \
         INNER JOIN provider_accounts AS account ON account.id = task.provider_account_id \
         WHERE draft.id = ? AND draft.task_id = ? AND draft.question_snapshot_id = ? \
           AND draft.provider_id = ?",
    )
    .bind(result.submission_draft_id.to_string())
    .bind(result.task_id.to_string())
    .bind(result.question_snapshot_id.to_string())
    .bind(result.provider_id.as_str())
    .fetch_one(&mut **transaction)
    .await?;
    Ok(SubmissionEvidenceContext {
        owner: parse_id(binding.try_get("owner_user_id")?)?,
        account: parse_id(binding.try_get("provider_account_id")?)?,
        course: binding
            .try_get::<Option<&str>, _>("course_id")?
            .map(parse_id)
            .transpose()?,
    })
}

fn submission_verification_digest(result: &SubmissionResult) -> Result<[u8; 32], StorageError> {
    let material = json!({
        "submission_draft_id": result.submission_draft_id,
        "execution_id": result.execution_id,
        "execution_attempt_id": result.execution_attempt_id,
        "task_id": result.task_id,
        "question_snapshot_id": result.question_snapshot_id,
        "provider_id": result.provider_id,
        "provider_version": result.provider_version,
        "verification": result.verification,
    });
    Ok(Sha256::digest(serde_json::to_vec(&material)?).into())
}

fn submission_result_query()
-> sqlx::query::Query<'static, sqlx::Sqlite, sqlx::sqlite::SqliteArguments<'static>> {
    sqlx::query(
        "SELECT result.* FROM submission_results AS result \
         INNER JOIN submission_drafts AS draft ON draft.id = result.submission_draft_id \
         INNER JOIN question_snapshots AS snapshot ON snapshot.id = draft.question_snapshot_id \
         INNER JOIN tasks AS task ON task.id = snapshot.task_id \
         INNER JOIN provider_accounts AS account ON account.id = task.provider_account_id \
         WHERE account.owner_user_id = ? AND result.id = ?",
    )
}

async fn decode_submission_result(
    database: &Database,
    row: &SqliteRow,
) -> Result<SubmissionResult, StorageError> {
    let receipt_json: Option<&str> = row.try_get("receipt_json")?;
    let receipt_bytes = row
        .try_get::<Option<i64>, _>("receipt_bytes")?
        .map(usize::try_from)
        .transpose()
        .map_err(|_| invalid_submission_result())?;
    if receipt_json.map(str::len) != receipt_bytes
        || receipt_bytes.is_some_and(|bytes| bytes == 0 || bytes > MAX_SUBMISSION_RECEIPT_BYTES)
    {
        return Err(invalid_submission_result());
    }
    let receipt = receipt_json
        .map(serde_json::from_str::<SubmissionReceipt>)
        .transpose()?;
    let verification_json: &str = row.try_get("verification_json")?;
    let verification_bytes = usize::try_from(row.try_get::<i64, _>("verification_bytes")?)
        .map_err(|_| invalid_submission_result())?;
    if verification_json.len() != verification_bytes
        || verification_bytes == 0
        || verification_bytes > MAX_SUBMISSION_VERIFICATION_BYTES
    {
        return Err(invalid_submission_result());
    }
    let verification: SubmissionVerificationSnapshot = serde_json::from_str(verification_json)?;
    let result = SubmissionResult {
        id: parse_id::<SubmissionResultId>(row.try_get("id")?)?,
        submission_draft_id: parse_id::<SubmissionDraftId>(row.try_get("submission_draft_id")?)?,
        execution_id: parse_id::<ExecutionId>(row.try_get("execution_id")?)?,
        execution_attempt_id: parse_id::<ExecutionAttemptId>(row.try_get("execution_attempt_id")?)?,
        task_id: parse_id::<TaskId>(row.try_get("task_id")?)?,
        question_snapshot_id: parse_id::<QuestionSnapshotId>(row.try_get("question_snapshot_id")?)?,
        provider_id: ProviderId::new(row.try_get::<String, _>("provider_id")?)
            .map_err(|_| invalid_submission_result())?,
        provider_version: row.try_get("provider_version")?,
        status: decode_submission_result_status(row.try_get("status")?)?,
        receipt,
        verification,
        created_at: decode_timestamp(row.try_get("created_at")?)?,
    };
    result.validate().map_err(|_| invalid_submission_result())?;
    let draft_questions = sqlx::query_scalar::<_, String>(
        "SELECT question_id FROM submission_draft_items WHERE draft_id = ?",
    )
    .bind(result.submission_draft_id.to_string())
    .fetch_all(database.pool())
    .await?
    .into_iter()
    .map(|value| parse_id::<QuestionId>(&value))
    .collect::<Result<BTreeSet<_>, _>>()?;
    if result
        .verification
        .questions
        .iter()
        .any(|question| !draft_questions.contains(&question.question_id))
    {
        return Err(invalid_submission_result());
    }
    Ok(result)
}

const fn encode_submission_result_status(status: SubmissionResultStatus) -> &'static str {
    match status {
        SubmissionResultStatus::Confirmed => "confirmed",
        SubmissionResultStatus::Rejected => "rejected",
        SubmissionResultStatus::ExecutionFailed => "execution_failed",
        SubmissionResultStatus::Inconclusive => "inconclusive",
    }
}

fn decode_submission_result_status(value: &str) -> Result<SubmissionResultStatus, StorageError> {
    match value {
        "confirmed" => Ok(SubmissionResultStatus::Confirmed),
        "rejected" => Ok(SubmissionResultStatus::Rejected),
        "execution_failed" => Ok(SubmissionResultStatus::ExecutionFailed),
        "inconclusive" => Ok(SubmissionResultStatus::Inconclusive),
        _ => Err(invalid_submission_result()),
    }
}

struct EncodedAnswerCandidate {
    id: AnswerCandidateId,
    question_id: QuestionId,
    source: AnswerSource,
    json: String,
    created_at: Timestamp,
}

struct EncodedAnswerCandidates {
    snapshot_id: QuestionSnapshotId,
    candidates: Vec<EncodedAnswerCandidate>,
    total_bytes: usize,
}

fn validate_and_encode_candidates(
    candidates: &[AnswerCandidateRecord],
) -> Result<EncodedAnswerCandidates, StorageError> {
    if candidates.is_empty() || candidates.len() > MAX_CANDIDATES_PER_SNAPSHOT {
        return Err(invalid_candidates());
    }
    let snapshot_id = candidates[0].question_snapshot_id;
    let mut ids = BTreeSet::new();
    let mut total_bytes = 0usize;
    let mut encoded = Vec::with_capacity(candidates.len());
    for record in candidates {
        if record.question_snapshot_id != snapshot_id
            || !ids.insert(record.id)
            || record.candidate.validate().is_err()
        {
            return Err(invalid_candidates());
        }
        let json = serde_json::to_string(&record.candidate)?;
        total_bytes = total_bytes
            .checked_add(json.len())
            .filter(|bytes| *bytes <= MAX_ANSWER_CANDIDATE_BYTES)
            .ok_or_else(invalid_candidates)?;
        encoded.push(EncodedAnswerCandidate {
            id: record.id,
            question_id: record.candidate.question_id,
            source: record.candidate.source,
            json,
            created_at: record.created_at,
        });
    }
    Ok(EncodedAnswerCandidates {
        snapshot_id,
        candidates: encoded,
        total_bytes,
    })
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

fn decode_answer_source(value: &str) -> Result<AnswerSource, StorageError> {
    match value {
        "manual" => Ok(AnswerSource::Manual),
        "local_cache" => Ok(AnswerSource::LocalCache),
        "provider_native" => Ok(AnswerSource::ProviderNative),
        "ai" => Ok(AnswerSource::Ai),
        "external_bank" => Ok(AnswerSource::ExternalBank),
        "other" => Ok(AnswerSource::Other),
        _ => Err(invalid_candidates()),
    }
}

struct EncodedQuestion {
    id: QuestionId,
    remote_id: Option<String>,
    position: u32,
    json: String,
    content_fingerprint: QuestionContentFingerprint,
    semantic_fingerprint: QuestionSemanticFingerprint,
}

struct EncodedSnapshot {
    questions: Vec<EncodedQuestion>,
    groups: Vec<EncodedQuestionGroup>,
    total_bytes: usize,
}

struct EncodedQuestionGroup {
    id: QuestionGroupId,
    remote_id: Option<String>,
    json: String,
}

impl EncodedSnapshot {
    fn len(&self) -> usize {
        self.questions.len()
    }
}

fn validate_and_encode(snapshot: &QuestionSnapshot) -> Result<EncodedSnapshot, StorageError> {
    if snapshot.questions.len() > MAX_QUESTIONS_PER_SNAPSHOT
        || !valid_text(&snapshot.provider_version, MAX_PROVIDER_VERSION_BYTES)
        || validate_question_groups(snapshot.task_id, &snapshot.questions, &snapshot.groups)
            .is_err()
    {
        return Err(invalid_snapshot());
    }
    let semantic_view =
        QuestionSetView::try_new(snapshot.task_id, &snapshot.questions, &snapshot.groups)
            .map_err(|_| invalid_snapshot())?;
    let mut ids = BTreeSet::new();
    let mut remote_ids = BTreeSet::new();
    let mut positions = BTreeSet::new();
    let mut total_bytes = 0usize;
    let mut questions = Vec::with_capacity(snapshot.questions.len());
    for question in &snapshot.questions {
        if question.task_id != snapshot.task_id
            || question.validate().is_err()
            || !ids.insert(question.id)
            || !positions.insert(question.position)
            || question
                .remote_question_id
                .as_deref()
                .is_some_and(|remote_id| !remote_ids.insert(remote_id))
        {
            return Err(invalid_snapshot());
        }
        let json = serde_json::to_string(question)?;
        total_bytes = total_bytes
            .checked_add(json.len())
            .filter(|bytes| *bytes <= MAX_QUESTION_SNAPSHOT_BYTES)
            .ok_or_else(invalid_snapshot)?;
        questions.push(EncodedQuestion {
            id: question.id,
            remote_id: question.remote_question_id.clone(),
            position: question.position,
            json,
            content_fingerprint: question
                .content_fingerprint()
                .map_err(|_| invalid_snapshot())?,
            semantic_fingerprint: semantic_view
                .semantic_fingerprint(question.id)
                .map_err(|_| invalid_snapshot())?,
        });
    }
    let mut group_bytes = 0_usize;
    let mut groups = Vec::with_capacity(snapshot.groups.len());
    for group in &snapshot.groups {
        let json = serde_json::to_string(group)?;
        group_bytes = group_bytes
            .checked_add(json.len())
            .filter(|bytes| *bytes <= MAX_QUESTION_GROUP_BYTES)
            .ok_or_else(invalid_snapshot)?;
        groups.push(EncodedQuestionGroup {
            id: group.id,
            remote_id: group.remote_group_id.clone(),
            json,
        });
    }
    Ok(EncodedSnapshot {
        questions,
        groups,
        total_bytes,
    })
}

fn valid_text(value: &str, maximum: usize) -> bool {
    !value.is_empty()
        && value.len() <= maximum
        && value.trim() == value
        && !value.chars().any(char::is_control)
}

fn parse_id<T>(value: &str) -> Result<T, StorageError>
where
    T: FromStr,
    T::Err: std::fmt::Display,
{
    value
        .parse()
        .map_err(|error: T::Err| StorageError::InvalidData(error.to_string()))
}

fn encode_timestamp(value: Timestamp) -> String {
    value.to_rfc3339_opts(SecondsFormat::Nanos, true)
}

fn decode_timestamp(value: &str) -> Result<Timestamp, StorageError> {
    DateTime::parse_from_rfc3339(value)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|_| invalid_snapshot())
}

fn invalid_snapshot() -> StorageError {
    StorageError::InvalidData("Question snapshot is invalid or exceeds its bounds".to_owned())
}

fn invalid_candidates() -> StorageError {
    StorageError::InvalidData(
        "AnswerCandidate batch is invalid, foreign, or exceeds its bounds".to_owned(),
    )
}

fn invalid_submission_draft() -> StorageError {
    StorageError::InvalidData(
        "SubmissionDraft is invalid, foreign, or exceeds its bounds".to_owned(),
    )
}

fn invalid_submission_result() -> StorageError {
    StorageError::InvalidData(
        "SubmissionResult is invalid, foreign, or exceeds its bounds".to_owned(),
    )
}

#[cfg(test)]
mod tests {
    use asterism_domain::{
        AnswerConfidence, NormalizedAnswer, QuestionAttachment, QuestionAttachmentKind,
        QuestionGroupChild, QuestionKind, QuestionOption, SubmissionPayloadEncoding,
        SubmissionPayloadFieldPreview, SubmissionQuestionVerification,
        SubmissionQuestionVerificationStatus, SubmissionScore, SubmissionVerificationStatus,
    };
    use chrono::Duration;

    use super::*;
    use crate::AnswerEvidenceRepository;

    #[tokio::test]
    async fn snapshots_are_immutable_latest_and_owner_scoped() {
        let fixture = Fixture::new().await;
        let repository = SqliteQuestionSnapshotRepository::new(fixture.database.clone());
        let first = fixture.snapshot("First stem", fixture.now);
        repository.save_question_snapshot(&first).await.unwrap();
        let second = fixture.snapshot("Second stem", fixture.now + Duration::seconds(1));
        repository.save_question_snapshot(&second).await.unwrap();

        let latest = repository
            .find_latest_owned_question_snapshot(fixture.owner, fixture.task)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(latest, second);
        assert_eq!(
            repository
                .find_owned_question_snapshot(fixture.owner, first.id)
                .await
                .unwrap(),
            Some(first.clone())
        );
        assert!(
            repository
                .find_latest_owned_question_snapshot(UserId::new(), fixture.task)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            repository
                .find_owned_question_snapshot(UserId::new(), first.id)
                .await
                .unwrap()
                .is_none()
        );
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM question_snapshots")
            .fetch_one(fixture.database.pool())
            .await
            .unwrap();
        assert_eq!(count, 2);
    }

    #[tokio::test]
    async fn shared_question_groups_round_trip_and_fail_closed_on_tampering() {
        let fixture = Fixture::new().await;
        let repository = SqliteQuestionSnapshotRepository::new(fixture.database.clone());
        let mut snapshot = fixture.snapshot("Child stem", fixture.now);
        snapshot.groups.push(QuestionGroup {
            id: QuestionGroupId::new(),
            task_id: fixture.task,
            remote_group_id: Some("reading-material-1".to_owned()),
            stem: Some("Read the shared passage.".to_owned()),
            options: Vec::new(),
            attachments: Vec::new(),
            metadata_sanitized: serde_json::json!({"family": "reading"}),
            children: vec![QuestionGroupChild::Question(snapshot.questions[0].id)],
        });
        repository.save_question_snapshot(&snapshot).await.unwrap();
        assert_eq!(
            repository
                .find_owned_question_snapshot(fixture.owner, snapshot.id)
                .await
                .unwrap(),
            Some(snapshot.clone())
        );

        sqlx::query("UPDATE question_snapshot_groups SET group_id = ? WHERE snapshot_id = ?")
            .bind(QuestionGroupId::new().to_string())
            .bind(snapshot.id.to_string())
            .execute(fixture.database.pool())
            .await
            .unwrap();
        assert!(
            repository
                .find_owned_question_snapshot(fixture.owner, snapshot.id)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn invalid_batches_and_provider_drift_leave_no_partial_snapshot() {
        let fixture = Fixture::new().await;
        let repository = SqliteQuestionSnapshotRepository::new(fixture.database.clone());
        let mut mixed = fixture.snapshot("Mixed task", fixture.now);
        mixed.questions[0].task_id = TaskId::new();
        assert!(repository.save_question_snapshot(&mixed).await.is_err());

        let mut wrong_provider = fixture.snapshot("Wrong provider", fixture.now);
        wrong_provider.provider_id = ProviderId::new("provider-beta").unwrap();
        assert!(
            repository
                .save_question_snapshot(&wrong_provider)
                .await
                .is_err()
        );
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM question_snapshots")
            .fetch_one(fixture.database.pool())
            .await
            .unwrap();
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn multi_source_candidates_round_trip_with_snapshot_ownership() {
        let fixture = Fixture::new().await;
        let repository = SqliteQuestionSnapshotRepository::new(fixture.database.clone());
        let snapshot = fixture.snapshot("Candidate question", fixture.now);
        repository.save_question_snapshot(&snapshot).await.unwrap();
        let first = Fixture::candidate(
            &snapshot,
            AnswerSource::ProviderNative,
            fixture.now + Duration::seconds(1),
        );
        let second = Fixture::candidate(
            &snapshot,
            AnswerSource::ExternalBank,
            fixture.now + Duration::seconds(2),
        );
        repository
            .save_answer_candidate_batch(&[first.clone(), second.clone()])
            .await
            .unwrap();

        assert_eq!(
            repository
                .list_owned_answer_candidates(fixture.owner, snapshot.id)
                .await
                .unwrap(),
            [first, second]
        );
        assert!(
            repository
                .list_owned_answer_candidates(UserId::new(), snapshot.id)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn foreign_or_unsanitized_candidates_leave_no_partial_batch() {
        let fixture = Fixture::new().await;
        let repository = SqliteQuestionSnapshotRepository::new(fixture.database.clone());
        let snapshot = fixture.snapshot("Candidate question", fixture.now);
        repository.save_question_snapshot(&snapshot).await.unwrap();
        let valid = Fixture::candidate(&snapshot, AnswerSource::Manual, fixture.now);
        let mut foreign = Fixture::candidate(&snapshot, AnswerSource::LocalCache, fixture.now);
        foreign.candidate.question_id = QuestionId::new();
        assert!(
            repository
                .save_answer_candidate_batch(&[valid.clone(), foreign])
                .await
                .is_err()
        );

        let mut secret = valid;
        secret.candidate.provenance_sanitized =
            serde_json::json!({"access_token": "must-not-persist"});
        assert!(
            repository
                .save_answer_candidate_batch(&[secret])
                .await
                .is_err()
        );
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM answer_candidates")
            .fetch_one(fixture.database.pool())
            .await
            .unwrap();
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn prior_answer_evidence_is_direct_unambiguous_and_owner_task_scoped() {
        let fixture = Fixture::new().await;
        let repository = SqliteQuestionSnapshotRepository::new(fixture.database.clone());
        let prior = fixture.snapshot("Stable question", fixture.now);
        let mut target = fixture.snapshot("Stable question", fixture.now + Duration::seconds(1));
        target.questions[0].remote_question_id = Some("fresh-attempt-question".to_owned());
        repository.save_question_snapshot(&prior).await.unwrap();
        repository.save_question_snapshot(&target).await.unwrap();

        let direct = Fixture::candidate(
            &prior,
            AnswerSource::ProviderNative,
            fixture.now + Duration::milliseconds(100),
        );
        let copied = Fixture::candidate(
            &prior,
            AnswerSource::LocalCache,
            fixture.now + Duration::milliseconds(200),
        );
        repository
            .save_answer_candidate_batch(&[direct.clone(), copied])
            .await
            .unwrap();

        let evidence = repository
            .list_owned_prior_answer_evidence(fixture.owner, fixture.task, target.id)
            .await
            .unwrap();
        assert_eq!(evidence.len(), 1);
        assert_eq!(evidence[0].source_question, prior.questions[0]);
        assert_eq!(evidence[0].source_candidate, direct);
        assert_eq!(
            evidence[0].question_content_fingerprint,
            target.questions[0].content_fingerprint().unwrap()
        );
        assert!(
            repository
                .list_owned_prior_answer_evidence(UserId::new(), fixture.task, target.id)
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            repository
                .list_owned_prior_answer_evidence(fixture.owner, TaskId::new(), target.id)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn prior_answer_evidence_skips_ambiguous_or_changed_questions() {
        let fixture = Fixture::new().await;
        let repository = SqliteQuestionSnapshotRepository::new(fixture.database.clone());
        let mut ambiguous_prior = fixture.snapshot("Duplicate question", fixture.now);
        let mut duplicate = ambiguous_prior.questions[0].clone();
        duplicate.id = QuestionId::new();
        duplicate.remote_question_id = Some("attempt-question-2".to_owned());
        duplicate.position = 2;
        ambiguous_prior.questions.push(duplicate);
        let mut matching_target =
            fixture.snapshot("Duplicate question", fixture.now + Duration::seconds(1));
        matching_target.questions[0].remote_question_id = Some("next-question-1".to_owned());
        let changed_target =
            fixture.snapshot("Changed question", fixture.now + Duration::seconds(2));
        repository
            .save_question_snapshot(&ambiguous_prior)
            .await
            .unwrap();
        repository
            .save_question_snapshot(&matching_target)
            .await
            .unwrap();
        repository
            .save_question_snapshot(&changed_target)
            .await
            .unwrap();
        let candidate = Fixture::candidate(
            &ambiguous_prior,
            AnswerSource::Manual,
            fixture.now + Duration::milliseconds(100),
        );
        repository
            .save_answer_candidate_batch(std::slice::from_ref(&candidate))
            .await
            .unwrap();

        assert!(
            repository
                .list_owned_prior_answer_evidence(fixture.owner, fixture.task, matching_target.id,)
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            repository
                .list_owned_prior_answer_evidence(fixture.owner, fixture.task, changed_target.id,)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn exact_prior_answer_evidence_skips_structured_snapshots() {
        let fixture = Fixture::new().await;
        let repository = SqliteQuestionSnapshotRepository::new(fixture.database.clone());
        let mut grouped_prior = fixture.snapshot("Shared leaf", fixture.now);
        grouped_prior.groups.push(QuestionGroup {
            id: QuestionGroupId::new(),
            task_id: fixture.task,
            remote_group_id: Some("prior-passage".to_owned()),
            stem: Some("Prior shared context".to_owned()),
            options: Vec::new(),
            attachments: Vec::new(),
            metadata_sanitized: serde_json::json!({}),
            children: vec![QuestionGroupChild::Question(grouped_prior.questions[0].id)],
        });
        let flat_target = fixture.snapshot("Shared leaf", fixture.now + Duration::seconds(1));
        repository
            .save_question_snapshot(&grouped_prior)
            .await
            .unwrap();
        repository
            .save_question_snapshot(&flat_target)
            .await
            .unwrap();
        let grouped_candidate =
            Fixture::candidate(&grouped_prior, AnswerSource::ProviderNative, fixture.now);
        repository
            .save_answer_candidate_batch(std::slice::from_ref(&grouped_candidate))
            .await
            .unwrap();
        assert!(
            repository
                .list_owned_prior_answer_evidence(fixture.owner, fixture.task, flat_target.id)
                .await
                .unwrap()
                .is_empty()
        );

        let flat_prior = fixture.snapshot("Second shared leaf", fixture.now + Duration::seconds(2));
        let mut grouped_target =
            fixture.snapshot("Second shared leaf", fixture.now + Duration::seconds(3));
        grouped_target.groups.push(QuestionGroup {
            id: QuestionGroupId::new(),
            task_id: fixture.task,
            remote_group_id: Some("target-passage".to_owned()),
            stem: Some("Target shared context".to_owned()),
            options: Vec::new(),
            attachments: Vec::new(),
            metadata_sanitized: serde_json::json!({}),
            children: vec![QuestionGroupChild::Question(grouped_target.questions[0].id)],
        });
        repository
            .save_question_snapshot(&flat_prior)
            .await
            .unwrap();
        repository
            .save_question_snapshot(&grouped_target)
            .await
            .unwrap();
        let flat_candidate =
            Fixture::candidate(&flat_prior, AnswerSource::ProviderNative, fixture.now);
        repository
            .save_answer_candidate_batch(std::slice::from_ref(&flat_candidate))
            .await
            .unwrap();
        assert!(
            repository
                .list_owned_prior_answer_evidence(fixture.owner, fixture.task, grouped_target.id)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn submission_drafts_are_immutable_candidate_bound_and_owner_scoped() {
        let fixture = Fixture::new().await;
        let repository = SqliteQuestionSnapshotRepository::new(fixture.database.clone());
        let snapshot = fixture.snapshot("Draft question", fixture.now);
        repository.save_question_snapshot(&snapshot).await.unwrap();
        let candidate = Fixture::candidate(&snapshot, AnswerSource::Manual, fixture.now);
        repository
            .save_answer_candidate_batch(std::slice::from_ref(&candidate))
            .await
            .unwrap();
        let draft = fixture.draft(&snapshot, &candidate);
        repository.save_submission_draft(&draft).await.unwrap();

        assert_eq!(
            repository
                .find_owned_submission_draft(fixture.owner, draft.id)
                .await
                .unwrap(),
            Some(draft.clone())
        );
        assert!(
            repository
                .find_owned_submission_draft(UserId::new(), draft.id)
                .await
                .unwrap()
                .is_none()
        );
        assert!(repository.save_submission_draft(&draft).await.is_err());
    }

    #[tokio::test]
    async fn partial_submission_draft_requires_the_exact_snapshot_partition() {
        let fixture = Fixture::new().await;
        let repository = SqliteQuestionSnapshotRepository::new(fixture.database.clone());
        let mut snapshot = fixture.snapshot("Selected question", fixture.now);
        let mut unanswered = snapshot.questions[0].clone();
        unanswered.id = QuestionId::new();
        unanswered.remote_question_id = Some("remote-question-2".to_owned());
        unanswered.stem = "Unanswered question".to_owned();
        unanswered.position = 2;
        snapshot.questions.push(unanswered.clone());
        repository.save_question_snapshot(&snapshot).await.unwrap();
        let candidate = Fixture::candidate(&snapshot, AnswerSource::Manual, fixture.now);
        repository
            .save_answer_candidate_batch(std::slice::from_ref(&candidate))
            .await
            .unwrap();
        let mut draft = fixture.draft(&snapshot, &candidate);
        draft.answer_coverage = SubmissionAnswerCoverage {
            total_question_count: 2,
            minimum_coverage_millis: 500,
            unanswered_question_ids: vec![unanswered.id],
        };
        repository.save_submission_draft(&draft).await.unwrap();
        assert_eq!(
            repository
                .find_owned_submission_draft(fixture.owner, draft.id)
                .await
                .unwrap(),
            Some(draft.clone())
        );

        sqlx::query("UPDATE submission_drafts SET unanswered_question_ids_json = ? WHERE id = ?")
            .bind(serde_json::to_string(&[QuestionId::new()]).unwrap())
            .bind(draft.id.to_string())
            .execute(fixture.database.pool())
            .await
            .unwrap();
        assert!(
            repository
                .find_owned_submission_draft(fixture.owner, draft.id)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn foreign_candidate_binding_leaves_no_partial_submission_draft() {
        let fixture = Fixture::new().await;
        let repository = SqliteQuestionSnapshotRepository::new(fixture.database.clone());
        let first = fixture.snapshot("First draft question", fixture.now);
        let second = fixture.snapshot("Second draft question", fixture.now);
        repository.save_question_snapshot(&first).await.unwrap();
        repository.save_question_snapshot(&second).await.unwrap();
        let first_candidate = Fixture::candidate(&first, AnswerSource::Manual, fixture.now);
        let second_candidate = Fixture::candidate(&second, AnswerSource::Manual, fixture.now);
        repository
            .save_answer_candidate_batch(std::slice::from_ref(&first_candidate))
            .await
            .unwrap();
        repository
            .save_answer_candidate_batch(std::slice::from_ref(&second_candidate))
            .await
            .unwrap();

        let mut draft = fixture.draft(&second, &second_candidate);
        draft.items[0].selected.candidate_id = first_candidate.id;
        assert!(repository.save_submission_draft(&draft).await.is_err());
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM submission_drafts")
            .fetch_one(fixture.database.pool())
            .await
            .unwrap();
        assert_eq!(count, 0);
    }

    #[tokio::test]
    #[allow(
        clippy::too_many_lines,
        reason = "the result history regression keeps both scored attempts, evidence projection and owner boundaries in one fixture"
    )]
    async fn submission_results_bind_draft_execution_attempt_and_owner() {
        let fixture = Fixture::new().await;
        let repository = SqliteQuestionSnapshotRepository::new(fixture.database.clone());
        let mut snapshot = fixture.snapshot("Result question", fixture.now);
        snapshot.questions[0].attachments.clear();
        repository.save_question_snapshot(&snapshot).await.unwrap();
        let candidate = Fixture::candidate(&snapshot, AnswerSource::Manual, fixture.now);
        repository
            .save_answer_candidate_batch(std::slice::from_ref(&candidate))
            .await
            .unwrap();
        let draft = fixture.draft(&snapshot, &candidate);
        repository.save_submission_draft(&draft).await.unwrap();
        let (execution_id, attempt_id) = fixture.execution_attempt().await;
        let result = fixture.result(&draft, execution_id, attempt_id);
        repository.save_submission_result(&result).await.unwrap();

        assert_eq!(
            repository
                .find_owned_submission_result(fixture.owner, result.id)
                .await
                .unwrap(),
            Some(result.clone())
        );
        assert_eq!(
            repository
                .find_latest_owned_submission_result(fixture.owner, draft.id)
                .await
                .unwrap(),
            Some(result.clone())
        );
        assert!(
            repository
                .find_owned_submission_result(UserId::new(), result.id)
                .await
                .unwrap()
                .is_none()
        );
        let corpus_counts: (i64, i64, i64) = sqlx::query_as(
            "SELECT official_evidence_count, verified_historical_evidence_count, \
                    negative_evidence_count FROM global_answer_corpus_entries",
        )
        .fetch_one(fixture.database.pool())
        .await
        .unwrap();
        assert_eq!(corpus_counts, (0, 1, 0));
        assert_eq!(
            count_rows(&fixture.database, "private_answer_evidence").await,
            1
        );
        assert_eq!(
            crate::SqliteAnswerEvidenceRepository::new(fixture.database.clone())
                .count_owned_execution_attempt_evidence(fixture.owner, attempt_id)
                .await
                .unwrap(),
            Some(crate::AnswerEvidenceClassCounts {
                official: 0,
                verified_historical: 1,
                negative: 0,
            })
        );
        assert_eq!(
            repository
                .find_previous_owned_submission_score(fixture.owner, fixture.task, result.id)
                .await
                .unwrap(),
            None
        );

        let later = fixture.now + chrono::Duration::seconds(1);
        let mut later_snapshot = fixture.snapshot("Later result question", later);
        later_snapshot.questions[0].attachments.clear();
        repository
            .save_question_snapshot(&later_snapshot)
            .await
            .unwrap();
        let later_candidate = Fixture::candidate(&later_snapshot, AnswerSource::Manual, later);
        repository
            .save_answer_candidate_batch(std::slice::from_ref(&later_candidate))
            .await
            .unwrap();
        let later_draft = fixture.draft(&later_snapshot, &later_candidate);
        repository
            .save_submission_draft(&later_draft)
            .await
            .unwrap();
        let (later_execution_id, later_attempt_id) = fixture.execution_attempt().await;
        let mut later_result = fixture.result(&later_draft, later_execution_id, later_attempt_id);
        later_result.verification.score = Some(SubmissionScore {
            earned_milli_points: 90_000,
            possible_milli_points: 100_000,
        });
        later_result.verification.verified_at = later;
        later_result.created_at = later;
        repository
            .save_submission_result(&later_result)
            .await
            .unwrap();
        assert_eq!(
            repository
                .find_previous_owned_submission_score(fixture.owner, fixture.task, later_result.id,)
                .await
                .unwrap(),
            result.verification.score
        );
        assert!(
            crate::SqliteAnswerEvidenceRepository::new(fixture.database.clone())
                .count_owned_execution_attempt_evidence(UserId::new(), attempt_id)
                .await
                .unwrap()
                .is_none()
        );
        assert!(repository.save_submission_result(&result).await.is_err());
    }

    #[tokio::test]
    async fn grouped_question_results_do_not_project_without_shared_context() {
        let fixture = Fixture::new().await;
        let repository = SqliteQuestionSnapshotRepository::new(fixture.database.clone());
        let mut snapshot = fixture.snapshot("Shared-context result question", fixture.now);
        snapshot.questions[0].attachments.clear();
        snapshot.groups.push(QuestionGroup {
            id: QuestionGroupId::new(),
            task_id: fixture.task,
            remote_group_id: Some("shared-passage-1".to_owned()),
            stem: Some("Read this passage before answering.".to_owned()),
            options: Vec::new(),
            attachments: Vec::new(),
            metadata_sanitized: serde_json::json!({"kind": "passage"}),
            children: vec![QuestionGroupChild::Question(snapshot.questions[0].id)],
        });
        repository.save_question_snapshot(&snapshot).await.unwrap();
        let candidate = Fixture::candidate(&snapshot, AnswerSource::Manual, fixture.now);
        repository
            .save_answer_candidate_batch(std::slice::from_ref(&candidate))
            .await
            .unwrap();
        let draft = fixture.draft(&snapshot, &candidate);
        repository.save_submission_draft(&draft).await.unwrap();
        let (execution_id, attempt_id) = fixture.execution_attempt().await;
        let result = fixture.result(&draft, execution_id, attempt_id);
        repository.save_submission_result(&result).await.unwrap();

        let projection: (String, Option<String>) = sqlx::query_as(
            "SELECT projection_state, unmatched_reason FROM private_answer_evidence",
        )
        .fetch_one(fixture.database.pool())
        .await
        .unwrap();
        assert_eq!(
            projection,
            (
                "unmatched".to_owned(),
                Some("missing_shared_context".to_owned())
            )
        );
        assert_eq!(
            count_rows(&fixture.database, "global_answer_corpus_entries").await,
            0
        );
        assert_eq!(
            count_rows(&fixture.database, "global_answer_corpus_projections").await,
            0
        );
    }

    #[tokio::test]
    async fn rejected_submission_projects_negative_but_unverified_projects_nothing() {
        let fixture = Fixture::new().await;
        let repository = SqliteQuestionSnapshotRepository::new(fixture.database.clone());
        let mut snapshot = fixture.snapshot("Rejected result question", fixture.now);
        snapshot.questions[0].attachments.clear();
        repository.save_question_snapshot(&snapshot).await.unwrap();
        let candidate = Fixture::candidate(&snapshot, AnswerSource::Manual, fixture.now);
        repository
            .save_answer_candidate_batch(std::slice::from_ref(&candidate))
            .await
            .unwrap();
        let draft = fixture.draft(&snapshot, &candidate);
        repository.save_submission_draft(&draft).await.unwrap();
        let (execution_id, attempt_id) = fixture.execution_attempt().await;
        let mut rejected = fixture.result(&draft, execution_id, attempt_id);
        rejected.status = SubmissionResultStatus::Rejected;
        rejected.verification.status = SubmissionVerificationStatus::Rejected;
        rejected.verification.remote_state = Some(asterism_domain::RemoteState::Pending);
        rejected.verification.questions[0].status = SubmissionQuestionVerificationStatus::Rejected;
        repository.save_submission_result(&rejected).await.unwrap();
        let negative_count: i64 =
            sqlx::query_scalar("SELECT negative_evidence_count FROM global_answer_corpus_entries")
                .fetch_one(fixture.database.pool())
                .await
                .unwrap();
        assert_eq!(negative_count, 1);

        let second = Fixture::new().await;
        let second_repository = SqliteQuestionSnapshotRepository::new(second.database.clone());
        let second_snapshot = second.snapshot("Unverified result question", second.now);
        second_repository
            .save_question_snapshot(&second_snapshot)
            .await
            .unwrap();
        let second_candidate =
            Fixture::candidate(&second_snapshot, AnswerSource::Manual, second.now);
        second_repository
            .save_answer_candidate_batch(std::slice::from_ref(&second_candidate))
            .await
            .unwrap();
        let second_draft = second.draft(&second_snapshot, &second_candidate);
        second_repository
            .save_submission_draft(&second_draft)
            .await
            .unwrap();
        let (second_execution, second_attempt) = second.execution_attempt().await;
        let mut unverified = second.result(&second_draft, second_execution, second_attempt);
        unverified.verification.questions[0].status =
            SubmissionQuestionVerificationStatus::Unverified;
        second_repository
            .save_submission_result(&unverified)
            .await
            .unwrap();
        assert_eq!(
            count_rows(&second.database, "private_answer_evidence").await,
            0
        );
        assert_eq!(
            count_rows(&second.database, "global_answer_corpus_entries").await,
            0
        );
    }

    #[tokio::test]
    async fn foreign_attempt_or_question_leaves_no_partial_submission_result() {
        let fixture = Fixture::new().await;
        let repository = SqliteQuestionSnapshotRepository::new(fixture.database.clone());
        let snapshot = fixture.snapshot("Invalid result question", fixture.now);
        repository.save_question_snapshot(&snapshot).await.unwrap();
        let candidate = Fixture::candidate(&snapshot, AnswerSource::Manual, fixture.now);
        repository
            .save_answer_candidate_batch(std::slice::from_ref(&candidate))
            .await
            .unwrap();
        let draft = fixture.draft(&snapshot, &candidate);
        repository.save_submission_draft(&draft).await.unwrap();
        let (execution_id, attempt_id) = fixture.execution_attempt().await;

        let mut foreign_attempt = fixture.result(&draft, execution_id, attempt_id);
        foreign_attempt.execution_attempt_id = ExecutionAttemptId::new();
        assert!(
            repository
                .save_submission_result(&foreign_attempt)
                .await
                .is_err()
        );
        let mut foreign_question = fixture.result(&draft, execution_id, attempt_id);
        foreign_question.verification.questions[0].question_id = QuestionId::new();
        assert!(
            repository
                .save_submission_result(&foreign_question)
                .await
                .is_err()
        );
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM submission_results")
            .fetch_one(fixture.database.pool())
            .await
            .unwrap();
        assert_eq!(count, 0);
        assert_eq!(
            count_rows(&fixture.database, "private_answer_evidence").await,
            0
        );
    }

    struct Fixture {
        database: Database,
        owner: UserId,
        task: TaskId,
        provider: ProviderId,
        now: Timestamp,
    }

    impl Fixture {
        async fn new() -> Self {
            let database = Database::connect("sqlite::memory:").await.unwrap();
            database.migrate().await.unwrap();
            let owner = UserId::new();
            let account = asterism_domain::ProviderAccountId::new();
            let task = TaskId::new();
            let provider = ProviderId::new("provider-alpha").unwrap();
            let now = Utc::now();
            let timestamp = encode_timestamp(now);
            sqlx::query(
                "INSERT INTO users \
                 (id, username, password_hash, status, roles_json, permissions_json, created_at, updated_at) \
                 VALUES (?, 'question-owner', '$argon2id$test', 'active', '[\"user\"]', '[]', ?, ?)",
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
                 VALUES (?, ?, ?, 'questions', '\"authenticated\"', ?, ?)",
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
                 VALUES (?, ?, 'remote-work', 'v1:test', 'work', 'unknown', 'Work', \
                         'pending', 'discovered', ?, ?, '[]')",
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
                task,
                provider,
                now,
            }
        }

        fn snapshot(&self, stem: &str, captured_at: Timestamp) -> QuestionSnapshot {
            QuestionSnapshot {
                id: QuestionSnapshotId::new(),
                task_id: self.task,
                provider_id: self.provider.clone(),
                provider_version: "1.0.0-test".to_owned(),
                captured_at,
                questions: vec![Question {
                    id: QuestionId::new(),
                    task_id: self.task,
                    remote_question_id: Some("attempt-question-1".to_owned()),
                    kind: QuestionKind::SingleChoice,
                    stem: stem.to_owned(),
                    options: vec![QuestionOption {
                        id: "A".to_owned(),
                        content: Some("Option A".to_owned()),
                        attachments: Vec::new(),
                        metadata_sanitized: serde_json::json!({}),
                    }],
                    attachments: vec![QuestionAttachment {
                        kind: QuestionAttachmentKind::Image,
                        remote_id: Some("image-1".to_owned()),
                        label: Some("Question image".to_owned()),
                        metadata_sanitized: serde_json::json!({}),
                    }],
                    metadata_sanitized: serde_json::json!({"page_kind": "work_preview"}),
                    position: 1,
                }],
                groups: Vec::new(),
            }
        }

        fn candidate(
            snapshot: &QuestionSnapshot,
            source: AnswerSource,
            created_at: Timestamp,
        ) -> AnswerCandidateRecord {
            AnswerCandidateRecord {
                id: AnswerCandidateId::new(),
                question_snapshot_id: snapshot.id,
                candidate: AnswerCandidate {
                    question_id: snapshot.questions[0].id,
                    source,
                    answer: NormalizedAnswer::Selections(vec!["A".to_owned()]),
                    confidence: Some(AnswerConfidence::try_new(8_000).unwrap()),
                    explanation: Some("Bounded candidate explanation".to_owned()),
                    provenance_sanitized: serde_json::json!({"resolver": "fixture"}),
                },
                created_at,
            }
        }

        fn draft(
            &self,
            snapshot: &QuestionSnapshot,
            candidate: &AnswerCandidateRecord,
        ) -> SubmissionDraft {
            SubmissionDraft {
                id: SubmissionDraftId::new(),
                task_id: snapshot.task_id,
                question_snapshot_id: snapshot.id,
                provider_id: snapshot.provider_id.clone(),
                provider_version: "1.0.0-builder".to_owned(),
                answer_coverage: SubmissionAnswerCoverage {
                    total_question_count: 1,
                    minimum_coverage_millis: 1_000,
                    unanswered_question_ids: Vec::new(),
                },
                items: vec![SubmissionDraftItem {
                    question: snapshot.questions[0].clone(),
                    selected: SelectedAnswer {
                        candidate_id: candidate.id,
                        question_id: candidate.candidate.question_id,
                        answer: candidate.candidate.answer.clone(),
                        source: candidate.candidate.source,
                        confidence: candidate.candidate.confidence,
                    },
                }],
                payload_preview: SubmissionPayloadPreview {
                    encoding: SubmissionPayloadEncoding::Form,
                    format: "provider-alpha.work.v1".to_owned(),
                    fields: vec![SubmissionPayloadFieldPreview {
                        question_id: snapshot.questions[0].id,
                        field_name: "answer[attempt-question-1]".to_owned(),
                    }],
                },
                created_at: self.now,
            }
        }

        async fn execution_attempt(&self) -> (ExecutionId, ExecutionAttemptId) {
            let execution_id = ExecutionId::new();
            let attempt_id = ExecutionAttemptId::new();
            let timestamp = encode_timestamp(self.now);
            sqlx::query(
                "INSERT INTO executions \
                 (id, task_id, requested_by, request_source, state, started_at, created_at) \
                 VALUES (?, ?, ?, 'api', 'running', ?, ?)",
            )
            .bind(execution_id.to_string())
            .bind(self.task.to_string())
            .bind(self.owner.to_string())
            .bind(&timestamp)
            .bind(&timestamp)
            .execute(self.database.pool())
            .await
            .unwrap();
            sqlx::query(
                "INSERT INTO execution_attempts \
                 (id, execution_id, attempt_no, started_at) VALUES (?, ?, 1, ?)",
            )
            .bind(attempt_id.to_string())
            .bind(execution_id.to_string())
            .bind(timestamp)
            .execute(self.database.pool())
            .await
            .unwrap();
            (execution_id, attempt_id)
        }

        fn result(
            &self,
            draft: &SubmissionDraft,
            execution_id: ExecutionId,
            execution_attempt_id: ExecutionAttemptId,
        ) -> SubmissionResult {
            SubmissionResult {
                id: SubmissionResultId::new(),
                submission_draft_id: draft.id,
                execution_id,
                execution_attempt_id,
                task_id: draft.task_id,
                question_snapshot_id: draft.question_snapshot_id,
                provider_id: draft.provider_id.clone(),
                provider_version: "1.0.0-submit".to_owned(),
                status: SubmissionResultStatus::Confirmed,
                receipt: Some(SubmissionReceipt {
                    remote_status: "accepted".to_owned(),
                    message_sanitized: None,
                    provider_trace_id: Some("trace-1".to_owned()),
                    received_at: self.now,
                }),
                verification: SubmissionVerificationSnapshot {
                    status: SubmissionVerificationStatus::Confirmed,
                    remote_state: Some(asterism_domain::RemoteState::Completed),
                    score: Some(SubmissionScore {
                        earned_milli_points: 100_000,
                        possible_milli_points: 100_000,
                    }),
                    progress_percent: Some(100),
                    questions: vec![SubmissionQuestionVerification {
                        question_id: draft.items[0].question.id,
                        status: SubmissionQuestionVerificationStatus::Confirmed,
                    }],
                    verified_at: self.now,
                },
                created_at: self.now,
            }
        }
    }

    async fn count_rows(database: &Database, table: &str) -> i64 {
        sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {table}"))
            .fetch_one(database.pool())
            .await
            .unwrap()
    }
}
