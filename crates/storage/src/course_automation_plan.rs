use std::str::FromStr;

use asterism_domain::{
    AutomationPlan, AutomationPlanId, AutomationPlanStatus, BillingPolicy, CourseId, CoverageSpec,
    ExecutionPolicy, InheritanceMode, PlanScope, SchedulePolicy, Timestamp, UserId,
};
use async_trait::async_trait;
use chrono::{DateTime, SecondsFormat, Utc};
use sqlx::{Row, Sqlite, Transaction, sqlite::SqliteRow};

use crate::{
    CourseAutomationPlanRepository, CourseAutomationPlanWriteOutcome,
    CourseAutomationPlanWriteRequest, Database, StorageError,
};

const COURSE_PATROL_PLAN_NAME: &str = "Course patrol auto-execution";

#[derive(Clone, Debug)]
pub struct SqliteCourseAutomationPlanRepository {
    database: Database,
}

impl SqliteCourseAutomationPlanRepository {
    pub const fn new(database: Database) -> Self {
        Self { database }
    }
}

#[async_trait]
impl CourseAutomationPlanRepository for SqliteCourseAutomationPlanRepository {
    async fn find_owned_course_automation_plan(
        &self,
        owner_user_id: UserId,
        course_id: CourseId,
    ) -> Result<Option<AutomationPlan>, StorageError> {
        let rows = sqlx::query(
            "SELECT id, owner_user_id, name, scope_json, coverage_json, inheritance_mode, \
                    execution_policy, billing_policy_json, schedule_policy_json, effective_from, \
                    expires_at, priority, status, created_by, created_at, updated_at \
             FROM automation_plans WHERE owner_user_id = ? AND name = ? \
             ORDER BY updated_at DESC, id DESC",
        )
        .bind(owner_user_id.to_string())
        .bind(COURSE_PATROL_PLAN_NAME)
        .fetch_all(self.database.pool())
        .await?;
        decode_matching_course_plan(rows.iter(), course_id)
    }

    async fn list_effective_course_automation_plans(
        &self,
        at: Timestamp,
    ) -> Result<Vec<AutomationPlan>, StorageError> {
        let rows = sqlx::query(
            "SELECT id, owner_user_id, name, scope_json, coverage_json, inheritance_mode, \
                    execution_policy, billing_policy_json, schedule_policy_json, effective_from, \
                    expires_at, priority, status, created_by, created_at, updated_at \
             FROM automation_plans \
             WHERE name = ? AND status = 'active' AND effective_from <= ? \
               AND (expires_at IS NULL OR expires_at > ?) \
             ORDER BY priority DESC, updated_at DESC, id DESC",
        )
        .bind(COURSE_PATROL_PLAN_NAME)
        .bind(encode_timestamp(at))
        .bind(encode_timestamp(at))
        .fetch_all(self.database.pool())
        .await?;
        let mut plans = Vec::with_capacity(rows.len());
        for row in &rows {
            let plan = decode_plan(row)?;
            if matches!(plan.scope, PlanScope::Course(_))
                && plan.execution_policy == ExecutionPolicy::Auto
            {
                plans.push(plan);
            }
        }
        Ok(plans)
    }

    async fn write_course_automation_plan(
        &self,
        request: CourseAutomationPlanWriteRequest,
    ) -> Result<CourseAutomationPlanWriteOutcome, StorageError> {
        let mut transaction = self.database.pool().begin_with("BEGIN IMMEDIATE").await?;
        if !owns_course(&mut transaction, request.owner_user_id, request.course_id).await? {
            transaction.commit().await?;
            return Ok(CourseAutomationPlanWriteOutcome::CourseNotFound);
        }

        let rows = select_owner_plans(&mut transaction, request.owner_user_id).await?;
        let current = decode_matching_course_plan(rows.iter(), request.course_id)?;
        let status = if request.enabled {
            AutomationPlanStatus::Active
        } else {
            AutomationPlanStatus::Paused
        };
        let plan = if let Some(mut current) = current {
            if request.updated_at < current.updated_at {
                return Err(StorageError::InvalidData(
                    "course automation plan timestamp regresses".to_owned(),
                ));
            }
            current.status = status;
            if let Some(profile) = request.ai_profile.clone() {
                current.schedule_policy.ai_profile = profile;
            }
            current.updated_at = request.updated_at;
            sqlx::query(
                "UPDATE automation_plans SET status = ?, schedule_policy_json = ?, updated_at = ? \
                 WHERE id = ? AND owner_user_id = ?",
            )
            .bind(encode_status(status))
            .bind(serde_json::to_string(&current.schedule_policy)?)
            .bind(encode_timestamp(request.updated_at))
            .bind(current.id.to_string())
            .bind(request.owner_user_id.to_string())
            .execute(&mut *transaction)
            .await?;
            current
        } else {
            let plan = AutomationPlan {
                id: AutomationPlanId::new(),
                owner_user_id: request.owner_user_id,
                name: COURSE_PATROL_PLAN_NAME.to_owned(),
                scope: PlanScope::Course(request.course_id),
                coverage: CoverageSpec {
                    all_supported: true,
                    ..CoverageSpec::default()
                },
                inheritance_mode: InheritanceMode::Override,
                execution_policy: ExecutionPolicy::Auto,
                billing_policy: BillingPolicy::UsageBased,
                schedule_policy: SchedulePolicy {
                    grace_period_seconds: None,
                    quiet_hours_start: None,
                    quiet_hours_end: None,
                    ai_profile: request.ai_profile.clone().flatten(),
                },
                effective_from: request.updated_at,
                expires_at: None,
                priority: 0,
                status,
                created_by: request.owner_user_id,
                created_at: request.updated_at,
                updated_at: request.updated_at,
            };
            insert_plan(&mut transaction, &plan).await?;
            plan
        };
        transaction.commit().await?;
        Ok(CourseAutomationPlanWriteOutcome::Stored(plan))
    }
}

async fn owns_course(
    transaction: &mut Transaction<'_, Sqlite>,
    owner_user_id: UserId,
    course_id: CourseId,
) -> Result<bool, StorageError> {
    Ok(sqlx::query_scalar::<_, i64>(
        "SELECT EXISTS(\
             SELECT 1 FROM courses AS course \
             INNER JOIN provider_accounts AS account ON account.id = course.provider_account_id \
             WHERE course.id = ? AND account.owner_user_id = ?\
         )",
    )
    .bind(course_id.to_string())
    .bind(owner_user_id.to_string())
    .fetch_one(&mut **transaction)
    .await?
        == 1)
}

async fn select_owner_plans(
    transaction: &mut Transaction<'_, Sqlite>,
    owner_user_id: UserId,
) -> Result<Vec<SqliteRow>, StorageError> {
    Ok(sqlx::query(
        "SELECT id, owner_user_id, name, scope_json, coverage_json, inheritance_mode, \
                execution_policy, billing_policy_json, schedule_policy_json, effective_from, \
                expires_at, priority, status, created_by, created_at, updated_at \
         FROM automation_plans WHERE owner_user_id = ? AND name = ? \
         ORDER BY updated_at DESC, id DESC",
    )
    .bind(owner_user_id.to_string())
    .bind(COURSE_PATROL_PLAN_NAME)
    .fetch_all(&mut **transaction)
    .await?)
}

fn decode_matching_course_plan<'a>(
    rows: impl IntoIterator<Item = &'a SqliteRow>,
    course_id: CourseId,
) -> Result<Option<AutomationPlan>, StorageError> {
    for row in rows {
        let plan = decode_plan(row)?;
        if plan.scope == PlanScope::Course(course_id) {
            return Ok(Some(plan));
        }
    }
    Ok(None)
}

async fn insert_plan(
    transaction: &mut Transaction<'_, Sqlite>,
    plan: &AutomationPlan,
) -> Result<(), StorageError> {
    sqlx::query(
        "INSERT INTO automation_plans \
         (id, owner_user_id, name, scope_json, coverage_json, inheritance_mode, \
          execution_policy, billing_policy_json, schedule_policy_json, notification_policy_json, \
          effective_from, expires_at, priority, status, created_by, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, '{}', ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(plan.id.to_string())
    .bind(plan.owner_user_id.to_string())
    .bind(&plan.name)
    .bind(serde_json::to_string(&plan.scope)?)
    .bind(serde_json::to_string(&plan.coverage)?)
    .bind(encode_inheritance_mode(plan.inheritance_mode))
    .bind(encode_execution_policy(plan.execution_policy))
    .bind(serde_json::to_string(&plan.billing_policy)?)
    .bind(serde_json::to_string(&plan.schedule_policy)?)
    .bind(encode_timestamp(plan.effective_from))
    .bind(plan.expires_at.map(encode_timestamp))
    .bind(plan.priority)
    .bind(encode_status(plan.status))
    .bind(plan.created_by.to_string())
    .bind(encode_timestamp(plan.created_at))
    .bind(encode_timestamp(plan.updated_at))
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

fn decode_plan(row: &SqliteRow) -> Result<AutomationPlan, StorageError> {
    Ok(AutomationPlan {
        id: AutomationPlanId::from_str(row.try_get("id")?).map_err(invalid_data)?,
        owner_user_id: UserId::from_str(row.try_get("owner_user_id")?).map_err(invalid_data)?,
        name: row.try_get("name")?,
        scope: serde_json::from_str(row.try_get("scope_json")?)?,
        coverage: serde_json::from_str(row.try_get("coverage_json")?)?,
        inheritance_mode: decode_inheritance_mode(row.try_get("inheritance_mode")?)?,
        execution_policy: decode_execution_policy(row.try_get("execution_policy")?)?,
        billing_policy: serde_json::from_str(row.try_get("billing_policy_json")?)?,
        schedule_policy: serde_json::from_str(row.try_get("schedule_policy_json")?)?,
        effective_from: decode_timestamp(row.try_get("effective_from")?)?,
        expires_at: row
            .try_get::<Option<&str>, _>("expires_at")?
            .map(decode_timestamp)
            .transpose()?,
        priority: row.try_get("priority")?,
        status: decode_status(row.try_get("status")?)?,
        created_by: UserId::from_str(row.try_get("created_by")?).map_err(invalid_data)?,
        created_at: decode_timestamp(row.try_get("created_at")?)?,
        updated_at: decode_timestamp(row.try_get("updated_at")?)?,
    })
}

const fn encode_inheritance_mode(value: InheritanceMode) -> &'static str {
    match value {
        InheritanceMode::Merge => "merge",
        InheritanceMode::Override => "override",
    }
}

fn decode_inheritance_mode(value: &str) -> Result<InheritanceMode, StorageError> {
    match value {
        "merge" => Ok(InheritanceMode::Merge),
        "override" => Ok(InheritanceMode::Override),
        _ => Err(invalid_data("automation plan inheritance mode is invalid")),
    }
}

const fn encode_execution_policy(value: ExecutionPolicy) -> &'static str {
    match value {
        ExecutionPolicy::Auto => "auto",
        ExecutionPolicy::DeferredApproval => "deferred_approval",
        ExecutionPolicy::ManualApproval => "manual_approval",
        ExecutionPolicy::NotifyOnly => "notify_only",
    }
}

fn decode_execution_policy(value: &str) -> Result<ExecutionPolicy, StorageError> {
    match value {
        "auto" => Ok(ExecutionPolicy::Auto),
        "deferred_approval" => Ok(ExecutionPolicy::DeferredApproval),
        "manual_approval" => Ok(ExecutionPolicy::ManualApproval),
        "notify_only" => Ok(ExecutionPolicy::NotifyOnly),
        _ => Err(invalid_data("automation plan execution policy is invalid")),
    }
}

const fn encode_status(value: AutomationPlanStatus) -> &'static str {
    match value {
        AutomationPlanStatus::Draft => "draft",
        AutomationPlanStatus::Active => "active",
        AutomationPlanStatus::Paused => "paused",
        AutomationPlanStatus::Expired => "expired",
        AutomationPlanStatus::Cancelled => "cancelled",
    }
}

fn decode_status(value: &str) -> Result<AutomationPlanStatus, StorageError> {
    match value {
        "draft" => Ok(AutomationPlanStatus::Draft),
        "active" => Ok(AutomationPlanStatus::Active),
        "paused" => Ok(AutomationPlanStatus::Paused),
        "expired" => Ok(AutomationPlanStatus::Expired),
        "cancelled" => Ok(AutomationPlanStatus::Cancelled),
        _ => Err(invalid_data("automation plan status is invalid")),
    }
}

fn encode_timestamp(value: Timestamp) -> String {
    value.to_rfc3339_opts(SecondsFormat::Micros, true)
}

fn decode_timestamp(value: &str) -> Result<Timestamp, StorageError> {
    DateTime::parse_from_rfc3339(value)
        .map(|value| value.with_timezone(&Utc))
        .map_err(invalid_data)
}

#[allow(clippy::needless_pass_by_value)]
fn invalid_data(error: impl ToString) -> StorageError {
    StorageError::InvalidData(error.to_string())
}

#[cfg(test)]
mod tests {
    use asterism_domain::{AuthState, ProviderAccountId};
    use chrono::{Duration, Timelike};

    use super::*;

    #[tokio::test]
    async fn course_plan_is_absent_by_default_and_owner_bound() {
        let database = Database::connect("sqlite::memory:").await.unwrap();
        database.migrate().await.unwrap();
        let repository = SqliteCourseAutomationPlanRepository::new(database.clone());
        let now = Utc::now().with_nanosecond(0).unwrap();
        let owner = UserId::new();
        let other_owner = UserId::new();
        seed_user(&database, owner, "owner", now).await;
        seed_user(&database, other_owner, "other", now).await;
        let account_id = ProviderAccountId::new();
        sqlx::query(
            "INSERT INTO provider_accounts \
             (id, owner_user_id, provider_id, display_name, auth_state_json, created_at, updated_at) \
             VALUES (?, ?, 'chaoxing', 'Chaoxing', ?, ?, ?)",
        )
        .bind(account_id.to_string())
        .bind(owner.to_string())
        .bind(serde_json::to_string(&AuthState::Authenticated).unwrap())
        .bind(encode_timestamp(now))
        .bind(encode_timestamp(now))
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
        .bind(encode_timestamp(now))
        .execute(database.pool())
        .await
        .unwrap();

        assert!(
            repository
                .find_owned_course_automation_plan(owner, course_id)
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(
            repository
                .write_course_automation_plan(CourseAutomationPlanWriteRequest {
                    owner_user_id: other_owner,
                    course_id,
                    enabled: true,
                    ai_profile: None,
                    updated_at: now,
                })
                .await
                .unwrap(),
            CourseAutomationPlanWriteOutcome::CourseNotFound
        );

        let outcome = repository
            .write_course_automation_plan(CourseAutomationPlanWriteRequest {
                owner_user_id: owner,
                course_id,
                enabled: false,
                ai_profile: None,
                updated_at: now,
            })
            .await
            .unwrap();
        let CourseAutomationPlanWriteOutcome::Stored(paused) = outcome else {
            panic!("owned course should accept its plan")
        };
        assert_eq!(paused.status, AutomationPlanStatus::Paused);
        assert!(
            repository
                .list_effective_course_automation_plans(now)
                .await
                .unwrap()
                .is_empty()
        );

        let later = now + Duration::seconds(1);
        let outcome = repository
            .write_course_automation_plan(CourseAutomationPlanWriteRequest {
                owner_user_id: owner,
                course_id,
                enabled: true,
                ai_profile: Some(Some("gpt_only".to_owned())),
                updated_at: later,
            })
            .await
            .unwrap();
        let CourseAutomationPlanWriteOutcome::Stored(active) = outcome else {
            panic!("owned course should update its plan")
        };
        assert_eq!(active.id, paused.id);
        assert_eq!(active.created_at, paused.created_at);
        assert_eq!(active.status, AutomationPlanStatus::Active);
        assert_eq!(
            active.schedule_policy.ai_profile.as_deref(),
            Some("gpt_only")
        );
        assert!(
            repository
                .find_owned_course_automation_plan(other_owner, course_id)
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(
            repository
                .list_effective_course_automation_plans(later)
                .await
                .unwrap(),
            vec![active]
        );
    }

    #[tokio::test]
    async fn regressing_update_is_rejected() {
        let database = Database::connect("sqlite::memory:").await.unwrap();
        database.migrate().await.unwrap();
        let repository = SqliteCourseAutomationPlanRepository::new(database.clone());
        let now = Utc::now().with_nanosecond(0).unwrap();
        let owner = UserId::new();
        seed_user(&database, owner, "owner", now).await;
        let account_id = ProviderAccountId::new();
        sqlx::query(
            "INSERT INTO provider_accounts \
             (id, owner_user_id, provider_id, display_name, auth_state_json, created_at, updated_at) \
             VALUES (?, ?, 'chaoxing', 'Chaoxing', ?, ?, ?)",
        )
        .bind(account_id.to_string())
        .bind(owner.to_string())
        .bind(serde_json::to_string(&AuthState::Authenticated).unwrap())
        .bind(encode_timestamp(now))
        .bind(encode_timestamp(now))
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
        .bind(encode_timestamp(now))
        .execute(database.pool())
        .await
        .unwrap();
        repository
            .write_course_automation_plan(CourseAutomationPlanWriteRequest {
                owner_user_id: owner,
                course_id,
                enabled: true,
                ai_profile: None,
                updated_at: now,
            })
            .await
            .unwrap();
        let error = repository
            .write_course_automation_plan(CourseAutomationPlanWriteRequest {
                owner_user_id: owner,
                course_id,
                enabled: false,
                ai_profile: None,
                updated_at: now - Duration::seconds(1),
            })
            .await
            .unwrap_err();
        assert!(matches!(error, StorageError::InvalidData(_)));
    }

    async fn seed_user(database: &Database, user_id: UserId, username: &str, at: Timestamp) {
        sqlx::query(
            "INSERT INTO users \
             (id, username, password_hash, status, roles_json, permissions_json, created_at, updated_at) \
             VALUES (?, ?, 'hash', 'active', '[\"user\"]', '[]', ?, ?)",
        )
        .bind(user_id.to_string())
        .bind(username)
        .bind(encode_timestamp(at))
        .bind(encode_timestamp(at))
        .execute(database.pool())
        .await
        .unwrap();
    }
}
