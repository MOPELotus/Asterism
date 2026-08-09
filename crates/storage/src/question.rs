use std::{collections::BTreeSet, str::FromStr};

use asterism_domain::{
    ProviderId, Question, QuestionId, QuestionSnapshotId, TaskId, Timestamp, UserId,
};
use async_trait::async_trait;
use chrono::{DateTime, SecondsFormat, Utc};
use sqlx::Row;

use crate::{Database, QuestionSnapshot, QuestionSnapshotRepository, StorageError};

const MAX_QUESTIONS_PER_SNAPSHOT: usize = 5_000;
const MAX_QUESTION_SNAPSHOT_BYTES: usize = 16 * 1_024 * 1_024;
const MAX_PROVIDER_VERSION_BYTES: usize = 128;

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
        let Some(row) = row else {
            return Ok(None);
        };

        let snapshot_id = parse_id::<QuestionSnapshotId>(row.try_get("id")?)?;
        let stored_count = usize::try_from(row.try_get::<i64, _>("question_count")?)
            .map_err(|_| invalid_snapshot())?;
        let stored_bytes = usize::try_from(row.try_get::<i64, _>("total_bytes")?)
            .map_err(|_| invalid_snapshot())?;
        let item_rows = sqlx::query(
            "SELECT question_id, remote_question_id, position, question_json \
             FROM question_snapshot_items WHERE snapshot_id = ? ORDER BY position",
        )
        .bind(snapshot_id.to_string())
        .fetch_all(self.database.pool())
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
            let stored_position = u32::try_from(item.try_get::<i64, _>("position")?)
                .map_err(|_| invalid_snapshot())?;
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
        Ok(Some(snapshot))
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

#[cfg(test)]
mod tests {
    use asterism_domain::{
        QuestionAttachment, QuestionAttachmentKind, QuestionKind, QuestionOption,
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
        assert!(
            repository
                .find_latest_owned_question_snapshot(UserId::new(), fixture.task)
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
    }
}
