use std::{
    collections::{BTreeMap, BTreeSet},
    str::FromStr,
};

use asterism_domain::{
    AssessmentClass, AuditActor, AuditRecordId, CourseId, OrchestrationState, ProviderAccountId,
    ProviderId, RemoteState, SourceType, Task, TaskCapability, TaskDiffId, TaskDiffKind, TaskId,
    TaskSnapshotId, Timestamp, classify_task_changes,
};
use asterism_events::{DomainEvent, EventEnvelope};
use async_trait::async_trait;
use chrono::SecondsFormat;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{Row, Sqlite, Transaction};

use crate::{Database, StorageError, outbox::enqueue_in_transaction, task::decode_task};

const MAX_COURSES_PER_SCAN: usize = 10_000;
const MAX_TASKS_PER_SCAN: usize = 50_000;
const MAX_JSON_BYTES: usize = 1024 * 1024;
const MAX_REMOTE_ID_BYTES: usize = 512;

#[derive(Clone, Debug, PartialEq)]
pub struct ProviderScanBatch {
    pub provider_account_id: ProviderAccountId,
    pub provider_id: ProviderId,
    pub provider_version: String,
    pub observed_at: Timestamp,
    pub correlation_id: String,
    pub initiated_by: Option<AuditActor>,
    pub courses: Vec<ScannedCourse>,
    pub tasks: Vec<ScannedTask>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ScannedCourse {
    pub remote_id: String,
    pub title: String,
    pub term: Option<String>,
    pub teacher: Option<String>,
    pub remote_status: Option<String>,
    pub metadata_sanitized: Value,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ScannedTask {
    pub remote_id: String,
    pub course_remote_id: Option<String>,
    pub fingerprint: String,
    pub source_type: SourceType,
    pub assessment_class: AssessmentClass,
    pub title: String,
    pub remote_state: RemoteState,
    pub opens_at: Option<Timestamp>,
    pub due_at: Option<Timestamp>,
    pub closes_at: Option<Timestamp>,
    pub capabilities: Vec<TaskCapability>,
    pub normalized: Value,
    pub remote_raw_sanitized: Value,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TaskScanChange {
    pub task_id: TaskId,
    pub changes: Vec<TaskDiffKind>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProviderScanReport {
    pub courses_seen: usize,
    pub tasks_created: usize,
    pub tasks_updated: usize,
    pub tasks_unchanged: usize,
    pub task_changes: Vec<TaskScanChange>,
}

#[async_trait]
pub trait ProviderScanRepository: Send + Sync {
    /// Persists a successfully read course inventory without changing any
    /// previously observed tasks. This lets a slow or failed per-course task
    /// scan expose the already verified course list instead of discarding it.
    async fn ingest_course_inventory(
        &self,
        batch: &ProviderScanBatch,
    ) -> Result<ProviderScanReport, StorageError>;

    async fn ingest_scan(
        &self,
        batch: &ProviderScanBatch,
    ) -> Result<ProviderScanReport, StorageError>;
}

#[derive(Clone, Debug)]
pub struct SqliteProviderScanRepository {
    database: Database,
}

impl SqliteProviderScanRepository {
    pub const fn new(database: Database) -> Self {
        Self { database }
    }
}

#[async_trait]
impl ProviderScanRepository for SqliteProviderScanRepository {
    async fn ingest_course_inventory(
        &self,
        batch: &ProviderScanBatch,
    ) -> Result<ProviderScanReport, StorageError> {
        validate_batch(batch)?;
        if !batch.tasks.is_empty() {
            return Err(StorageError::InvalidData(
                "course inventory checkpoint must not contain tasks".to_owned(),
            ));
        }
        let mut transaction = self.database.pool().begin().await?;
        verify_account(&mut transaction, batch).await?;
        let courses = upsert_courses(&mut transaction, batch).await?;
        transaction.commit().await?;
        Ok(ProviderScanReport {
            courses_seen: courses.len(),
            tasks_created: 0,
            tasks_updated: 0,
            tasks_unchanged: 0,
            task_changes: Vec::new(),
        })
    }

    async fn ingest_scan(
        &self,
        batch: &ProviderScanBatch,
    ) -> Result<ProviderScanReport, StorageError> {
        validate_batch(batch)?;
        let mut transaction = self.database.pool().begin().await?;
        verify_account(&mut transaction, batch).await?;
        let courses = upsert_courses(&mut transaction, batch).await?;
        let mut report = ProviderScanReport {
            courses_seen: courses.len(),
            tasks_created: 0,
            tasks_updated: 0,
            tasks_unchanged: 0,
            task_changes: Vec::new(),
        };
        for scanned in &batch.tasks {
            ingest_task(&mut transaction, batch, scanned, &courses, &mut report).await?;
        }
        retire_missing_course_tasks(&mut transaction, batch, &courses, &mut report).await?;
        insert_scan_audit(&mut transaction, batch, &report).await?;
        transaction.commit().await?;
        Ok(report)
    }
}

async fn retire_missing_course_tasks(
    transaction: &mut Transaction<'_, Sqlite>,
    batch: &ProviderScanBatch,
    scanned_courses: &BTreeMap<String, CourseId>,
    report: &mut ProviderScanReport,
) -> Result<(), StorageError> {
    let course_ids = scanned_courses.values().copied().collect::<BTreeSet<_>>();
    if course_ids.is_empty() {
        return Ok(());
    }
    let mut observed = BTreeSet::new();
    for task in &batch.tasks {
        let Some(course_remote_id) = task.course_remote_id.as_ref() else {
            continue;
        };
        let Some(course_id) = scanned_courses.get(course_remote_id) else {
            continue;
        };
        observed.insert((
            *course_id,
            enum_name(task.source_type)?,
            task.remote_id.clone(),
        ));
    }
    let rows = sqlx::query(
        "SELECT id, provider_account_id, course_id, remote_id, remote_fingerprint, source_type, \
                assessment_class, title, remote_state, orchestration_state, opens_at, due_at, \
                closes_at, discovered_at, updated_at, latest_snapshot_id, capabilities_json \
         FROM tasks WHERE provider_account_id = ? AND course_id IS NOT NULL \
           AND remote_state != 'removed'",
    )
    .bind(batch.provider_account_id.to_string())
    .fetch_all(&mut **transaction)
    .await?;
    for row in rows {
        let mut task = decode_task(&row)?;
        let Some(course_id) = task.course_id else {
            continue;
        };
        let source_type = enum_name(task.source_type)?;
        if !course_ids.contains(&course_id)
            || observed.contains(&(course_id, source_type, task.remote_id.clone()))
        {
            continue;
        }
        let fingerprint: String = row.try_get("remote_fingerprint")?;
        let previous_snapshot_id = task.latest_snapshot_id;
        let (normalized, remote_raw_sanitized) =
            load_snapshot_payload(transaction, previous_snapshot_id).await?;
        let snapshot_id = TaskSnapshotId::new();
        task.remote_state = RemoteState::Removed;
        task.updated_at = batch.observed_at;
        task.latest_snapshot_id = Some(snapshot_id);
        update_task(transaction, &task, &fingerprint).await?;
        let scanned = ScannedTask {
            remote_id: task.remote_id.clone(),
            course_remote_id: None,
            fingerprint,
            source_type: task.source_type,
            assessment_class: task.assessment_class,
            title: task.title.clone(),
            remote_state: RemoteState::Removed,
            opens_at: task.opens_at,
            due_at: task.due_at,
            closes_at: task.closes_at,
            capabilities: task.capabilities.clone(),
            normalized,
            remote_raw_sanitized,
        };
        let changes = vec![TaskDiffKind::Removed];
        insert_snapshot_and_diff(
            transaction,
            batch,
            &scanned,
            &task,
            snapshot_id,
            previous_snapshot_id,
            &changes,
        )
        .await?;
        enqueue_in_transaction(
            transaction,
            &EventEnvelope::at(
                &batch.correlation_id,
                DomainEvent::TaskChanged {
                    task_id: task.id,
                    changes: changes.clone(),
                },
                batch.observed_at,
            ),
        )
        .await?;
        report.tasks_updated += 1;
        report.task_changes.push(TaskScanChange {
            task_id: task.id,
            changes,
        });
    }
    Ok(())
}

async fn load_snapshot_payload(
    transaction: &mut Transaction<'_, Sqlite>,
    snapshot_id: Option<TaskSnapshotId>,
) -> Result<(Value, Value), StorageError> {
    let Some(snapshot_id) = snapshot_id else {
        return Ok((serde_json::json!({}), serde_json::json!({})));
    };
    let row = sqlx::query(
        "SELECT normalized_json, remote_raw_sanitized_json \
         FROM task_snapshots WHERE id = ?",
    )
    .bind(snapshot_id.to_string())
    .fetch_optional(&mut **transaction)
    .await?
    .ok_or_else(|| StorageError::InvalidData("task latest snapshot does not exist".to_owned()))?;
    Ok((
        serde_json::from_str(row.try_get("normalized_json")?)?,
        serde_json::from_str(row.try_get("remote_raw_sanitized_json")?)?,
    ))
}

async fn insert_scan_audit(
    transaction: &mut Transaction<'_, Sqlite>,
    batch: &ProviderScanBatch,
    report: &ProviderScanReport,
) -> Result<(), StorageError> {
    let (actor_type, actor_id) = match batch.initiated_by {
        Some(AuditActor::User(id)) => ("user", Some(id.to_string())),
        Some(AuditActor::ServiceToken(id)) => ("service_token", Some(id.to_string())),
        None => ("system", None),
    };
    let metadata = serde_json::json!({
        "provider_id": batch.provider_id,
        "provider_version": batch.provider_version,
        "courses_seen": report.courses_seen,
        "tasks_created": report.tasks_created,
        "tasks_updated": report.tasks_updated,
        "tasks_unchanged": report.tasks_unchanged,
    });
    sqlx::query(
        "INSERT INTO audit_records \
         (id, occurred_at, actor_type, actor_id, action, resource_type, resource_id, \
          correlation_id, outcome, metadata_sanitized_json) \
         VALUES (?, ?, ?, ?, 'provider_account_scanned', 'provider_account', ?, ?, \
                 'succeeded', ?)",
    )
    .bind(AuditRecordId::new().to_string())
    .bind(encode_timestamp(batch.observed_at))
    .bind(actor_type)
    .bind(actor_id)
    .bind(batch.provider_account_id.to_string())
    .bind(&batch.correlation_id)
    .bind(serde_json::to_string(&metadata)?)
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

async fn verify_account(
    transaction: &mut Transaction<'_, Sqlite>,
    batch: &ProviderScanBatch,
) -> Result<(), StorageError> {
    let provider_id: Option<String> =
        sqlx::query_scalar("SELECT provider_id FROM provider_accounts WHERE id = ?")
            .bind(batch.provider_account_id.to_string())
            .fetch_optional(&mut **transaction)
            .await?;
    match provider_id {
        Some(provider_id) if provider_id == batch.provider_id.as_str() => Ok(()),
        Some(_) => Err(StorageError::InvalidData(
            "scan provider does not match the Provider account".to_owned(),
        )),
        None => Err(StorageError::InvalidData(
            "scan Provider account does not exist".to_owned(),
        )),
    }
}

async fn upsert_courses(
    transaction: &mut Transaction<'_, Sqlite>,
    batch: &ProviderScanBatch,
) -> Result<BTreeMap<String, CourseId>, StorageError> {
    let mut courses = BTreeMap::new();
    for course in &batch.courses {
        let existing_id: Option<String> = sqlx::query_scalar(
            "SELECT id FROM courses WHERE provider_account_id = ? AND remote_id = ?",
        )
        .bind(batch.provider_account_id.to_string())
        .bind(&course.remote_id)
        .fetch_optional(&mut **transaction)
        .await?;
        let id = existing_id
            .map(|id| CourseId::from_str(&id))
            .transpose()
            .map_err(|error| StorageError::InvalidData(error.to_string()))?
            .unwrap_or_default();
        sqlx::query(
            "INSERT INTO courses \
             (id, provider_account_id, remote_id, title, term, teacher, remote_status, \
              metadata_json, last_seen_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT(provider_account_id, remote_id) DO UPDATE SET \
              title = excluded.title, term = excluded.term, teacher = excluded.teacher, \
              remote_status = excluded.remote_status, metadata_json = excluded.metadata_json, \
              last_seen_at = excluded.last_seen_at",
        )
        .bind(id.to_string())
        .bind(batch.provider_account_id.to_string())
        .bind(&course.remote_id)
        .bind(&course.title)
        .bind(&course.term)
        .bind(&course.teacher)
        .bind(&course.remote_status)
        .bind(serde_json::to_string(&course.metadata_sanitized)?)
        .bind(encode_timestamp(batch.observed_at))
        .execute(&mut **transaction)
        .await?;
        courses.insert(course.remote_id.clone(), id);
    }
    Ok(courses)
}

async fn ingest_task(
    transaction: &mut Transaction<'_, Sqlite>,
    batch: &ProviderScanBatch,
    scanned: &ScannedTask,
    scanned_courses: &BTreeMap<String, CourseId>,
    report: &mut ProviderScanReport,
) -> Result<(), StorageError> {
    let source_type = enum_name(scanned.source_type)?;
    let existing = find_existing_task(
        transaction,
        batch.provider_account_id,
        &source_type,
        &scanned.remote_id,
        &scanned.fingerprint,
    )
    .await?;
    let course_id = resolve_course(
        transaction,
        batch.provider_account_id,
        scanned.course_remote_id.as_deref(),
        scanned_courses,
    )
    .await?;
    let mut current = build_current_task(batch, scanned, course_id, existing.as_ref());
    let mut changes =
        classify_scan_changes(transaction, existing.as_ref(), &current, scanned).await?;
    changes.sort_unstable();
    changes.dedup();
    if changes.is_empty() {
        report.tasks_unchanged += 1;
        return Ok(());
    }

    let previous_snapshot_id = current.latest_snapshot_id;
    let snapshot_id = TaskSnapshotId::new();
    current.latest_snapshot_id = Some(snapshot_id);
    if existing.is_some() {
        update_task(transaction, &current, &scanned.fingerprint).await?;
        report.tasks_updated += 1;
    } else {
        insert_task(transaction, &current, &scanned.fingerprint).await?;
        report.tasks_created += 1;
    }
    insert_snapshot_and_diff(
        transaction,
        batch,
        scanned,
        &current,
        snapshot_id,
        previous_snapshot_id,
        &changes,
    )
    .await?;
    let event = EventEnvelope::at(
        &batch.correlation_id,
        DomainEvent::TaskChanged {
            task_id: current.id,
            changes: changes.clone(),
        },
        batch.observed_at,
    );
    enqueue_in_transaction(transaction, &event).await?;
    report.task_changes.push(TaskScanChange {
        task_id: current.id,
        changes,
    });
    Ok(())
}

struct ExistingTask {
    task: Task,
    fingerprint: String,
}

fn build_current_task(
    batch: &ProviderScanBatch,
    scanned: &ScannedTask,
    course_id: Option<CourseId>,
    existing: Option<&ExistingTask>,
) -> Task {
    let mut capabilities = scanned.capabilities.clone();
    capabilities.sort_unstable();
    capabilities.dedup();
    Task {
        id: existing.map_or_else(TaskId::new, |existing| existing.task.id),
        provider_account_id: batch.provider_account_id,
        course_id,
        remote_id: scanned.remote_id.clone(),
        source_type: scanned.source_type,
        assessment_class: scanned.assessment_class,
        title: scanned.title.clone(),
        remote_state: scanned.remote_state,
        orchestration_state: existing.map_or(OrchestrationState::Discovered, |existing| {
            existing.task.orchestration_state
        }),
        opens_at: scanned.opens_at,
        due_at: scanned.due_at,
        closes_at: scanned.closes_at,
        discovered_at: existing.map_or(batch.observed_at, |existing| existing.task.discovered_at),
        updated_at: batch.observed_at,
        latest_snapshot_id: existing.and_then(|existing| existing.task.latest_snapshot_id),
        capabilities,
    }
}

async fn classify_scan_changes(
    transaction: &mut Transaction<'_, Sqlite>,
    existing: Option<&ExistingTask>,
    current: &Task,
    scanned: &ScannedTask,
) -> Result<Vec<TaskDiffKind>, StorageError> {
    let Some(existing) = existing else {
        return Ok(vec![TaskDiffKind::Created]);
    };
    let previous_normalized = load_normalized_snapshot(transaction, &existing.task).await?;
    let mut changes = classify_task_changes(
        &existing.task,
        current,
        &previous_normalized,
        &scanned.normalized,
    );
    if existing.fingerprint != scanned.fingerprint
        && !changes.contains(&TaskDiffKind::MetadataChanged)
    {
        changes.insert(0, TaskDiffKind::MetadataChanged);
    }
    Ok(changes)
}

async fn find_existing_task(
    transaction: &mut Transaction<'_, Sqlite>,
    account_id: ProviderAccountId,
    source_type: &str,
    remote_id: &str,
    fingerprint: &str,
) -> Result<Option<ExistingTask>, StorageError> {
    let rows = sqlx::query(
        "SELECT id, provider_account_id, course_id, remote_id, remote_fingerprint, source_type, \
                assessment_class, title, remote_state, orchestration_state, opens_at, due_at, \
                closes_at, discovered_at, updated_at, latest_snapshot_id, capabilities_json \
         FROM tasks WHERE provider_account_id = ? AND source_type = ? \
           AND (remote_id = ? OR remote_fingerprint = ?) \
         ORDER BY CASE WHEN remote_id = ? THEN 0 ELSE 1 END",
    )
    .bind(account_id.to_string())
    .bind(source_type)
    .bind(remote_id)
    .bind(fingerprint)
    .bind(remote_id)
    .fetch_all(&mut **transaction)
    .await?;
    if rows.len() > 1 {
        return Err(StorageError::InvalidData(
            "scan remote key and fingerprint refer to different tasks".to_owned(),
        ));
    }
    rows.first()
        .map(|row| {
            Ok(ExistingTask {
                task: decode_task(row)?,
                fingerprint: row.try_get("remote_fingerprint")?,
            })
        })
        .transpose()
}

async fn resolve_course(
    transaction: &mut Transaction<'_, Sqlite>,
    account_id: ProviderAccountId,
    remote_id: Option<&str>,
    scanned_courses: &BTreeMap<String, CourseId>,
) -> Result<Option<CourseId>, StorageError> {
    let Some(remote_id) = remote_id else {
        return Ok(None);
    };
    if let Some(course_id) = scanned_courses.get(remote_id) {
        return Ok(Some(*course_id));
    }
    let id: Option<String> = sqlx::query_scalar(
        "SELECT id FROM courses WHERE provider_account_id = ? AND remote_id = ?",
    )
    .bind(account_id.to_string())
    .bind(remote_id)
    .fetch_optional(&mut **transaction)
    .await?;
    id.map(|id| CourseId::from_str(&id))
        .transpose()
        .map_err(|error| StorageError::InvalidData(error.to_string()))?
        .map_or_else(
            || {
                Err(StorageError::InvalidData(format!(
                    "scan task references unknown course remote ID {remote_id}"
                )))
            },
            |id| Ok(Some(id)),
        )
}

async fn load_normalized_snapshot(
    transaction: &mut Transaction<'_, Sqlite>,
    task: &Task,
) -> Result<Value, StorageError> {
    let Some(snapshot_id) = task.latest_snapshot_id else {
        return Ok(Value::Null);
    };
    let normalized: Option<String> =
        sqlx::query_scalar("SELECT normalized_json FROM task_snapshots WHERE id = ?")
            .bind(snapshot_id.to_string())
            .fetch_optional(&mut **transaction)
            .await?;
    normalized
        .map(|normalized| serde_json::from_str(&normalized))
        .transpose()?
        .ok_or_else(|| StorageError::InvalidData("task latest snapshot does not exist".to_owned()))
}

async fn insert_task(
    transaction: &mut Transaction<'_, Sqlite>,
    task: &Task,
    fingerprint: &str,
) -> Result<(), StorageError> {
    sqlx::query(
        "INSERT INTO tasks \
         (id, provider_account_id, course_id, remote_id, remote_fingerprint, source_type, \
          assessment_class, title, remote_state, orchestration_state, opens_at, due_at, \
          closes_at, discovered_at, updated_at, latest_snapshot_id, capabilities_json) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(task.id.to_string())
    .bind(task.provider_account_id.to_string())
    .bind(task.course_id.map(|id| id.to_string()))
    .bind(&task.remote_id)
    .bind(fingerprint)
    .bind(enum_name(task.source_type)?)
    .bind(enum_name(task.assessment_class)?)
    .bind(&task.title)
    .bind(enum_name(task.remote_state)?)
    .bind(enum_name(task.orchestration_state)?)
    .bind(task.opens_at.map(encode_timestamp))
    .bind(task.due_at.map(encode_timestamp))
    .bind(task.closes_at.map(encode_timestamp))
    .bind(encode_timestamp(task.discovered_at))
    .bind(encode_timestamp(task.updated_at))
    .bind(task.latest_snapshot_id.map(|id| id.to_string()))
    .bind(serde_json::to_string(&task.capabilities)?)
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

async fn update_task(
    transaction: &mut Transaction<'_, Sqlite>,
    task: &Task,
    fingerprint: &str,
) -> Result<(), StorageError> {
    let result = sqlx::query(
        "UPDATE tasks SET course_id = ?, remote_id = ?, remote_fingerprint = ?, \
         assessment_class = ?, title = ?, remote_state = ?, opens_at = ?, due_at = ?, \
         closes_at = ?, updated_at = ?, latest_snapshot_id = ?, capabilities_json = ? \
         WHERE id = ? AND provider_account_id = ? AND source_type = ?",
    )
    .bind(task.course_id.map(|id| id.to_string()))
    .bind(&task.remote_id)
    .bind(fingerprint)
    .bind(enum_name(task.assessment_class)?)
    .bind(&task.title)
    .bind(enum_name(task.remote_state)?)
    .bind(task.opens_at.map(encode_timestamp))
    .bind(task.due_at.map(encode_timestamp))
    .bind(task.closes_at.map(encode_timestamp))
    .bind(encode_timestamp(task.updated_at))
    .bind(task.latest_snapshot_id.map(|id| id.to_string()))
    .bind(serde_json::to_string(&task.capabilities)?)
    .bind(task.id.to_string())
    .bind(task.provider_account_id.to_string())
    .bind(enum_name(task.source_type)?)
    .execute(&mut **transaction)
    .await?;
    if result.rows_affected() == 1 {
        Ok(())
    } else {
        Err(StorageError::InvalidData(
            "scan task identity changed during update".to_owned(),
        ))
    }
}

async fn insert_snapshot_and_diff(
    transaction: &mut Transaction<'_, Sqlite>,
    batch: &ProviderScanBatch,
    scanned: &ScannedTask,
    task: &Task,
    snapshot_id: TaskSnapshotId,
    previous_snapshot_id: Option<TaskSnapshotId>,
    changes: &[TaskDiffKind],
) -> Result<(), StorageError> {
    sqlx::query(
        "INSERT INTO task_snapshots \
         (id, task_id, captured_at, provider_version, normalized_json, \
          remote_raw_sanitized_json) VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(snapshot_id.to_string())
    .bind(task.id.to_string())
    .bind(encode_timestamp(batch.observed_at))
    .bind(&batch.provider_version)
    .bind(serde_json::to_string(&scanned.normalized)?)
    .bind(serde_json::to_string(&scanned.remote_raw_sanitized)?)
    .execute(&mut **transaction)
    .await?;
    sqlx::query(
        "INSERT INTO task_diffs \
         (id, task_id, from_snapshot_id, to_snapshot_id, changes_json, created_at) \
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(TaskDiffId::new().to_string())
    .bind(task.id.to_string())
    .bind(previous_snapshot_id.map(|id| id.to_string()))
    .bind(snapshot_id.to_string())
    .bind(serde_json::to_string(changes)?)
    .bind(encode_timestamp(batch.observed_at))
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

fn validate_batch(batch: &ProviderScanBatch) -> Result<(), StorageError> {
    if batch.courses.len() > MAX_COURSES_PER_SCAN
        || batch.tasks.len() > MAX_TASKS_PER_SCAN
        || !valid_text(&batch.provider_version, 128)
        || !valid_text(&batch.correlation_id, 128)
    {
        return invalid_scan();
    }
    let mut course_ids = BTreeSet::new();
    for course in &batch.courses {
        if !valid_remote_id(&course.remote_id)
            || !valid_text(&course.title, 512)
            || !valid_optional_text(course.term.as_deref(), 256)
            || !valid_optional_text(course.teacher.as_deref(), 256)
            || !valid_optional_text(course.remote_status.as_deref(), 256)
            || !valid_sanitized_json(&course.metadata_sanitized)
            || !course_ids.insert(course.remote_id.as_str())
        {
            return invalid_scan();
        }
    }
    let mut remote_keys = BTreeSet::new();
    let mut fingerprints = BTreeSet::new();
    for task in &batch.tasks {
        if !valid_remote_id(&task.remote_id)
            || !valid_optional_text(task.course_remote_id.as_deref(), MAX_REMOTE_ID_BYTES)
            || !valid_fingerprint(&task.fingerprint)
            || !valid_text(&task.title, 512)
            || !valid_sanitized_json(&task.normalized)
            || !valid_sanitized_json(&task.remote_raw_sanitized)
            || !remote_keys.insert((task.source_type, task.remote_id.as_str()))
            || !fingerprints.insert((task.source_type, task.fingerprint.as_str()))
        {
            return invalid_scan();
        }
    }
    Ok(())
}

fn valid_remote_id(value: &str) -> bool {
    valid_text(value, MAX_REMOTE_ID_BYTES)
}

fn valid_fingerprint(value: &str) -> bool {
    let Some((version, fingerprint)) = value.split_once(':') else {
        return false;
    };
    version.strip_prefix('v').is_some_and(|version| {
        !version.is_empty() && version.bytes().all(|byte| byte.is_ascii_digit())
    }) && valid_text(fingerprint, 256)
}

fn valid_text(value: &str, maximum: usize) -> bool {
    !value.is_empty()
        && value.len() <= maximum
        && value.trim() == value
        && !value.chars().any(char::is_control)
}

fn valid_optional_text(value: Option<&str>, maximum: usize) -> bool {
    value.is_none_or(|value| valid_text(value, maximum))
}

fn valid_sanitized_json(value: &Value) -> bool {
    serde_json::to_vec(value).is_ok_and(|encoded| encoded.len() <= MAX_JSON_BYTES)
        && !contains_secret_key(value)
}

fn contains_secret_key(value: &Value) -> bool {
    match value {
        Value::Object(object) => object.iter().any(|(key, value)| {
            let normalized: String = key
                .chars()
                .filter(char::is_ascii_alphanumeric)
                .flat_map(char::to_lowercase)
                .collect();
            matches!(
                normalized.as_str(),
                "cookie"
                    | "authorization"
                    | "password"
                    | "accesstoken"
                    | "refreshtoken"
                    | "sessionsecret"
                    | "clientsecret"
            ) || contains_secret_key(value)
        }),
        Value::Array(items) => items.iter().any(contains_secret_key),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => false,
    }
}

fn invalid_scan<T>() -> Result<T, StorageError> {
    Err(StorageError::InvalidData(
        "Provider scan contains invalid, duplicate, oversized, or unsanitized data".to_owned(),
    ))
}

fn enum_name(value: impl Serialize) -> Result<String, StorageError> {
    serde_json::to_value(value)?
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| StorageError::InvalidData("enum did not serialize as a string".to_owned()))
}

fn encode_timestamp(value: Timestamp) -> String {
    value.to_rfc3339_opts(SecondsFormat::Nanos, true)
}

#[cfg(test)]
mod tests {
    use asterism_domain::{AuthState, Role};
    use chrono::Utc;

    use super::*;

    #[tokio::test]
    async fn scan_ingestion_is_transactional_diffed_and_idempotent() {
        let (database, account_id) = setup().await;
        let repository = SqliteProviderScanRepository::new(database.clone());
        let now = Utc::now();
        let first = scan(account_id, now);
        let created = repository.ingest_scan(&first).await.unwrap();
        assert_eq!(created.tasks_created, 1);
        assert_eq!(created.task_changes[0].changes, [TaskDiffKind::Created]);
        let task_id = created.task_changes[0].task_id;

        let unchanged = repository.ingest_scan(&first).await.unwrap();
        assert_eq!(unchanged.tasks_unchanged, 1);
        assert!(unchanged.task_changes.is_empty());

        let mut changed = scan(account_id, now + chrono::Duration::minutes(1));
        changed.tasks[0].remote_id = "remote-b".to_owned();
        changed.tasks[0].title = "renamed".to_owned();
        changed.tasks[0].remote_state = RemoteState::Completed;
        changed.tasks[0].due_at = Some(now + chrono::Duration::hours(1));
        changed.tasks[0].normalized = serde_json::json!({"revision": 2});
        let updated = repository.ingest_scan(&changed).await.unwrap();
        assert_eq!(updated.tasks_updated, 1);
        assert_eq!(updated.task_changes[0].task_id, task_id);
        assert_eq!(
            updated.task_changes[0].changes,
            [
                TaskDiffKind::MetadataChanged,
                TaskDiffKind::ContentChanged,
                TaskDiffKind::DeadlineChanged,
                TaskDiffKind::CompletedExternally,
            ]
        );

        for table in ["task_snapshots", "task_diffs", "event_outbox"] {
            let query = format!("SELECT COUNT(*) FROM {table}");
            let count: i64 = sqlx::query_scalar(&query)
                .fetch_one(database.pool())
                .await
                .unwrap();
            assert_eq!(count, 2, "unexpected row count for {table}");
        }
    }

    #[tokio::test]
    async fn scan_rejects_unsanitized_payload_without_partial_writes() {
        let (database, account_id) = setup().await;
        let repository = SqliteProviderScanRepository::new(database.clone());
        let mut batch = scan(account_id, Utc::now());
        batch.tasks[0].remote_raw_sanitized =
            serde_json::json!({"nested": {"Authorization": "secret"}});
        assert!(matches!(
            repository.ingest_scan(&batch).await,
            Err(StorageError::InvalidData(_))
        ));
        let tasks: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tasks")
            .fetch_one(database.pool())
            .await
            .unwrap();
        let courses: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM courses")
            .fetch_one(database.pool())
            .await
            .unwrap();
        assert_eq!((tasks, courses), (0, 0));
    }

    #[tokio::test]
    async fn full_course_scan_retires_tasks_that_disappeared_from_inventory() {
        let (database, account_id) = setup().await;
        let repository = SqliteProviderScanRepository::new(database.clone());
        let now = Utc::now();
        repository
            .ingest_scan(&scan(account_id, now))
            .await
            .unwrap();
        let mut empty = scan(account_id, now + chrono::Duration::minutes(1));
        empty.tasks.clear();

        let retired = repository.ingest_scan(&empty).await.unwrap();
        assert_eq!(retired.tasks_updated, 1);
        assert_eq!(retired.task_changes.len(), 1);
        assert_eq!(retired.task_changes[0].changes, [TaskDiffKind::Removed]);
        let state: String = sqlx::query_scalar("SELECT remote_state FROM tasks")
            .fetch_one(database.pool())
            .await
            .unwrap();
        assert_eq!(state, "removed");

        let unchanged = repository.ingest_scan(&empty).await.unwrap();
        assert_eq!(unchanged.tasks_updated, 0);
        assert!(unchanged.task_changes.is_empty());
    }

    #[tokio::test]
    async fn course_inventory_checkpoint_never_retires_existing_tasks() {
        let (database, account_id) = setup().await;
        let repository = SqliteProviderScanRepository::new(database.clone());
        let now = Utc::now();
        repository
            .ingest_scan(&scan(account_id, now))
            .await
            .unwrap();
        let mut checkpoint = scan(account_id, now + chrono::Duration::minutes(1));
        checkpoint.tasks.clear();

        let report = repository
            .ingest_course_inventory(&checkpoint)
            .await
            .unwrap();
        assert_eq!(report.courses_seen, 1);
        assert_eq!(report.tasks_updated, 0);
        let state: String = sqlx::query_scalar("SELECT remote_state FROM tasks")
            .fetch_one(database.pool())
            .await
            .unwrap();
        assert_eq!(state, "pending");
    }

    #[tokio::test]
    async fn scan_rejects_provider_account_mismatch() {
        let (database, account_id) = setup().await;
        let repository = SqliteProviderScanRepository::new(database.clone());
        let mut batch = scan(account_id, Utc::now());
        batch.provider_id = ProviderId::new("provider-beta").unwrap();

        assert!(matches!(
            repository.ingest_scan(&batch).await,
            Err(StorageError::InvalidData(message))
                if message.contains("does not match")
        ));
        let tasks: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tasks")
            .fetch_one(database.pool())
            .await
            .unwrap();
        assert_eq!(tasks, 0);
    }

    #[tokio::test]
    async fn scan_rejects_remote_key_and_fingerprint_collision() {
        let (database, account_id) = setup().await;
        let repository = SqliteProviderScanRepository::new(database.clone());
        let mut first = scan(account_id, Utc::now());
        let mut second_task = first.tasks[0].clone();
        second_task.remote_id = "remote-b".to_owned();
        second_task.fingerprint = "v1:fingerprint-b".to_owned();
        first.tasks.push(second_task);
        repository.ingest_scan(&first).await.unwrap();

        let mut conflicting = scan(account_id, Utc::now() + chrono::Duration::minutes(1));
        conflicting.tasks[0].fingerprint = "v1:fingerprint-b".to_owned();
        assert!(matches!(
            repository.ingest_scan(&conflicting).await,
            Err(StorageError::InvalidData(message))
                if message.contains("different tasks")
        ));
        let tasks: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tasks")
            .fetch_one(database.pool())
            .await
            .unwrap();
        assert_eq!(tasks, 2);
    }

    async fn setup() -> (Database, ProviderAccountId) {
        let database = Database::connect("sqlite::memory:").await.unwrap();
        database.migrate().await.unwrap();
        let user_id = asterism_domain::UserId::new();
        let account_id = ProviderAccountId::new();
        let now = encode_timestamp(Utc::now());
        sqlx::query(
            "INSERT INTO users \
             (id, username, password_hash, status, roles_json, permissions_json, created_at, updated_at) \
             VALUES (?, 'owner', '$argon2id$test', 'active', ?, '[]', ?, ?)",
        )
        .bind(user_id.to_string())
        .bind(serde_json::to_string(&[Role::User]).unwrap())
        .bind(&now)
        .bind(&now)
        .execute(database.pool())
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO provider_accounts \
             (id, owner_user_id, provider_id, display_name, auth_state_json, created_at, updated_at) \
             VALUES (?, ?, 'provider-alpha', 'primary', ?, ?, ?)",
        )
        .bind(account_id.to_string())
        .bind(user_id.to_string())
        .bind(serde_json::to_string(&AuthState::Idle).unwrap())
        .bind(&now)
        .bind(&now)
        .execute(database.pool())
        .await
        .unwrap();
        (database, account_id)
    }

    fn scan(account_id: ProviderAccountId, observed_at: Timestamp) -> ProviderScanBatch {
        ProviderScanBatch {
            provider_account_id: account_id,
            provider_id: ProviderId::new("provider-alpha").unwrap(),
            provider_version: "0.1.0".to_owned(),
            observed_at,
            correlation_id: "scan-test".to_owned(),
            initiated_by: None,
            courses: vec![ScannedCourse {
                remote_id: "course-a".to_owned(),
                title: "course".to_owned(),
                term: None,
                teacher: None,
                remote_status: None,
                metadata_sanitized: serde_json::json!({"revision": 1}),
            }],
            tasks: vec![ScannedTask {
                remote_id: "remote-a".to_owned(),
                course_remote_id: Some("course-a".to_owned()),
                fingerprint: "v1:fingerprint-a".to_owned(),
                source_type: SourceType::Exam,
                assessment_class: AssessmentClass::Routine,
                title: "weekly".to_owned(),
                remote_state: RemoteState::Pending,
                opens_at: None,
                due_at: None,
                closes_at: None,
                capabilities: vec![TaskCapability::ProgressRead],
                normalized: serde_json::json!({"revision": 1}),
                remote_raw_sanitized: serde_json::json!({"task": "safe"}),
            }],
        }
    }
}
