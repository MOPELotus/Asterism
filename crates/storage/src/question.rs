use std::{collections::BTreeSet, str::FromStr};

use asterism_domain::{
    AnswerCandidate, AnswerCandidateId, AnswerSource, ProviderId, Question, QuestionId,
    QuestionSnapshotId, TaskId, Timestamp, UserId,
};
use async_trait::async_trait;
use chrono::{DateTime, SecondsFormat, Utc};
use sqlx::{Row, sqlite::SqliteRow};

use crate::{
    AnswerCandidateRecord, AnswerCandidateRepository, Database, QuestionSnapshot,
    QuestionSnapshotRepository, StorageError,
};

const MAX_QUESTIONS_PER_SNAPSHOT: usize = 5_000;
const MAX_QUESTION_SNAPSHOT_BYTES: usize = 16 * 1_024 * 1_024;
const MAX_PROVIDER_VERSION_BYTES: usize = 128;
const MAX_CANDIDATES_PER_SNAPSHOT: usize = 20_000;
const MAX_ANSWER_CANDIDATE_BYTES: usize = 16 * 1_024 * 1_024;

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
        let encoded = validate_and_encode(snapshot)?;
        let mut transaction = self.database.pool().begin().await?;
        let actual_provider: Option<String> = sqlx::query_scalar(
            "SELECT account.provider_id FROM tasks AS task \
             INNER JOIN provider_accounts AS account \
                ON account.id = task.provider_account_id \
             WHERE task.id = ?",
        )
        .bind(snapshot.task_id.to_string())
        .fetch_optional(&mut *transaction)
        .await?;
        if actual_provider.as_deref() != Some(snapshot.provider_id.as_str()) {
            return Err(StorageError::InvalidData(
                "question snapshot task/provider binding is invalid".to_owned(),
            ));
        }

        sqlx::query(
            "INSERT INTO question_snapshots \
             (id, task_id, provider_id, provider_version, captured_at, \
              question_count, total_bytes) VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(snapshot.id.to_string())
        .bind(snapshot.task_id.to_string())
        .bind(snapshot.provider_id.as_str())
        .bind(&snapshot.provider_version)
        .bind(encode_timestamp(snapshot.captured_at))
        .bind(i64::try_from(encoded.len()).expect("bounded Question count fits i64"))
        .bind(
            i64::try_from(encoded.total_bytes)
                .expect("bounded Question snapshot byte count fits i64"),
        )
        .execute(&mut *transaction)
        .await?;

        for question in encoded.questions {
            sqlx::query(
                "INSERT INTO question_snapshot_items \
                 (snapshot_id, question_id, remote_question_id, position, question_json) \
                 VALUES (?, ?, ?, ?, ?)",
            )
            .bind(snapshot.id.to_string())
            .bind(question.id.to_string())
            .bind(question.remote_id)
            .bind(i64::from(question.position))
            .bind(question.json)
            .execute(&mut *transaction)
            .await?;
        }
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
                    snapshot.question_count, snapshot.total_bytes \
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
                    snapshot.question_count, snapshot.total_bytes \
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

async fn decode_question_snapshot(
    database: &Database,
    row: &SqliteRow,
) -> Result<QuestionSnapshot, StorageError> {
    let snapshot_id = parse_id::<QuestionSnapshotId>(row.try_get("id")?)?;
    let stored_count = usize::try_from(row.try_get::<i64, _>("question_count")?)
        .map_err(|_| invalid_snapshot())?;
    let stored_bytes =
        usize::try_from(row.try_get::<i64, _>("total_bytes")?).map_err(|_| invalid_snapshot())?;
    let item_rows = sqlx::query(
        "SELECT question_id, remote_question_id, position, question_json \
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
        if question.id != stored_id
            || question.remote_question_id != stored_remote_id
            || question.position != stored_position
        {
            return Err(invalid_snapshot());
        }
        questions.push(question);
    }
    if total_bytes != stored_bytes || total_bytes > MAX_QUESTION_SNAPSHOT_BYTES {
        return Err(invalid_snapshot());
    }

    let snapshot = QuestionSnapshot {
        id: snapshot_id,
        task_id: parse_id::<TaskId>(row.try_get("task_id")?)?,
        provider_id: ProviderId::new(row.try_get::<String, _>("provider_id")?)
            .map_err(|_| invalid_snapshot())?,
        provider_version: row.try_get("provider_version")?,
        captured_at: decode_timestamp(row.try_get("captured_at")?)?,
        questions,
    };
    validate_and_encode(&snapshot)?;
    Ok(snapshot)
}

#[async_trait]
impl AnswerCandidateRepository for SqliteQuestionSnapshotRepository {
    async fn save_answer_candidate_batch(
        &self,
        candidates: &[AnswerCandidateRecord],
    ) -> Result<(), StorageError> {
        let encoded = validate_and_encode_candidates(candidates)?;
        let mut transaction = self.database.pool().begin().await?;
        let question_ids = sqlx::query_scalar::<_, String>(
            "SELECT question_id FROM question_snapshot_items WHERE snapshot_id = ?",
        )
        .bind(encoded.snapshot_id.to_string())
        .fetch_all(&mut *transaction)
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
        .fetch_one(&mut *transaction)
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
            .execute(&mut *transaction)
            .await?;
        }
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
        AnswerSource::ExternalBank => "external_bank",
        AnswerSource::Other => "other",
    }
}

fn decode_answer_source(value: &str) -> Result<AnswerSource, StorageError> {
    match value {
        "manual" => Ok(AnswerSource::Manual),
        "local_cache" => Ok(AnswerSource::LocalCache),
        "provider_native" => Ok(AnswerSource::ProviderNative),
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
}

struct EncodedSnapshot {
    questions: Vec<EncodedQuestion>,
    total_bytes: usize,
}

impl EncodedSnapshot {
    fn len(&self) -> usize {
        self.questions.len()
    }
}

fn validate_and_encode(snapshot: &QuestionSnapshot) -> Result<EncodedSnapshot, StorageError> {
    if snapshot.questions.len() > MAX_QUESTIONS_PER_SNAPSHOT
        || !valid_text(&snapshot.provider_version, MAX_PROVIDER_VERSION_BYTES)
    {
        return Err(invalid_snapshot());
    }
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
        });
    }
    Ok(EncodedSnapshot {
        questions,
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

#[cfg(test)]
mod tests {
    use asterism_domain::{
        AnswerConfidence, NormalizedAnswer, QuestionAttachment, QuestionAttachmentKind,
        QuestionKind, QuestionOption,
    };
    use chrono::Duration;

    use super::*;

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
    }
}
