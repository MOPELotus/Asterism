use std::collections::BTreeSet;

use asterism_domain::{AuthMethod, ProviderId, SessionKind};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProviderMetadata {
    pub id: ProviderId,
    pub display_name: String,
    pub implementation_version: String,
    pub verification: VerificationLevel,
    pub scan_min_interval_seconds: Option<u64>,
    pub capture_recipe_version: Option<u32>,
    pub capabilities: BTreeSet<ProviderCapability>,
    pub auth_methods: BTreeSet<AuthMethod>,
    pub session_kinds: BTreeSet<SessionKind>,
}

impl ProviderMetadata {
    pub fn advertises(&self, capability: ProviderCapability) -> bool {
        self.capabilities.contains(&capability)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationLevel {
    Development,
    Experimental,
    CommunityVerified,
    Verified,
    Broken,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderCapability {
    Authentication,
    CourseInventory,
    TaskInventory,
    TaskDetail,
    TaskProgressRead,
    ResourceExecution,
    /// The Provider can independently verify a non-idempotent task execution
    /// against the same frozen execution goal, allowing Core to recover without
    /// replaying the remote mutation or reducing success to generic completion.
    ExecutionVerify,
    QuestionInventory,
    QuestionParse,
    AnswerResolve,
    SubmissionBuild,
    SubmissionExecute,
    SubmissionVerify,
    AnswerHistoryHarvest,
    DurationRead,
    DurationReport,
    Discussion,
    Practice,
    BrowserBridge,
}
