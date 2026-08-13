use std::{collections::BTreeMap, fmt};

use asterism_domain::{
    AnswerCandidate, AssessmentClass, AuthMethod, AuthSessionId, CourseId, ExecutionId, LogLevel,
    ProviderAccountId, ProviderId, Question, QuestionKind, RemoteState, SecretId, SelectedAnswer,
    SessionKind, SourceType, SubmissionDraft, SubmissionPayloadPreview, SubmissionReceipt,
    SubmissionVerificationSnapshot, TaskCapability, TaskId, Timestamp, WaitingUserState,
};
use asterism_secrets::{CredentialBundle, CredentialField, SecretPurpose, SecretString};
use async_trait::async_trait;
use http::Uri;
use serde::{Deserialize, Serialize};
use zeroize::Zeroize;

use crate::ResolvedProviderRuntimeSettings;
use crate::{ProviderMetadata, ProviderResult};

const MAX_ROUTE_CONTEXT_FIELDS: usize = 32;
const MAX_ROUTE_CONTEXT_KEY_BYTES: usize = 64;
const MAX_ROUTE_CONTEXT_VALUE_BYTES: usize = 4_096;
const MAX_REMOTE_QUESTION_ID_BYTES: usize = 512;
const MAX_QUESTION_REF_METADATA_BYTES: usize = 64 * 1_024;
const MAX_QUESTION_POSITION: u32 = 100_000;
const MAX_CAPTURE_RECIPE_BYTES: usize = 64 * 1_024;
const MAX_CAPTURE_ORIGINS: usize = 8;
const MAX_CAPTURE_OUTPUTS: usize = 16;
const MAX_CAPTURE_SOURCES: usize = 8;
const MAX_CAPTURE_JSON_FIELDS: usize = 16;

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

/// Declarative, code-free browser-state acquisition contract delivered to a
/// paired Capture helper. Every required output must be read from one logical
/// browser snapshot before the resulting credential bundle is submitted.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CaptureRecipe {
    pub version: u32,
    pub start_url: String,
    /// Top-level origins the isolated helper may visit during this auth flow.
    /// This may include exact third-party OAuth origins, but grants no access
    /// to their storage, headers or Cookies.
    pub navigation_origins: Vec<String>,
    /// Origins from which declared credential sources may be read. This is a
    /// strict subset of `navigation_origins`.
    pub read_origins: Vec<String>,
    pub poll_interval_millis: u64,
    pub auth_method: AuthMethod,
    pub session_kind: SessionKind,
    pub readiness: CaptureReadiness,
    pub outputs: Vec<CaptureCredentialOutput>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CaptureReadiness {
    /// Legacy-safe only for Providers whose required outputs cannot exist in
    /// an anonymous/pre-login session.
    OutputsComplete,
    /// Requires an exact request bound to the current top-level document
    /// loader before any resolved credential is accepted. This prevents
    /// anonymous Cookies created by a login page from prematurely completing
    /// Capture.
    RequestObserved {
        origin: String,
        method: String,
        path_and_query: String,
    },
    /// Requires the exact request to receive an exact successful response
    /// media type under the same current document loader. This distinguishes
    /// authenticated API data from login HTML returned with status 200.
    ResponseObserved {
        origin: String,
        method: String,
        path_and_query: String,
        status: u16,
        mime_type: String,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CaptureCredentialOutput {
    pub purpose: SecretPurpose,
    pub required: bool,
    /// Ordered alternatives. A helper uses the first complete source available
    /// in the current snapshot and never combines snapshots across polls.
    pub sources: Vec<CaptureValueSource>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CaptureValueSource {
    RequestHeader {
        origin: String,
        name: String,
    },
    LocalStorage {
        origin: String,
        key: String,
    },
    SessionStorage {
        origin: String,
        key: String,
    },
    /// Produces the canonical Cookie request-header value visible to the
    /// selected origin. Cookie attributes never enter the credential value.
    CookieHeader {
        origin: String,
    },
    /// Builds one bounded JSON object from scalar browser facts without
    /// executing Provider-supplied JavaScript.
    JsonObject {
        fields: Vec<CaptureJsonField>,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CaptureJsonField {
    pub name: String,
    /// Ordered scalar alternatives for this object field.
    pub sources: Vec<CaptureScalarSource>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CaptureScalarSource {
    RequestHeader { origin: String, name: String },
    LocalStorage { origin: String, key: String },
    SessionStorage { origin: String, key: String },
}

impl CaptureRecipe {
    /// Validates bounded origins, output purposes and declarative sources.
    /// Recipes cannot carry scripts, selectors, proxy configuration or raw log
    /// instructions.
    ///
    /// # Errors
    ///
    /// Returns [`CaptureRecipeError::Invalid`] for an unsafe or inconsistent
    /// recipe.
    pub fn validate(&self) -> Result<(), CaptureRecipeError> {
        let start_origin = https_origin(&self.start_url, false)?;
        if self.version == 0
            || self.navigation_origins.is_empty()
            || self.navigation_origins.len() > MAX_CAPTURE_ORIGINS
            || self.read_origins.is_empty()
            || self.read_origins.len() > MAX_CAPTURE_ORIGINS
            || !(100..=5_000).contains(&self.poll_interval_millis)
            || !matches!(
                self.auth_method,
                AuthMethod::QrCode | AuthMethod::ExternalBrowserOauth | AuthMethod::AssistedSession
            )
            || self.outputs.is_empty()
            || self.outputs.len() > MAX_CAPTURE_OUTPUTS
        {
            return Err(CaptureRecipeError::Invalid);
        }
        let mut navigation_origins = self.navigation_origins.clone();
        for origin in &navigation_origins {
            if https_origin(origin, true)? != *origin {
                return Err(CaptureRecipeError::Invalid);
            }
        }
        navigation_origins.sort_unstable();
        if navigation_origins.windows(2).any(|pair| pair[0] == pair[1])
            || !navigation_origins
                .iter()
                .any(|origin| origin == &start_origin)
        {
            return Err(CaptureRecipeError::Invalid);
        }
        let mut read_origins = self.read_origins.clone();
        for origin in &read_origins {
            if https_origin(origin, true)? != *origin
                || !navigation_origins.iter().any(|allowed| allowed == origin)
            {
                return Err(CaptureRecipeError::Invalid);
            }
        }
        read_origins.sort_unstable();
        if read_origins.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err(CaptureRecipeError::Invalid);
        }
        validate_capture_readiness(&self.readiness, &navigation_origins)?;

        let mut purposes = Vec::with_capacity(self.outputs.len());
        for output in &self.outputs {
            if !output.purpose.is_provider_credential()
                || output.sources.is_empty()
                || output.sources.len() > MAX_CAPTURE_SOURCES
            {
                return Err(CaptureRecipeError::Invalid);
            }
            purposes.push(output.purpose);
            for source in &output.sources {
                validate_capture_source(source, &read_origins)?;
            }
        }
        if !self.outputs.iter().any(|output| output.required) {
            return Err(CaptureRecipeError::Invalid);
        }
        purposes.sort_by_key(|purpose| capture_purpose_rank(*purpose));
        if purposes.windows(2).any(|pair| pair[0] == pair[1])
            || serde_json::to_vec(self)
                .map_or(true, |encoded| encoded.len() > MAX_CAPTURE_RECIPE_BYTES)
        {
            return Err(CaptureRecipeError::Invalid);
        }
        Ok(())
    }
}

fn validate_capture_readiness(
    readiness: &CaptureReadiness,
    navigation_origins: &[String],
) -> Result<(), CaptureRecipeError> {
    let (origin, method, path_and_query) = match readiness {
        CaptureReadiness::OutputsComplete => return Ok(()),
        CaptureReadiness::RequestObserved {
            origin,
            method,
            path_and_query,
        }
        | CaptureReadiness::ResponseObserved {
            origin,
            method,
            path_and_query,
            ..
        } => (origin, method, path_and_query),
    };
    validate_source_origin(origin, navigation_origins)?;
    if !matches!(method.as_str(), "GET" | "POST")
        || path_and_query.is_empty()
        || path_and_query.len() > 2_048
        || !path_and_query.starts_with('/')
        || path_and_query.contains('#')
        || path_and_query
            .bytes()
            .any(|byte| byte.is_ascii_control() || byte == b' ')
    {
        return Err(CaptureRecipeError::Invalid);
    }
    let uri = format!("{origin}{path_and_query}")
        .parse::<Uri>()
        .map_err(|_| CaptureRecipeError::Invalid)?;
    if uri.scheme_str() != Some("https") || uri.authority().is_none() {
        return Err(CaptureRecipeError::Invalid);
    }
    if let CaptureReadiness::ResponseObserved {
        status, mime_type, ..
    } = readiness
        && (!(200..=299).contains(status) || !valid_capture_mime_type(mime_type))
    {
        return Err(CaptureRecipeError::Invalid);
    }
    Ok(())
}

fn valid_capture_mime_type(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.trim() == value
        && value.contains('/')
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || matches!(byte, b'/' | b'+' | b'.' | b'-')
        })
}

const fn capture_purpose_rank(purpose: SecretPurpose) -> u8 {
    match purpose {
        SecretPurpose::ProviderUsername => 0,
        SecretPurpose::ProviderPassword => 1,
        SecretPurpose::ProviderCookie => 2,
        SecretPurpose::ProviderAccessToken => 3,
        SecretPurpose::ProviderRefreshToken => 4,
        SecretPurpose::ProviderCompositeSession => 5,
        SecretPurpose::WebSessionToken
        | SecretPurpose::ServiceToken
        | SecretPurpose::IntegrationCredential
        | SecretPurpose::BrowserJobCredential => u8::MAX,
    }
}

fn validate_capture_source(
    source: &CaptureValueSource,
    allowed_origins: &[String],
) -> Result<(), CaptureRecipeError> {
    match source {
        CaptureValueSource::RequestHeader { origin, name } => {
            validate_source_origin(origin, allowed_origins)?;
            validate_header_name(name)
        }
        CaptureValueSource::LocalStorage { origin, key }
        | CaptureValueSource::SessionStorage { origin, key } => {
            validate_storage_source(origin, key, allowed_origins)
        }
        CaptureValueSource::CookieHeader { origin } => {
            validate_source_origin(origin, allowed_origins)
        }
        CaptureValueSource::JsonObject { fields } => {
            if fields.is_empty() || fields.len() > MAX_CAPTURE_JSON_FIELDS {
                return Err(CaptureRecipeError::Invalid);
            }
            let mut names = Vec::with_capacity(fields.len());
            for field in fields {
                if !valid_capture_label(&field.name)
                    || field.sources.is_empty()
                    || field.sources.len() > MAX_CAPTURE_SOURCES
                {
                    return Err(CaptureRecipeError::Invalid);
                }
                names.push(field.name.as_str());
                for scalar in &field.sources {
                    validate_scalar_source(scalar, allowed_origins)?;
                }
            }
            names.sort_unstable();
            if names.windows(2).any(|pair| pair[0] == pair[1]) {
                Err(CaptureRecipeError::Invalid)
            } else {
                Ok(())
            }
        }
    }
}

fn validate_scalar_source(
    source: &CaptureScalarSource,
    allowed_origins: &[String],
) -> Result<(), CaptureRecipeError> {
    match source {
        CaptureScalarSource::RequestHeader { origin, name } => {
            validate_source_origin(origin, allowed_origins)?;
            validate_header_name(name)
        }
        CaptureScalarSource::LocalStorage { origin, key }
        | CaptureScalarSource::SessionStorage { origin, key } => {
            validate_storage_source(origin, key, allowed_origins)
        }
    }
}

fn validate_storage_source(
    origin: &str,
    key: &str,
    allowed_origins: &[String],
) -> Result<(), CaptureRecipeError> {
    validate_source_origin(origin, allowed_origins)?;
    if valid_bounded_capture_text(key, 128) {
        Ok(())
    } else {
        Err(CaptureRecipeError::Invalid)
    }
}

fn validate_source_origin(
    origin: &str,
    allowed_origins: &[String],
) -> Result<(), CaptureRecipeError> {
    https_origin(origin, true)?;
    if allowed_origins.iter().any(|allowed| allowed == origin) {
        Ok(())
    } else {
        Err(CaptureRecipeError::Invalid)
    }
}

fn validate_header_name(name: &str) -> Result<(), CaptureRecipeError> {
    if !name.is_empty()
        && name.len() <= 128
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        Ok(())
    } else {
        Err(CaptureRecipeError::Invalid)
    }
}

fn valid_capture_label(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn valid_bounded_capture_text(value: &str, maximum: usize) -> bool {
    !value.is_empty()
        && value.len() <= maximum
        && value.trim() == value
        && !value.chars().any(char::is_control)
}

fn https_origin(value: &str, origin_only: bool) -> Result<String, CaptureRecipeError> {
    if !valid_bounded_capture_text(value, 2_048) {
        return Err(CaptureRecipeError::Invalid);
    }
    let uri = value
        .parse::<Uri>()
        .map_err(|_| CaptureRecipeError::Invalid)?;
    let authority = uri.authority().ok_or(CaptureRecipeError::Invalid)?;
    if uri.scheme_str() != Some("https") || authority.as_str().contains('@') {
        return Err(CaptureRecipeError::Invalid);
    }
    if origin_only && (uri.path() != "/" && !uri.path().is_empty() || uri.query().is_some()) {
        return Err(CaptureRecipeError::Invalid);
    }
    Ok(format!("https://{authority}"))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum CaptureRecipeError {
    #[error("Capture recipe is unsafe, unbounded, or internally inconsistent")]
    Invalid,
}

#[cfg(test)]
mod capture_recipe_tests {
    use super::*;

    fn recipe() -> CaptureRecipe {
        CaptureRecipe {
            version: 1,
            start_url: "https://provider.example/login".to_owned(),
            navigation_origins: vec!["https://provider.example".to_owned()],
            read_origins: vec!["https://provider.example".to_owned()],
            poll_interval_millis: 500,
            auth_method: AuthMethod::AssistedSession,
            session_kind: SessionKind::Composite,
            readiness: CaptureReadiness::OutputsComplete,
            outputs: vec![CaptureCredentialOutput {
                purpose: SecretPurpose::ProviderCompositeSession,
                required: true,
                sources: vec![CaptureValueSource::JsonObject {
                    fields: vec![CaptureJsonField {
                        name: "access_token".to_owned(),
                        sources: vec![CaptureScalarSource::RequestHeader {
                            origin: "https://provider.example".to_owned(),
                            name: "Authorization".to_owned(),
                        }],
                    }],
                }],
            }],
        }
    }

    #[test]
    fn recipe_accepts_only_bounded_declarative_origin_bound_sources() {
        assert_eq!(recipe().validate(), Ok(()));

        let mut foreign_header = recipe();
        let CaptureValueSource::JsonObject { fields } = &mut foreign_header.outputs[0].sources[0]
        else {
            unreachable!();
        };
        let CaptureScalarSource::RequestHeader { origin, .. } = &mut fields[0].sources[0] else {
            unreachable!();
        };
        *origin = "https://foreign.example".to_owned();
        assert_eq!(foreign_header.validate(), Err(CaptureRecipeError::Invalid));

        let mut non_canonical_origin = recipe();
        non_canonical_origin.navigation_origins[0].push('/');
        assert_eq!(
            non_canonical_origin.validate(),
            Err(CaptureRecipeError::Invalid)
        );

        let mut script_shaped_field = recipe();
        let CaptureValueSource::JsonObject { fields } =
            &mut script_shaped_field.outputs[0].sources[0]
        else {
            unreachable!();
        };
        fields[0].name = "token);eval(payload)".to_owned();
        assert_eq!(
            script_shaped_field.validate(),
            Err(CaptureRecipeError::Invalid)
        );
    }

    #[test]
    fn recipe_requires_unique_provider_outputs_and_one_required_value() {
        let mut duplicate = recipe();
        duplicate.outputs.push(duplicate.outputs[0].clone());
        assert_eq!(duplicate.validate(), Err(CaptureRecipeError::Invalid));

        let mut optional_only = recipe();
        optional_only.outputs[0].required = false;
        assert_eq!(optional_only.validate(), Err(CaptureRecipeError::Invalid));

        let mut non_provider_secret = recipe();
        non_provider_secret.outputs[0].purpose = SecretPurpose::BrowserJobCredential;
        assert_eq!(
            non_provider_secret.validate(),
            Err(CaptureRecipeError::Invalid)
        );
    }

    #[test]
    fn navigation_does_not_grant_secret_reads_and_readiness_is_exact() {
        let mut oauth = recipe();
        oauth
            .navigation_origins
            .push("https://oauth.example".to_owned());
        oauth.readiness = CaptureReadiness::RequestObserved {
            origin: "https://provider.example".to_owned(),
            method: "GET".to_owned(),
            path_and_query: "/api/account?action=current".to_owned(),
        };
        assert_eq!(oauth.validate(), Ok(()));

        oauth
            .read_origins
            .push("https://unlisted.example".to_owned());
        assert_eq!(oauth.validate(), Err(CaptureRecipeError::Invalid));
        oauth.read_origins.pop();
        oauth.readiness = CaptureReadiness::RequestObserved {
            origin: "https://oauth.example".to_owned(),
            method: "PATCH".to_owned(),
            path_and_query: "/callback".to_owned(),
        };
        assert_eq!(oauth.validate(), Err(CaptureRecipeError::Invalid));
        oauth.readiness = CaptureReadiness::RequestObserved {
            origin: "https://provider.example".to_owned(),
            method: "POST".to_owned(),
            path_and_query: "missing-leading-slash".to_owned(),
        };
        assert_eq!(oauth.validate(), Err(CaptureRecipeError::Invalid));

        oauth.readiness = CaptureReadiness::ResponseObserved {
            origin: "https://provider.example".to_owned(),
            method: "GET".to_owned(),
            path_and_query: "/api/account?action=current".to_owned(),
            status: 200,
            mime_type: "application/json".to_owned(),
        };
        assert_eq!(oauth.validate(), Ok(()));
        let CaptureReadiness::ResponseObserved { status, .. } = &mut oauth.readiness else {
            unreachable!();
        };
        *status = 302;
        assert_eq!(oauth.validate(), Err(CaptureRecipeError::Invalid));

        let CaptureReadiness::ResponseObserved {
            status, mime_type, ..
        } = &mut oauth.readiness
        else {
            unreachable!();
        };
        *status = 200;
        *mime_type = "Application/JSON".to_owned();
        assert_eq!(oauth.validate(), Err(CaptureRecipeError::Invalid));
    }
}

pub trait ProviderIdentity: Send + Sync {
    fn metadata(&self) -> &ProviderMetadata;
}

#[async_trait]
pub trait AuthenticationCapability: ProviderIdentity {
    /// Returns the legacy/default declarative Capture recipe advertised by
    /// metadata. New Providers with more than one valid acquisition route
    /// should override [`AuthenticationCapability::capture_recipes`] instead.
    fn capture_recipe(&self) -> Option<CaptureRecipe> {
        None
    }

    /// Returns ordered alternative Capture recipes. Every recipe is a
    /// complete atomic credential-bundle contract; Core freezes exactly one
    /// recipe version into each bootstrap session and never combines outputs
    /// between alternatives.
    fn capture_recipes(&self) -> Vec<CaptureRecipe> {
        self.capture_recipe().into_iter().collect()
    }

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

    /// Consumes one already Core-claimed external OAuth callback exactly once
    /// and returns Provider credential material for Core validation/storage.
    /// The default keeps the exchange opt-in even when a Provider advertises
    /// another interactive authentication method.
    async fn exchange_external_oauth_callback(
        &self,
        _context: &ProviderAuthContext,
        _callback_url: SecretString,
        _binding: ExternalOauthCallbackBinding,
    ) -> ProviderResult<CredentialReplacement> {
        Err(crate::ProviderError::new(
            crate::ProviderErrorKind::UnsupportedTask,
            "Provider does not implement an external OAuth callback exchange",
        ))
    }

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

/// Reads normalized learning duration without mutating remote state. This is
/// deliberately independent from progress and duration reporting.
#[async_trait]
pub trait DurationReadCapability: ProviderIdentity {
    async fn read_duration(
        &self,
        context: &ProviderContext,
        remote_task_id: &str,
    ) -> ProviderResult<RemoteDuration>;
}

#[async_trait]
pub trait QuestionInventoryCapability: ProviderIdentity {
    async fn list_question_refs(
        &self,
        context: &ProviderContext,
        remote_task_id: &str,
    ) -> ProviderResult<Vec<RemoteQuestionRef>>;
}

#[async_trait]
pub trait QuestionParseCapability: ProviderIdentity {
    async fn parse_question(
        &self,
        context: &ProviderContext,
        task_id: TaskId,
        remote_task_id: &str,
        question: &RemoteQuestionRef,
    ) -> ProviderResult<Question>;
}

/// Provider-native answer lookup kept separate from question parsing, draft
/// construction and every remote mutation capability. Non-Provider sources
/// such as manual input or external banks use their own Core-side contracts.
#[async_trait]
pub trait AnswerResolveCapability: ProviderIdentity {
    async fn resolve_answers(
        &self,
        context: &ProviderContext,
        remote_task_id: &str,
        questions: &[Question],
    ) -> ProviderResult<Vec<AnswerCandidate>>;
}

/// Builds a bounded, credential-free description of the payload shape for an
/// explicit set of selected persisted candidates. This capability must not
/// mutate remote state or return executable request material.
#[async_trait]
pub trait SubmissionBuildCapability: ProviderIdentity {
    async fn build_submission_preview(
        &self,
        context: &ProviderContext,
        remote_task_id: &str,
        questions: &[Question],
        selected_answers: &[SelectedAnswer],
    ) -> ProviderResult<SubmissionPayloadPreview>;
}

/// Performs only the remote mutation represented by one validated immutable
/// draft. Verification remains a separate capability and a receipt alone never
/// marks the Task complete.
#[async_trait]
pub trait SubmissionExecuteCapability: ProviderIdentity {
    async fn execute_submission(
        &self,
        context: &ProviderContext,
        remote_task_id: &str,
        draft: &SubmissionDraft,
        runtime_settings: &ResolvedProviderRuntimeSettings,
        events: &(dyn ExecutionEventSink + Send + Sync),
    ) -> ProviderResult<SubmissionReceipt>;
}

/// Re-reads remote state after submission and returns bounded verification
/// facts. It must not issue or repeat the submission mutation.
#[async_trait]
pub trait SubmissionVerifyCapability: ProviderIdentity {
    async fn verify_submission(
        &self,
        context: &ProviderContext,
        remote_task_id: &str,
        draft: &SubmissionDraft,
        receipt: Option<&SubmissionReceipt>,
    ) -> ProviderResult<SubmissionVerificationSnapshot>;
}

#[async_trait]
pub trait TaskExecutionCapability: ProviderIdentity {
    /// Declares an evidenced Provider-specific exception to Core's ordinary
    /// executable remote states. The default grants no exception. Providers
    /// may opt into a state such as `NotOpen` only for an exact action set
    /// whose audited protocol still accepts that mutation; Core continues to
    /// reject terminally unavailable `Expired` and `Removed` tasks.
    fn allows_execution_from_remote_state(
        &self,
        _requested_capabilities: &[TaskCapability],
        _remote_state: RemoteState,
    ) -> bool {
        false
    }

    /// Returns the exact durable order for a requested executable capability
    /// set. Core validates that the plan is a permutation of the request and
    /// persists every phase before scheduling. Providers must explicitly opt
    /// into multi-capability semantics rather than inheriting an arbitrary
    /// enum or caller order.
    ///
    /// # Errors
    ///
    /// Returns a typed unsupported-task error unless the Provider explicitly
    /// defines this exact multi-capability combination.
    fn execution_plan(
        &self,
        requested_capabilities: &[TaskCapability],
    ) -> ProviderResult<Vec<TaskCapability>> {
        if requested_capabilities.len() == 1 {
            Ok(requested_capabilities.to_vec())
        } else {
            Err(crate::ProviderError::new(
                crate::ProviderErrorKind::UnsupportedTask,
                "Provider does not define a multi-capability execution plan",
            ))
        }
    }

    /// Declares whether this exact selected action set requires the Provider's
    /// goal-bound, read-only verification path. Task-level `ExecutionVerify`
    /// metadata only advertises that at least one action supports this path;
    /// this method binds the requirement to the frozen Execution selection.
    fn requires_execution_verification(&self, _requested_capabilities: &[TaskCapability]) -> bool {
        true
    }

    async fn execute(
        &self,
        context: &ProviderContext,
        request: &ExecutionRequest,
        events: &(dyn ExecutionEventSink + Send + Sync),
    ) -> ProviderResult<ExecutionOutcome>;

    /// Rebinds the same frozen execution goal and reads enough fresh remote
    /// state to verify it without repeating any mutation. Providers which
    /// advertise `ExecutionVerify` must override this method.
    async fn verify_execution(
        &self,
        _context: &ProviderContext,
        _request: &ExecutionRequest,
    ) -> ProviderResult<ExecutionOutcome> {
        Err(crate::ProviderError::new(
            crate::ProviderErrorKind::UnsupportedTask,
            "Provider does not implement goal-bound execution verification",
        ))
    }
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

#[derive(Clone, Copy, Default, Eq, PartialEq)]
pub struct ExternalOauthCallbackBinding {
    state_digest: [u8; 32],
    provider_context_digest: [u8; 32],
}

impl ExternalOauthCallbackBinding {
    pub const fn from_digests(state_digest: [u8; 32], provider_context_digest: [u8; 32]) -> Self {
        Self {
            state_digest,
            provider_context_digest,
        }
    }

    pub const fn state_digest(self) -> [u8; 32] {
        self.state_digest
    }

    pub const fn provider_context_digest(self) -> [u8; 32] {
        self.provider_context_digest
    }

    pub fn validate(self) -> bool {
        self.state_digest != [0; 32]
            && self.provider_context_digest != [0; 32]
            && self.state_digest != self.provider_context_digest
    }
}

impl fmt::Debug for ExternalOauthCallbackBinding {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ExternalOauthCallbackBinding([HASHED])")
    }
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct ExternalOauthAuthorization {
    pub authorization_url: String,
    #[serde(skip)]
    pub callback_binding: ExternalOauthCallbackBinding,
}

impl ExternalOauthAuthorization {
    pub fn validate(&self) -> bool {
        if self.authorization_url.is_empty()
            || self.authorization_url.len() > 4_096
            || self.authorization_url.trim() != self.authorization_url
            || self.authorization_url.chars().any(char::is_control)
            || !self.callback_binding.validate()
        {
            return false;
        }
        let Ok(uri) = self.authorization_url.parse::<Uri>() else {
            return false;
        };
        uri.scheme_str() == Some("https")
            && uri
                .authority()
                .is_some_and(|authority| !authority.as_str().contains('@'))
    }
}

impl fmt::Debug for ExternalOauthAuthorization {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ExternalOauthAuthorization")
            .field("authorization_url", &"[REDACTED]")
            .field("callback_binding", &self.callback_binding)
            .finish()
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AuthChallenge {
    pub session_id: AuthSessionId,
    pub method: AuthMethod,
    pub waiting_for: WaitingUserState,
    pub user_action: Option<String>,
    pub expires_at: Option<Timestamp>,
    pub external_oauth: Option<ExternalOauthAuthorization>,
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

/// Bounded question identity discovered for one fresh task attempt. Ephemeral
/// route facts can be consumed by the same Provider during parsing but never
/// serialize across the Core boundary.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct RemoteQuestionRef {
    pub remote_id: String,
    pub position: u32,
    pub kind_hint: QuestionKind,
    pub metadata_sanitized: serde_json::Value,
    #[serde(skip)]
    pub route_context: ProviderRouteContext,
}

impl RemoteQuestionRef {
    /// Validates the bounded, sanitized question-discovery contract while
    /// leaving ephemeral route context opaque and non-serialized.
    ///
    /// # Errors
    ///
    /// Returns [`RemoteQuestionRefError`] for malformed identity/position or
    /// oversized, credential-shaped metadata.
    pub fn validate(&self) -> Result<(), RemoteQuestionRefError> {
        if self.remote_id.is_empty()
            || self.remote_id.len() > MAX_REMOTE_QUESTION_ID_BYTES
            || self.remote_id.trim() != self.remote_id
            || self.remote_id.chars().any(char::is_control)
            || self.position == 0
            || self.position > MAX_QUESTION_POSITION
            || serde_json::to_vec(&self.metadata_sanitized).map_or(true, |encoded| {
                encoded.len() > MAX_QUESTION_REF_METADATA_BYTES
            })
            || contains_secret_key(&self.metadata_sanitized)
        {
            Err(RemoteQuestionRefError::Invalid)
        } else {
            Ok(())
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RemoteProgress {
    pub remote_state: RemoteState,
    pub percent: Option<u8>,
    pub duration_seconds: Option<u64>,
    pub updated_at: Timestamp,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RemoteDuration {
    pub duration_seconds: u64,
    pub updated_at: Timestamp,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ExecutionRequest {
    /// Stable Core-owned identity for this immutable execution intent. A
    /// Provider may derive deterministic per-execution choices from it; retry
    /// and recovery retain the same value while a later execution gets a new
    /// identity.
    pub execution_id: ExecutionId,
    pub task_id: TaskId,
    pub remote_task_id: String,
    pub course_id: Option<CourseId>,
    /// Exact capability set authorized for this Provider call. Composite
    /// executions deliberately narrow this to the current mutation step so a
    /// Provider cannot use plan context as authority to issue a later step.
    pub requested_capabilities: Vec<TaskCapability>,
    /// Immutable, Core-validated capability plan persisted with the parent
    /// Execution. This preserves donor step semantics across retry/recovery
    /// without broadening `requested_capabilities`.
    pub capability_plan: Vec<TaskCapability>,
    /// One-based position of the active Provider call in `capability_plan`.
    pub capability_step_position: u8,
    /// Immutable Core-resolved settings captured when this Execution was
    /// scheduled. Retries receive the same versioned values.
    pub runtime_settings: ResolvedProviderRuntimeSettings,
}

impl ExecutionRequest {
    /// Validates that active mutation authority is an exact, bounded slice of
    /// the immutable execution plan and that the current step is unambiguous.
    pub fn has_valid_capability_step(&self) -> bool {
        !self.requested_capabilities.is_empty()
            && !self.capability_plan.is_empty()
            && self.capability_plan.len() <= 5
            && usize::from(self.capability_step_position) <= self.capability_plan.len()
            && self.capability_step_position != 0
            && (self.capability_plan.len() == 1
                && self.requested_capabilities == self.capability_plan
                || self.requested_capabilities.len() == 1
                    && self.capability_plan[usize::from(self.capability_step_position) - 1]
                        == self.requested_capabilities[0])
    }
}

#[cfg(test)]
mod execution_request_tests {
    use super::*;

    fn request() -> ExecutionRequest {
        ExecutionRequest {
            execution_id: ExecutionId::new(),
            task_id: TaskId::new(),
            remote_task_id: "remote-task".to_owned(),
            course_id: None,
            requested_capabilities: vec![TaskCapability::ResourceExecution],
            capability_plan: vec![
                TaskCapability::DurationReport,
                TaskCapability::ResourceExecution,
            ],
            capability_step_position: 2,
            runtime_settings: ResolvedProviderRuntimeSettings {
                schema_version: 1,
                values: BTreeMap::new(),
            },
        }
    }

    #[test]
    fn active_step_is_bound_to_the_immutable_plan_without_broadening_authority() {
        assert!(request().has_valid_capability_step());

        let mut wrong_position = request();
        wrong_position.capability_step_position = 1;
        assert!(!wrong_position.has_valid_capability_step());

        let mut broad_authority = request();
        broad_authority.requested_capabilities = broad_authority.capability_plan.clone();
        assert!(!broad_authority.has_valid_capability_step());

        let mut missing_plan = request();
        missing_plan.capability_plan.clear();
        assert!(!missing_plan.has_valid_capability_step());
    }
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

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum RemoteQuestionRefError {
    #[error("remote question reference is invalid, oversized, or not sanitized")]
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

#[cfg(test)]
mod execution_outcome_tests {
    use super::*;

    #[test]
    fn execution_outcome_is_bounded_and_rejects_secret_keys() {
        let valid = ExecutionOutcome {
            remote_state: RemoteState::Completed,
            verified: true,
            result_sanitized: serde_json::json!({"completed": true, "score": 100}),
        };
        assert_eq!(valid.validate(), Ok(()));

        let secret = ExecutionOutcome {
            result_sanitized: serde_json::json!({"nested": {"access_token": "secret"}}),
            ..valid.clone()
        };
        assert_eq!(secret.validate(), Err(ExecutionOutcomeError::Invalid));

        let oversized = ExecutionOutcome {
            result_sanitized: serde_json::json!({"result": "x".repeat(65 * 1_024)}),
            ..valid
        };
        assert_eq!(oversized.validate(), Err(ExecutionOutcomeError::Invalid));
    }
}

#[cfg(test)]
mod question_ref_tests {
    use super::*;

    fn question_ref() -> RemoteQuestionRef {
        RemoteQuestionRef {
            remote_id: "question-1".to_owned(),
            position: 1,
            kind_hint: QuestionKind::SingleChoice,
            metadata_sanitized: serde_json::json!({"provider_kind": "single"}),
            route_context: ProviderRouteContext::try_from_pairs([(
                "chaoxing.question-route".to_owned(),
                "ephemeral-value".to_owned(),
            )])
            .unwrap(),
        }
    }

    #[test]
    fn remote_question_reference_is_bounded_sanitized_and_hides_routes() {
        let reference = question_ref();
        assert_eq!(reference.validate(), Ok(()));
        let encoded = serde_json::to_string(&reference).unwrap();
        assert!(!encoded.contains("ephemeral-value"));

        let mut secret = question_ref();
        secret.metadata_sanitized = serde_json::json!({"session_secret": "forbidden"});
        assert_eq!(secret.validate(), Err(RemoteQuestionRefError::Invalid));

        let mut invalid_position = question_ref();
        invalid_position.position = 0;
        assert_eq!(
            invalid_position.validate(),
            Err(RemoteQuestionRefError::Invalid)
        );
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ExecutionOutcome {
    pub remote_state: RemoteState,
    pub verified: bool,
    pub result_sanitized: serde_json::Value,
}

impl ExecutionOutcome {
    /// Validates bounded, credential-free execution/verification facts.
    ///
    /// # Errors
    ///
    /// Returns [`ExecutionOutcomeError::Invalid`] when the sanitized result is
    /// oversized or contains credential-shaped keys.
    pub fn validate(&self) -> Result<(), ExecutionOutcomeError> {
        if serde_json::to_vec(&self.result_sanitized)
            .map_or(true, |encoded| encoded.len() > 64 * 1_024)
            || contains_secret_key(&self.result_sanitized)
        {
            Err(ExecutionOutcomeError::Invalid)
        } else {
            Ok(())
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ExecutionOutcomeError {
    #[error("Provider execution outcome is oversized or not sanitized")]
    Invalid,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BrowserSessionSpec {
    /// Provider-owned wire revision for the browser policy represented by this
    /// immutable snapshot.
    pub version: u32,
    pub isolation_key: String,
    pub allowed_origins: Vec<String>,
    pub headless: bool,
}

impl BrowserSessionSpec {
    /// Validates a credential-free, bounded and exact browser-session policy.
    /// An allowed origin grants navigation authority only; credential
    /// injection remains a separate Core-owned contract.
    ///
    /// # Errors
    ///
    /// Returns [`BrowserSessionSpecError::Invalid`] for a zero revision,
    /// malformed isolation key, unsafe origin or duplicate/unbounded origin
    /// set.
    pub fn validate(&self) -> Result<(), BrowserSessionSpecError> {
        if self.version == 0
            || self.isolation_key.is_empty()
            || self.isolation_key.len() > 128
            || self.isolation_key.trim() != self.isolation_key
            || !self.isolation_key.bytes().all(|byte| {
                byte.is_ascii_lowercase()
                    || byte.is_ascii_digit()
                    || matches!(byte, b'-' | b'_' | b'.')
            })
            || self.allowed_origins.is_empty()
            || self.allowed_origins.len() > MAX_CAPTURE_ORIGINS
        {
            return Err(BrowserSessionSpecError::Invalid);
        }
        let mut origins = self.allowed_origins.clone();
        for origin in &origins {
            if https_origin(origin, true).map_err(|_| BrowserSessionSpecError::Invalid)? != *origin
            {
                return Err(BrowserSessionSpecError::Invalid);
            }
        }
        origins.sort_unstable();
        if origins.windows(2).any(|pair| pair[0] == pair[1])
            || serde_json::to_vec(self).map_or(true, |encoded| encoded.len() > 4 * 1_024)
        {
            Err(BrowserSessionSpecError::Invalid)
        } else {
            Ok(())
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum BrowserSessionSpecError {
    #[error("Browser session specification is unsafe, unbounded, or internally inconsistent")]
    Invalid,
}

#[cfg(test)]
mod browser_session_spec_tests {
    use super::*;

    fn spec() -> BrowserSessionSpec {
        BrowserSessionSpec {
            version: 1,
            isolation_key: "provider-task-a1".to_owned(),
            allowed_origins: vec!["https://provider.example".to_owned()],
            headless: false,
        }
    }

    #[test]
    fn exact_https_origins_and_bounded_isolation_are_required() {
        assert_eq!(spec().validate(), Ok(()));

        let mut duplicate = spec();
        duplicate
            .allowed_origins
            .push("https://provider.example".to_owned());
        assert_eq!(duplicate.validate(), Err(BrowserSessionSpecError::Invalid));

        let mut route = spec();
        route.allowed_origins[0].push_str("/task");
        assert_eq!(route.validate(), Err(BrowserSessionSpecError::Invalid));

        let mut secret_shaped = spec();
        secret_shaped.isolation_key = "Provider:token".to_owned();
        assert_eq!(
            secret_shaped.validate(),
            Err(BrowserSessionSpecError::Invalid)
        );
    }
}
