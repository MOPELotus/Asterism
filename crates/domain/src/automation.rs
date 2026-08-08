use serde::{Deserialize, Serialize};

use crate::{
    AutomationPlanId, CourseId, CreditAmount, ProviderAccountId, ProviderId, SourceType,
    TaskCapability, TaskId, Timestamp, UserId,
};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "id")]
pub enum PlanScope {
    UserAll,
    Provider(ProviderId),
    ProviderAccount(ProviderAccountId),
    Course(CourseId),
    Task(TaskId),
}

impl PlanScope {
    pub const fn specificity(&self) -> u8 {
        match self {
            Self::UserAll => 0,
            Self::Provider(_) => 1,
            Self::ProviderAccount(_) => 2,
            Self::Course(_) => 3,
            Self::Task(_) => 4,
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct CoverageSpec {
    pub include_source_types: Vec<SourceType>,
    pub exclude_source_types: Vec<SourceType>,
    pub include_capabilities: Vec<TaskCapability>,
    pub exclude_capabilities: Vec<TaskCapability>,
    pub all_supported: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InheritanceMode {
    Merge,
    Override,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionPolicy {
    Auto,
    DeferredApproval,
    ManualApproval,
    NotifyOnly,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum BillingPolicy {
    UsageBased,
    FixedByTaskType { amount: CreditAmount },
    FlatPackage { entitlement_id: String },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SchedulePolicy {
    pub grace_period_seconds: Option<u64>,
    pub quiet_hours_start: Option<String>,
    pub quiet_hours_end: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AutomationPlanStatus {
    Draft,
    Active,
    Paused,
    Expired,
    Cancelled,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AutomationPlan {
    pub id: AutomationPlanId,
    pub owner_user_id: UserId,
    pub name: String,
    pub scope: PlanScope,
    pub coverage: CoverageSpec,
    pub inheritance_mode: InheritanceMode,
    pub execution_policy: ExecutionPolicy,
    pub billing_policy: BillingPolicy,
    pub schedule_policy: SchedulePolicy,
    pub effective_from: Timestamp,
    pub expires_at: Option<Timestamp>,
    pub priority: i32,
    pub status: AutomationPlanStatus,
    pub created_by: UserId,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_specificity_is_deterministic() {
        assert!(
            PlanScope::Task(TaskId::new()).specificity()
                > PlanScope::Course(CourseId::new()).specificity()
        );
        assert!(
            PlanScope::ProviderAccount(ProviderAccountId::new()).specificity()
                > PlanScope::Provider(ProviderId::new("chaoxing").unwrap()).specificity()
        );
        assert_eq!(PlanScope::UserAll.specificity(), 0);
    }
}
