use std::{collections::BTreeMap, fmt};

use asterism_domain::{
    AssessmentClass, AuthMethod, AuthSessionId, CourseId, LogLevel, ProviderAccountId, ProviderId,
    RemoteState, SecretId, SessionKind, SourceType, TaskCapability, TaskId, Timestamp,
    WaitingUserState,
};
use asterism_secrets::{CredentialBundle, CredentialField};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use zeroize::Zeroize;

use crate::{ProviderMetadata, ProviderResult};

const MAX_ROUTE_CONTEXT_FIELDS: usize = 32;
const MAX_ROUTE_CONTEXT_KEY_BYTES: usize = 64;
const MAX_ROUTE_CONTEXT_VALUE_BYTES: usize = 4_096;

#[derive(Clone, Debug)]
pub struct ProviderContext {
    pub provider_id: ProviderId,
    pub account_id: ProviderAccountId,
    /// Opaque references are resolved only by the secrets boundary at runtime.
    pub credential_refs: Vec<SecretId>,
    pub correlation_id: String,
}

#[derive(Clone, Debug)]
pub struct ProviderAuthContext {
    pub provider_id: ProviderId,
    pub account_id: ProviderAccountId,
    pub auth_session_id: Option<AuthSessionId>,
    pub correlation_id: String,
}

pub trait ProviderIdentity: Send + Sync {
    fn metadata(&self) -> &ProviderMetadata;
}

#[async_trait]
pub trait AuthenticationCapability: ProviderIdentity {
    async fn begin_authentication(
        &self,
        context: &ProviderAuthContext,
        method: AuthMethod,
    ) -> ProviderResult<AuthChallenge>;

    /// Validates a plaintext candidate before Core permits persistence.
    async fn validate_credential(
        &self,
        context: &ProviderAuthContext,
        credential: &CredentialBundle,
    ) -> ProviderResult<CredentialValidation>;

    async fn validate_session(&self, context: &ProviderContext) -> ProviderResult<SessionStatus>;
}

#[async_trait]
pub trait CourseInventoryCapability: ProviderIdentity {
    async fn list_courses(&self, context: &ProviderContext) -> ProviderResult<Vec<RemoteCourse>>;
}

#[async_trait]
pub trait TaskInventoryCapability: ProviderIdentity {
    async fn list_tasks(
        &self,
        context: &ProviderContext,
        course: Option<&RemoteCourse>,
    ) -> ProviderResult<Vec<RemoteTask>>;
}

#[async_trait]
pub trait TaskDetailCapability: ProviderIdentity {
    async fn task_detail(
        &self,
        context: &ProviderContext,
        remote_task_id: &str,
    ) -> ProviderResult<RemoteTaskDetail>;
}

#[async_trait]
pub trait TaskProgressCapability: ProviderIdentity {
    async fn read_progress(
        &self,
        context: &ProviderContext,
        remote_task_id: &str,
    ) -> ProviderResult<RemoteProgress>;
}

#[async_trait]
pub trait TaskExecutionCapability: ProviderIdentity {
    async fn execute(
        &self,
        context: &ProviderContext,
        request: &ExecutionRequest,
        events: &(dyn ExecutionEventSink + Send + Sync),
    ) -> ProviderResult<ExecutionOutcome>;
}

#[async_trait]
pub trait BrowserBridgeCapability: ProviderIdentity {
    async fn browser_session_spec(
        &self,
        context: &ProviderContext,
        remote_task_id: &str,
    ) -> ProviderResult<BrowserSessionSpec>;
}

#[async_trait]
pub trait ExecutionEventSink {
    async fn report(&self, update: ProviderProgress) -> ProviderResult<()>;

    async fn log(&self, event: ProviderExecutionLog) -> ProviderResult<()>;
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AuthChallenge {
    pub session_id: AuthSessionId,
    pub method: AuthMethod,
    pub waiting_for: WaitingUserState,
    pub user_action: Option<String>,
    pub expires_at: Option<Timestamp>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SessionStatus {
    pub valid: bool,
    pub kind: SessionKind,
    pub expires_at: Option<Timestamp>,
    pub account_hint: Option<String>,
}

/// Provider-produced secret fields which replace a successfully validated
/// candidate before Core persists it.
///
/// Provider code can use this boundary to turn a native password exchange into
/// a renewable session without gaining access to `SecretStore`. Core retains all
/// immutable candidate metadata and validates this replacement again.
#[derive(Debug)]
pub struct CredentialReplacement {
    pub session_kind: SessionKind,
    pub fields: Vec<CredentialField>,
}

/// Result of Provider-side credential validation.
#[derive(Debug)]
pub struct CredentialValidation {
    pub status: SessionStatus,
    pub replacement: Option<CredentialReplacement>,
}

impl CredentialValidation {
    pub const fn accepted(status: SessionStatus) -> Self {
        Self {
            status,
            replacement: None,
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct RemoteCourse {
    pub remote_id: String,
    pub title: String,
    pub term: Option<String>,
    pub teacher: Option<String>,
    pub remote_status: Option<String>,
    pub metadata_sanitized: serde_json::Value,
    /// Bounded, scan-local routing facts passed from course discovery to later
    /// Provider capabilities. This field is never serialized or persisted.
    #[serde(skip)]
    pub route_context: ProviderRouteContext,
}

/// Ephemeral Provider routing facts which are redacted in diagnostics, omitted
/// from serialization, and zeroized when their final owner is dropped.
#[derive(Clone, Default, Eq, PartialEq)]
pub struct ProviderRouteContext {
    values: BTreeMap<String, String>,
}

impl ProviderRouteContext {
    /// Builds one bounded route context from Provider-owned key/value pairs.
    ///
    /// # Errors
    ///
    /// Returns an invalid-response error for duplicate, malformed, empty, or
    /// oversized route facts.
    pub fn try_from_pairs(
        pairs: impl IntoIterator<Item = (String, String)>,
    ) -> ProviderResult<Self> {
        let mut context = Self::default();
        for (key, mut value) in pairs {
            if context.values.len() >= MAX_ROUTE_CONTEXT_FIELDS
                || !valid_route_context_key(&key)
                || !valid_route_context_value(&value)
                || context.values.contains_key(&key)
            {
                value.zeroize();
                return Err(crate::ProviderError::new(
                    crate::ProviderErrorKind::InvalidResponse,
                    "Provider route context contains invalid or duplicate facts",
                ));
            }
            context.values.insert(key, value);
        }
        Ok(context)
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.values.get(key).map(String::as_str)
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }
}

impl fmt::Debug for ProviderRouteContext {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProviderRouteContext")
            .field("field_count", &self.values.len())
            .field("values", &"[REDACTED]")
            .finish()
    }
}

impl Drop for ProviderRouteContext {
    fn drop(&mut self) {
        for value in self.values.values_mut() {
            value.zeroize();
        }
    }
}

fn valid_route_context_key(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_ROUTE_CONTEXT_KEY_BYTES
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_' | b'.')
        })
}

fn valid_route_context_value(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_ROUTE_CONTEXT_VALUE_BYTES
        && !value.chars().any(char::is_control)
}

#[cfg(test)]
mod route_context_tests {
    use super::*;

    #[test]
    fn route_context_is_bounded_redacted_and_not_serialized() {
        let private_value = "account-scoped-route-value";
        let context = ProviderRouteContext::try_from_pairs([(
            "chaoxing.cpi".to_owned(),
            private_value.to_owned(),
        )])
        .unwrap();
        assert_eq!(context.get("chaoxing.cpi"), Some(private_value));
        assert!(!format!("{context:?}").contains(private_value));

        let course = RemoteCourse {
            remote_id: "course:100:200".to_owned(),
            title: "course".to_owned(),
            term: None,
            teacher: None,
            remote_status: None,
            metadata_sanitized: serde_json::json!({"safe": true}),
            route_context: context,
        };
        assert!(!format!("{course:?}").contains(private_value));
        let encoded = serde_json::to_string(&course).unwrap();
        assert!(!encoded.contains(private_value));
        assert!(!encoded.contains("route_context"));

        let decoded: RemoteCourse = serde_json::from_str(&encoded).unwrap();
        assert!(decoded.route_context.is_empty());
    }

    #[test]
    fn route_context_rejects_duplicate_or_malformed_facts() {
        assert!(
            ProviderRouteContext::try_from_pairs([
                ("route".to_owned(), "one".to_owned()),
                ("route".to_owned(), "two".to_owned()),
            ])
            .is_err()
        );
        assert!(
            ProviderRouteContext::try_from_pairs([
                ("Chaoxing.CPI".to_owned(), "value".to_owned(),)
            ])
            .is_err()
        );
        assert!(
            ProviderRouteContext::try_from_pairs([("route".to_owned(), "bad\nvalue".to_owned(),)])
                .is_err()
        );
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct RemoteTask {
    pub remote_id: String,
    pub course_remote_id: Option<String>,
    pub title: String,
    pub source_type: SourceType,
    pub assessment_class: AssessmentClass,
    pub remote_state: RemoteState,
    pub opens_at: Option<Timestamp>,
    pub due_at: Option<Timestamp>,
    pub closes_at: Option<Timestamp>,
    pub capabilities: Vec<TaskCapability>,
    pub fingerprint: String,
    pub normalized: serde_json::Value,
    pub raw_sanitized: serde_json::Value,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct RemoteTaskDetail {
    pub task: RemoteTask,
    pub normalized_detail: serde_json::Value,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RemoteProgress {
    pub remote_state: RemoteState,
    pub percent: Option<u8>,
    pub duration_seconds: Option<u64>,
    pub updated_at: Timestamp,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ExecutionRequest {
    pub task_id: TaskId,
    pub remote_task_id: String,
    pub course_id: Option<CourseId>,
    pub requested_capabilities: Vec<TaskCapability>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProviderProgress {
    pub percent: Option<u8>,
    pub stage: String,
    pub status_text: Option<String>,
    pub completed_items: Option<u32>,
    pub total_items: Option<u32>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ProviderExecutionLog {
    pub level: LogLevel,
    pub stage: String,
    pub message: String,
    pub provider_trace_id: Option<String>,
    pub metadata_sanitized: Option<serde_json::Value>,
}

impl ProviderExecutionLog {
    /// Validates the bounded, sanitized Provider-to-Core log contract.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderExecutionLogError`] for oversized or control-bearing
    /// text, oversized metadata, or a credential-shaped metadata key.
    pub fn validate(&self) -> Result<(), ProviderExecutionLogError> {
        let valid_text = |value: &str, maximum: usize| {
            !value.is_empty()
                && value.len() <= maximum
                && value.trim() == value
                && !value.chars().any(char::is_control)
        };
        if !valid_text(&self.stage, 64)
            || !valid_text(&self.message, 2_048)
            || self
                .provider_trace_id
                .as_deref()
                .is_some_and(|value| !valid_text(value, 256))
            || self.metadata_sanitized.as_ref().is_some_and(|value| {
                serde_json::to_vec(value).map_or(true, |encoded| encoded.len() > 8 * 1_024)
                    || contains_secret_key(value)
            })
        {
            Err(ProviderExecutionLogError::Invalid)
        } else {
            Ok(())
        }
    }
}

fn contains_secret_key(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Object(object) => object.iter().any(|(key, value)| {
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
        serde_json::Value::Array(items) => items.iter().any(contains_secret_key),
        serde_json::Value::Null
        | serde_json::Value::Bool(_)
        | serde_json::Value::Number(_)
        | serde_json::Value::String(_) => false,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ProviderExecutionLogError {
    #[error("Provider execution log is oversized or not sanitized")]
    Invalid,
}

#[cfg(test)]
mod execution_log_tests {
    use super::*;

    fn valid_log() -> ProviderExecutionLog {
        ProviderExecutionLog {
            level: LogLevel::Info,
            stage: "resource_verify".to_owned(),
            message: "remote completion verified".to_owned(),
            provider_trace_id: Some("trace-safe".to_owned()),
            metadata_sanitized: Some(serde_json::json!({"verified": true})),
        }
    }

    #[test]
    fn provider_execution_log_is_bounded_and_rejects_secret_keys() {
        assert!(valid_log().validate().is_ok());

        let mut secret = valid_log();
        secret.metadata_sanitized = Some(serde_json::json!({
            "nested": {"access_token": "must-not-enter-log-stream"}
        }));
        assert_eq!(secret.validate(), Err(ProviderExecutionLogError::Invalid));

        let mut multiline = valid_log();
        multiline.message = "forged\nlog line".to_owned();
        assert_eq!(
            multiline.validate(),
            Err(ProviderExecutionLogError::Invalid)
        );

        let mut oversized = valid_log();
        oversized.message = "x".repeat(2_049);
        assert_eq!(
            oversized.validate(),
            Err(ProviderExecutionLogError::Invalid)
        );
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ExecutionOutcome {
    pub remote_state: RemoteState,
    pub verified: bool,
    pub result_sanitized: serde_json::Value,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BrowserSessionSpec {
    pub isolation_key: String,
    pub allowed_origins: Vec<String>,
    pub headless: bool,
}
