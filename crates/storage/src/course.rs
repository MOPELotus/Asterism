use std::str::FromStr;

use asterism_domain::{
    Course, CourseAggregateProgress, CourseId, CourseScoreProgress, ProviderAccountId,
    SubmissionScore, SubmissionVerificationSnapshot, Timestamp, UserId,
};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::Row;

use crate::{CourseAggregateProgressRecord, CourseProgressRepository, Database, StorageError};

#[derive(Clone, Debug)]
pub struct SqliteCourseProgressRepository {
    database: Database,
}

impl SqliteCourseProgressRepository {
    pub const fn new(database: Database) -> Self {
        Self { database }
    }
}

#[async_trait]
impl CourseProgressRepository for SqliteCourseProgressRepository {
    #[allow(
        clippy::too_many_lines,
        reason = "the aggregate keeps Course ownership, Task counts and latest-score selection in one read boundary"
    )]
    async fn find_owned_course_aggregate_progress(
        &self,
        owner_id: UserId,
        course_id: CourseId,
    ) -> Result<Option<CourseAggregateProgressRecord>, StorageError> {
        let course = sqlx::query(
            "SELECT course.id, course.provider_account_id, course.remote_id, course.title, \
                    course.term, course.teacher, course.remote_status, course.metadata_json, \
                    course.last_seen_at \
             FROM courses AS course \
             INNER JOIN provider_accounts AS account \
                     ON account.id = course.provider_account_id \
             WHERE course.id = ? AND account.owner_user_id = ?",
        )
        .bind(course_id.to_string())
        .bind(owner_id.to_string())
        .fetch_optional(self.database.pool())
        .await?;
        let Some(course) = course else {
            return Ok(None);
        };
        let course = decode_course(&course)?;
        let counts = sqlx::query(
            "SELECT COUNT(*) AS total, \
                    COALESCE(SUM(CASE WHEN remote_state <> 'removed' THEN 1 ELSE 0 END), 0) AS countable, \
                    COALESCE(SUM(CASE WHEN remote_state = 'completed' THEN 1 ELSE 0 END), 0) AS completed, \
                    COALESCE(SUM(CASE WHEN remote_state <> 'removed' AND remote_state <> 'completed' THEN 1 ELSE 0 END), 0) AS remaining, \
                    COALESCE(SUM(CASE WHEN remote_state = 'not_open' THEN 1 ELSE 0 END), 0) AS not_open, \
                    COALESCE(SUM(CASE WHEN remote_state <> 'removed' AND orchestration_state = 'credit_blocked' THEN 1 ELSE 0 END), 0) AS credit_blocked, \
                    COALESCE(SUM(CASE WHEN remote_state <> 'removed' AND orchestration_state = 'human_required' THEN 1 ELSE 0 END), 0) AS human_required, \
                    COALESCE(SUM(CASE WHEN remote_state <> 'removed' AND orchestration_state = 'failed' THEN 1 ELSE 0 END), 0) AS failed, \
                    MAX(updated_at) AS last_task_update \
             FROM tasks WHERE course_id = ?",
        )
        .bind(course_id.to_string())
        .fetch_one(self.database.pool())
        .await?;
        let total_task_count = decode_count(&counts, "total")?;
        let countable_task_count = decode_count(&counts, "countable")?;
        let completed_task_count = decode_count(&counts, "completed")?;
        let remaining_task_count = decode_count(&counts, "remaining")?;
        let mut observed_at = course.last_seen_at;
        if let Some(last_task_update) = counts.try_get::<Option<&str>, _>("last_task_update")? {
            observed_at = observed_at.max(decode_timestamp(last_task_update)?);
        }

        let score_rows = sqlx::query(
            "WITH ranked AS ( \
                 SELECT result.verification_json, \
                        ROW_NUMBER() OVER ( \
                            PARTITION BY task.id \
                            ORDER BY result.created_at DESC, result.id DESC \
                        ) AS result_position \
                 FROM tasks AS task \
                 INNER JOIN submission_results AS result ON result.task_id = task.id \
                 WHERE task.course_id = ? AND task.remote_state <> 'removed' \
             ) \
             SELECT verification_json FROM ranked WHERE result_position = 1",
        )
        .bind(course_id.to_string())
        .fetch_all(self.database.pool())
        .await?;
        let mut score_total = 0_u64;
        let mut scored_task_count = 0_u32;
        let mut last_score_verified_at = None;
        for row in score_rows {
            let verification: SubmissionVerificationSnapshot =
                serde_json::from_str(row.try_get("verification_json")?)?;
            verification.validate().map_err(|_| invalid_progress())?;
            let Some(score) = verification.score else {
                continue;
            };
            score_total = score_total
                .checked_add(u64::from(score_millis(score)))
                .ok_or_else(invalid_progress)?;
            scored_task_count = scored_task_count
                .checked_add(1)
                .ok_or_else(invalid_progress)?;
            last_score_verified_at = Some(
                last_score_verified_at.map_or(verification.verified_at, |current: Timestamp| {
                    current.max(verification.verified_at)
                }),
            );
            observed_at = observed_at.max(verification.verified_at);
        }
        let score = if scored_task_count == 0 {
            None
        } else {
            Some(CourseScoreProgress {
                scored_task_count,
                average_score_millis: u16::try_from(score_total / u64::from(scored_task_count))
                    .map_err(|_| invalid_progress())?,
                last_verified_at: last_score_verified_at.ok_or_else(invalid_progress)?,
            })
        };
        let progress = CourseAggregateProgress {
            course_id,
            provider_account_id: course.provider_account_id,
            total_task_count,
            countable_task_count,
            completed_task_count,
            remaining_task_count,
            not_open_task_count: decode_count(&counts, "not_open")?,
            credit_blocked_task_count: decode_count(&counts, "credit_blocked")?,
            human_required_task_count: decode_count(&counts, "human_required")?,
            failed_task_count: decode_count(&counts, "failed")?,
            completion_millis: (countable_task_count > 0).then(|| {
                u16::try_from(
                    u64::from(completed_task_count) * 1_000 / u64::from(countable_task_count),
                )
                .expect("bounded course completion ratio fits u16")
            }),
            required: None,
            duration: None,
            score,
            observed_at,
        };
        progress.validate().map_err(|_| invalid_progress())?;
        Ok(Some(CourseAggregateProgressRecord { course, progress }))
    }
}

fn decode_course(row: &sqlx::sqlite::SqliteRow) -> Result<Course, StorageError> {
    Ok(Course {
        id: CourseId::from_str(row.try_get("id")?)
            .map_err(|error| StorageError::InvalidData(error.to_string()))?,
        provider_account_id: ProviderAccountId::from_str(row.try_get("provider_account_id")?)
            .map_err(|error| StorageError::InvalidData(error.to_string()))?,
        remote_id: row.try_get("remote_id")?,
        title: row.try_get("title")?,
        term: row.try_get("term")?,
        teacher: row.try_get("teacher")?,
        remote_status: row.try_get("remote_status")?,
        metadata: serde_json::from_str(row.try_get("metadata_json")?)?,
        last_seen_at: decode_timestamp(row.try_get("last_seen_at")?)?,
    })
}

fn decode_count(row: &sqlx::sqlite::SqliteRow, column: &str) -> Result<u32, StorageError> {
    u32::try_from(row.try_get::<i64, _>(column)?).map_err(|_| invalid_progress())
}

fn score_millis(score: SubmissionScore) -> u16 {
    u16::try_from(
        u128::from(score.earned_milli_points) * 1_000 / u128::from(score.possible_milli_points),
    )
    .expect("validated score ratio fits u16")
}

fn decode_timestamp(value: &str) -> Result<Timestamp, StorageError> {
    DateTime::parse_from_rfc3339(value)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|error| StorageError::InvalidData(error.to_string()))
}

fn invalid_progress() -> StorageError {
    StorageError::InvalidData("course aggregate progress is inconsistent".to_owned())
}

#[cfg(test)]
mod tests {
    use asterism_domain::{
        AuthState, ExecutionAttemptId, ExecutionId, QuestionSnapshotId, SubmissionDraftId,
        SubmissionResultId,
    };
    use chrono::Duration;

    use super::*;

    #[tokio::test]
    #[allow(
        clippy::too_many_lines,
        reason = "the Course fixture keeps overlapping Task states, score history and owner isolation together"
    )]
    async fn course_progress_is_owner_scoped_and_uses_latest_verified_task_score() {
        let database = Database::connect("sqlite::memory:").await.unwrap();
        database.migrate().await.unwrap();
        let now = Utc::now();
        let owner = UserId::new();
        let other_owner = UserId::new();
        for (user, username) in [(owner, "owner"), (other_owner, "other")] {
            sqlx::query(
                "INSERT INTO users \
                 (id, username, password_hash, status, roles_json, permissions_json, created_at, updated_at) \
                 VALUES (?, ?, 'hash', 'active', '[\"user\"]', '[]', ?, ?)",
            )
            .bind(user.to_string())
            .bind(username)
            .bind(now.to_rfc3339())
            .bind(now.to_rfc3339())
            .execute(database.pool())
            .await
            .unwrap();
        }
        let account_id = ProviderAccountId::new();
        sqlx::query(
            "INSERT INTO provider_accounts \
             (id, owner_user_id, provider_id, display_name, auth_state_json, created_at, updated_at) \
             VALUES (?, ?, 'provider-alpha', 'Provider', ?, ?, ?)",
        )
        .bind(account_id.to_string())
        .bind(owner.to_string())
        .bind(serde_json::to_string(&AuthState::Authenticated).unwrap())
        .bind(now.to_rfc3339())
        .bind(now.to_rfc3339())
        .execute(database.pool())
        .await
        .unwrap();
        let course_id = CourseId::new();
        sqlx::query(
            "INSERT INTO courses \
             (id, provider_account_id, remote_id, title, metadata_json, last_seen_at) \
             VALUES (?, ?, 'course-1', 'Course', '{}', ?)",
        )
        .bind(course_id.to_string())
        .bind(account_id.to_string())
        .bind(now.to_rfc3339())
        .execute(database.pool())
        .await
        .unwrap();
        let states = [
            ("completed", "ready"),
            ("pending", "human_required"),
            ("not_open", "credit_blocked"),
            ("pending", "failed"),
            ("removed", "human_required"),
        ];
        let mut task_ids = Vec::new();
        for (position, (remote_state, orchestration_state)) in states.into_iter().enumerate() {
            let task_id = asterism_domain::TaskId::new();
            task_ids.push(task_id);
            let updated_at = now + Duration::seconds(i64::try_from(position).unwrap());
            sqlx::query(
                "INSERT INTO tasks \
                 (id, provider_account_id, course_id, remote_id, remote_fingerprint, source_type, \
                  assessment_class, title, remote_state, orchestration_state, discovered_at, \
                  updated_at, capabilities_json) \
                 VALUES (?, ?, ?, ?, ?, 'work', 'routine', ?, ?, ?, ?, ?, '[]')",
            )
            .bind(task_id.to_string())
            .bind(account_id.to_string())
            .bind(course_id.to_string())
            .bind(format!("task-{position}"))
            .bind(format!("fingerprint-{position}"))
            .bind(format!("Task {position}"))
            .bind(remote_state)
            .bind(orchestration_state)
            .bind(now.to_rfc3339())
            .bind(updated_at.to_rfc3339())
            .execute(database.pool())
            .await
            .unwrap();
        }
        insert_score_result(&database, task_ids[0], now, 600).await;
        insert_score_result(&database, task_ids[0], now + Duration::seconds(1), 800).await;

        let repository = SqliteCourseProgressRepository::new(database);
        let record = repository
            .find_owned_course_aggregate_progress(owner, course_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(record.course.id, course_id);
        assert_eq!(record.progress.total_task_count, 5);
        assert_eq!(record.progress.countable_task_count, 4);
        assert_eq!(record.progress.completed_task_count, 1);
        assert_eq!(record.progress.remaining_task_count, 3);
        assert_eq!(record.progress.not_open_task_count, 1);
        assert_eq!(record.progress.credit_blocked_task_count, 1);
        assert_eq!(record.progress.human_required_task_count, 1);
        assert_eq!(record.progress.failed_task_count, 1);
        assert_eq!(record.progress.completion_millis, Some(250));
        assert_eq!(
            record.progress.score,
            Some(CourseScoreProgress {
                scored_task_count: 1,
                average_score_millis: 800,
                last_verified_at: now + Duration::seconds(1),
            })
        );
        assert!(record.progress.required.is_none());
        assert!(record.progress.duration.is_none());
        assert!(
            repository
                .find_owned_course_aggregate_progress(other_owner, course_id)
                .await
                .unwrap()
                .is_none()
        );
    }

    async fn insert_score_result(
        database: &Database,
        task_id: asterism_domain::TaskId,
        at: Timestamp,
        score_millis: u64,
    ) {
        let snapshot_id = QuestionSnapshotId::new();
        sqlx::query(
            "INSERT INTO question_snapshots \
             (id, task_id, provider_id, provider_version, captured_at, question_count, total_bytes) \
             VALUES (?, ?, 'provider-alpha', 'test', ?, 0, 0)",
        )
        .bind(snapshot_id.to_string())
        .bind(task_id.to_string())
        .bind(at.to_rfc3339())
        .execute(database.pool())
        .await
        .unwrap();
        let draft_id = SubmissionDraftId::new();
        sqlx::query(
            "INSERT INTO submission_drafts \
             (id, question_snapshot_id, task_id, provider_id, provider_version, \
              payload_preview_json, preview_bytes, item_count, created_at) \
             VALUES (?, ?, ?, 'provider-alpha', 'test', '{}', 2, 1, ?)",
        )
        .bind(draft_id.to_string())
        .bind(snapshot_id.to_string())
        .bind(task_id.to_string())
        .bind(at.to_rfc3339())
        .execute(database.pool())
        .await
        .unwrap();
        let execution_id = ExecutionId::new();
        let attempt_id = ExecutionAttemptId::new();
        sqlx::query(
            "INSERT INTO executions (id, task_id, request_source, state, started_at, created_at) \
             VALUES (?, ?, 'system', 'running', ?, ?)",
        )
        .bind(execution_id.to_string())
        .bind(task_id.to_string())
        .bind(at.to_rfc3339())
        .bind(at.to_rfc3339())
        .execute(database.pool())
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO execution_attempts (id, execution_id, attempt_no, started_at) \
             VALUES (?, ?, 1, ?)",
        )
        .bind(attempt_id.to_string())
        .bind(execution_id.to_string())
        .bind(at.to_rfc3339())
        .execute(database.pool())
        .await
        .unwrap();
        let verification = SubmissionVerificationSnapshot {
            status: asterism_domain::SubmissionVerificationStatus::Confirmed,
            remote_state: Some(asterism_domain::RemoteState::Completed),
            score: Some(SubmissionScore {
                earned_milli_points: score_millis,
                possible_milli_points: 1_000,
            }),
            progress_percent: Some(100),
            questions: Vec::new(),
            verified_at: at,
        };
        let verification_json = serde_json::to_string(&verification).unwrap();
        sqlx::query(
            "INSERT INTO submission_results \
             (id, submission_draft_id, execution_id, execution_attempt_id, task_id, \
              question_snapshot_id, provider_id, provider_version, status, verification_json, \
              verification_bytes, created_at) \
             VALUES (?, ?, ?, ?, ?, ?, 'provider-alpha', 'test', 'confirmed', ?, ?, ?)",
        )
        .bind(SubmissionResultId::new().to_string())
        .bind(draft_id.to_string())
        .bind(execution_id.to_string())
        .bind(attempt_id.to_string())
        .bind(task_id.to_string())
        .bind(snapshot_id.to_string())
        .bind(&verification_json)
        .bind(i64::try_from(verification_json.len()).unwrap())
        .bind(at.to_rfc3339())
        .execute(database.pool())
        .await
        .unwrap();
    }
}
