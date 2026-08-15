use std::{collections::BTreeMap, fmt};

use asterism_domain::{
    AnswerCandidate, AssessmentClass, AuthMethod, AuthSessionId, BrowserBridgeExchange,
    BrowserBridgeExchangeState, BrowserBridgeResultArtifactMetadata, BrowserBridgeRuntimeBinding,
    BrowserBridgeRuntimeStateMetadata, CourseId, ExecutionId, LogLevel, ProviderAccountId,
    ProviderId, Question, QuestionKind, RemoteState, SecretId, SelectedAnswer, SessionKind,
    SourceType, SubmissionDraft, SubmissionPayloadPreview, SubmissionReceipt,
    SubmissionVerificationSnapshot, TaskCapability, TaskId, Timestamp, WaitingUserState,
};
use asterism_secrets::{
    CredentialBundle, CredentialField, SecretPurpose, SecretString, SecretValue,
};
use async_trait::async_trait;
use http::Uri;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
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
const MAX_QUESTION_READ_LABEL_BYTES: usize = 96;
const MAX_QUESTION_READ_ITEMS: usize = 5_000;
const MAX_QUESTION_READ_TTL_SECONDS: u64 = 24 * 60 * 60;
const MAX_INTERACTIVE_AUTH_CONTINUATION_BYTES: usize = 1024 * 1024;
const MAX_INTERACTIVE_AUTH_TTL_SECONDS: u64 = 60 * 60;
const MAX_INTERACTIVE_AUTH_POLLS: u32 = 10_000;

fn valid_provider_label(provider_id: &ProviderId, value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_QUESTION_READ_LABEL_BYTES
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_' | b'.')
        })
        && value
            .strip_prefix(provider_id.as_str())
            .is_some_and(|suffix| suffix.starts_with('.') && suffix.len() > 1)
}

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
    /// Declares that Provider-native interactive methods use Core's encrypted
    /// continuation and serialized poll lifecycle.
    fn supports_durable_interactive_authentication(&self) -> bool {
        false
    }

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

    /// Starts one Provider-native interactive authentication flow and returns
    /// the private state Core must encrypt before exposing the challenge.
    async fn begin_interactive_authentication(
        &self,
        _context: &ProviderAuthContext,
        _method: AuthMethod,
    ) -> ProviderResult<ProviderInteractiveAuthBegin> {
        Err(crate::ProviderError::new(
            crate::ProviderErrorKind::UnsupportedTask,
            "Provider does not implement durable interactive authentication",
        ))
    }

    /// Performs exactly one poll against a Core-resolved continuation. Core
    /// serializes calls and persists the returned replacement before another
    /// poll can be issued.
    async fn poll_interactive_authentication(
        &self,
        _context: &ProviderAuthContext,
        _continuation: ResolvedProviderInteractiveAuthContinuation<'_>,
    ) -> ProviderResult<ProviderInteractiveAuthPollOutcome> {
        Err(crate::ProviderError::new(
            crate::ProviderErrorKind::UnsupportedTask,
            "Provider does not implement durable interactive authentication polling",
        ))
    }

    /// Deterministically converts a persisted authenticated continuation into
    /// a candidate credential bundle. Implementations must not repeat the
    /// interactive remote exchange here.
    async fn finalize_interactive_authentication(
        &self,
        _context: &ProviderAuthContext,
        _continuation: ResolvedProviderInteractiveAuthContinuation<'_>,
    ) -> ProviderResult<CredentialBundle> {
        Err(crate::ProviderError::new(
            crate::ProviderErrorKind::UnsupportedTask,
            "Provider does not implement interactive authentication finalization",
        ))
    }

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

/// Provider-private encrypted state handed directly to Core's durable
/// pre-Question continuation store. The plaintext is redacted and zeroized by
/// `SecretValue`; only its bounded type, digest, phase, and expiry cross the
/// public contract.
pub struct ProviderQuestionReadContinuation {
    continuation_type: String,
    continuation_digest: [u8; 32],
    phase: String,
    value: SecretValue,
    ttl_seconds: u64,
}

impl ProviderQuestionReadContinuation {
    /// Creates a bounded Provider-scoped continuation and derives the digest
    /// from the exact plaintext bytes Core will encrypt.
    ///
    /// # Errors
    ///
    /// Rejects empty, oversized, foreign, or malformed labels and TTLs.
    pub fn try_new(
        provider_id: &ProviderId,
        continuation_type: impl Into<String>,
        phase: impl Into<String>,
        value: SecretValue,
        ttl_seconds: u64,
    ) -> ProviderResult<Self> {
        let continuation_type = continuation_type.into();
        let phase = phase.into();
        if !valid_provider_label(provider_id, &continuation_type)
            || !valid_provider_label(provider_id, &phase)
            || value.expose_secret().is_empty()
            || ttl_seconds == 0
            || ttl_seconds > MAX_QUESTION_READ_TTL_SECONDS
        {
            return Err(crate::ProviderError::new(
                crate::ProviderErrorKind::InvalidResponse,
                "Provider Question continuation metadata is invalid",
            ));
        }
        let continuation_digest = Sha256::digest(value.expose_secret()).into();
        Ok(Self {
            continuation_type,
            continuation_digest,
            phase,
            value,
            ttl_seconds,
        })
    }

    pub fn continuation_type(&self) -> &str {
        &self.continuation_type
    }

    pub const fn continuation_digest(&self) -> [u8; 32] {
        self.continuation_digest
    }

    pub fn phase(&self) -> &str {
        &self.phase
    }

    pub const fn ttl_seconds(&self) -> u64 {
        self.ttl_seconds
    }

    pub fn into_parts(self) -> (String, [u8; 32], String, SecretValue, u64) {
        (
            self.continuation_type,
            self.continuation_digest,
            self.phase,
            self.value,
            self.ttl_seconds,
        )
    }
}

impl fmt::Debug for ProviderQuestionReadContinuation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProviderQuestionReadContinuation")
            .field("continuation_type", &self.continuation_type)
            .field("continuation_digest", &self.continuation_digest)
            .field("phase", &self.phase)
            .field("value", &"[REDACTED]")
            .field("ttl_seconds", &self.ttl_seconds)
            .finish()
    }
}

/// Borrowed, already authenticated continuation resolved by Core for one
/// owner/account/Task-bound attempt.
pub struct ResolvedProviderQuestionReadContinuation<'a> {
    pub continuation_type: &'a str,
    pub continuation_digest: [u8; 32],
    pub phase: &'a str,
    pub revision: u32,
    pub value: &'a SecretValue,
}

/// Provider-private state attached to a claimed `QuestionSession`. This is the
/// same encrypted continuation shape used while materializing Questions, now
/// named for post-materialization Answer/Save/Submit operations.
pub type ResolvedProviderQuestionSessionContinuation<'a> =
    ResolvedProviderQuestionReadContinuation<'a>;

/// Exact last issued post-materialization operation retained for read-only
/// ambiguity recovery.
pub type AmbiguousProviderQuestionSessionOperation = AmbiguousProviderQuestionReadOperation;

impl fmt::Debug for ResolvedProviderQuestionReadContinuation<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ResolvedProviderQuestionReadContinuation")
            .field("continuation_type", &self.continuation_type)
            .field("continuation_digest", &self.continuation_digest)
            .field("phase", &self.phase)
            .field("revision", &self.revision)
            .field("value", &"[REDACTED]")
            .finish()
    }
}

/// Exact last issued request retained for fresh, read-only ambiguity recovery.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AmbiguousProviderQuestionReadOperation {
    pub continuation_revision: u32,
    pub operation_type: String,
    pub request_digest: [u8; 32],
    pub issued_at: Timestamp,
    pub ambiguous_at: Timestamp,
}

/// One real immutable Question set and the encrypted Provider material needed
/// to continue or submit it. Core persists the snapshot, `QuestionSession`, and
/// artifact atomically with the accepted pre-Question operation.
pub struct ProviderQuestionMaterialization {
    questions: Vec<Question>,
    artifact: ProviderQuestionReadContinuation,
    response_digest: [u8; 32],
    received_at: Timestamp,
}

impl ProviderQuestionMaterialization {
    /// # Errors
    ///
    /// Rejects empty, oversized, invalid, duplicate-position, or zero-digest
    /// materializations.
    pub fn try_new(
        questions: Vec<Question>,
        artifact: ProviderQuestionReadContinuation,
        response_digest: [u8; 32],
        received_at: Timestamp,
    ) -> ProviderResult<Self> {
        let mut positions = std::collections::BTreeSet::new();
        let mut remote_ids = std::collections::BTreeSet::new();
        if questions.is_empty()
            || questions.len() > MAX_QUESTION_READ_ITEMS
            || response_digest == [0; 32]
            || questions.iter().any(|question| {
                question.validate().is_err()
                    || !positions.insert(question.position)
                    || question
                        .remote_question_id
                        .as_ref()
                        .is_some_and(|remote_id| !remote_ids.insert(remote_id.as_str()))
            })
        {
            return Err(crate::ProviderError::new(
                crate::ProviderErrorKind::InvalidResponse,
                "Provider Question materialization is invalid",
            ));
        }
        Ok(Self {
            questions,
            artifact,
            response_digest,
            received_at,
        })
    }

    pub fn questions(&self) -> &[Question] {
        &self.questions
    }

    pub fn artifact(&self) -> &ProviderQuestionReadContinuation {
        &self.artifact
    }

    pub const fn response_digest(&self) -> [u8; 32] {
        self.response_digest
    }

    pub const fn received_at(&self) -> Timestamp {
        self.received_at
    }

    pub fn into_parts(
        self,
    ) -> (
        Vec<Question>,
        ProviderQuestionReadContinuation,
        [u8; 32],
        Timestamp,
    ) {
        (
            self.questions,
            self.artifact,
            self.response_digest,
            self.received_at,
        )
    }
}

impl fmt::Debug for ProviderQuestionMaterialization {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProviderQuestionMaterialization")
            .field("question_count", &self.questions.len())
            .field("artifact", &self.artifact)
            .field("response_digest", &self.response_digest)
            .field("received_at", &self.received_at)
            .finish()
    }
}

#[derive(Debug)]
pub enum ProviderQuestionReadStepOutcome {
    Continue {
        continuation: ProviderQuestionReadContinuation,
        response_digest: [u8; 32],
        received_at: Timestamp,
    },
    Materialize(ProviderQuestionMaterialization),
    Completed {
        receipt: SubmissionReceipt,
        response_digest: [u8; 32],
    },
}

impl ProviderQuestionReadStepOutcome {
    /// # Errors
    ///
    /// Rejects a continuation outcome without an exact raw response digest.
    pub fn continuing(
        continuation: ProviderQuestionReadContinuation,
        response_digest: [u8; 32],
        received_at: Timestamp,
    ) -> ProviderResult<Self> {
        if response_digest == [0; 32] {
            return Err(crate::ProviderError::new(
                crate::ProviderErrorKind::InvalidResponse,
                "Provider Question continuation response digest is empty",
            ));
        }
        Ok(Self::Continue {
            continuation,
            response_digest,
            received_at,
        })
    }

    /// Creates a definite successful terminal result for a Provider that
    /// reports completion before yielding any real Question.
    ///
    /// # Errors
    ///
    /// Rejects invalid receipts or a missing raw response digest.
    pub fn completed(
        receipt: SubmissionReceipt,
        response_digest: [u8; 32],
    ) -> ProviderResult<Self> {
        if response_digest == [0; 32] || receipt.validate().is_err() {
            return Err(crate::ProviderError::new(
                crate::ProviderErrorKind::InvalidResponse,
                "Provider pre-Question completion receipt is invalid",
            ));
        }
        Ok(Self::Completed {
            receipt,
            response_digest,
        })
    }
}

/// One opaque, in-memory Provider command whose exact identity is exposed to
/// Core before any remote mutation can occur. Implementations consume the
/// command on execute, preventing accidental in-process replay.
#[async_trait]
pub trait PreparedProviderQuestionReadOperation: fmt::Debug + Send {
    fn operation_type(&self) -> &str;
    fn request_digest(&self) -> [u8; 32];
    fn delay_before_execute_seconds(&self) -> u64;

    async fn execute(
        self: Box<Self>,
        context: &ProviderContext,
    ) -> ProviderResult<ProviderQuestionReadStepOutcome>;
}

#[async_trait]
pub trait QuestionInventoryCapability: ProviderIdentity {
    async fn list_question_refs(
        &self,
        context: &ProviderContext,
        remote_task_id: &str,
    ) -> ProviderResult<Vec<RemoteQuestionRef>>;

    /// Produces the initial encrypted state for Providers whose first real
    /// Question requires one or more non-idempotent operations. `None` selects
    /// the ordinary read-only inventory/parse pipeline.
    async fn prepare_question_read_attempt(
        &self,
        _context: &ProviderContext,
        _task_id: TaskId,
        _remote_task_id: &str,
        _runtime_settings: &ResolvedProviderRuntimeSettings,
    ) -> ProviderResult<Option<ProviderQuestionReadContinuation>> {
        Ok(None)
    }

    /// Rebinds one encrypted phase to fresh account/Task state and freezes the
    /// exact next command. Core records its identity before calling `execute`.
    async fn prepare_question_read_operation(
        &self,
        _context: &ProviderContext,
        _task_id: TaskId,
        _remote_task_id: &str,
        _continuation: ResolvedProviderQuestionReadContinuation<'_>,
        _runtime_settings: &ResolvedProviderRuntimeSettings,
    ) -> ProviderResult<Box<dyn PreparedProviderQuestionReadOperation>> {
        Err(crate::ProviderError::new(
            crate::ProviderErrorKind::UnsupportedTask,
            "Provider does not implement a pre-Question operation flow",
        ))
    }

    /// Performs only fresh readback for an ambiguous operation. Returning
    /// `None` keeps the attempt locked for manual/live recovery.
    async fn recover_ambiguous_question_read_operation(
        &self,
        _context: &ProviderContext,
        _task_id: TaskId,
        _remote_task_id: &str,
        _continuation: ResolvedProviderQuestionReadContinuation<'_>,
        _operation: &AmbiguousProviderQuestionReadOperation,
        _runtime_settings: &ResolvedProviderRuntimeSettings,
    ) -> ProviderResult<Option<ProviderQuestionReadStepOutcome>> {
        Ok(None)
    }
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

    /// Parses one complete read-only inventory and optionally returns one
    /// encrypted Provider continuation bound to the resulting immutable
    /// Question set. Providers without runtime artifacts inherit the ordinary
    /// per-reference parser path.
    async fn parse_question_set(
        &self,
        context: &ProviderContext,
        task_id: TaskId,
        remote_task_id: &str,
        references: &[RemoteQuestionRef],
    ) -> ProviderResult<ProviderQuestionParseSet> {
        let mut questions = Vec::with_capacity(references.len());
        for reference in references {
            questions.push(
                self.parse_question(context, task_id, remote_task_id, reference)
                    .await?,
            );
        }
        ProviderQuestionParseSet::try_new(questions, None)
    }
}

/// Complete output of the ordinary read-only Question parser. The optional
/// continuation is encrypted by Core and never enters a normalized Question,
/// Draft, API response or diagnostic output.
pub struct ProviderQuestionParseSet {
    questions: Vec<Question>,
    artifact: Option<ProviderQuestionReadContinuation>,
}

impl ProviderQuestionParseSet {
    /// # Errors
    ///
    /// Rejects empty, oversized, invalid or duplicate Question output.
    pub fn try_new(
        questions: Vec<Question>,
        artifact: Option<ProviderQuestionReadContinuation>,
    ) -> ProviderResult<Self> {
        let mut ids = std::collections::BTreeSet::new();
        let mut positions = std::collections::BTreeSet::new();
        let mut remote_ids = std::collections::BTreeSet::new();
        if questions.is_empty()
            || questions.len() > MAX_QUESTION_READ_ITEMS
            || questions.iter().any(|question| {
                question.validate().is_err()
                    || !ids.insert(question.id)
                    || !positions.insert(question.position)
                    || question
                        .remote_question_id
                        .as_ref()
                        .is_some_and(|remote_id| !remote_ids.insert(remote_id.as_str()))
            })
        {
            return Err(crate::ProviderError::new(
                crate::ProviderErrorKind::InvalidResponse,
                "Provider Question parse set is invalid",
            ));
        }
        Ok(Self {
            questions,
            artifact,
        })
    }

    pub fn questions(&self) -> &[Question] {
        &self.questions
    }

    pub fn artifact(&self) -> Option<&ProviderQuestionReadContinuation> {
        self.artifact.as_ref()
    }

    pub fn into_parts(self) -> (Vec<Question>, Option<ProviderQuestionReadContinuation>) {
        (self.questions, self.artifact)
    }
}

impl fmt::Debug for ProviderQuestionParseSet {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProviderQuestionParseSet")
            .field("question_count", &self.questions.len())
            .field("artifact", &self.artifact.as_ref().map(|_| "[REDACTED]"))
            .finish()
    }
}

#[cfg(test)]
mod question_read_flow_tests {
    use asterism_domain::{QuestionId, QuestionKind};

    use super::*;

    #[test]
    fn continuation_is_provider_scoped_digest_bound_and_debug_redacted() {
        let provider_id = ProviderId::new("cidaren").unwrap();
        let continuation = ProviderQuestionReadContinuation::try_new(
            &provider_id,
            "cidaren.pre-question.v1",
            "cidaren.ready-to-start",
            SecretValue::new(b"one-time-topic-code".to_vec()),
            300,
        )
        .unwrap();
        let expected_digest: [u8; 32] = Sha256::digest(b"one-time-topic-code").into();
        assert_eq!(continuation.continuation_digest(), expected_digest);
        let debug = format!("{continuation:?}");
        assert!(debug.contains("[REDACTED]"));
        assert!(!debug.contains("one-time-topic-code"));
        assert!(
            ProviderQuestionReadContinuation::try_new(
                &provider_id,
                "uai.question.v1",
                "cidaren.ready",
                SecretValue::new(vec![1]),
                300,
            )
            .is_err()
        );
    }

    #[test]
    fn materialization_requires_real_unique_questions_and_response_digest() {
        let provider_id = ProviderId::new("chaoxing").unwrap();
        let question = Question {
            id: QuestionId::new(),
            task_id: TaskId::new(),
            remote_question_id: Some("exam-question-1".to_owned()),
            kind: QuestionKind::SingleChoice,
            stem: "Bounded stem".to_owned(),
            options: Vec::new(),
            attachments: Vec::new(),
            metadata_sanitized: serde_json::json!({}),
            position: 1,
        };
        let artifact = || {
            ProviderQuestionReadContinuation::try_new(
                &provider_id,
                "chaoxing.exam-attempt.v1",
                "chaoxing.questions-ready",
                SecretValue::new(b"exam-state".to_vec()),
                600,
            )
            .unwrap()
        };
        assert!(
            ProviderQuestionMaterialization::try_new(
                vec![question.clone()],
                artifact(),
                [7; 32],
                chrono::Utc::now(),
            )
            .is_ok()
        );
        assert!(
            ProviderQuestionMaterialization::try_new(
                vec![question.clone(), question],
                artifact(),
                [7; 32],
                chrono::Utc::now(),
            )
            .is_err()
        );
    }

    #[test]
    fn completion_before_first_question_keeps_receipt_distinct_from_materialization() {
        let received_at = chrono::Utc::now();
        let receipt = SubmissionReceipt {
            remote_status: "completed".to_owned(),
            message_sanitized: Some("already complete".to_owned()),
            provider_trace_id: None,
            received_at,
        };
        assert!(matches!(
            ProviderQuestionReadStepOutcome::completed(receipt, [9; 32]).unwrap(),
            ProviderQuestionReadStepOutcome::Completed {
                response_digest,
                ..
            } if response_digest == [9; 32]
        ));
    }

    #[test]
    fn durable_submission_steps_require_rotated_state_and_response_evidence() {
        let provider_id = ProviderId::new("chaoxing").unwrap();
        let continuation = || {
            ProviderQuestionReadContinuation::try_new(
                &provider_id,
                "chaoxing.exam-question-attempt.v1",
                "chaoxing.exam-answer-saved",
                SecretValue::new(b"rotated-exam-state".to_vec()),
                600,
            )
            .unwrap()
        };
        let received_at = chrono::Utc::now();
        assert!(
            ProviderSubmissionStepOutcome::continuing(continuation(), [0; 32], received_at)
                .is_err()
        );
        let receipt = SubmissionReceipt {
            remote_status: "accepted".to_owned(),
            message_sanitized: None,
            provider_trace_id: Some("attempt-1".to_owned()),
            received_at,
        };
        assert!(matches!(
            ProviderSubmissionStepOutcome::submitted(receipt, [5; 32], received_at).unwrap(),
            ProviderSubmissionStepOutcome::Submitted {
                response_digest,
                ..
            } if response_digest == [5; 32]
        ));
    }
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

    /// Resolves answers while exposing an optional encrypted continuation
    /// attached to this exact immutable Question snapshot. Legacy and
    /// artifact-free Providers continue through `resolve_answers`.
    async fn resolve_answers_with_session(
        &self,
        context: &ProviderContext,
        remote_task_id: &str,
        questions: &[Question],
        _continuation: Option<ResolvedProviderQuestionSessionContinuation<'_>>,
    ) -> ProviderResult<Vec<AnswerCandidate>> {
        self.resolve_answers(context, remote_task_id, questions)
            .await
    }
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

/// Result of one accepted post-materialization Question-session operation.
/// Continuing steps rotate encrypted Provider state. A next-Question response
/// materializes a new immutable snapshot/session, while terminal completion
/// yields a Receipt without inventing a replacement continuation. Neither is
/// equivalent to whole-Task verification.
#[derive(Debug)]
pub enum ProviderSubmissionStepOutcome {
    Continue {
        continuation: ProviderQuestionReadContinuation,
        response_digest: [u8; 32],
        received_at: Timestamp,
    },
    NextQuestion(ProviderQuestionMaterialization),
    Submitted {
        receipt: SubmissionReceipt,
        response_digest: [u8; 32],
        received_at: Timestamp,
    },
}

impl ProviderSubmissionStepOutcome {
    /// # Errors
    ///
    /// Rejects an accepted continuation without an exact response digest.
    pub fn continuing(
        continuation: ProviderQuestionReadContinuation,
        response_digest: [u8; 32],
        received_at: Timestamp,
    ) -> ProviderResult<Self> {
        if response_digest == [0; 32] {
            return Err(crate::ProviderError::new(
                crate::ProviderErrorKind::InvalidResponse,
                "Provider Question-session continuation response digest is empty",
            ));
        }
        Ok(Self::Continue {
            continuation,
            response_digest,
            received_at,
        })
    }

    /// # Errors
    ///
    /// Rejects an invalid Receipt or missing response digest.
    pub fn submitted(
        receipt: SubmissionReceipt,
        response_digest: [u8; 32],
        received_at: Timestamp,
    ) -> ProviderResult<Self> {
        if response_digest == [0; 32] || receipt.validate().is_err() {
            return Err(crate::ProviderError::new(
                crate::ProviderErrorKind::InvalidResponse,
                "Provider Question-session submission result is invalid",
            ));
        }
        Ok(Self::Submitted {
            receipt,
            response_digest,
            received_at,
        })
    }
}

/// One in-memory Provider command whose exact identity is persisted before a
/// post-materialization Answer/Save/Submit mutation can run.
#[async_trait]
pub trait PreparedProviderSubmissionOperation: fmt::Debug + Send {
    fn operation_type(&self) -> &str;
    fn request_digest(&self) -> [u8; 32];
    fn delay_before_execute_seconds(&self) -> u64;

    async fn execute(
        self: Box<Self>,
        context: &ProviderContext,
        events: &(dyn ExecutionEventSink + Send + Sync),
    ) -> ProviderResult<ProviderSubmissionStepOutcome>;
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

    /// Freezes the exact next mutation for an artifact-bearing
    /// `QuestionSession`. Returning `None` selects the legacy single-call path
    /// only when no claimed session exists.
    async fn prepare_submission_operation(
        &self,
        _context: &ProviderContext,
        _remote_task_id: &str,
        _draft: &SubmissionDraft,
        _continuation: ResolvedProviderQuestionSessionContinuation<'_>,
        _runtime_settings: &ResolvedProviderRuntimeSettings,
    ) -> ProviderResult<Option<Box<dyn PreparedProviderSubmissionOperation>>> {
        Ok(None)
    }

    /// Performs fresh read-only recovery for one ambiguous issued operation.
    /// Returning `None` keeps the session locked and grants no replay.
    async fn recover_ambiguous_submission_operation(
        &self,
        _context: &ProviderContext,
        _remote_task_id: &str,
        _draft: &SubmissionDraft,
        _continuation: ResolvedProviderQuestionSessionContinuation<'_>,
        _operation: &AmbiguousProviderQuestionSessionOperation,
        _runtime_settings: &ResolvedProviderRuntimeSettings,
    ) -> ProviderResult<Option<ProviderSubmissionStepOutcome>> {
        Ok(None)
    }
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

    /// Verifies a submission while exposing the latest claimed, encrypted
    /// Provider continuation. Legacy submissions use `verify_submission`.
    async fn verify_submission_with_session(
        &self,
        context: &ProviderContext,
        remote_task_id: &str,
        draft: &SubmissionDraft,
        receipt: Option<&SubmissionReceipt>,
        _continuation: ResolvedProviderQuestionSessionContinuation<'_>,
    ) -> ProviderResult<SubmissionVerificationSnapshot> {
        self.verify_submission(context, remote_task_id, draft, receipt)
            .await
    }

    /// Maps Provider-specific verified result facts to a shared reason why the
    /// Task is not complete. Core ignores this diagnosis once fresh remote
    /// state is already Completed.
    fn completion_diagnosis(
        &self,
        _verification: &SubmissionVerificationSnapshot,
    ) -> Option<asterism_domain::CompletionDiagnosis> {
        None
    }
}

pub struct ExecutionPlanningRequest<'a> {
    pub execution_id: ExecutionId,
    pub task_id: TaskId,
    pub remote_task_id: &'a str,
    pub course_id: Option<CourseId>,
    pub requested_capabilities: &'a [TaskCapability],
    pub runtime_settings: &'a ResolvedProviderRuntimeSettings,
}

impl fmt::Debug for ExecutionPlanningRequest<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ExecutionPlanningRequest")
            .field("execution_id", &self.execution_id)
            .field("task_id", &self.task_id)
            .field("remote_task_id", &"[REDACTED]")
            .field("course_id", &self.course_id)
            .field("requested_capabilities", &self.requested_capabilities)
            .field("runtime_settings", &"[REDACTED]")
            .finish()
    }
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

    /// Groups the exact execution plan into durable Provider-call boundaries.
    /// The default preserves the historical one-capability-per-call behavior.
    /// Providers may group adjacent capabilities only when one evidenced
    /// remote transaction implements and verifies the whole group atomically.
    ///
    /// # Errors
    ///
    /// Returns the same typed error as [`Self::execution_plan`] when the
    /// capability selection is unsupported.
    fn execution_call_plan(
        &self,
        requested_capabilities: &[TaskCapability],
        _runtime_settings: &ResolvedProviderRuntimeSettings,
    ) -> ProviderResult<Vec<Vec<TaskCapability>>> {
        self.execution_plan(requested_capabilities).map(|plan| {
            plan.into_iter()
                .map(|capability| vec![capability])
                .collect()
        })
    }

    /// Freezes the complete Provider execution plan selected only from inputs
    /// already durable at scheduling time. The optional artifact is
    /// credential-free Provider-private evidence, not executable authority.
    ///
    /// # Errors
    ///
    /// Rejects invalid call groups or an unsafe Provider artifact.
    fn execution_plan_snapshot(
        &self,
        requested_capabilities: &[TaskCapability],
        runtime_settings: &ResolvedProviderRuntimeSettings,
    ) -> ProviderResult<ProviderExecutionPlan> {
        ProviderExecutionPlan::try_new(
            self.metadata().id.clone(),
            self.execution_call_plan(requested_capabilities, runtime_settings)?,
            None,
        )
    }

    /// Performs an optional read-only fresh planning pass before Core freezes
    /// an Execution. The default remains deterministic from already resolved
    /// settings and preserves the existing single-Task contract.
    ///
    /// Implementations may use opaque credential references from `context` to
    /// rediscover Provider facts, but must not perform any remote mutation.
    /// The returned plan and optional credential-free artifact become
    /// immutable scheduling evidence.
    ///
    /// # Errors
    ///
    /// Rejects unsupported selections, remote drift or unsafe plan evidence.
    async fn prepare_execution_plan(
        &self,
        _context: &ProviderContext,
        request: &ExecutionPlanningRequest<'_>,
    ) -> ProviderResult<ProviderExecutionPlan> {
        self.execution_plan_snapshot(request.requested_capabilities, request.runtime_settings)
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

    /// Maps Provider-specific verified execution facts to a shared reason why
    /// the Task is not complete. Returning `None` leaves Core on the
    /// conservative `RemoteUnknown` path.
    fn completion_diagnosis(
        &self,
        _request: &ExecutionRequest,
        _outcome: &ExecutionOutcome,
    ) -> Option<asterism_domain::CompletionDiagnosis> {
        None
    }
}

#[derive(Clone, Eq, PartialEq)]
pub struct ProviderExecutionPlanArtifact {
    provider_id: ProviderId,
    artifact_type: String,
    artifact_digest: [u8; 32],
    payload_sanitized: serde_json::Value,
}

impl ProviderExecutionPlanArtifact {
    /// Creates bounded, credential-free Provider-private scheduling evidence.
    ///
    /// # Errors
    ///
    /// Rejects a foreign/unsafe type, non-object payload, credential-shaped
    /// keys, or an encoded payload larger than 64 KiB.
    pub fn try_new(
        provider_id: ProviderId,
        artifact_type: impl Into<String>,
        payload_sanitized: serde_json::Value,
    ) -> ProviderResult<Self> {
        let artifact_type = artifact_type.into();
        let encoded = serde_json::to_vec(&payload_sanitized).map_err(|_| {
            crate::ProviderError::new(
                crate::ProviderErrorKind::InvalidResponse,
                "Provider execution plan artifact is invalid",
            )
        })?;
        if !valid_provider_execution_artifact_type(&provider_id, &artifact_type)
            || !payload_sanitized.is_object()
            || encoded.is_empty()
            || encoded.len() > 64 * 1_024
            || contains_secret_key(&payload_sanitized)
        {
            return Err(crate::ProviderError::new(
                crate::ProviderErrorKind::InvalidResponse,
                "Provider execution plan artifact is invalid",
            ));
        }
        let mut digest = Sha256::new();
        digest.update(b"asterism.provider-execution-plan-artifact.v1\0");
        digest.update(provider_id.as_str().as_bytes());
        digest.update(b"\0");
        digest.update(artifact_type.as_bytes());
        digest.update(b"\0");
        digest.update(encoded);
        Ok(Self {
            provider_id,
            artifact_type,
            artifact_digest: digest.finalize().into(),
            payload_sanitized,
        })
    }

    pub const fn provider_id(&self) -> &ProviderId {
        &self.provider_id
    }

    pub fn artifact_type(&self) -> &str {
        &self.artifact_type
    }

    pub const fn artifact_digest(&self) -> [u8; 32] {
        self.artifact_digest
    }

    pub const fn payload_sanitized(&self) -> &serde_json::Value {
        &self.payload_sanitized
    }
}

impl fmt::Debug for ProviderExecutionPlanArtifact {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProviderExecutionPlanArtifact")
            .field("provider_id", &self.provider_id)
            .field("artifact_type", &self.artifact_type)
            .field("artifact_digest", &"[HASHED]")
            .field("payload_sanitized", &"[REDACTED]")
            .finish()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderExecutionPlan {
    provider_id: ProviderId,
    calls: Vec<Vec<TaskCapability>>,
    artifact: Option<ProviderExecutionPlanArtifact>,
}

impl ProviderExecutionPlan {
    /// # Errors
    ///
    /// Rejects empty, duplicate or oversized call groups and foreign artifacts.
    pub fn try_new(
        provider_id: ProviderId,
        calls: Vec<Vec<TaskCapability>>,
        artifact: Option<ProviderExecutionPlanArtifact>,
    ) -> ProviderResult<Self> {
        let flattened = calls.iter().flatten().copied().collect::<Vec<_>>();
        if calls.is_empty()
            || calls.len() > 5
            || calls.iter().any(Vec::is_empty)
            || flattened.len() > 5
            || flattened
                .iter()
                .copied()
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                != flattened.len()
            || artifact
                .as_ref()
                .is_some_and(|artifact| artifact.provider_id != provider_id)
        {
            return Err(crate::ProviderError::new(
                crate::ProviderErrorKind::InvalidResponse,
                "Provider execution plan is invalid",
            ));
        }
        Ok(Self {
            provider_id,
            calls,
            artifact,
        })
    }

    pub const fn provider_id(&self) -> &ProviderId {
        &self.provider_id
    }

    pub fn calls(&self) -> &[Vec<TaskCapability>] {
        &self.calls
    }

    pub const fn artifact(&self) -> Option<&ProviderExecutionPlanArtifact> {
        self.artifact.as_ref()
    }
}

fn valid_provider_execution_artifact_type(provider_id: &ProviderId, value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 96
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-')
        })
        && value
            .strip_prefix(provider_id.as_str())
            .is_some_and(|suffix| suffix.starts_with('.') && suffix.len() > 1)
}

const MAX_EXECUTION_MUTATION_PLAN_STEPS: usize = 100_000;
const MAX_EXECUTION_MUTATION_DEPENDENCIES: usize = 32;

#[derive(Clone, Eq, PartialEq)]
pub struct ExecutionMutationPlanStep {
    ordinal: u32,
    operation_type: String,
    request_digest: Option<[u8; 32]>,
    dependency_ordinals: Vec<u32>,
}

impl ExecutionMutationPlanStep {
    /// Creates one immutable mutation step and its verified dependencies.
    ///
    /// # Errors
    ///
    /// Rejects invalid identities, digests, unordered dependencies, forward
    /// dependencies and dependency fan-out beyond the Core bound.
    pub fn try_new(
        ordinal: u32,
        operation_type: impl Into<String>,
        request_digest: Option<[u8; 32]>,
        dependency_ordinals: Vec<u32>,
    ) -> ProviderResult<Self> {
        let operation_type = operation_type.into();
        if !(1..=100_000).contains(&ordinal)
            || !valid_execution_mutation_operation_type(&operation_type)
            || request_digest.is_some_and(|digest| digest == [0; 32])
            || dependency_ordinals.len() > MAX_EXECUTION_MUTATION_DEPENDENCIES
            || dependency_ordinals
                .iter()
                .any(|dependency| *dependency == 0 || *dependency >= ordinal)
            || !dependency_ordinals.windows(2).all(|pair| pair[0] < pair[1])
        {
            return Err(crate::ProviderError::new(
                crate::ProviderErrorKind::InvalidResponse,
                "Provider execution mutation plan step is invalid",
            ));
        }
        Ok(Self {
            ordinal,
            operation_type,
            request_digest,
            dependency_ordinals,
        })
    }

    pub const fn ordinal(&self) -> u32 {
        self.ordinal
    }

    pub fn operation_type(&self) -> &str {
        &self.operation_type
    }

    pub const fn request_digest(&self) -> Option<[u8; 32]> {
        self.request_digest
    }

    pub fn dependency_ordinals(&self) -> &[u32] {
        &self.dependency_ordinals
    }
}

impl fmt::Debug for ExecutionMutationPlanStep {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ExecutionMutationPlanStep")
            .field("ordinal", &self.ordinal)
            .field("operation_type", &self.operation_type)
            .field("request_digest", &self.request_digest.map(|_| "[HASHED]"))
            .field("dependency_ordinals", &self.dependency_ordinals)
            .finish()
    }
}

#[derive(Clone, Eq, PartialEq)]
pub struct ExecutionMutationPlan {
    plan_digest: [u8; 32],
    artifact_digest: [u8; 32],
    steps: Vec<ExecutionMutationPlanStep>,
}

impl ExecutionMutationPlan {
    /// Freezes a complete fixed-topology mutation DAG against the independently
    /// durable Provider execution artifact. A step may leave its request digest
    /// unbound when the exact request depends on verified predecessors; Core
    /// then binds it once in the same transaction that issues the step.
    /// Receipt-conditional sequences whose topology or ordinals change at
    /// runtime require a separate sequence contract.
    ///
    /// # Errors
    ///
    /// Rejects an empty artifact digest, empty/oversized plans, non-contiguous
    /// ordinals or a step whose own validation no longer holds.
    pub fn try_new(
        artifact_digest: [u8; 32],
        steps: Vec<ExecutionMutationPlanStep>,
    ) -> ProviderResult<Self> {
        if artifact_digest == [0; 32]
            || steps.is_empty()
            || steps.len() > MAX_EXECUTION_MUTATION_PLAN_STEPS
            || steps.iter().enumerate().any(|(index, step)| {
                u32::try_from(index + 1).ok() != Some(step.ordinal)
                    || ExecutionMutationPlanStep::try_new(
                        step.ordinal,
                        step.operation_type.clone(),
                        step.request_digest,
                        step.dependency_ordinals.clone(),
                    )
                    .is_err()
            })
        {
            return Err(crate::ProviderError::new(
                crate::ProviderErrorKind::InvalidResponse,
                "Provider execution mutation plan is invalid",
            ));
        }
        let plan_digest = execution_mutation_plan_digest(artifact_digest, &steps);
        Ok(Self {
            plan_digest,
            artifact_digest,
            steps,
        })
    }

    pub const fn plan_digest(&self) -> [u8; 32] {
        self.plan_digest
    }

    pub const fn artifact_digest(&self) -> [u8; 32] {
        self.artifact_digest
    }

    pub fn steps(&self) -> &[ExecutionMutationPlanStep] {
        &self.steps
    }
}

impl fmt::Debug for ExecutionMutationPlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ExecutionMutationPlan")
            .field("plan_digest", &"[HASHED]")
            .field("artifact_digest", &"[HASHED]")
            .field("step_count", &self.steps.len())
            .finish()
    }
}

fn execution_mutation_plan_digest(
    artifact_digest: [u8; 32],
    steps: &[ExecutionMutationPlanStep],
) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"asterism.execution-mutation-plan.v1\0");
    digest.update(artifact_digest);
    digest.update(u32::try_from(steps.len()).unwrap_or(u32::MAX).to_be_bytes());
    for step in steps {
        digest.update(step.ordinal.to_be_bytes());
        digest.update(
            u32::try_from(step.operation_type.len())
                .unwrap_or(u32::MAX)
                .to_be_bytes(),
        );
        digest.update(step.operation_type.as_bytes());
        match step.request_digest {
            Some(request_digest) => {
                digest.update([1]);
                digest.update(request_digest);
            }
            None => digest.update([0]),
        }
        digest.update(
            u32::try_from(step.dependency_ordinals.len())
                .unwrap_or(u32::MAX)
                .to_be_bytes(),
        );
        for dependency in &step.dependency_ordinals {
            digest.update(dependency.to_be_bytes());
        }
    }
    digest.finalize().into()
}

const MAX_EXECUTION_MUTATION_SEQUENCE_PHASES: usize = 32;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExecutionMutationSequenceAdvanceCondition {
    MaximumReached,
    AcceptedMaximumReached,
    RejectedOrMaximumReached,
}

#[derive(Clone, Eq, PartialEq)]
pub struct ExecutionMutationSequencePhase {
    operation_type: String,
    minimum_occurrences: u32,
    maximum_occurrences: u32,
    stop_repeating_after_rejection: bool,
    advance_condition: ExecutionMutationSequenceAdvanceCondition,
    required_observation_type: Option<String>,
}

impl ExecutionMutationSequencePhase {
    /// Creates one bounded receipt-conditional sequence phase.
    ///
    /// # Errors
    ///
    /// Rejects invalid operation/observation labels, inverted or unbounded
    /// occurrence ranges, and an accepted-only transition with no occurrence.
    pub fn try_new(
        operation_type: impl Into<String>,
        minimum_occurrences: u32,
        maximum_occurrences: u32,
        stop_repeating_after_rejection: bool,
        advance_condition: ExecutionMutationSequenceAdvanceCondition,
        required_observation_type: Option<String>,
    ) -> ProviderResult<Self> {
        let operation_type = operation_type.into();
        if !valid_execution_mutation_operation_type(&operation_type)
            || maximum_occurrences > 100_000
            || minimum_occurrences > maximum_occurrences
            || matches!(
                advance_condition,
                ExecutionMutationSequenceAdvanceCondition::AcceptedMaximumReached
            ) && maximum_occurrences == 0
            || required_observation_type
                .as_deref()
                .is_some_and(|value| !valid_execution_mutation_operation_type(value))
            || required_observation_type.is_some() && maximum_occurrences == 0
        {
            return Err(crate::ProviderError::new(
                crate::ProviderErrorKind::InvalidResponse,
                "Provider execution mutation sequence phase is invalid",
            ));
        }
        Ok(Self {
            operation_type,
            minimum_occurrences,
            maximum_occurrences,
            stop_repeating_after_rejection,
            advance_condition,
            required_observation_type,
        })
    }

    pub fn operation_type(&self) -> &str {
        &self.operation_type
    }

    pub const fn minimum_occurrences(&self) -> u32 {
        self.minimum_occurrences
    }

    pub const fn maximum_occurrences(&self) -> u32 {
        self.maximum_occurrences
    }

    pub const fn stop_repeating_after_rejection(&self) -> bool {
        self.stop_repeating_after_rejection
    }

    pub const fn advance_condition(&self) -> ExecutionMutationSequenceAdvanceCondition {
        self.advance_condition
    }

    pub fn required_observation_type(&self) -> Option<&str> {
        self.required_observation_type.as_deref()
    }
}

impl fmt::Debug for ExecutionMutationSequencePhase {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ExecutionMutationSequencePhase")
            .field("operation_type", &self.operation_type)
            .field("minimum_occurrences", &self.minimum_occurrences)
            .field("maximum_occurrences", &self.maximum_occurrences)
            .field(
                "stop_repeating_after_rejection",
                &self.stop_repeating_after_rejection,
            )
            .field("advance_condition", &self.advance_condition)
            .field("required_observation_type", &self.required_observation_type)
            .finish()
    }
}

#[derive(Clone, Eq, PartialEq)]
pub struct ExecutionMutationSequencePlan {
    plan_digest: [u8; 32],
    artifact_digest: [u8; 32],
    sequence_type: String,
    phases: Vec<ExecutionMutationSequencePhase>,
}

impl ExecutionMutationSequencePlan {
    /// Freezes a bounded receipt-conditional phase machine before the first
    /// mutation. Operation labels are unique so Core can unambiguously map
    /// each issued ordinal back to its phase.
    ///
    /// # Errors
    ///
    /// Rejects invalid authority digests/types, empty/oversized phase sets,
    /// duplicate operations or a total occurrence ceiling above 100,000.
    pub fn try_new(
        artifact_digest: [u8; 32],
        sequence_type: impl Into<String>,
        phases: Vec<ExecutionMutationSequencePhase>,
    ) -> ProviderResult<Self> {
        let sequence_type = sequence_type.into();
        let unique_operations = phases
            .iter()
            .map(ExecutionMutationSequencePhase::operation_type)
            .collect::<std::collections::BTreeSet<_>>();
        let maximum_mutations = phases.iter().try_fold(0_u32, |total, phase| {
            total.checked_add(phase.maximum_occurrences)
        });
        if artifact_digest == [0; 32]
            || !valid_execution_mutation_operation_type(&sequence_type)
            || phases.is_empty()
            || phases.len() > MAX_EXECUTION_MUTATION_SEQUENCE_PHASES
            || unique_operations.len() != phases.len()
            || maximum_mutations.is_none_or(|total| total == 0 || total > 100_000)
            || phases.iter().any(|phase| {
                ExecutionMutationSequencePhase::try_new(
                    phase.operation_type.clone(),
                    phase.minimum_occurrences,
                    phase.maximum_occurrences,
                    phase.stop_repeating_after_rejection,
                    phase.advance_condition,
                    phase.required_observation_type.clone(),
                )
                .is_err()
            })
        {
            return Err(crate::ProviderError::new(
                crate::ProviderErrorKind::InvalidResponse,
                "Provider execution mutation sequence plan is invalid",
            ));
        }
        let plan_digest =
            execution_mutation_sequence_plan_digest(artifact_digest, &sequence_type, &phases);
        Ok(Self {
            plan_digest,
            artifact_digest,
            sequence_type,
            phases,
        })
    }

    pub const fn plan_digest(&self) -> [u8; 32] {
        self.plan_digest
    }

    pub const fn artifact_digest(&self) -> [u8; 32] {
        self.artifact_digest
    }

    pub fn sequence_type(&self) -> &str {
        &self.sequence_type
    }

    pub fn phases(&self) -> &[ExecutionMutationSequencePhase] {
        &self.phases
    }
}

impl fmt::Debug for ExecutionMutationSequencePlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ExecutionMutationSequencePlan")
            .field("plan_digest", &"[HASHED]")
            .field("artifact_digest", &"[HASHED]")
            .field("sequence_type", &self.sequence_type)
            .field("phase_count", &self.phases.len())
            .finish()
    }
}

#[derive(Clone, Eq, PartialEq)]
pub struct ExecutionMutationSequenceObservation {
    phase_position: u8,
    observation_type: String,
    observation_digest: [u8; 32],
}

impl ExecutionMutationSequenceObservation {
    /// # Errors
    ///
    /// Rejects an empty phase position, invalid type or empty digest.
    pub fn try_new(
        phase_position: u8,
        observation_type: impl Into<String>,
        observation_digest: [u8; 32],
    ) -> ProviderResult<Self> {
        let observation_type = observation_type.into();
        if phase_position == 0
            || !valid_execution_mutation_operation_type(&observation_type)
            || observation_digest == [0; 32]
        {
            return Err(crate::ProviderError::new(
                crate::ProviderErrorKind::InvalidResponse,
                "Provider execution mutation sequence observation is invalid",
            ));
        }
        Ok(Self {
            phase_position,
            observation_type,
            observation_digest,
        })
    }

    pub const fn phase_position(&self) -> u8 {
        self.phase_position
    }

    pub fn observation_type(&self) -> &str {
        &self.observation_type
    }

    pub const fn observation_digest(&self) -> [u8; 32] {
        self.observation_digest
    }
}

impl fmt::Debug for ExecutionMutationSequenceObservation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ExecutionMutationSequenceObservation")
            .field("phase_position", &self.phase_position)
            .field("observation_type", &self.observation_type)
            .field("observation_digest", &"[HASHED]")
            .finish()
    }
}

fn execution_mutation_sequence_plan_digest(
    artifact_digest: [u8; 32],
    sequence_type: &str,
    phases: &[ExecutionMutationSequencePhase],
) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"asterism.execution-mutation-sequence-plan.v1\0");
    digest.update(artifact_digest);
    digest.update(
        u32::try_from(sequence_type.len())
            .unwrap_or(u32::MAX)
            .to_be_bytes(),
    );
    digest.update(sequence_type.as_bytes());
    digest.update(
        u32::try_from(phases.len())
            .unwrap_or(u32::MAX)
            .to_be_bytes(),
    );
    for phase in phases {
        digest.update(
            u32::try_from(phase.operation_type.len())
                .unwrap_or(u32::MAX)
                .to_be_bytes(),
        );
        digest.update(phase.operation_type.as_bytes());
        digest.update(phase.minimum_occurrences.to_be_bytes());
        digest.update(phase.maximum_occurrences.to_be_bytes());
        digest.update([u8::from(phase.stop_repeating_after_rejection)]);
        digest.update([match phase.advance_condition {
            ExecutionMutationSequenceAdvanceCondition::MaximumReached => 1,
            ExecutionMutationSequenceAdvanceCondition::AcceptedMaximumReached => 2,
            ExecutionMutationSequenceAdvanceCondition::RejectedOrMaximumReached => 3,
        }]);
        match &phase.required_observation_type {
            Some(observation_type) => {
                digest.update([1]);
                digest.update(
                    u32::try_from(observation_type.len())
                        .unwrap_or(u32::MAX)
                        .to_be_bytes(),
                );
                digest.update(observation_type.as_bytes());
            }
            None => digest.update([0]),
        }
    }
    digest.finalize().into()
}

#[derive(Clone, Eq, PartialEq)]
pub struct ExecutionMutationIssue {
    ordinal: u32,
    operation_type: String,
    request_digest: [u8; 32],
}

impl fmt::Debug for ExecutionMutationIssue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ExecutionMutationIssue")
            .field("ordinal", &self.ordinal)
            .field("operation_type", &self.operation_type)
            .field("request_digest", &"[HASHED]")
            .finish()
    }
}

impl ExecutionMutationIssue {
    /// Creates one bounded Provider mutation identity for durable issuance.
    ///
    /// # Errors
    ///
    /// Rejects an empty ordinal or digest and operation labels which cannot be
    /// safely persisted. Core additionally requires a current-Provider prefix.
    pub fn new(
        ordinal: u32,
        operation_type: impl Into<String>,
        request_digest: [u8; 32],
    ) -> ProviderResult<Self> {
        let operation_type = operation_type.into();
        if !(1..=100_000).contains(&ordinal)
            || !valid_execution_mutation_operation_type(&operation_type)
            || request_digest == [0; 32]
        {
            return Err(crate::ProviderError::new(
                crate::ProviderErrorKind::InvalidResponse,
                "Provider execution mutation issue is invalid",
            ));
        }
        Ok(Self {
            ordinal,
            operation_type,
            request_digest,
        })
    }

    pub const fn ordinal(&self) -> u32 {
        self.ordinal
    }

    pub fn operation_type(&self) -> &str {
        &self.operation_type
    }

    pub const fn request_digest(&self) -> [u8; 32] {
        self.request_digest
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub struct ExecutionMutationReceipt {
    ordinal: u32,
    response_digest: [u8; 32],
    accepted: bool,
}

impl fmt::Debug for ExecutionMutationReceipt {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ExecutionMutationReceipt")
            .field("ordinal", &self.ordinal)
            .field("response_digest", &"[HASHED]")
            .field("accepted", &self.accepted)
            .finish()
    }
}

impl ExecutionMutationReceipt {
    /// Creates one explicit remote response identity.
    ///
    /// # Errors
    ///
    /// Rejects an empty ordinal or response digest.
    pub fn new(ordinal: u32, response_digest: [u8; 32], accepted: bool) -> ProviderResult<Self> {
        if !(1..=100_000).contains(&ordinal) || response_digest == [0; 32] {
            return Err(crate::ProviderError::new(
                crate::ProviderErrorKind::InvalidResponse,
                "Provider execution mutation receipt is invalid",
            ));
        }
        Ok(Self {
            ordinal,
            response_digest,
            accepted,
        })
    }

    pub const fn ordinal(self) -> u32 {
        self.ordinal
    }

    pub const fn response_digest(self) -> [u8; 32] {
        self.response_digest
    }

    pub const fn accepted(self) -> bool {
        self.accepted
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub struct ExecutionMutationVerification {
    ordinal: u32,
    observation_digest: [u8; 32],
    verified: bool,
}

impl fmt::Debug for ExecutionMutationVerification {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ExecutionMutationVerification")
            .field("ordinal", &self.ordinal)
            .field("observation_digest", &"[HASHED]")
            .field("verified", &self.verified)
            .finish()
    }
}

impl ExecutionMutationVerification {
    /// Creates one read-only verification observation for an accepted remote
    /// mutation.
    ///
    /// # Errors
    ///
    /// Rejects an empty ordinal or observation digest.
    pub fn new(ordinal: u32, observation_digest: [u8; 32], verified: bool) -> ProviderResult<Self> {
        if !(1..=100_000).contains(&ordinal) || observation_digest == [0; 32] {
            return Err(crate::ProviderError::new(
                crate::ProviderErrorKind::InvalidResponse,
                "Provider execution mutation verification is invalid",
            ));
        }
        Ok(Self {
            ordinal,
            observation_digest,
            verified,
        })
    }

    pub const fn ordinal(self) -> u32 {
        self.ordinal
    }

    pub const fn observation_digest(self) -> [u8; 32] {
        self.observation_digest
    }

    pub const fn verified(self) -> bool {
        self.verified
    }
}

#[async_trait]
pub trait ExecutionMutationSink {
    /// Atomically freezes a complete immutable mutation plan before any remote
    /// step is issued. Repeating the exact preparation is idempotent.
    async fn prepare_compound_plan(&self, _plan: &ExecutionMutationPlan) -> ProviderResult<()> {
        Err(crate::ProviderError::new(
            crate::ProviderErrorKind::UnsupportedTask,
            "Compound execution mutation planning is not available for this sink",
        ))
    }

    /// Atomically freezes a receipt-conditional phase machine before any
    /// remote mutation is issued.
    async fn prepare_sequence_plan(
        &self,
        _plan: &ExecutionMutationSequencePlan,
    ) -> ProviderResult<()> {
        Err(crate::ProviderError::new(
            crate::ProviderErrorKind::UnsupportedTask,
            "Execution mutation sequence planning is not available for this sink",
        ))
    }

    /// Persists one hash-only observation required before entering its frozen
    /// sequence phase. Exact repeats are idempotent.
    async fn record_sequence_observation(
        &self,
        _observation: ExecutionMutationSequenceObservation,
    ) -> ProviderResult<()> {
        Err(crate::ProviderError::new(
            crate::ProviderErrorKind::UnsupportedTask,
            "Execution mutation sequence observations are not available for this sink",
        ))
    }

    /// Persists the exact request identity before the Provider performs the
    /// corresponding remote mutation. A repeated issuance fails closed.
    async fn issue(&self, issue: &ExecutionMutationIssue) -> ProviderResult<()>;

    /// Persists a definite, parsed remote response before another ordinal may
    /// be issued. Missing or ambiguous responses must not call this method.
    async fn record_receipt(&self, receipt: ExecutionMutationReceipt) -> ProviderResult<()>;

    /// Persists an independent read-only observation after an accepted receipt.
    /// Legacy fixture sinks may omit this until their Provider adopts compound
    /// step verification; Core's execution sink always overrides it.
    async fn record_verification(
        &self,
        _verification: ExecutionMutationVerification,
    ) -> ProviderResult<()> {
        Err(crate::ProviderError::new(
            crate::ProviderErrorKind::UnsupportedTask,
            "Execution mutation verification is not available for this sink",
        ))
    }
}

fn valid_execution_mutation_operation_type(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 96
        && value.trim() == value
        && value.is_ascii()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

#[cfg(test)]
mod execution_mutation_tests {
    use super::*;

    #[test]
    fn issue_and_receipt_are_bounded_and_digest_redacted() {
        let issue = ExecutionMutationIssue::new(1, "welearn.atomic.start", [7; 32]).unwrap();
        assert_eq!(issue.ordinal(), 1);
        assert_eq!(issue.operation_type(), "welearn.atomic.start");
        assert_eq!(issue.request_digest(), [7; 32]);
        let debug = format!("{issue:?}");
        assert!(debug.contains("[HASHED]"));
        assert!(!debug.contains("7, 7"));

        let receipt = ExecutionMutationReceipt::new(1, [9; 32], false).unwrap();
        assert_eq!(receipt.ordinal(), 1);
        assert_eq!(receipt.response_digest(), [9; 32]);
        assert!(!receipt.accepted());
        let debug = format!("{receipt:?}");
        assert!(debug.contains("[HASHED]"));
        assert!(!debug.contains("9, 9"));

        let verification = ExecutionMutationVerification::new(1, [11; 32], true).unwrap();
        assert_eq!(verification.ordinal(), 1);
        assert_eq!(verification.observation_digest(), [11; 32]);
        assert!(verification.verified());
        let debug = format!("{verification:?}");
        assert!(debug.contains("[HASHED]"));
        assert!(!debug.contains("11, 11"));

        assert!(ExecutionMutationIssue::new(0, "welearn.atomic.start", [7; 32]).is_err());
        assert!(ExecutionMutationIssue::new(1, "foreign operation", [7; 32]).is_err());
        assert!(ExecutionMutationIssue::new(1, "welearn.atomic.start", [0; 32]).is_err());
        assert!(ExecutionMutationReceipt::new(100_001, [9; 32], true).is_err());
        assert!(ExecutionMutationReceipt::new(1, [0; 32], true).is_err());
        assert!(ExecutionMutationVerification::new(0, [11; 32], true).is_err());
        assert!(ExecutionMutationVerification::new(1, [0; 32], true).is_err());
    }

    #[test]
    fn compound_plan_is_artifact_bound_contiguous_and_dependency_ordered() {
        let steps = vec![
            ExecutionMutationPlanStep::try_new(1, "welearn.atomic.start", Some([1; 32]), vec![])
                .unwrap(),
            ExecutionMutationPlanStep::try_new(2, "welearn.atomic.set", None, vec![1]).unwrap(),
            ExecutionMutationPlanStep::try_new(3, "welearn.atomic.save", Some([3; 32]), vec![1, 2])
                .unwrap(),
        ];
        let plan = ExecutionMutationPlan::try_new([9; 32], steps.clone()).unwrap();
        assert_eq!(plan.artifact_digest(), [9; 32]);
        assert_ne!(plan.plan_digest(), [0; 32]);
        assert_eq!(plan.steps(), steps);
        let debug = format!("{plan:?}");
        assert!(debug.contains("step_count: 3"));
        assert!(!debug.contains("9, 9"));

        let changed = ExecutionMutationPlan::try_new(
            [9; 32],
            vec![
                ExecutionMutationPlanStep::try_new(
                    1,
                    "welearn.atomic.start",
                    Some([4; 32]),
                    vec![],
                )
                .unwrap(),
                steps[1].clone(),
                steps[2].clone(),
            ],
        )
        .unwrap();
        assert_ne!(changed.plan_digest(), plan.plan_digest());

        assert!(ExecutionMutationPlan::try_new([0; 32], steps.clone()).is_err());
        assert!(ExecutionMutationPlan::try_new([9; 32], Vec::new()).is_err());
        assert!(ExecutionMutationPlan::try_new([9; 32], steps[1..].to_vec()).is_err());
        assert!(
            ExecutionMutationPlanStep::try_new(2, "welearn.atomic.set", Some([2; 32]), vec![2],)
                .is_err()
        );
        assert!(
            ExecutionMutationPlanStep::try_new(
                3,
                "welearn.atomic.save",
                Some([3; 32]),
                vec![2, 1],
            )
            .is_err()
        );
    }

    #[test]
    fn receipt_conditional_sequence_freezes_early_rejection_and_observation_gate() {
        let phases = vec![
            ExecutionMutationSequencePhase::try_new(
                "welearn.atomic.start",
                1,
                1,
                true,
                ExecutionMutationSequenceAdvanceCondition::AcceptedMaximumReached,
                None,
            )
            .unwrap(),
            ExecutionMutationSequencePhase::try_new(
                "welearn.atomic.keep_counter",
                1,
                3,
                true,
                ExecutionMutationSequenceAdvanceCondition::RejectedOrMaximumReached,
                None,
            )
            .unwrap(),
            ExecutionMutationSequencePhase::try_new(
                "welearn.atomic.set",
                1,
                1,
                false,
                ExecutionMutationSequenceAdvanceCondition::MaximumReached,
                Some("welearn.atomic.pre-final.v1".to_owned()),
            )
            .unwrap(),
            ExecutionMutationSequencePhase::try_new(
                "welearn.atomic.save",
                1,
                1,
                false,
                ExecutionMutationSequenceAdvanceCondition::MaximumReached,
                None,
            )
            .unwrap(),
        ];
        let plan = ExecutionMutationSequencePlan::try_new(
            [7; 32],
            "welearn.atomic.fany-sequence.v1",
            phases.clone(),
        )
        .unwrap();
        assert_eq!(plan.phases(), phases);
        assert_ne!(plan.plan_digest(), [0; 32]);
        assert!(!format!("{plan:?}").contains("7, 7"));

        let zero_keep = ExecutionMutationSequencePhase::try_new(
            "welearn.atomic.keep_counter",
            0,
            0,
            true,
            ExecutionMutationSequenceAdvanceCondition::RejectedOrMaximumReached,
            None,
        )
        .unwrap();
        assert_eq!(zero_keep.maximum_occurrences(), 0);
        assert!(
            ExecutionMutationSequencePlan::try_new(
                [7; 32],
                "welearn.atomic.duplicate.v1",
                vec![phases[0].clone(), phases[0].clone()],
            )
            .is_err()
        );
        assert!(
            ExecutionMutationSequencePhase::try_new(
                "welearn.atomic.invalid",
                0,
                0,
                false,
                ExecutionMutationSequenceAdvanceCondition::AcceptedMaximumReached,
                None,
            )
            .is_err()
        );

        let observation = ExecutionMutationSequenceObservation::try_new(
            3,
            "welearn.atomic.pre-final.v1",
            [8; 32],
        )
        .unwrap();
        assert_eq!(observation.phase_position(), 3);
        assert!(!format!("{observation:?}").contains("8, 8"));
        assert!(
            ExecutionMutationSequenceObservation::try_new(
                0,
                "welearn.atomic.pre-final.v1",
                [8; 32],
            )
            .is_err()
        );
    }
}

#[cfg(test)]
mod provider_execution_plan_tests {
    use super::*;

    #[test]
    fn artifact_is_provider_bound_hashed_bounded_and_redacted() {
        let provider_id = ProviderId::new("welearn").unwrap();
        let artifact = ProviderExecutionPlanArtifact::try_new(
            provider_id.clone(),
            "welearn.atomic-child.v1",
            serde_json::json!({
                "profile": "fanyuchang_fresh_set_save_100",
                "target_seconds": 120,
            }),
        )
        .unwrap();
        assert_eq!(artifact.provider_id(), &provider_id);
        assert_ne!(artifact.artifact_digest(), [0; 32]);
        let debug = format!("{artifact:?}");
        assert!(debug.contains("[HASHED]"));
        assert!(debug.contains("[REDACTED]"));
        assert!(!debug.contains("target_seconds"));

        let changed = ProviderExecutionPlanArtifact::try_new(
            provider_id.clone(),
            "welearn.atomic-child.v1",
            serde_json::json!({
                "profile": "fanyuchang_fresh_set_save_100",
                "target_seconds": 121,
            }),
        )
        .unwrap();
        assert_ne!(artifact.artifact_digest(), changed.artifact_digest());
        assert!(
            ProviderExecutionPlanArtifact::try_new(
                provider_id.clone(),
                "uai.atomic-child.v1",
                serde_json::json!({"target_seconds": 120}),
            )
            .is_err()
        );
        assert!(
            ProviderExecutionPlanArtifact::try_new(
                provider_id.clone(),
                "welearn.atomic-child.v1",
                serde_json::json!({"access_token": "secret"}),
            )
            .is_err()
        );

        let plan = ProviderExecutionPlan::try_new(
            provider_id,
            vec![vec![
                TaskCapability::DurationReport,
                TaskCapability::ResourceExecution,
            ]],
            Some(artifact),
        )
        .unwrap();
        assert_eq!(plan.calls().len(), 1);
        assert!(plan.artifact().is_some());
    }
}

#[async_trait]
pub trait BrowserBridgeCapability: ProviderIdentity {
    async fn browser_session_spec(
        &self,
        context: &ProviderContext,
        remote_task_id: &str,
    ) -> ProviderResult<BrowserSessionSpec>;

    /// Classifies one Provider-namespaced result type without parsing result
    /// bytes. Unknown types remain outside terminal credential orchestration.
    fn browser_bridge_result_disposition(
        &self,
        _result_type: &str,
    ) -> Option<BrowserBridgeResultDisposition> {
        None
    }

    /// Enumerates the exact Provider-namespaced result types that may replace
    /// credentials. Core uses this closed set to select a bounded durable
    /// inbox without allowing unknown or non-credential results to starve it.
    fn browser_bridge_credential_result_types(&self) -> &'static [&'static str] {
        &[]
    }

    /// Enumerates the exact non-terminal result types that advance a durable
    /// Provider browser workflow to another command.
    fn browser_bridge_intermediate_result_types(&self) -> &'static [&'static str] {
        &[]
    }

    /// Enumerates the exact result types that end browser mutation and require
    /// independent execution verification before Core can accept success.
    fn browser_bridge_execution_result_types(&self) -> &'static [&'static str] {
        &[]
    }

    /// Validates one recovered terminal credential result. Providers whose
    /// `BrowserBridge` workflow does not replace credentials remain fail-closed.
    async fn complete_browser_bridge_credential_result(
        &self,
        _context: &ProviderContext,
        _request: BrowserBridgeCredentialResultRequest<'_>,
    ) -> ProviderResult<BrowserBridgeCredentialResult> {
        Err(crate::ProviderError::new(
            crate::ProviderErrorKind::UnsupportedTask,
            "Provider does not accept BrowserBridge credential results",
        ))
    }

    /// Consumes one Core-recovered intermediate or execution-terminal result.
    /// Providers without a durable multi-command workflow remain fail-closed.
    async fn complete_browser_bridge_workflow_result(
        &self,
        _context: &ProviderContext,
        _settings: &ResolvedProviderRuntimeSettings,
        _request: BrowserBridgeWorkflowResultRequest,
    ) -> ProviderResult<BrowserBridgeWorkflowResult> {
        Err(crate::ProviderError::new(
            crate::ProviderErrorKind::UnsupportedTask,
            "Provider does not accept BrowserBridge workflow results",
        ))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BrowserBridgeResultDisposition {
    CredentialTerminal,
    Intermediate,
    ExecutionTerminal,
}

/// Provider-private workflow-plan bytes encrypted by Core and rebound by type
/// and digest after restart.
pub struct BrowserBridgeWorkflowPlanArtifact {
    artifact_type: String,
    artifact_digest: [u8; 32],
    artifact: SecretValue,
}

impl BrowserBridgeWorkflowPlanArtifact {
    /// # Errors
    ///
    /// Rejects an unsafe type, empty/oversized bytes or digest mismatch.
    pub fn try_new(artifact_type: String, artifact: SecretValue) -> ProviderResult<Self> {
        let bytes = artifact.expose_secret();
        let artifact_digest: [u8; 32] = Sha256::digest(bytes).into();
        if !valid_browser_bridge_type(&artifact_type)
            || bytes.is_empty()
            || bytes.len() > 256 * 1_024
            || artifact_digest == [0; 32]
        {
            return Err(browser_bridge_workflow_error());
        }
        Ok(Self {
            artifact_type,
            artifact_digest,
            artifact,
        })
    }

    pub fn artifact_type(&self) -> &str {
        &self.artifact_type
    }

    pub const fn artifact_digest(&self) -> [u8; 32] {
        self.artifact_digest
    }

    pub fn into_parts(self) -> (String, [u8; 32], SecretValue) {
        (self.artifact_type, self.artifact_digest, self.artifact)
    }
}

impl fmt::Debug for BrowserBridgeWorkflowPlanArtifact {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BrowserBridgeWorkflowPlanArtifact")
            .field("artifact_type", &self.artifact_type)
            .field("artifact_digest", &self.artifact_digest)
            .field("artifact", &"[REDACTED]")
            .finish()
    }
}

/// Exact encrypted runtime sidecar paired with the issued command.
pub struct BrowserBridgeWorkflowRuntimeState {
    pub metadata: BrowserBridgeRuntimeStateMetadata,
    pub artifact: SecretValue,
}

impl BrowserBridgeWorkflowRuntimeState {
    fn validate_for_exchange(&self, exchange: &BrowserBridgeExchange) -> ProviderResult<()> {
        let digest: [u8; 32] = Sha256::digest(self.artifact.expose_secret()).into();
        if self.metadata.validate().is_err()
            || self.metadata.session_id != exchange.session_id
            || self.metadata.sequence != exchange.sequence
            || self.metadata.stored_at != exchange.issued_at
            || self.metadata.state_digest != digest
            || self.artifact.expose_secret().is_empty()
            || self.artifact.expose_secret().len() > 256 * 1_024
        {
            Err(browser_bridge_workflow_error())
        } else {
            Ok(())
        }
    }
}

impl fmt::Debug for BrowserBridgeWorkflowRuntimeState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BrowserBridgeWorkflowRuntimeState")
            .field("metadata", &self.metadata)
            .field("artifact", &"[REDACTED]")
            .finish()
    }
}

/// Owned Core recovery evidence for a Provider multi-command workflow result.
pub struct BrowserBridgeWorkflowResultRequest {
    pub remote_task_id: String,
    pub issued_exchange: BrowserBridgeExchange,
    pub command_artifact: SecretValue,
    pub workflow_plan: Option<BrowserBridgeWorkflowPlanArtifact>,
    pub runtime_state: Option<BrowserBridgeWorkflowRuntimeState>,
    pub result_metadata: BrowserBridgeResultArtifactMetadata,
    pub result_artifact: SecretValue,
    pub runtime_binding: BrowserBridgeRuntimeBinding,
}

impl BrowserBridgeWorkflowResultRequest {
    /// Validates every generic session/sequence/digest/time binding before
    /// Provider-specific plan, cursor or result parsing.
    ///
    /// # Errors
    ///
    /// Rejects incomplete, oversized, changed or cross-session evidence.
    pub fn validate(&self) -> ProviderResult<()> {
        let command = self.command_artifact.expose_secret();
        let result = self.result_artifact.expose_secret();
        let command_digest: [u8; 32] = Sha256::digest(command).into();
        let result_digest: [u8; 32] = Sha256::digest(result).into();
        let valid_remote_task = !self.remote_task_id.is_empty()
            && self.remote_task_id.len() <= 2_048
            && self.remote_task_id.trim() == self.remote_task_id
            && !self.remote_task_id.chars().any(char::is_control);
        if !valid_remote_task
            || self.issued_exchange.validate().is_err()
            || self.issued_exchange.state != BrowserBridgeExchangeState::Issued
            || self.runtime_binding.validate().is_err()
            || self.runtime_binding.session_id != self.issued_exchange.session_id
            || self.runtime_binding.bound_at > self.issued_exchange.issued_at
            || self.result_metadata.validate().is_err()
            || self.result_metadata.session_id != self.issued_exchange.session_id
            || self.result_metadata.sequence != self.issued_exchange.sequence
            || self.result_metadata.received_at < self.issued_exchange.issued_at
            || command.is_empty()
            || command.len() > 256 * 1_024
            || command_digest != self.issued_exchange.command_digest
            || result.is_empty()
            || result.len() > 256 * 1_024
            || result_digest != self.result_metadata.result_digest
        {
            return Err(browser_bridge_workflow_error());
        }
        if let Some(runtime_state) = &self.runtime_state {
            runtime_state.validate_for_exchange(&self.issued_exchange)?;
        }
        Ok(())
    }
}

impl fmt::Debug for BrowserBridgeWorkflowResultRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BrowserBridgeWorkflowResultRequest")
            .field("remote_task_id", &"[REDACTED]")
            .field("issued_exchange", &self.issued_exchange)
            .field("command_artifact", &"[REDACTED]")
            .field("workflow_plan", &self.workflow_plan)
            .field("runtime_state", &self.runtime_state)
            .field("result_metadata", &self.result_metadata)
            .field("result_artifact", &"[REDACTED]")
            .field("runtime_binding", &self.runtime_binding)
            .finish()
    }
}

/// One exact next command returned after Provider validation of an
/// intermediate result.
pub struct BrowserBridgeWorkflowNextCommand {
    pub exchange: BrowserBridgeExchange,
    pub command_artifact: SecretValue,
    pub runtime_state: Option<BrowserBridgeWorkflowRuntimeState>,
}

impl BrowserBridgeWorkflowNextCommand {
    fn validate_after(&self, completed: &BrowserBridgeExchange) -> ProviderResult<()> {
        let command = self.command_artifact.expose_secret();
        let digest: [u8; 32] = Sha256::digest(command).into();
        if self.exchange.validate().is_err()
            || self.exchange.state != BrowserBridgeExchangeState::Issued
            || self.exchange.session_id != completed.session_id
            || self.exchange.sequence != completed.sequence.checked_add(1).unwrap_or(0)
            || self.exchange.issued_at < completed.completed_at.unwrap_or(completed.issued_at)
            || command.is_empty()
            || command.len() > 256 * 1_024
            || self.exchange.command_digest != digest
        {
            return Err(browser_bridge_workflow_error());
        }
        if let Some(runtime_state) = &self.runtime_state {
            runtime_state.validate_for_exchange(&self.exchange)?;
        }
        Ok(())
    }
}

impl fmt::Debug for BrowserBridgeWorkflowNextCommand {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BrowserBridgeWorkflowNextCommand")
            .field("exchange", &self.exchange)
            .field("command_artifact", &"[REDACTED]")
            .field("runtime_state", &self.runtime_state)
            .finish()
    }
}

/// Provider-validated workflow transition ready for Core's atomic Storage
/// boundary.
pub enum BrowserBridgeWorkflowResult {
    Intermediate {
        completed_exchange: BrowserBridgeExchange,
        next: Box<BrowserBridgeWorkflowNextCommand>,
    },
    ExecutionTerminal {
        completed_exchange: BrowserBridgeExchange,
        verified_progress: RemoteProgress,
    },
}

impl BrowserBridgeWorkflowResult {
    /// # Errors
    ///
    /// Rejects a mismatched completion, non-contiguous next command or stale
    /// terminal verification.
    pub fn try_intermediate(
        completed_exchange: BrowserBridgeExchange,
        next: BrowserBridgeWorkflowNextCommand,
        issued: &BrowserBridgeExchange,
        result: &BrowserBridgeResultArtifactMetadata,
    ) -> ProviderResult<Self> {
        validate_workflow_completion(&completed_exchange, issued, result)?;
        next.validate_after(&completed_exchange)?;
        Ok(Self::Intermediate {
            completed_exchange,
            next: Box::new(next),
        })
    }

    /// # Errors
    ///
    /// Rejects a mismatched completion or progress observed before the raw
    /// terminal result was received.
    pub fn try_execution_terminal(
        completed_exchange: BrowserBridgeExchange,
        verified_progress: RemoteProgress,
        issued: &BrowserBridgeExchange,
        result: &BrowserBridgeResultArtifactMetadata,
    ) -> ProviderResult<Self> {
        validate_workflow_completion(&completed_exchange, issued, result)?;
        if verified_progress.updated_at < result.received_at
            || verified_progress
                .percent
                .is_some_and(|percent| percent > 100)
        {
            return Err(browser_bridge_workflow_error());
        }
        Ok(Self::ExecutionTerminal {
            completed_exchange,
            verified_progress,
        })
    }
}

impl fmt::Debug for BrowserBridgeWorkflowResult {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Intermediate {
                completed_exchange,
                next,
            } => formatter
                .debug_struct("BrowserBridgeWorkflowResult::Intermediate")
                .field("completed_exchange", completed_exchange)
                .field("next", next)
                .finish(),
            Self::ExecutionTerminal {
                completed_exchange,
                verified_progress,
            } => formatter
                .debug_struct("BrowserBridgeWorkflowResult::ExecutionTerminal")
                .field("completed_exchange", completed_exchange)
                .field("verified_progress", verified_progress)
                .finish(),
        }
    }
}

fn validate_workflow_completion(
    completed: &BrowserBridgeExchange,
    issued: &BrowserBridgeExchange,
    result: &BrowserBridgeResultArtifactMetadata,
) -> ProviderResult<()> {
    if issued.validate().is_err()
        || issued.state != BrowserBridgeExchangeState::Issued
        || completed.validate().is_err()
        || completed.state != BrowserBridgeExchangeState::Completed
        || completed.session_id != issued.session_id
        || completed.sequence != issued.sequence
        || completed.command_type != issued.command_type
        || completed.command_digest != issued.command_digest
        || completed.issued_at != issued.issued_at
        || completed.session_id != result.session_id
        || completed.sequence != result.sequence
        || completed.result_type.as_deref() != Some(result.result_type.as_str())
        || completed.result_digest != Some(result.result_digest)
        || completed.completed_at != Some(result.received_at)
    {
        Err(browser_bridge_workflow_error())
    } else {
        Ok(())
    }
}

fn valid_browser_bridge_type(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.trim() == value
        && value.is_ascii()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn browser_bridge_workflow_error() -> crate::ProviderError {
    crate::ProviderError::new(
        crate::ProviderErrorKind::ProtocolDrift,
        "BrowserBridge workflow evidence is incomplete, changed or foreign",
    )
}

#[cfg(test)]
mod browser_bridge_workflow_result_tests {
    use asterism_domain::BrowserBridgeSessionId;
    use chrono::{Duration, Utc};

    use super::*;

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one fixture proves request, intermediate, terminal, foreign-command and stale-readback bindings"
    )]
    fn workflow_evidence_transitions_are_exact_and_redacted() {
        let now = Utc::now();
        let session_id = BrowserBridgeSessionId::new();
        let command = SecretValue::new(b"command-one".to_vec());
        let issued = BrowserBridgeExchange::issue(
            session_id,
            1,
            "uai.browser.command".to_owned(),
            Sha256::digest(command.expose_secret()).into(),
            now,
        )
        .unwrap();
        let result = SecretValue::new(b"intermediate-result".to_vec());
        let result_metadata = BrowserBridgeResultArtifactMetadata {
            session_id,
            sequence: 1,
            result_type: "uai.browser.event".to_owned(),
            result_digest: Sha256::digest(result.expose_secret()).into(),
            received_at: now + Duration::seconds(1),
        };
        let state = SecretValue::new(b"cursor-state".to_vec());
        let state_metadata = BrowserBridgeRuntimeStateMetadata {
            session_id,
            sequence: 1,
            state_type: "uai.browser.cursor.v4".to_owned(),
            state_digest: Sha256::digest(state.expose_secret()).into(),
            stored_at: now,
        };
        let request = BrowserBridgeWorkflowResultRequest {
            remote_task_id: "group:course:unit:task".to_owned(),
            issued_exchange: issued.clone(),
            command_artifact: command,
            workflow_plan: Some(
                BrowserBridgeWorkflowPlanArtifact::try_new(
                    "uai.browser.batch.v1".to_owned(),
                    SecretValue::new(b"batch-plan-secret".to_vec()),
                )
                .unwrap(),
            ),
            runtime_state: Some(BrowserBridgeWorkflowRuntimeState {
                metadata: state_metadata,
                artifact: state,
            }),
            result_metadata: result_metadata.clone(),
            result_artifact: result,
            runtime_binding: BrowserBridgeRuntimeBinding {
                session_id,
                observed_origin: "https://ucontent.unipus.cn".to_owned(),
                frame_id: "top-frame:1".to_owned(),
                bound_at: now,
            },
        };
        request.validate().unwrap();
        let debug = format!("{request:?}");
        assert!(!debug.contains("batch-plan-secret"));
        assert!(!debug.contains("intermediate-result"));

        let mut completed = issued.clone();
        completed
            .complete(
                result_metadata.result_type.clone(),
                result_metadata.result_digest,
                result_metadata.received_at,
            )
            .unwrap();
        let next_command = SecretValue::new(b"command-two".to_vec());
        let next_exchange = BrowserBridgeExchange::issue(
            session_id,
            2,
            "uai.browser.command".to_owned(),
            Sha256::digest(next_command.expose_secret()).into(),
            result_metadata.received_at,
        )
        .unwrap();
        BrowserBridgeWorkflowResult::try_intermediate(
            completed.clone(),
            BrowserBridgeWorkflowNextCommand {
                exchange: next_exchange,
                command_artifact: next_command,
                runtime_state: None,
            },
            &issued,
            &result_metadata,
        )
        .unwrap();

        let verified = RemoteProgress {
            remote_state: RemoteState::InProgress,
            percent: Some(50),
            duration_seconds: Some(700),
            updated_at: result_metadata.received_at,
        };
        BrowserBridgeWorkflowResult::try_execution_terminal(
            completed.clone(),
            verified.clone(),
            &issued,
            &result_metadata,
        )
        .unwrap();
        let mut foreign_issued = issued.clone();
        foreign_issued.command_digest = [9; 32];
        assert!(
            BrowserBridgeWorkflowResult::try_execution_terminal(
                completed.clone(),
                verified,
                &foreign_issued,
                &result_metadata,
            )
            .is_err()
        );
        assert!(
            BrowserBridgeWorkflowResult::try_execution_terminal(
                completed,
                RemoteProgress {
                    updated_at: now,
                    ..RemoteProgress {
                        remote_state: RemoteState::InProgress,
                        percent: Some(50),
                        duration_seconds: Some(700),
                        updated_at: result_metadata.received_at,
                    }
                },
                &issued,
                &result_metadata,
            )
            .is_err()
        );
    }
}

/// Complete Core-owned evidence supplied to Provider terminal-result
/// validation after encrypted recovery.
pub struct BrowserBridgeCredentialResultRequest<'a> {
    pub remote_task_id: &'a str,
    pub issued_exchange: &'a BrowserBridgeExchange,
    pub command_artifact: &'a SecretValue,
    pub result_metadata: &'a BrowserBridgeResultArtifactMetadata,
    pub result_artifact: &'a SecretValue,
    pub runtime_binding: &'a BrowserBridgeRuntimeBinding,
}

impl BrowserBridgeCredentialResultRequest<'_> {
    /// Validates shared artifact, exchange and runtime bindings before Provider
    /// protocol parsing.
    ///
    /// # Errors
    ///
    /// Rejects incomplete, oversized, digest-mismatched or cross-session
    /// evidence.
    pub fn validate(&self) -> ProviderResult<()> {
        let command = self.command_artifact.expose_secret();
        let result = self.result_artifact.expose_secret();
        let valid_remote_task = !self.remote_task_id.is_empty()
            && self.remote_task_id.len() <= 2_048
            && self.remote_task_id.trim() == self.remote_task_id
            && !self.remote_task_id.chars().any(char::is_control);
        let command_digest: [u8; 32] = Sha256::digest(command).into();
        let result_digest: [u8; 32] = Sha256::digest(result).into();
        if !valid_remote_task
            || self.issued_exchange.validate().is_err()
            || self.issued_exchange.state != BrowserBridgeExchangeState::Issued
            || self.runtime_binding.validate().is_err()
            || self.runtime_binding.session_id != self.issued_exchange.session_id
            || self.runtime_binding.bound_at > self.issued_exchange.issued_at
            || self.result_metadata.validate().is_err()
            || self.result_metadata.session_id != self.issued_exchange.session_id
            || self.result_metadata.sequence != self.issued_exchange.sequence
            || self.result_metadata.received_at < self.issued_exchange.issued_at
            || command.is_empty()
            || command.len() > 256 * 1_024
            || command_digest != self.issued_exchange.command_digest
            || result.is_empty()
            || result.len() > 256 * 1_024
            || result_digest != self.result_metadata.result_digest
        {
            Err(crate::ProviderError::new(
                crate::ProviderErrorKind::ProtocolDrift,
                "BrowserBridge credential result evidence is incomplete or foreign",
            ))
        } else {
            Ok(())
        }
    }
}

impl fmt::Debug for BrowserBridgeCredentialResultRequest<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BrowserBridgeCredentialResultRequest")
            .field("remote_task_id", &"[REDACTED]")
            .field("issued_exchange", self.issued_exchange)
            .field("command_artifact", &"[REDACTED]")
            .field("result_metadata", self.result_metadata)
            .field("result_artifact", &"[REDACTED]")
            .field("runtime_binding", self.runtime_binding)
            .finish()
    }
}

/// Provider-validated credential replacement paired with the exact completed
/// exchange Core must commit atomically.
pub struct BrowserBridgeCredentialResult {
    replacement: CredentialReplacement,
    completed_exchange: BrowserBridgeExchange,
}

impl BrowserBridgeCredentialResult {
    /// # Errors
    ///
    /// Rejects a non-terminal exchange or malformed/duplicate Provider secret
    /// fields.
    pub fn try_new(
        replacement: CredentialReplacement,
        completed_exchange: BrowserBridgeExchange,
    ) -> ProviderResult<Self> {
        let mut purposes = Vec::with_capacity(replacement.fields.len());
        let valid_fields = !replacement.fields.is_empty()
            && replacement.fields.len() <= 16
            && replacement.fields.iter().all(|field| {
                purposes.push(field.purpose);
                field.purpose.is_provider_credential()
                    && !field.value.expose_secret().is_empty()
                    && field.value.expose_secret().len() <= 1024 * 1024
            });
        purposes.sort_unstable_by_key(|purpose| capture_purpose_rank(*purpose));
        if !valid_fields
            || purposes.windows(2).any(|pair| pair[0] == pair[1])
            || completed_exchange.validate().is_err()
            || completed_exchange.state != BrowserBridgeExchangeState::Completed
        {
            return Err(crate::ProviderError::new(
                crate::ProviderErrorKind::ProtocolDrift,
                "BrowserBridge credential result is invalid or incomplete",
            ));
        }
        Ok(Self {
            replacement,
            completed_exchange,
        })
    }

    pub fn into_parts(self) -> (CredentialReplacement, BrowserBridgeExchange) {
        (self.replacement, self.completed_exchange)
    }
}

impl fmt::Debug for BrowserBridgeCredentialResult {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BrowserBridgeCredentialResult")
            .field("session_kind", &self.replacement.session_kind)
            .field("credential_count", &self.replacement.fields.len())
            .field("completed_exchange", &self.completed_exchange)
            .finish()
    }
}

#[cfg(test)]
mod browser_bridge_credential_result_tests {
    use asterism_domain::BrowserBridgeSessionId;
    use chrono::{Duration, Utc};

    use super::*;

    #[test]
    fn shared_evidence_and_terminal_credentials_are_exact_and_redacted() {
        let now = Utc::now();
        let session_id = BrowserBridgeSessionId::new();
        let command = SecretValue::new(b"bounded-command".to_vec());
        let result = SecretValue::new(b"captured-secret-result".to_vec());
        let command_digest = Sha256::digest(command.expose_secret()).into();
        let result_digest = Sha256::digest(result.expose_secret()).into();
        let exchange = BrowserBridgeExchange::issue(
            session_id,
            1,
            "cidaren.capture.snapshot".to_owned(),
            command_digest,
            now,
        )
        .unwrap();
        let metadata = BrowserBridgeResultArtifactMetadata {
            session_id,
            sequence: 1,
            result_type: "cidaren.capture.snapshot.result".to_owned(),
            result_digest,
            received_at: now + Duration::seconds(1),
        };
        let binding = BrowserBridgeRuntimeBinding {
            session_id,
            observed_origin: "https://app.vocabgo.com".to_owned(),
            frame_id: "top-frame:1".to_owned(),
            bound_at: now,
        };
        let request = BrowserBridgeCredentialResultRequest {
            remote_task_id: "class-task:1",
            issued_exchange: &exchange,
            command_artifact: &command,
            result_metadata: &metadata,
            result_artifact: &result,
            runtime_binding: &binding,
        };
        assert_eq!(request.validate(), Ok(()));
        assert!(!format!("{request:?}").contains("captured-secret-result"));

        let mut changed_metadata = metadata.clone();
        changed_metadata.sequence = 2;
        assert!(
            BrowserBridgeCredentialResultRequest {
                result_metadata: &changed_metadata,
                ..request
            }
            .validate()
            .is_err()
        );

        let mut completed = exchange;
        completed
            .complete(
                metadata.result_type.clone(),
                metadata.result_digest,
                metadata.received_at,
            )
            .unwrap();
        let accepted = BrowserBridgeCredentialResult::try_new(
            CredentialReplacement {
                session_kind: SessionKind::Composite,
                fields: vec![CredentialField {
                    purpose: SecretPurpose::ProviderAccessToken,
                    value: SecretValue::new(b"provider-token".to_vec()),
                }],
            },
            completed,
        )
        .unwrap();
        let debug = format!("{accepted:?}");
        assert!(debug.contains("credential_count: 1"));
        assert!(!debug.contains("provider-token"));
    }
}

#[async_trait]
pub trait ExecutionEventSink {
    async fn report(&self, update: ProviderProgress) -> ProviderResult<()>;

    async fn log(&self, event: ProviderExecutionLog) -> ProviderResult<()>;

    /// Returns the durable mutation boundary supplied by Core for a real
    /// execution attempt. Lightweight or read-only callers provide none.
    fn mutation_sink(&self) -> Option<&(dyn ExecutionMutationSink + Send + Sync)> {
        None
    }
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

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct AuthChallenge {
    pub session_id: AuthSessionId,
    pub method: AuthMethod,
    pub waiting_for: WaitingUserState,
    pub user_action: Option<String>,
    pub expires_at: Option<Timestamp>,
    pub external_oauth: Option<ExternalOauthAuthorization>,
}

impl fmt::Debug for AuthChallenge {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AuthChallenge")
            .field("session_id", &self.session_id)
            .field("method", &self.method)
            .field("waiting_for", &self.waiting_for)
            .field(
                "user_action",
                &self.user_action.as_ref().map(|_| "[REDACTED]"),
            )
            .field("expires_at", &self.expires_at)
            .field("external_oauth", &self.external_oauth)
            .finish()
    }
}

/// Provider-private state for one restart-safe interactive authentication
/// flow. Core persists the plaintext only through an encrypted, Provider-
/// scoped continuation repository.
pub struct ProviderInteractiveAuthContinuation {
    continuation_type: String,
    continuation_digest: [u8; 32],
    phase: String,
    value: SecretValue,
    ttl_seconds: u64,
    maximum_polls: u32,
}

impl ProviderInteractiveAuthContinuation {
    /// Creates a bounded continuation and derives its digest from the exact
    /// plaintext bytes Core will encrypt.
    ///
    /// # Errors
    ///
    /// Rejects foreign labels, empty or oversized values, unsafe lifetimes and
    /// unbounded poll counts.
    pub fn try_new(
        provider_id: &ProviderId,
        continuation_type: impl Into<String>,
        phase: impl Into<String>,
        value: SecretValue,
        ttl_seconds: u64,
        maximum_polls: u32,
    ) -> ProviderResult<Self> {
        let continuation_type = continuation_type.into();
        let phase = phase.into();
        let value_length = value.expose_secret().len();
        if !valid_provider_label(provider_id, &continuation_type)
            || !valid_provider_label(provider_id, &phase)
            || value_length == 0
            || value_length > MAX_INTERACTIVE_AUTH_CONTINUATION_BYTES
            || ttl_seconds == 0
            || ttl_seconds > MAX_INTERACTIVE_AUTH_TTL_SECONDS
            || maximum_polls == 0
            || maximum_polls > MAX_INTERACTIVE_AUTH_POLLS
        {
            return Err(crate::ProviderError::new(
                crate::ProviderErrorKind::InvalidResponse,
                "Provider interactive authentication continuation is invalid",
            ));
        }
        let continuation_digest = Sha256::digest(value.expose_secret()).into();
        Ok(Self {
            continuation_type,
            continuation_digest,
            phase,
            value,
            ttl_seconds,
            maximum_polls,
        })
    }

    pub fn continuation_type(&self) -> &str {
        &self.continuation_type
    }

    pub const fn continuation_digest(&self) -> [u8; 32] {
        self.continuation_digest
    }

    pub fn phase(&self) -> &str {
        &self.phase
    }

    pub const fn value(&self) -> &SecretValue {
        &self.value
    }

    pub const fn ttl_seconds(&self) -> u64 {
        self.ttl_seconds
    }

    pub const fn maximum_polls(&self) -> u32 {
        self.maximum_polls
    }

    pub fn into_parts(self) -> (String, String, SecretValue, u64, u32) {
        (
            self.continuation_type,
            self.phase,
            self.value,
            self.ttl_seconds,
            self.maximum_polls,
        )
    }
}

impl fmt::Debug for ProviderInteractiveAuthContinuation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProviderInteractiveAuthContinuation")
            .field("continuation_type", &self.continuation_type)
            .field("continuation_digest", &self.continuation_digest)
            .field("phase", &self.phase)
            .field("value", &"[REDACTED]")
            .field("ttl_seconds", &self.ttl_seconds)
            .field("maximum_polls", &self.maximum_polls)
            .finish()
    }
}

/// Decrypted continuation exposed only during one Provider call authorized by
/// a Core poll claim.
pub struct ResolvedProviderInteractiveAuthContinuation<'a> {
    pub continuation_type: &'a str,
    pub continuation_digest: [u8; 32],
    pub phase: &'a str,
    pub revision: u32,
    pub poll_sequence: u32,
    pub value: &'a SecretValue,
}

impl fmt::Debug for ResolvedProviderInteractiveAuthContinuation<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ResolvedProviderInteractiveAuthContinuation")
            .field("continuation_type", &self.continuation_type)
            .field("continuation_digest", &self.continuation_digest)
            .field("phase", &self.phase)
            .field("revision", &self.revision)
            .field("poll_sequence", &self.poll_sequence)
            .field("value", &"[REDACTED]")
            .finish()
    }
}

#[derive(Debug)]
pub struct ProviderInteractiveAuthBegin {
    pub challenge: AuthChallenge,
    pub continuation: ProviderInteractiveAuthContinuation,
}

impl ProviderInteractiveAuthBegin {
    /// Validates the method/state matrix specific to Provider-native QR flows.
    ///
    /// # Errors
    ///
    /// Rejects non-QR challenges and external-OAuth state mixed into a native
    /// continuation.
    pub fn validate(&self) -> ProviderResult<()> {
        if self.challenge.method != AuthMethod::QrCode
            || !matches!(
                self.challenge.waiting_for,
                WaitingUserState::QrScan | WaitingUserState::QrConfirm
            )
            || self.challenge.external_oauth.is_some()
        {
            Err(crate::ProviderError::new(
                crate::ProviderErrorKind::InvalidResponse,
                "Provider interactive authentication challenge is invalid",
            ))
        } else {
            Ok(())
        }
    }
}

/// One definite response to a claimed interactive authentication poll. An
/// authenticated result carries a terminal continuation so a crash before
/// credential commit can resume without replaying the remote exchange.
#[derive(Debug)]
pub enum ProviderInteractiveAuthPollOutcome {
    Waiting {
        waiting_for: WaitingUserState,
        user_action: Option<String>,
        continuation: ProviderInteractiveAuthContinuation,
        result_digest: [u8; 32],
    },
    Authenticated {
        continuation: ProviderInteractiveAuthContinuation,
        result_digest: [u8; 32],
    },
    Rejected {
        result_digest: [u8; 32],
    },
    Expired {
        result_digest: [u8; 32],
    },
}

impl ProviderInteractiveAuthPollOutcome {
    /// Validates common result and continuation invariants before persistence.
    ///
    /// # Errors
    ///
    /// Rejects zero evidence digests and waiting states outside an interactive
    /// QR flow.
    pub fn validate(&self) -> ProviderResult<()> {
        let (result_digest, waiting_for, user_action) = match self {
            Self::Waiting {
                waiting_for,
                user_action,
                result_digest,
                ..
            } => (*result_digest, Some(*waiting_for), user_action.as_deref()),
            Self::Authenticated { result_digest, .. }
            | Self::Rejected { result_digest }
            | Self::Expired { result_digest } => (*result_digest, None, None),
        };
        if result_digest == [0; 32]
            || waiting_for.is_some_and(|waiting_for| {
                !matches!(
                    waiting_for,
                    WaitingUserState::QrScan | WaitingUserState::QrConfirm
                )
            })
            || user_action.is_some_and(|action| {
                action.is_empty()
                    || action.len() > 4_096
                    || action.trim() != action
                    || action.chars().any(char::is_control)
            })
        {
            Err(crate::ProviderError::new(
                crate::ProviderErrorKind::InvalidResponse,
                "Provider interactive authentication poll result is invalid",
            ))
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod interactive_auth_tests {
    use asterism_domain::{AuthSessionId, ProviderId};

    use super::*;

    #[test]
    fn continuation_is_bounded_provider_scoped_and_redacted() {
        let provider_id = ProviderId::new("chaoxing").unwrap();
        let continuation = ProviderInteractiveAuthContinuation::try_new(
            &provider_id,
            "chaoxing.qr.v1",
            "chaoxing.qr-scan",
            SecretValue::new(b"uuid-cookie-secret".to_vec()),
            300,
            900,
        )
        .unwrap();
        assert_eq!(continuation.maximum_polls(), 900);
        let debug = format!("{continuation:?}");
        assert!(!debug.contains("uuid-cookie-secret"));
        assert!(
            ProviderInteractiveAuthContinuation::try_new(
                &provider_id,
                "uai.qr.v1",
                "chaoxing.qr-scan",
                SecretValue::new(b"foreign".to_vec()),
                300,
                900,
            )
            .is_err()
        );
        assert!(
            ProviderInteractiveAuthContinuation::try_new(
                &provider_id,
                "chaoxing.qr.v1",
                "chaoxing.qr-scan",
                SecretValue::new(b"unbounded".to_vec()),
                300,
                MAX_INTERACTIVE_AUTH_POLLS + 1,
            )
            .is_err()
        );
    }

    #[test]
    fn interactive_begin_and_poll_enforce_qr_state_matrix() {
        let provider_id = ProviderId::new("chaoxing").unwrap();
        let continuation = || {
            ProviderInteractiveAuthContinuation::try_new(
                &provider_id,
                "chaoxing.qr.v1",
                "chaoxing.qr-scan",
                SecretValue::new(b"bound-state".to_vec()),
                300,
                10,
            )
            .unwrap()
        };
        let session_id = AuthSessionId::new();
        let valid = ProviderInteractiveAuthBegin {
            challenge: AuthChallenge {
                session_id,
                method: AuthMethod::QrCode,
                waiting_for: WaitingUserState::QrScan,
                user_action: Some("https://passport2.chaoxing.com/toauthlogin?opaque".to_owned()),
                expires_at: None,
                external_oauth: None,
            },
            continuation: continuation(),
        };
        valid.validate().unwrap();
        assert!(!format!("{:?}", valid.challenge).contains("toauthlogin"));

        let invalid = ProviderInteractiveAuthBegin {
            challenge: AuthChallenge {
                method: AuthMethod::Password,
                ..valid.challenge.clone()
            },
            continuation: continuation(),
        };
        assert!(invalid.validate().is_err());
        let waiting = ProviderInteractiveAuthPollOutcome::Waiting {
            waiting_for: WaitingUserState::CredentialInput,
            user_action: None,
            continuation: continuation(),
            result_digest: [1; 32],
        };
        assert!(waiting.validate().is_err());
        assert!(
            ProviderInteractiveAuthPollOutcome::Rejected {
                result_digest: [0; 32]
            }
            .validate()
            .is_err()
        );
    }
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
    /// Optional credential-free Provider-private planning evidence frozen in
    /// the same scheduling transaction. It is read-only context and never
    /// broadens the active capability authority above.
    #[serde(skip)]
    pub provider_plan_artifact: Option<ProviderExecutionPlanArtifact>,
}

impl ExecutionRequest {
    /// Validates that active mutation authority is an exact, bounded slice of
    /// the immutable execution plan and that the current step is unambiguous.
    pub fn has_valid_capability_step(&self) -> bool {
        let Some(start) = usize::from(self.capability_step_position).checked_sub(1) else {
            return false;
        };
        let Some(end) = start.checked_add(self.requested_capabilities.len()) else {
            return false;
        };
        !self.requested_capabilities.is_empty()
            && !self.capability_plan.is_empty()
            && self.capability_plan.len() <= 5
            && end <= self.capability_plan.len()
            && self.capability_plan[start..end] == self.requested_capabilities
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
            provider_plan_artifact: None,
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

        let mut atomic_group = request();
        atomic_group.capability_step_position = 1;
        atomic_group.requested_capabilities = atomic_group.capability_plan.clone();
        assert!(atomic_group.has_valid_capability_step());

        let mut missing_plan = request();
        missing_plan.capability_plan.clear();
        assert!(!missing_plan.has_valid_capability_step());
    }

    #[test]
    fn private_plan_artifact_stays_out_of_generic_request_serialization() {
        let mut request = request();
        request.provider_plan_artifact = Some(
            ProviderExecutionPlanArtifact::try_new(
                ProviderId::new("test").unwrap(),
                "test.execution-plan.v1",
                serde_json::json!({"target_seconds": 120}),
            )
            .unwrap(),
        );

        let encoded = serde_json::to_string(&request).unwrap();
        assert!(!encoded.contains("provider_plan_artifact"));
        assert!(!encoded.contains("target_seconds"));
        let decoded: ExecutionRequest = serde_json::from_str(&encoded).unwrap();
        assert!(decoded.provider_plan_artifact.is_none());
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
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BrowserBridgeReadSource {
    /// Retains only this exact lower-case request header from this origin.
    RequestHeader { origin: String, name: String },
    /// Authorizes an exact key read from the origin's local storage.
    LocalStorage { origin: String, key: String },
    /// Authorizes an exact key read from the origin's session storage.
    SessionStorage { origin: String, key: String },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BrowserSessionSpec {
    /// Provider-owned wire revision for the browser policy represented by this
    /// immutable snapshot.
    pub version: u32,
    /// Exact credential-free HTTPS route the isolated helper opens first.
    pub start_url: String,
    pub isolation_key: String,
    pub allowed_origins: Vec<String>,
    /// Credential-free browser-state read authority frozen before Chromium is
    /// launched. Navigation authority never implies read authority.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub read_sources: Vec<BrowserBridgeReadSource>,
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
    /// unsafe start route, malformed isolation key, unsafe origin or
    /// duplicate/unbounded origin set.
    pub fn validate(&self) -> Result<(), BrowserSessionSpecError> {
        let start_origin =
            https_origin(&self.start_url, false).map_err(|_| BrowserSessionSpecError::Invalid)?;
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
            || self.read_sources.len() > MAX_CAPTURE_SOURCES
            || !self
                .allowed_origins
                .iter()
                .any(|origin| origin == &start_origin)
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
            || !valid_browser_bridge_read_sources(&self.read_sources, &origins)
            || serde_json::to_vec(self).map_or(true, |encoded| encoded.len() > 4 * 1_024)
        {
            Err(BrowserSessionSpecError::Invalid)
        } else {
            Ok(())
        }
    }

    /// Computes the stable digest Core freezes onto a durable helper session.
    ///
    /// # Errors
    ///
    /// Returns [`BrowserSessionSpecError::Invalid`] when the specification is
    /// invalid or cannot be serialized after validation.
    pub fn digest(&self) -> Result<[u8; 32], BrowserSessionSpecError> {
        self.validate()?;
        let encoded = serde_json::to_vec(self).map_err(|_| BrowserSessionSpecError::Invalid)?;
        Ok(Sha256::digest(encoded).into())
    }
}

fn valid_browser_bridge_read_sources(
    sources: &[BrowserBridgeReadSource],
    allowed_origins: &[String],
) -> bool {
    let mut identities = Vec::with_capacity(sources.len());
    for source in sources {
        let (kind, origin, name) = match source {
            BrowserBridgeReadSource::RequestHeader { origin, name } => {
                if name.to_ascii_lowercase() != *name || validate_header_name(name).is_err() {
                    return false;
                }
                (0_u8, origin, name)
            }
            BrowserBridgeReadSource::LocalStorage { origin, key } => {
                if !valid_bounded_capture_text(key, 128) {
                    return false;
                }
                (1_u8, origin, key)
            }
            BrowserBridgeReadSource::SessionStorage { origin, key } => {
                if !valid_bounded_capture_text(key, 128) {
                    return false;
                }
                (2_u8, origin, key)
            }
        };
        if !allowed_origins.iter().any(|allowed| allowed == origin) {
            return false;
        }
        identities.push((kind, origin, name));
    }
    identities.sort_unstable();
    !identities.windows(2).any(|pair| pair[0] == pair[1])
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
            start_url: "https://provider.example/task/a1".to_owned(),
            isolation_key: "provider-task-a1".to_owned(),
            allowed_origins: vec!["https://provider.example".to_owned()],
            read_sources: Vec::new(),
            headless: false,
        }
    }

    #[test]
    fn exact_https_origins_and_bounded_isolation_are_required() {
        assert_eq!(spec().validate(), Ok(()));
        assert_eq!(spec().digest(), spec().digest());
        let legacy_json = serde_json::to_string(&spec()).unwrap();
        assert!(!legacy_json.contains("read_sources"));
        assert_eq!(
            serde_json::from_str::<BrowserSessionSpec>(&legacy_json).unwrap(),
            spec()
        );

        let mut changed_start = spec();
        changed_start.start_url = "https://provider.example/task/a2".to_owned();
        assert_ne!(spec().digest(), changed_start.digest());

        let mut duplicate = spec();
        duplicate
            .allowed_origins
            .push("https://provider.example".to_owned());
        assert_eq!(duplicate.validate(), Err(BrowserSessionSpecError::Invalid));

        let mut route = spec();
        route.allowed_origins[0].push_str("/task");
        assert_eq!(route.validate(), Err(BrowserSessionSpecError::Invalid));

        let mut foreign_start = spec();
        foreign_start.start_url = "https://foreign.example/task/a1".to_owned();
        assert_eq!(
            foreign_start.validate(),
            Err(BrowserSessionSpecError::Invalid)
        );

        let mut unsafe_start = spec();
        unsafe_start.start_url = "http://provider.example/task/a1".to_owned();
        assert_eq!(
            unsafe_start.validate(),
            Err(BrowserSessionSpecError::Invalid)
        );

        let mut secret_shaped = spec();
        secret_shaped.isolation_key = "Provider:token".to_owned();
        assert_eq!(
            secret_shaped.validate(),
            Err(BrowserSessionSpecError::Invalid)
        );

        let mut authorized = spec();
        authorized.read_sources = vec![
            BrowserBridgeReadSource::RequestHeader {
                origin: "https://provider.example".to_owned(),
                name: "authorization".to_owned(),
            },
            BrowserBridgeReadSource::LocalStorage {
                origin: "https://provider.example".to_owned(),
                key: "SESSION_INFO".to_owned(),
            },
        ];
        assert_eq!(authorized.validate(), Ok(()));
        assert_ne!(authorized.digest(), spec().digest());

        let mut mixed_case_header = authorized.clone();
        let BrowserBridgeReadSource::RequestHeader { name, .. } =
            &mut mixed_case_header.read_sources[0]
        else {
            unreachable!();
        };
        *name = "Authorization".to_owned();
        assert_eq!(
            mixed_case_header.validate(),
            Err(BrowserSessionSpecError::Invalid)
        );

        let mut foreign_read = authorized.clone();
        let BrowserBridgeReadSource::LocalStorage { origin, .. } =
            &mut foreign_read.read_sources[1]
        else {
            unreachable!();
        };
        *origin = "https://foreign.example".to_owned();
        assert_eq!(
            foreign_read.validate(),
            Err(BrowserSessionSpecError::Invalid)
        );

        authorized
            .read_sources
            .push(authorized.read_sources[0].clone());
        assert_eq!(authorized.validate(), Err(BrowserSessionSpecError::Invalid));
    }
}
