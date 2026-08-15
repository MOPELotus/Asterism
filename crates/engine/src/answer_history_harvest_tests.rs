use std::{
    collections::BTreeSet,
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

use asterism_domain::{
    AnswerBootstrapHarvestId, AuthState, NormalizedAnswer, ProtocolObservationKind,
    ProtocolSurface, ProviderAccountId, ProviderId, Question, QuestionId, QuestionKind,
    QuestionOption, ScheduleId, SubmissionScore, TaskId, Timestamp, UserId,
};
use asterism_provider_api::{
    AnswerHistoryCursor, AnswerHistoryHarvestCapability, AnswerHistoryPage,
    AnswerHistoryQuestionEvidence, AnswerHistoryRetakeFacts, AnswerHistoryTaskRef,
    AnswerHistoryTaskRequest, ProviderAnswerHistoryTaskEvidence, ProviderCapability,
    ProviderContext, ProviderEntry, ProviderError, ProviderErrorKind, ProviderIdentity,
    ProviderMetadata, ProviderRegistry, ProviderResult, ProviderRouteContext, VerificationLevel,
};
use asterism_scheduler::ScheduledJobKind;
use asterism_storage::{
    Database, SqliteAnswerBootstrapHarvestRepository, SqliteAnswerHistoryIngestionRepository,
    SqliteProtocolObservationRepository, SqliteProviderAccountRepository,
    SqliteTaskQueryRepository,
};
use async_trait::async_trait;
use chrono::{Duration, SecondsFormat, Utc};
use serde_json::json;

use super::*;

#[derive(Debug)]
struct FakeHistoryProvider {
    metadata: ProviderMetadata,
    observed_at: Timestamp,
    list_cursors: Mutex<Vec<Option<AnswerHistoryCursor>>>,
    fail_list_with_drift: AtomicBool,
}

impl ProviderIdentity for FakeHistoryProvider {
    fn metadata(&self) -> &ProviderMetadata {
        &self.metadata
    }
}

#[async_trait]
impl AnswerHistoryHarvestCapability for FakeHistoryProvider {
    async fn list_answer_history(
        &self,
        _context: &ProviderContext,
        cursor: Option<&AnswerHistoryCursor>,
    ) -> ProviderResult<AnswerHistoryPage> {
        self.list_cursors.lock().unwrap().push(cursor.cloned());
        if self.fail_list_with_drift.load(Ordering::Relaxed) {
            return Err(ProviderError::new(
                ProviderErrorKind::ProtocolDrift,
                "history result page shape changed",
            )
            .try_with_protocol_observation(
                ProtocolSurface::SubmissionVerify,
                ProtocolObservationKind::UnknownResultShape,
                json!({"document": "history_result", "answer_list_kind": "object"}),
            )
            .unwrap());
        }
        let (ordinal, next_cursor, complete) = if cursor.is_none() {
            (
                1_u8,
                Some(AnswerHistoryCursor {
                    version: 1,
                    cursor_type: "provider-alpha.history.v1".to_owned(),
                    value_sanitized: json!({"page": 2}),
                }),
                false,
            )
        } else {
            (2_u8, None, true)
        };
        AnswerHistoryPage::try_new(
            &self.metadata.id,
            vec![AnswerHistoryTaskRef {
                remote_task_id: format!("history-task-{ordinal}"),
                course_remote_id: None,
                provider_attempt_digest: [ordinal; 32],
                completed_at: Some(self.observed_at),
                metadata_sanitized: json!({"page": ordinal}),
                route_context: ProviderRouteContext::default(),
            }],
            next_cursor,
            complete,
        )
        .map_err(|_| {
            ProviderError::new(ProviderErrorKind::Internal, "fake history page is invalid")
        })
    }

    async fn read_answer_history_task(
        &self,
        _context: &ProviderContext,
        request: &AnswerHistoryTaskRequest,
    ) -> ProviderResult<ProviderAnswerHistoryTaskEvidence> {
        let ordinal = u8::from(request.reference.remote_task_id != "history-task-1") + 1;
        let question_id = QuestionId::new();
        Ok(ProviderAnswerHistoryTaskEvidence {
            task_id: request.task_id,
            provider_attempt_digest: [ordinal; 32],
            result_digest: [ordinal + 10; 32],
            questions: vec![Question {
                id: question_id,
                task_id: request.task_id,
                remote_question_id: Some("question-1".to_owned()),
                kind: QuestionKind::SingleChoice,
                stem: "Choose Alpha".to_owned(),
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
            }],
            question_evidence: vec![AnswerHistoryQuestionEvidence {
                question_id,
                submitted_answer: Some(NormalizedAnswer::Selections(vec!["B".to_owned()])),
                official_answer: Some(NormalizedAnswer::Selections(vec!["A".to_owned()])),
                submitted_answer_correct: (ordinal == 2).then_some(false),
                provenance_sanitized: json!({"ordinal": ordinal}),
            }],
            score: Some(SubmissionScore {
                earned_milli_points: 50_000,
                possible_milli_points: 100_000,
            }),
            retake: Some(AnswerHistoryRetakeFacts {
                allowed: true,
                remaining_attempts: Some(1),
                closes_at: None,
                metadata_sanitized: json!({"action": "redo"}),
            }),
            provenance_sanitized: json!({"surface": "result"}),
            observed_at: self.observed_at,
        })
    }
}

struct Fixture {
    database: Database,
    provider: Arc<FakeHistoryProvider>,
    now: Timestamp,
}

impl Fixture {
    #[allow(clippy::too_many_lines)]
    async fn new() -> Self {
        let database = Database::connect("sqlite::memory:").await.unwrap();
        database.migrate().await.unwrap();
        let created_at = Utc::now();
        let now = created_at + Duration::seconds(1);
        let owner = UserId::new();
        let account = ProviderAccountId::new();
        let provider_id = ProviderId::new("provider-alpha").unwrap();
        let harvest_id = AnswerBootstrapHarvestId::new();
        let schedule_id = ScheduleId::new();
        let timestamp = encode_timestamp(created_at);
        sqlx::query(
            "INSERT INTO users \
             (id, username, password_hash, status, roles_json, permissions_json, created_at, updated_at) \
             VALUES (?, 'history-worker-owner', '$argon2id$test', 'active', '[\"user\"]', '[]', ?, ?)",
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
             VALUES (?, ?, ?, 'history', ?, ?, ?)",
        )
        .bind(account.to_string())
        .bind(owner.to_string())
        .bind(provider_id.as_str())
        .bind(serde_json::to_string(&AuthState::Authenticated).unwrap())
        .bind(&timestamp)
        .bind(&timestamp)
        .execute(database.pool())
        .await
        .unwrap();
        for ordinal in 1..=2 {
            insert_task(&database, account, ordinal, &timestamp).await;
        }
        let kind = ScheduledJobKind::AnswerBootstrapHarvest {
            harvest_id,
            provider_account_id: account,
            generation: 1,
        };
        sqlx::query(
            "INSERT INTO scheduled_jobs \
             (id, job_kind, payload_json, run_at, state, attempts, idempotency_key, created_at, updated_at) \
             VALUES (?, 'answer_bootstrap_harvest', ?, ?, 'pending', 0, ?, ?, ?)",
        )
        .bind(schedule_id.to_string())
        .bind(serde_json::to_string(&kind).unwrap())
        .bind(&timestamp)
        .bind(format!("answer-bootstrap-harvest:{account}:1"))
        .bind(&timestamp)
        .bind(&timestamp)
        .execute(database.pool())
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO answer_bootstrap_harvests \
             (id, owner_user_id, provider_id, provider_account_id, generation, schedule_id, \
              state, scanned_task_count, total_task_count, watermark_sanitized_json, \
              created_at, updated_at) \
             VALUES (?, ?, ?, ?, 1, ?, 'pending', 0, NULL, '{}', ?, ?)",
        )
        .bind(harvest_id.to_string())
        .bind(owner.to_string())
        .bind(provider_id.as_str())
        .bind(account.to_string())
        .bind(schedule_id.to_string())
        .bind(&timestamp)
        .bind(&timestamp)
        .execute(database.pool())
        .await
        .unwrap();
        let metadata = ProviderMetadata {
            id: provider_id,
            display_name: "Provider Alpha".to_owned(),
            implementation_version: "history-v1".to_owned(),
            verification: VerificationLevel::Development,
            scan_min_interval_seconds: None,
            capture_recipe_version: None,
            capabilities: BTreeSet::from([ProviderCapability::AnswerHistoryHarvest]),
            auth_methods: BTreeSet::new(),
            session_kinds: BTreeSet::new(),
        };
        let provider = Arc::new(FakeHistoryProvider {
            metadata,
            observed_at: created_at,
            list_cursors: Mutex::new(Vec::new()),
            fail_list_with_drift: AtomicBool::new(false),
        });
        Self {
            database,
            provider,
            now,
        }
    }

    fn worker(
        &self,
    ) -> AnswerHistoryHarvestWorker<
        SqliteAnswerBootstrapHarvestRepository,
        SqliteProviderAccountRepository,
        SqliteTaskQueryRepository,
        SqliteAnswerHistoryIngestionRepository,
    > {
        let mut registry = ProviderRegistry::default();
        let mut entry = ProviderEntry::metadata_only(self.provider.metadata.clone());
        entry.answer_history_harvest = Some(self.provider.clone());
        registry.register(entry).unwrap();
        AnswerHistoryHarvestWorker::new(
            Arc::new(registry),
            SqliteAnswerBootstrapHarvestRepository::new(self.database.clone()),
            SqliteProviderAccountRepository::new(self.database.clone()),
            SqliteTaskQueryRepository::new(self.database.clone()),
            SqliteAnswerHistoryIngestionRepository::new(self.database.clone()),
            config(),
        )
        .unwrap()
        .with_protocol_observations(Arc::new(SqliteProtocolObservationRepository::new(
            self.database.clone(),
        )))
    }
}

#[tokio::test]
async fn two_pages_yield_then_complete_with_private_and_global_evidence() {
    let fixture = Fixture::new().await;
    let worker = fixture.worker();
    assert_eq!(
        worker.tick_once(fixture.now).await.unwrap(),
        AnswerHistoryHarvestTickReport {
            claimed: 1,
            yielded: 1,
            imported_tasks: 1,
            ..AnswerHistoryHarvestTickReport::default()
        }
    );
    assert_counts(&fixture.database, (1, 2, 1, 1, 1)).await;
    assert_eq!(
        worker
            .tick_once(fixture.now + Duration::seconds(1))
            .await
            .unwrap(),
        AnswerHistoryHarvestTickReport {
            claimed: 1,
            completed: 1,
            imported_tasks: 1,
            ..AnswerHistoryHarvestTickReport::default()
        }
    );
    assert_counts(&fixture.database, (2, 4, 3, 2, 2)).await;
    let progress: (String, i64, Option<i64>) = sqlx::query_as(
        "SELECT state, scanned_task_count, total_task_count FROM answer_bootstrap_harvests",
    )
    .fetch_one(fixture.database.pool())
    .await
    .unwrap();
    assert_eq!(progress, ("completed".to_owned(), 2, Some(2)));
    let cursors = fixture.provider.list_cursors.lock().unwrap();
    assert!(cursors[0].is_none());
    assert_eq!(
        cursors[1].as_ref().unwrap().value_sanitized,
        json!({"page": 2})
    );
}

#[tokio::test]
async fn missing_materialized_task_retries_without_advancing_the_page_cursor() {
    let fixture = Fixture::new().await;
    let worker = fixture.worker();
    worker.tick_once(fixture.now).await.unwrap();
    sqlx::query("DELETE FROM tasks WHERE remote_id = 'history-task-2'")
        .execute(fixture.database.pool())
        .await
        .unwrap();
    assert_eq!(
        worker
            .tick_once(fixture.now + Duration::seconds(1))
            .await
            .unwrap(),
        AnswerHistoryHarvestTickReport {
            claimed: 1,
            retry_scheduled: 1,
            ..AnswerHistoryHarvestTickReport::default()
        }
    );
    let state: (String, i64, String) = sqlx::query_as(
        "SELECT harvest.state, job.attempts, harvest.watermark_sanitized_json \
         FROM answer_bootstrap_harvests AS harvest \
         INNER JOIN scheduled_jobs AS job ON job.id = harvest.schedule_id",
    )
    .fetch_one(fixture.database.pool())
    .await
    .unwrap();
    assert_eq!(state.0, "paused");
    assert_eq!(state.1, 1);
    let watermark: HarvestWatermark = serde_json::from_str(&state.2).unwrap();
    assert_eq!(
        watermark.cursor.unwrap().value_sanitized,
        json!({"page": 2})
    );
}

#[tokio::test]
async fn history_drift_is_observed_without_advancing_or_importing() {
    let fixture = Fixture::new().await;
    fixture
        .provider
        .fail_list_with_drift
        .store(true, Ordering::Relaxed);
    let report = fixture.worker().tick_once(fixture.now).await.unwrap();
    assert_eq!(
        report,
        AnswerHistoryHarvestTickReport {
            claimed: 1,
            dead_lettered: 1,
            ..AnswerHistoryHarvestTickReport::default()
        }
    );
    let state: (String, i64, String) = sqlx::query_as(
        "SELECT state, scanned_task_count, watermark_sanitized_json \
         FROM answer_bootstrap_harvests",
    )
    .fetch_one(fixture.database.pool())
    .await
    .unwrap();
    assert_eq!(state, ("failed".to_owned(), 0, "{}".to_owned()));
    let observation: (String, String, Option<String>) =
        sqlx::query_as("SELECT surface, kind, last_execution_id FROM protocol_observations")
            .fetch_one(fixture.database.pool())
            .await
            .unwrap();
    assert_eq!(
        observation,
        (
            "submission_verify".to_owned(),
            "unknown_result_shape".to_owned(),
            None,
        )
    );
    assert_counts(&fixture.database, (0, 0, 0, 0, 0)).await;
}

#[test]
fn worker_rejects_zero_page_delay() {
    let mut invalid = config();
    invalid.page_yield_delay_seconds = 0;
    assert!(matches!(
        AnswerHistoryHarvestWorker::new(
            Arc::new(ProviderRegistry::default()),
            (),
            (),
            (),
            (),
            invalid,
        ),
        Err(AnswerHistoryHarvestWorkerError::InvalidConfig)
    ));
}

fn config() -> AnswerHistoryHarvestWorkerConfig {
    AnswerHistoryHarvestWorkerConfig {
        worker_id: "answer-history-worker".to_owned(),
        claim_limit: 1,
        claim_ttl_seconds: 60,
        page_yield_delay_seconds: 1,
        retry_delay_seconds: 10,
        max_provider_retry_delay_seconds: 60,
    }
}

async fn insert_task(
    database: &Database,
    account: ProviderAccountId,
    ordinal: u8,
    timestamp: &str,
) {
    sqlx::query(
        "INSERT INTO tasks \
         (id, provider_account_id, remote_id, remote_fingerprint, source_type, \
          assessment_class, title, remote_state, orchestration_state, discovered_at, \
          updated_at, capabilities_json) \
         VALUES (?, ?, ?, ?, 'work', 'routine', ?, 'completed', 'succeeded', ?, ?, '[]')",
    )
    .bind(TaskId::new().to_string())
    .bind(account.to_string())
    .bind(format!("history-task-{ordinal}"))
    .bind(format!("history-fingerprint-{ordinal}"))
    .bind(format!("History Task {ordinal}"))
    .bind(timestamp)
    .bind(timestamp)
    .execute(database.pool())
    .await
    .unwrap();
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

fn encode_timestamp(value: Timestamp) -> String {
    value.to_rfc3339_opts(SecondsFormat::Nanos, true)
}
