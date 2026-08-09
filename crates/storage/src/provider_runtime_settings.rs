use std::str::FromStr;

use asterism_domain::{
    AuditActor, AuditRecordId, ProviderAccountId, ProviderId, ProviderRuntimeSettingsId, TaskId,
    Timestamp,
};
use asterism_provider_api::ProviderRuntimeSettingsPatch;
use async_trait::async_trait;
use chrono::{DateTime, SecondsFormat, Utc};
use sqlx::{Row, Sqlite, Transaction, sqlite::SqliteRow};

use crate::{
    Database, ProviderRuntimeSettingsRecord, ProviderRuntimeSettingsRepository,
    ProviderRuntimeSettingsTarget, ProviderRuntimeSettingsWriteOutcome,
    ProviderRuntimeSettingsWriteRequest, StorageError,
};

const MAX_SETTINGS_JSON_BYTES: usize = 64 * 1024;
const MAX_CORRELATION_ID_BYTES: usize = 128;

#[derive(Clone, Debug)]
pub struct SqliteProviderRuntimeSettingsRepository {
    database: Database,
}

impl SqliteProviderRuntimeSettingsRepository {
    pub const fn new(database: Database) -> Self {
        Self { database }
    }
}

#[async_trait]
impl ProviderRuntimeSettingsRepository for SqliteProviderRuntimeSettingsRepository {
    async fn find_provider_runtime_settings(
        &self,
        target: &ProviderRuntimeSettingsTarget,
    ) -> Result<Option<ProviderRuntimeSettingsRecord>, StorageError> {
        let row =
            match target {
                ProviderRuntimeSettingsTarget::Provider { provider_id } => sqlx::query(
                    "SELECT id, scope, provider_id, provider_account_id, task_id, schema_version, \
                        revision, settings_json, created_at, updated_at \
                 FROM provider_runtime_settings \
                 WHERE scope = 'provider' AND provider_id = ?",
                )
                .bind(provider_id.as_str())
                .fetch_optional(self.database.pool())
                .await?,
                ProviderRuntimeSettingsTarget::ProviderAccount {
                    provider_id,
                    provider_account_id,
                } => sqlx::query(
                    "SELECT id, scope, provider_id, provider_account_id, task_id, schema_version, \
                        revision, settings_json, created_at, updated_at \
                 FROM provider_runtime_settings \
                 WHERE scope = 'provider_account' AND provider_account_id = ? AND provider_id = ?",
                )
                .bind(provider_account_id.to_string())
                .bind(provider_id.as_str())
                .fetch_optional(self.database.pool())
                .await?,
                ProviderRuntimeSettingsTarget::Task {
                    provider_id,
                    provider_account_id,
                    task_id,
                } => sqlx::query(
                    "SELECT id, scope, provider_id, provider_account_id, task_id, schema_version, \
                        revision, settings_json, created_at, updated_at \
                 FROM provider_runtime_settings WHERE scope = 'task' AND task_id = ? \
                   AND provider_account_id = ? AND provider_id = ?",
                )
                .bind(task_id.to_string())
                .bind(provider_account_id.to_string())
                .bind(provider_id.as_str())
                .fetch_optional(self.database.pool())
                .await?,
            };
        let record = row.as_ref().map(decode_record).transpose()?;
        if record
            .as_ref()
            .is_some_and(|record| &record.target != target)
        {
            return Err(StorageError::InvalidData(
                "provider runtime settings target binding is invalid".to_owned(),
            ));
        }
        Ok(record)
    }

    async fn write_provider_runtime_settings(
        &self,
        request: ProviderRuntimeSettingsWriteRequest<'_>,
    ) -> Result<ProviderRuntimeSettingsWriteOutcome, StorageError> {
        validate_write_request(&request)?;
        let settings_json = serde_json::to_string(request.patch)?;
        if settings_json.len() > MAX_SETTINGS_JSON_BYTES {
            return Err(StorageError::InvalidData(
                "provider runtime settings patch is too large".to_owned(),
            ));
        }

        let mut transaction = self.database.pool().begin_with("BEGIN IMMEDIATE").await?;
        if !target_exists(&mut transaction, &request.target).await? {
            transaction.commit().await?;
            return Ok(ProviderRuntimeSettingsWriteOutcome::TargetNotFound);
        }
        let current = find_record_in_transaction(&mut transaction, &request.target).await?;
        let record = if let Some(current) = current {
            if current.revision != request.expected_revision {
                transaction.commit().await?;
                return Ok(ProviderRuntimeSettingsWriteOutcome::RevisionConflict);
            }
            if request.updated_at < current.updated_at {
                return Err(StorageError::InvalidData(
                    "provider runtime settings timestamp regresses".to_owned(),
                ));
            }
            let revision = current.revision.checked_add(1).ok_or_else(|| {
                StorageError::InvalidData(
                    "provider runtime settings revision is exhausted".to_owned(),
                )
            })?;
            let result = sqlx::query(
                "UPDATE provider_runtime_settings SET schema_version = ?, revision = ?, \
                         settings_json = ?, updated_at = ? WHERE id = ? AND revision = ?",
            )
            .bind(i64::from(request.patch.schema_version))
            .bind(i64::from(revision))
            .bind(&settings_json)
            .bind(encode_timestamp(request.updated_at))
            .bind(current.id.to_string())
            .bind(i64::from(current.revision))
            .execute(&mut *transaction)
            .await?;
            if result.rows_affected() != 1 {
                transaction.commit().await?;
                return Ok(ProviderRuntimeSettingsWriteOutcome::RevisionConflict);
            }
            ProviderRuntimeSettingsRecord {
                id: current.id,
                target: request.target.clone(),
                patch: request.patch.clone(),
                revision,
                created_at: current.created_at,
                updated_at: request.updated_at,
            }
        } else {
            if request.expected_revision != 0 {
                transaction.commit().await?;
                return Ok(ProviderRuntimeSettingsWriteOutcome::RevisionConflict);
            }
            let record = ProviderRuntimeSettingsRecord {
                id: ProviderRuntimeSettingsId::new(),
                target: request.target.clone(),
                patch: request.patch.clone(),
                revision: 1,
                created_at: request.updated_at,
                updated_at: request.updated_at,
            };
            insert_record(&mut transaction, &record, &settings_json).await?;
            record
        };
        insert_settings_audit(
            &mut transaction,
            request.actor,
            request.correlation_id,
            &record,
        )
        .await?;
        transaction.commit().await?;
        Ok(ProviderRuntimeSettingsWriteOutcome::Stored(record))
    }
}

fn validate_write_request(
    request: &ProviderRuntimeSettingsWriteRequest<'_>,
) -> Result<(), StorageError> {
    if request.correlation_id.is_empty()
        || request.correlation_id.len() > MAX_CORRELATION_ID_BYTES
        || request.correlation_id.chars().any(char::is_control)
    {
        return Err(StorageError::InvalidData(
            "provider runtime settings correlation ID is invalid".to_owned(),
        ));
    }
    request
        .schema
        .validate_patch(request.target.scope(), request.patch)
        .map_err(|error| StorageError::InvalidData(error.to_string()))
}

async fn target_exists(
    transaction: &mut Transaction<'_, Sqlite>,
    target: &ProviderRuntimeSettingsTarget,
) -> Result<bool, StorageError> {
    let exists =
        match target {
            ProviderRuntimeSettingsTarget::Provider { .. } => true,
            ProviderRuntimeSettingsTarget::ProviderAccount {
                provider_id,
                provider_account_id,
            } => sqlx::query_scalar::<_, i64>(
                "SELECT EXISTS(SELECT 1 FROM provider_accounts WHERE id = ? AND provider_id = ?)",
            )
            .bind(provider_account_id.to_string())
            .bind(provider_id.as_str())
            .fetch_one(&mut **transaction)
            .await?
                == 1,
            ProviderRuntimeSettingsTarget::Task {
                provider_id,
                provider_account_id,
                task_id,
            } => {
                sqlx::query_scalar::<_, i64>(
                    "SELECT EXISTS(\
                     SELECT 1 FROM tasks t \
                     INNER JOIN provider_accounts pa ON pa.id = t.provider_account_id \
                     WHERE t.id = ? AND t.provider_account_id = ? AND pa.provider_id = ?\
                 )",
                )
                .bind(task_id.to_string())
                .bind(provider_account_id.to_string())
                .bind(provider_id.as_str())
                .fetch_one(&mut **transaction)
                .await?
                    == 1
            }
        };
    Ok(exists)
}

async fn find_record_in_transaction(
    transaction: &mut Transaction<'_, Sqlite>,
    target: &ProviderRuntimeSettingsTarget,
) -> Result<Option<ProviderRuntimeSettingsRecord>, StorageError> {
    let row = match target {
        ProviderRuntimeSettingsTarget::Provider { provider_id } => {
            sqlx::query(
                "SELECT id, scope, provider_id, provider_account_id, task_id, schema_version, \
                    revision, settings_json, created_at, updated_at \
             FROM provider_runtime_settings WHERE scope = 'provider' AND provider_id = ?",
            )
            .bind(provider_id.as_str())
            .fetch_optional(&mut **transaction)
            .await?
        }
        ProviderRuntimeSettingsTarget::ProviderAccount {
            provider_id,
            provider_account_id,
        } => {
            sqlx::query(
                "SELECT id, scope, provider_id, provider_account_id, task_id, schema_version, \
                    revision, settings_json, created_at, updated_at \
             FROM provider_runtime_settings \
             WHERE scope = 'provider_account' AND provider_account_id = ? AND provider_id = ?",
            )
            .bind(provider_account_id.to_string())
            .bind(provider_id.as_str())
            .fetch_optional(&mut **transaction)
            .await?
        }
        ProviderRuntimeSettingsTarget::Task {
            provider_id,
            provider_account_id,
            task_id,
        } => {
            sqlx::query(
                "SELECT id, scope, provider_id, provider_account_id, task_id, schema_version, \
                    revision, settings_json, created_at, updated_at \
             FROM provider_runtime_settings WHERE scope = 'task' AND task_id = ? \
               AND provider_account_id = ? AND provider_id = ?",
            )
            .bind(task_id.to_string())
            .bind(provider_account_id.to_string())
            .bind(provider_id.as_str())
            .fetch_optional(&mut **transaction)
            .await?
        }
    };
    let record = row.as_ref().map(decode_record).transpose()?;
    if record
        .as_ref()
        .is_some_and(|record| &record.target != target)
    {
        return Err(StorageError::InvalidData(
            "provider runtime settings target binding is invalid".to_owned(),
        ));
    }
    Ok(record)
}

async fn insert_record(
    transaction: &mut Transaction<'_, Sqlite>,
    record: &ProviderRuntimeSettingsRecord,
    settings_json: &str,
) -> Result<(), StorageError> {
    let (scope, provider_account_id, task_id) = target_columns(&record.target);
    sqlx::query(
        "INSERT INTO provider_runtime_settings \
         (id, scope, provider_id, provider_account_id, task_id, schema_version, revision, \
          settings_json, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(record.id.to_string())
    .bind(scope)
    .bind(record.target.provider_id().as_str())
    .bind(provider_account_id)
    .bind(task_id)
    .bind(i64::from(record.patch.schema_version))
    .bind(i64::from(record.revision))
    .bind(settings_json)
    .bind(encode_timestamp(record.created_at))
    .bind(encode_timestamp(record.updated_at))
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

async fn insert_settings_audit(
    transaction: &mut Transaction<'_, Sqlite>,
    actor: AuditActor,
    correlation_id: &str,
    record: &ProviderRuntimeSettingsRecord,
) -> Result<(), StorageError> {
    let (actor_type, actor_id) = match actor {
        AuditActor::User(id) => ("user", id.to_string()),
        AuditActor::ServiceToken(id) => ("service_token", id.to_string()),
    };
    let (scope, provider_account_id, task_id) = target_columns(&record.target);
    let keys = record.patch.values.keys().collect::<Vec<_>>();
    let metadata = serde_json::json!({
        "scope": scope,
        "provider_id": record.target.provider_id(),
        "provider_account_id": provider_account_id,
        "task_id": task_id,
        "schema_version": record.patch.schema_version,
        "revision": record.revision,
        "keys": keys,
    });
    sqlx::query(
        "INSERT INTO audit_records \
         (id, occurred_at, actor_type, actor_id, action, resource_type, resource_id, \
          correlation_id, outcome, metadata_sanitized_json) \
         VALUES (?, ?, ?, ?, 'provider_runtime_settings_configured', \
                 'provider_runtime_settings', ?, ?, 'succeeded', ?)",
    )
    .bind(AuditRecordId::new().to_string())
    .bind(encode_timestamp(record.updated_at))
    .bind(actor_type)
    .bind(actor_id)
    .bind(record.id.to_string())
    .bind(correlation_id)
    .bind(serde_json::to_string(&metadata)?)
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

fn target_columns(
    target: &ProviderRuntimeSettingsTarget,
) -> (&'static str, Option<String>, Option<String>) {
    match target {
        ProviderRuntimeSettingsTarget::Provider { .. } => ("provider", None, None),
        ProviderRuntimeSettingsTarget::ProviderAccount {
            provider_account_id,
            ..
        } => (
            "provider_account",
            Some(provider_account_id.to_string()),
            None,
        ),
        ProviderRuntimeSettingsTarget::Task {
            provider_account_id,
            task_id,
            ..
        } => (
            "task",
            Some(provider_account_id.to_string()),
            Some(task_id.to_string()),
        ),
    }
}

fn decode_record(row: &SqliteRow) -> Result<ProviderRuntimeSettingsRecord, StorageError> {
    let provider_id = ProviderId::new(row.try_get::<String, _>("provider_id")?)
        .map_err(|error| StorageError::InvalidData(error.to_string()))?;
    let provider_account_id = row
        .try_get::<Option<String>, _>("provider_account_id")?
        .map(|value| ProviderAccountId::from_str(&value))
        .transpose()
        .map_err(|error| StorageError::InvalidData(error.to_string()))?;
    let task_id = row
        .try_get::<Option<String>, _>("task_id")?
        .map(|value| TaskId::from_str(&value))
        .transpose()
        .map_err(|error| StorageError::InvalidData(error.to_string()))?;
    let target = match (
        row.try_get::<String, _>("scope")?.as_str(),
        provider_account_id,
        task_id,
    ) {
        ("provider", None, None) => ProviderRuntimeSettingsTarget::Provider { provider_id },
        ("provider_account", Some(provider_account_id), None) => {
            ProviderRuntimeSettingsTarget::ProviderAccount {
                provider_id,
                provider_account_id,
            }
        }
        ("task", Some(provider_account_id), Some(task_id)) => ProviderRuntimeSettingsTarget::Task {
            provider_id,
            provider_account_id,
            task_id,
        },
        _ => {
            return Err(StorageError::InvalidData(
                "provider runtime settings scope is invalid".to_owned(),
            ));
        }
    };
    let schema_version = u32::try_from(row.try_get::<i64, _>("schema_version")?)
        .map_err(|_| StorageError::InvalidData("invalid settings schema version".to_owned()))?;
    let settings_json = row.try_get::<String, _>("settings_json")?;
    if settings_json.len() > MAX_SETTINGS_JSON_BYTES {
        return Err(StorageError::InvalidData(
            "persisted provider runtime settings are too large".to_owned(),
        ));
    }
    let patch: ProviderRuntimeSettingsPatch = serde_json::from_str(&settings_json)?;
    if patch.schema_version != schema_version {
        return Err(StorageError::InvalidData(
            "persisted provider runtime settings schema binding is invalid".to_owned(),
        ));
    }
    let created_at = decode_timestamp(row.try_get("created_at")?)?;
    let updated_at = decode_timestamp(row.try_get("updated_at")?)?;
    if updated_at < created_at {
        return Err(StorageError::InvalidData(
            "provider runtime settings timestamps are invalid".to_owned(),
        ));
    }
    Ok(ProviderRuntimeSettingsRecord {
        id: ProviderRuntimeSettingsId::from_str(row.try_get("id")?)
            .map_err(|error| StorageError::InvalidData(error.to_string()))?,
        target,
        patch,
        revision: u32::try_from(row.try_get::<i64, _>("revision")?)
            .map_err(|_| StorageError::InvalidData("invalid settings revision".to_owned()))?,
        created_at,
        updated_at,
    })
}

fn encode_timestamp(value: Timestamp) -> String {
    value.to_rfc3339_opts(SecondsFormat::Nanos, true)
}

fn decode_timestamp(value: &str) -> Result<Timestamp, StorageError> {
    DateTime::parse_from_rfc3339(value)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|error| StorageError::InvalidData(error.to_string()))
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use asterism_domain::{Role, UserId};
    use asterism_provider_api::{
        ProviderRuntimeSettingsSchema, ProviderSettingDefinition, ProviderSettingKind,
        ProviderSettingScope, ProviderSettingValue,
    };

    use super::*;

    #[tokio::test]
    #[allow(
        clippy::too_many_lines,
        reason = "the integration test exercises every settings scope and binding in one fixture"
    )]
    async fn settings_are_revisioned_audited_and_bound_to_real_targets() {
        let database = Database::connect("sqlite::memory:").await.unwrap();
        database.migrate().await.unwrap();
        let owner_id = UserId::new();
        let account_id = ProviderAccountId::new();
        let other_account_id = ProviderAccountId::new();
        let task_id = TaskId::new();
        insert_fixture(&database, owner_id, account_id, other_account_id, task_id).await;
        let repository = SqliteProviderRuntimeSettingsRepository::new(database.clone());
        let schema = schema();
        let provider_id = ProviderId::new("provider-alpha").unwrap();
        let at = Utc::now();

        let provider_target = ProviderRuntimeSettingsTarget::Provider {
            provider_id: provider_id.clone(),
        };
        let provider = write(
            &repository,
            provider_target.clone(),
            0,
            &patch(4),
            &schema,
            owner_id,
            at,
        )
        .await;
        assert_eq!(provider.revision, 1);
        assert!(matches!(
            repository
                .write_provider_runtime_settings(ProviderRuntimeSettingsWriteRequest {
                    target: provider_target.clone(),
                    expected_revision: 0,
                    patch: &patch(3),
                    schema: &schema,
                    actor: AuditActor::User(owner_id),
                    correlation_id: "settings-stale",
                    updated_at: at,
                })
                .await
                .unwrap(),
            ProviderRuntimeSettingsWriteOutcome::RevisionConflict
        ));
        let provider = write(
            &repository,
            provider_target.clone(),
            1,
            &patch(3),
            &schema,
            owner_id,
            at + chrono::Duration::seconds(1),
        )
        .await;
        assert_eq!(provider.revision, 2);
        assert_eq!(
            repository
                .find_provider_runtime_settings(&provider_target)
                .await
                .unwrap(),
            Some(provider.clone())
        );

        let account_target = ProviderRuntimeSettingsTarget::ProviderAccount {
            provider_id: provider_id.clone(),
            provider_account_id: account_id,
        };
        let account = write(
            &repository,
            account_target,
            0,
            &patch(2),
            &schema,
            owner_id,
            at + chrono::Duration::seconds(2),
        )
        .await;
        let task_target = ProviderRuntimeSettingsTarget::Task {
            provider_id: provider_id.clone(),
            provider_account_id: account_id,
            task_id,
        };
        let task = write(
            &repository,
            task_target.clone(),
            0,
            &patch(1),
            &schema,
            owner_id,
            at + chrono::Duration::seconds(3),
        )
        .await;
        let resolved = schema
            .resolve(
                Some(&provider.patch),
                Some(&account.patch),
                Some(&task.patch),
            )
            .unwrap();
        assert_eq!(
            resolved.values["execution.max_concurrency"],
            ProviderSettingValue::Integer(1)
        );
        assert_eq!(
            repository
                .find_provider_runtime_settings(&task_target)
                .await
                .unwrap(),
            Some(task)
        );

        let wrong_account = ProviderRuntimeSettingsTarget::ProviderAccount {
            provider_id: ProviderId::new("provider-beta").unwrap(),
            provider_account_id: account_id,
        };
        assert!(matches!(
            repository
                .write_provider_runtime_settings(ProviderRuntimeSettingsWriteRequest {
                    target: wrong_account,
                    expected_revision: 0,
                    patch: &patch(2),
                    schema: &schema,
                    actor: AuditActor::User(owner_id),
                    correlation_id: "settings-wrong-provider",
                    updated_at: at,
                })
                .await
                .unwrap(),
            ProviderRuntimeSettingsWriteOutcome::TargetNotFound
        ));

        let wrong_task = ProviderRuntimeSettingsTarget::Task {
            provider_id,
            provider_account_id: other_account_id,
            task_id,
        };
        assert!(matches!(
            repository
                .write_provider_runtime_settings(ProviderRuntimeSettingsWriteRequest {
                    target: wrong_task,
                    expected_revision: 0,
                    patch: &patch(1),
                    schema: &schema,
                    actor: AuditActor::User(owner_id),
                    correlation_id: "settings-wrong-account",
                    updated_at: at,
                })
                .await
                .unwrap(),
            ProviderRuntimeSettingsWriteOutcome::TargetNotFound
        ));

        let audit_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_records \
             WHERE action = 'provider_runtime_settings_configured'",
        )
        .fetch_one(database.pool())
        .await
        .unwrap();
        assert_eq!(audit_count, 4);
    }

    async fn write(
        repository: &SqliteProviderRuntimeSettingsRepository,
        target: ProviderRuntimeSettingsTarget,
        expected_revision: u32,
        patch: &ProviderRuntimeSettingsPatch,
        schema: &ProviderRuntimeSettingsSchema,
        actor_id: UserId,
        updated_at: Timestamp,
    ) -> ProviderRuntimeSettingsRecord {
        match repository
            .write_provider_runtime_settings(ProviderRuntimeSettingsWriteRequest {
                target,
                expected_revision,
                patch,
                schema,
                actor: AuditActor::User(actor_id),
                correlation_id: "settings-write",
                updated_at,
            })
            .await
            .unwrap()
        {
            ProviderRuntimeSettingsWriteOutcome::Stored(record) => record,
            outcome => panic!("unexpected settings write outcome: {outcome:?}"),
        }
    }

    fn schema() -> ProviderRuntimeSettingsSchema {
        ProviderRuntimeSettingsSchema {
            version: 1,
            definitions: vec![ProviderSettingDefinition {
                key: "execution.max_concurrency".to_owned(),
                display_name: "Execution concurrency".to_owned(),
                description: "Maximum concurrent execution work.".to_owned(),
                kind: ProviderSettingKind::Integer {
                    minimum: 1,
                    maximum: 8,
                    step: 1,
                },
                default: ProviderSettingValue::Integer(2),
                scopes: BTreeSet::from([
                    ProviderSettingScope::Provider,
                    ProviderSettingScope::ProviderAccount,
                    ProviderSettingScope::Task,
                ]),
                core_behavior: None,
            }],
        }
    }

    fn patch(value: i64) -> ProviderRuntimeSettingsPatch {
        ProviderRuntimeSettingsPatch {
            schema_version: 1,
            values: BTreeMap::from([(
                "execution.max_concurrency".to_owned(),
                ProviderSettingValue::Integer(value),
            )]),
        }
    }

    async fn insert_fixture(
        database: &Database,
        owner_id: UserId,
        account_id: ProviderAccountId,
        other_account_id: ProviderAccountId,
        task_id: TaskId,
    ) {
        let now = encode_timestamp(Utc::now());
        sqlx::query(
            "INSERT INTO users \
             (id, username, password_hash, status, roles_json, permissions_json, created_at, updated_at) \
             VALUES (?, 'settings-owner', '$argon2id$test', 'active', ?, '[]', ?, ?)",
        )
        .bind(owner_id.to_string())
        .bind(serde_json::to_string(&BTreeSet::from([Role::Master])).unwrap())
        .bind(&now)
        .bind(&now)
        .execute(database.pool())
        .await
        .unwrap();
        for (account_id, provider_id) in [
            (account_id, "provider-alpha"),
            (other_account_id, "provider-alpha"),
        ] {
            sqlx::query(
                "INSERT INTO provider_accounts \
                 (id, owner_user_id, provider_id, display_name, auth_state_json, created_at, updated_at) \
                 VALUES (?, ?, ?, 'settings-account', ?, ?, ?)",
            )
            .bind(account_id.to_string())
            .bind(owner_id.to_string())
            .bind(provider_id)
            .bind(r#""idle""#)
            .bind(&now)
            .bind(&now)
            .execute(database.pool())
            .await
            .unwrap();
        }
        sqlx::query(
            "INSERT INTO tasks \
             (id, provider_account_id, remote_id, remote_fingerprint, source_type, \
              assessment_class, title, remote_state, orchestration_state, discovered_at, \
              updated_at, capabilities_json) \
             VALUES (?, ?, 'remote-settings-task', 'fingerprint-settings-task', 'chapter', \
                     'routine', 'Settings task', 'pending', 'ready', ?, ?, '[]')",
        )
        .bind(task_id.to_string())
        .bind(account_id.to_string())
        .bind(&now)
        .bind(&now)
        .execute(database.pool())
        .await
        .unwrap();
    }
}
