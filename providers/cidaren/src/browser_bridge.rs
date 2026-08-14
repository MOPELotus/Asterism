use std::{fmt, sync::Arc};

use asterism_domain::{
    BrowserBridgeExchange, BrowserBridgeExchangeState, BrowserBridgeSessionId, TaskCapability,
    Timestamp,
};
use asterism_provider_api::{
    BrowserBridgeCapability, BrowserSessionSpec, CredentialReplacement, ProviderContext,
    ProviderError, ProviderErrorKind, ProviderIdentity, ProviderMetadata, ProviderResult,
    TaskDetailCapability,
};
use asterism_secrets::SecretValue;
use async_trait::async_trait;
use sha2::{Digest, Sha256};

use crate::{
    CidarenBrowserCommandEnvelope, CidarenBrowserResultDocument, CidarenCaptureMode,
    CidarenCaptureSnapshot, EncodedCidarenBrowserCommandArtifact, metadata::development_metadata,
    parse_browser_event,
};

const CIDAREN_ORIGIN: &str = "https://app.vocabgo.com";
const MAX_REMOTE_COMPONENT_BYTES: usize = 256;

/// Account-isolated visible browser boundary for the Cidaren H5 client.
///
/// The bridge intentionally permits only the audited H5 origin. Capture
/// helpers may obtain the `UserToken` and the `CDR_LOGIN_INFO` crypto context
/// from that origin, but neither value is represented in this specification.
pub struct CidarenBrowserBridge {
    metadata: ProviderMetadata,
    details: Arc<dyn TaskDetailCapability>,
}

/// One immutable Provider command paired with Core's durable issued metadata.
///
/// The command contains no captured credential material. Its fields stay
/// private so callers cannot accidentally persist a digest for a different
/// command than the one dispatched to the helper.
#[derive(Debug)]
pub struct CidarenCaptureExchangeIssued {
    command: CidarenBrowserCommandEnvelope,
    command_artifact: EncodedCidarenBrowserCommandArtifact,
    exchange: BrowserBridgeExchange,
}

impl CidarenCaptureExchangeIssued {
    pub const fn command(&self) -> &CidarenBrowserCommandEnvelope {
        &self.command
    }

    pub const fn exchange(&self) -> &BrowserBridgeExchange {
        &self.exchange
    }

    pub const fn command_artifact(&self) -> &EncodedCidarenBrowserCommandArtifact {
        &self.command_artifact
    }

    /// Transfers the typed helper payload, encrypted recovery material and
    /// exact ledger row as one ownership boundary. Core can persist the
    /// artifact/exchange before dispatching the command without serializing a
    /// second, potentially different payload.
    pub fn into_parts(
        self,
    ) -> (
        CidarenBrowserCommandEnvelope,
        EncodedCidarenBrowserCommandArtifact,
        BrowserBridgeExchange,
    ) {
        (self.command, self.command_artifact, self.exchange)
    }
}

/// One Provider-validated Capture snapshot and its terminal Core metadata.
#[derive(Debug)]
pub struct CidarenCaptureExchangeCompleted {
    snapshot: CidarenCaptureSnapshot,
    exchange: BrowserBridgeExchange,
}

impl CidarenCaptureExchangeCompleted {
    pub const fn snapshot(&self) -> &CidarenCaptureSnapshot {
        &self.snapshot
    }

    pub const fn exchange(&self) -> &BrowserBridgeExchange {
        &self.exchange
    }

    /// Transfers the zeroizing credential material together with the exact
    /// completed ledger metadata so acceptance cannot silently discard either
    /// half of the result.
    pub fn into_parts(self) -> (CidarenCaptureSnapshot, BrowserBridgeExchange) {
        (self.snapshot, self.exchange)
    }

    /// Converts the validated Capture material while retaining the exact
    /// completed ledger row in the same consuming handoff.
    ///
    /// Core can use this pair as the input to one atomic result/credential
    /// transaction; there is no consuming path that yields secrets while
    /// silently discarding the exchange.
    ///
    /// # Errors
    ///
    /// Returns an internal error if the validated snapshot lost one of its
    /// required source bindings before conversion.
    pub fn into_credential_commit_parts(
        self,
    ) -> ProviderResult<(CredentialReplacement, BrowserBridgeExchange)> {
        let (snapshot, exchange) = self.into_parts();
        Ok((snapshot.into_credential_replacement()?, exchange))
    }
}

impl CidarenBrowserBridge {
    /// Builds the Development browser boundary.
    ///
    /// # Errors
    ///
    /// Returns an internal error if compile-time Provider metadata is invalid.
    pub fn try_new(details: Arc<dyn TaskDetailCapability>) -> ProviderResult<Self> {
        Ok(Self {
            metadata: development_metadata()?,
            details,
        })
    }

    /// Rebinds a fresh Task before creating one typed Capture command.
    ///
    /// The helper session nonce, frame and sequence are supplied by Core's
    /// durable `BrowserBridge` session. This method only validates the current
    /// Provider Task/BrowserSessionSpec policy and constructs the bounded
    /// Provider command; it never starts a browser or persists Capture output.
    ///
    /// # Errors
    ///
    /// Returns a typed error when the account/Task binding, visible origin or
    /// command fields are invalid.
    pub async fn capture_snapshot_command(
        &self,
        context: &ProviderContext,
        remote_task_id: &str,
        session_nonce: String,
        frame_id: String,
        sequence: u32,
        mode: CidarenCaptureMode,
    ) -> ProviderResult<CidarenBrowserCommandEnvelope> {
        let spec = self.browser_session_spec(context, remote_task_id).await?;
        if spec.allowed_origins != [CIDAREN_ORIGIN] || spec.headless {
            return Err(ProviderError::new(
                ProviderErrorKind::ProtocolDrift,
                "Cidaren BrowserBridge policy cannot issue a Capture command",
            ));
        }
        CidarenBrowserCommandEnvelope::capture_snapshot(
            session_nonce,
            frame_id,
            remote_task_id.to_owned(),
            sequence,
            mode,
        )
    }

    /// Builds one typed command together with the exact metadata Core must
    /// durably issue before dispatch.
    ///
    /// The durable session identifier is also the command's opaque session
    /// nonce. This prevents a caller from accidentally recording one session
    /// while dispatching a command bound to another.
    ///
    /// # Errors
    ///
    /// Returns a typed error when fresh Task rebinding fails, the Provider
    /// sequence exceeds its bounded wire representation, or Core exchange
    /// metadata cannot represent the validated command.
    #[expect(
        clippy::too_many_arguments,
        reason = "the Core session, Task, frame, sequence, recipe and issue time are independent bindings"
    )]
    pub async fn capture_snapshot_exchange(
        &self,
        context: &ProviderContext,
        remote_task_id: &str,
        session_id: BrowserBridgeSessionId,
        frame_id: String,
        sequence: u64,
        mode: CidarenCaptureMode,
        issued_at: Timestamp,
    ) -> ProviderResult<CidarenCaptureExchangeIssued> {
        let provider_sequence = u32::try_from(sequence).map_err(|_| {
            ProviderError::new(
                ProviderErrorKind::InvalidResponse,
                "Cidaren BrowserBridge sequence exceeds the Provider boundary",
            )
        })?;
        let command = self
            .capture_snapshot_command(
                context,
                remote_task_id,
                session_id.to_string(),
                frame_id,
                provider_sequence,
                mode,
            )
            .await?;
        let command_artifact = command.encode_artifact()?;
        let exchange = BrowserBridgeExchange::issue(
            session_id,
            sequence,
            CidarenBrowserCommandEnvelope::exchange_type().to_owned(),
            command_artifact.digest(),
            issued_at,
        )
        .map_err(|_| {
            ProviderError::new(
                ProviderErrorKind::Internal,
                "Cidaren command cannot be represented by the durable BrowserBridge exchange",
            )
        })?;
        Ok(CidarenCaptureExchangeIssued {
            command,
            command_artifact,
            exchange,
        })
    }

    /// Freshly rebinds a Task and parses one typed Capture result.
    ///
    /// Core still owns durable command issuance, sequence consumption and
    /// credential commit. This method only applies the Provider's origin,
    /// Task and Capture value validation to the returned transport document.
    ///
    /// # Errors
    ///
    /// Returns a typed error when the fresh Task no longer authorizes the
    /// session, the command binding differs from that Task, or the result is
    /// malformed/foreign.
    pub async fn parse_capture_snapshot_result(
        &self,
        context: &ProviderContext,
        remote_task_id: &str,
        command: &CidarenBrowserCommandEnvelope,
        document: CidarenBrowserResultDocument,
        observed_origin: &str,
    ) -> ProviderResult<CidarenCaptureSnapshot> {
        self.parse_capture_snapshot_result_inner(
            context,
            remote_task_id,
            command,
            document.as_str(),
            observed_origin,
        )
        .await
    }

    async fn parse_capture_snapshot_result_inner(
        &self,
        context: &ProviderContext,
        remote_task_id: &str,
        command: &CidarenBrowserCommandEnvelope,
        document: &str,
        observed_origin: &str,
    ) -> ProviderResult<CidarenCaptureSnapshot> {
        let spec = self.browser_session_spec(context, remote_task_id).await?;
        if spec.allowed_origins != [CIDAREN_ORIGIN]
            || spec.headless
            || command.remote_task_id != remote_task_id
        {
            return Err(ProviderError::new(
                ProviderErrorKind::RemoteChanged,
                "Cidaren Capture result Task binding is stale",
            ));
        }
        let event = parse_browser_event(document, command, observed_origin)?;
        event.into_capture_snapshot(command, observed_origin)
    }

    /// Freshly validates one helper result and attaches its digest to the
    /// immutable issued exchange metadata.
    ///
    /// The returned snapshot remains Provider-private and zeroizing. Only the
    /// completed exchange metadata is suitable for Core persistence; callers
    /// must commit the snapshot through the separate Capture credential path.
    ///
    /// # Errors
    ///
    /// Returns a typed error when fresh Task rebinding fails, the result is
    /// foreign or malformed, or its completion time regresses.
    pub async fn complete_capture_snapshot_exchange(
        &self,
        context: &ProviderContext,
        remote_task_id: &str,
        issued: &CidarenCaptureExchangeIssued,
        document: CidarenBrowserResultDocument,
        observed_origin: &str,
        completed_at: Timestamp,
    ) -> ProviderResult<CidarenCaptureExchangeCompleted> {
        self.complete_capture_snapshot_exchange_inner(
            context,
            remote_task_id,
            issued.command(),
            issued.exchange(),
            document,
            observed_origin,
            completed_at,
        )
        .await
    }

    /// Completes a result after Core resolves the exact encrypted command
    /// artifact for an issued exchange following process recovery.
    ///
    /// The persisted exchange remains the command identity authority. The
    /// helper result cannot choose its recipe, frame, Task or sequence; those
    /// values are reconstructed from the digest-bound Provider artifact.
    ///
    /// # Errors
    ///
    /// Returns a typed error for a non-issued/foreign exchange, artifact
    /// binding drift, stale Task or invalid helper result.
    #[allow(
        clippy::too_many_arguments,
        reason = "the persisted exchange, encrypted artifact, recipe and result observation are independent recovery bindings"
    )]
    pub async fn complete_recovered_capture_snapshot_exchange(
        &self,
        context: &ProviderContext,
        remote_task_id: &str,
        issued_exchange: &BrowserBridgeExchange,
        command_artifact: &SecretValue,
        mode: CidarenCaptureMode,
        document: CidarenBrowserResultDocument,
        observed_origin: &str,
        completed_at: Timestamp,
    ) -> ProviderResult<CidarenCaptureExchangeCompleted> {
        if issued_exchange.validate().is_err()
            || issued_exchange.state != BrowserBridgeExchangeState::Issued
            || issued_exchange.command_type != CidarenBrowserCommandEnvelope::exchange_type()
        {
            return Err(ProviderError::new(
                ProviderErrorKind::ProtocolDrift,
                "Cidaren recovered Capture exchange is stale or foreign",
            ));
        }
        let command = CidarenBrowserCommandEnvelope::decode_artifact_bound(
            command_artifact,
            issued_exchange.command_digest,
            issued_exchange.session_id,
            remote_task_id,
            issued_exchange.sequence,
            mode,
        )?;
        self.complete_capture_snapshot_exchange_inner(
            context,
            remote_task_id,
            &command,
            issued_exchange,
            document,
            observed_origin,
            completed_at,
        )
        .await
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "the resolved command and its result observation retain independent recovery bindings"
    )]
    async fn complete_capture_snapshot_exchange_inner(
        &self,
        context: &ProviderContext,
        remote_task_id: &str,
        command: &CidarenBrowserCommandEnvelope,
        issued_exchange: &BrowserBridgeExchange,
        document: CidarenBrowserResultDocument,
        observed_origin: &str,
        completed_at: Timestamp,
    ) -> ProviderResult<CidarenCaptureExchangeCompleted> {
        let result_digest = document.exchange_digest()?;
        let snapshot = self
            .parse_capture_snapshot_result_inner(
                context,
                remote_task_id,
                command,
                document.as_str(),
                observed_origin,
            )
            .await?;
        let mut exchange = issued_exchange.clone();
        exchange
            .complete(
                crate::CidarenBrowserEventEnvelope::exchange_type().to_owned(),
                result_digest,
                completed_at,
            )
            .map_err(|_| {
                ProviderError::new(
                    ProviderErrorKind::Internal,
                    "Cidaren result cannot complete the durable BrowserBridge exchange",
                )
            })?;
        Ok(CidarenCaptureExchangeCompleted { snapshot, exchange })
    }
}

impl fmt::Debug for CidarenBrowserBridge {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CidarenBrowserBridge")
            .field("metadata", &self.metadata)
            .field("details", &"configured")
            .finish()
    }
}

impl ProviderIdentity for CidarenBrowserBridge {
    fn metadata(&self) -> &ProviderMetadata {
        &self.metadata
    }
}

#[async_trait]
impl BrowserBridgeCapability for CidarenBrowserBridge {
    async fn browser_session_spec(
        &self,
        context: &ProviderContext,
        remote_task_id: &str,
    ) -> ProviderResult<BrowserSessionSpec> {
        validate_context(context, &self.metadata)?;
        validate_task_identity(remote_task_id)?;
        let detail = self.details.task_detail(context, remote_task_id).await?;
        if detail.task.remote_id != remote_task_id
            || !detail
                .task
                .capabilities
                .contains(&TaskCapability::BrowserBridge)
        {
            return Err(ProviderError::new(
                ProviderErrorKind::ProtocolDrift,
                "Cidaren fresh Task does not authorize a BrowserBridge session",
            ));
        }
        Ok(BrowserSessionSpec {
            version: 1,
            isolation_key: isolation_key(context, remote_task_id),
            allowed_origins: vec![CIDAREN_ORIGIN.to_owned()],
            // WeChat authorization and browser-storage capture both require a
            // user-visible browsing context in the audited donor flow.
            headless: false,
        })
    }
}

fn isolation_key(context: &ProviderContext, remote_task_id: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(b"asterism:cidaren:browser-session:v1\0");
    digest.update(context.account_id.to_string().as_bytes());
    digest.update(b"\0");
    digest.update(remote_task_id.as_bytes());
    format!("cidaren-task-{:x}", digest.finalize())
}

fn validate_context(context: &ProviderContext, metadata: &ProviderMetadata) -> ProviderResult<()> {
    if context.provider_id != metadata.id {
        return Err(ProviderError::new(
            ProviderErrorKind::Internal,
            "Cidaren BrowserBridge received a mismatched Provider context",
        ));
    }
    if context.credential_refs.is_empty() {
        return Err(ProviderError::new(
            ProviderErrorKind::Authentication,
            "Cidaren BrowserBridge requires an authenticated account binding",
        ));
    }
    Ok(())
}

fn validate_task_identity(remote_task_id: &str) -> ProviderResult<()> {
    let class = remote_task_id
        .strip_prefix("class-task:")
        .is_some_and(|release_id| {
            !release_id.is_empty()
                && release_id.len() <= 32
                && release_id.bytes().all(|byte| byte.is_ascii_digit())
                && release_id != "0"
                && !release_id.starts_with('0')
        });
    let study = remote_task_id
        .strip_prefix("study-task:")
        .and_then(|identity| identity.split_once(':'))
        .is_some_and(|(course_id, list_id)| valid_component(course_id) && valid_component(list_id));
    if class || study {
        Ok(())
    } else {
        Err(ProviderError::new(
            ProviderErrorKind::ProtocolDrift,
            "Cidaren BrowserBridge Task identity is invalid",
        ))
    }
}

fn valid_component(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_REMOTE_COMPONENT_BYTES
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

#[cfg(test)]
mod tests {
    use asterism_domain::{
        AssessmentClass, BrowserBridgeExchangeState, ProviderAccountId, ProviderId, RemoteState,
        SecretId, SessionKind, SourceType,
    };
    use asterism_provider_api::{RemoteTask, RemoteTaskDetail};
    use asterism_secrets::SecretPurpose;
    use chrono::{Duration, Utc};

    use super::*;

    #[derive(Debug)]
    struct FixtureDetail {
        metadata: ProviderMetadata,
        advertised: bool,
    }

    impl ProviderIdentity for FixtureDetail {
        fn metadata(&self) -> &ProviderMetadata {
            &self.metadata
        }
    }

    #[async_trait]
    impl TaskDetailCapability for FixtureDetail {
        async fn task_detail(
            &self,
            _context: &ProviderContext,
            remote_task_id: &str,
        ) -> ProviderResult<RemoteTaskDetail> {
            Ok(RemoteTaskDetail {
                task: RemoteTask {
                    remote_id: remote_task_id.to_owned(),
                    course_remote_id: Some("course:course-a".to_owned()),
                    title: "Synthetic task".to_owned(),
                    source_type: SourceType::Practice,
                    assessment_class: AssessmentClass::Routine,
                    remote_state: RemoteState::InProgress,
                    opens_at: None,
                    due_at: None,
                    closes_at: None,
                    capabilities: self
                        .advertised
                        .then_some(TaskCapability::BrowserBridge)
                        .into_iter()
                        .collect(),
                    fingerprint: "synthetic".to_owned(),
                    normalized: serde_json::json!({"schema": "cidaren.class-task.v1"}),
                    raw_sanitized: serde_json::json!({"schema": "cidaren.class-task.raw.v1"}),
                },
                normalized_detail: serde_json::json!({"schema": "cidaren.class-task.detail.v1"}),
            })
        }
    }

    #[tokio::test]
    async fn bridge_is_visible_origin_bounded_and_account_isolated() {
        let capability = bridge(true);
        let first = context();
        let second = context();
        let class = capability
            .browser_session_spec(&first, "class-task:2002")
            .await
            .unwrap();
        let study = capability
            .browser_session_spec(&first, "study-task:course-a:course-a_02")
            .await
            .unwrap();
        assert_ne!(class.isolation_key, study.isolation_key);
        assert_eq!(class.allowed_origins, study.allowed_origins);
        assert_eq!(class.headless, study.headless);
        assert_eq!(class.allowed_origins, [CIDAREN_ORIGIN]);
        assert!(!class.headless);
        assert_ne!(
            class.isolation_key,
            capability
                .browser_session_spec(&second, "class-task:2002")
                .await
                .unwrap()
                .isolation_key
        );
        assert_ne!(
            class.isolation_key,
            capability
                .browser_session_spec(&first, "class-task:2003")
                .await
                .unwrap()
                .isolation_key
        );
        assert!(!class.isolation_key.contains("2002"));
    }

    #[tokio::test]
    async fn bridge_rejects_foreign_context_and_unsafe_task_identity() {
        let capability = bridge(true);
        let mut foreign = context();
        foreign.provider_id = ProviderId::new("uai").unwrap();
        assert_eq!(
            capability
                .browser_session_spec(&foreign, "class-task:2002")
                .await
                .unwrap_err()
                .kind,
            ProviderErrorKind::Internal
        );
        for remote_task_id in [
            "class-task:0",
            "class-task:02",
            "class-task:unsafe/value",
            "study-task:course-a",
            "study-task:course-a:list:extra",
        ] {
            assert_eq!(
                capability
                    .browser_session_spec(&context(), remote_task_id)
                    .await
                    .unwrap_err()
                    .kind,
                ProviderErrorKind::ProtocolDrift
            );
        }

        assert_eq!(
            bridge(false)
                .browser_session_spec(&context(), "class-task:2002")
                .await
                .unwrap_err()
                .kind,
            ProviderErrorKind::ProtocolDrift
        );
    }

    #[tokio::test]
    async fn capture_command_rebinds_fresh_task_before_issue() {
        let capability = bridge(true);
        let command = capability
            .capture_snapshot_command(
                &context(),
                "class-task:2002",
                "synthetic-session-nonce".to_owned(),
                "frame-1".to_owned(),
                1,
                CidarenCaptureMode::TokenOnly,
            )
            .await
            .unwrap();
        assert_eq!(command.version, 1);
        assert_eq!(command.remote_task_id, "class-task:2002");
        assert_eq!(command.sequence, 1);

        assert_eq!(
            capability
                .capture_snapshot_command(
                    &context(),
                    "class-task:2002",
                    "synthetic-session-nonce".to_owned(),
                    "frame-1".to_owned(),
                    0,
                    CidarenCaptureMode::TokenOnly,
                )
                .await
                .unwrap_err()
                .kind,
            ProviderErrorKind::InvalidResponse
        );
    }

    #[tokio::test]
    async fn capture_result_rebinds_fresh_task_before_parse() {
        let capability = bridge(true);
        let command = capability
            .capture_snapshot_command(
                &context(),
                "class-task:2002",
                "synthetic-session-nonce".to_owned(),
                "frame-1".to_owned(),
                1,
                CidarenCaptureMode::TokenOnly,
            )
            .await
            .unwrap();
        let snapshot = capability
            .parse_capture_snapshot_result(
                &context(),
                "class-task:2002",
                &command,
                CidarenBrowserResultDocument::try_new(
                    include_str!(
                        "../../../fixtures/providers/cidaren/browser/capture-snapshot-token-only.json"
                    )
                    .to_owned(),
                )
                .unwrap(),
                CIDAREN_ORIGIN,
            )
            .await
            .unwrap();
        assert_eq!(snapshot.user_token(), Some("synthetic-user-token"));

        assert_eq!(
            capability
                .parse_capture_snapshot_result(
                    &context(),
                    "class-task:2003",
                    &command,
                    CidarenBrowserResultDocument::try_new(
                        include_str!(
                            "../../../fixtures/providers/cidaren/browser/capture-snapshot-token-only.json"
                        )
                        .to_owned(),
                    )
                    .unwrap(),
                    CIDAREN_ORIGIN,
                )
                .await
                .unwrap_err()
                .kind,
            ProviderErrorKind::RemoteChanged
        );
    }

    #[tokio::test]
    async fn typed_capture_exchange_freezes_core_metadata_and_validated_result() {
        let capability = bridge(true);
        let session_id = BrowserBridgeSessionId::new();
        let issued_at = Utc::now();
        let issued = capability
            .capture_snapshot_exchange(
                &context(),
                "class-task:2002",
                session_id,
                "frame-1".to_owned(),
                1,
                CidarenCaptureMode::TokenOnly,
                issued_at,
            )
            .await
            .unwrap();
        assert_eq!(issued.command().session_nonce, session_id.to_string());
        assert_eq!(issued.exchange().session_id, session_id);
        assert_eq!(issued.exchange().sequence, 1);
        assert_eq!(
            issued.exchange().command_type,
            CidarenBrowserCommandEnvelope::exchange_type()
        );
        assert_eq!(
            issued.exchange().command_digest,
            issued.command().exchange_digest().unwrap()
        );
        assert_eq!(
            issued.command_artifact().digest(),
            issued.exchange().command_digest
        );
        assert!(!format!("{:?}", issued.command_artifact()).contains(&session_id.to_string()));
        assert_eq!(issued.exchange().state, BrowserBridgeExchangeState::Issued);

        let document = include_str!(
            "../../../fixtures/providers/cidaren/browser/capture-snapshot-token-only.json"
        )
        .replace("synthetic-session-nonce", &session_id.to_string());
        let expected_result_digest = crate::browser_event_exchange_digest(&document).unwrap();
        let completed = capability
            .complete_capture_snapshot_exchange(
                &context(),
                "class-task:2002",
                &issued,
                CidarenBrowserResultDocument::try_new(document).unwrap(),
                CIDAREN_ORIGIN,
                issued_at + Duration::seconds(1),
            )
            .await
            .unwrap();
        assert_eq!(
            completed.snapshot().user_token(),
            Some("synthetic-user-token")
        );
        assert_eq!(
            completed.exchange().state,
            BrowserBridgeExchangeState::Completed
        );
        assert_eq!(
            completed.exchange().result_digest,
            Some(expected_result_digest)
        );
        assert_eq!(
            completed.exchange().result_type.as_deref(),
            Some(crate::CidarenBrowserEventEnvelope::exchange_type())
        );
        let (snapshot, completed_exchange) = completed.into_parts();
        assert_eq!(snapshot.user_token(), Some("synthetic-user-token"));
        assert_eq!(
            completed_exchange.state,
            BrowserBridgeExchangeState::Completed
        );
        let (command, command_artifact, issued_exchange) = issued.into_parts();
        assert_eq!(
            command.exchange_digest().unwrap(),
            command_artifact.digest()
        );
        assert_eq!(issued_exchange.command_digest, command_artifact.digest());
    }

    #[tokio::test]
    async fn recovered_capture_exchange_resolves_command_authority_before_result() {
        let capability = bridge(true);
        let context = context();
        let session_id = BrowserBridgeSessionId::new();
        let issued_at = Utc::now();
        let issued = capability
            .capture_snapshot_exchange(
                &context,
                "class-task:2002",
                session_id,
                "frame-recovery".to_owned(),
                3,
                CidarenCaptureMode::Composite,
                issued_at,
            )
            .await
            .unwrap();
        let (_, artifact, exchange) = issued.into_parts();
        let artifact = artifact.into_secret_value();
        let document = include_str!(
            "../../../fixtures/providers/cidaren/browser/capture-snapshot-composite.json"
        )
        .replace("synthetic-session-nonce", &session_id.to_string())
        .replace("frame-1", "frame-recovery")
        .replace("\"reply_to_sequence\": 1", "\"reply_to_sequence\": 3");

        assert_eq!(
            capability
                .complete_recovered_capture_snapshot_exchange(
                    &context,
                    "class-task:2002",
                    &exchange,
                    &artifact,
                    CidarenCaptureMode::TokenOnly,
                    CidarenBrowserResultDocument::try_new(document.clone()).unwrap(),
                    CIDAREN_ORIGIN,
                    issued_at + Duration::seconds(1),
                )
                .await
                .unwrap_err()
                .kind,
            ProviderErrorKind::RemoteChanged
        );

        let completed = capability
            .complete_recovered_capture_snapshot_exchange(
                &context,
                "class-task:2002",
                &exchange,
                &artifact,
                CidarenCaptureMode::Composite,
                CidarenBrowserResultDocument::try_new(document).unwrap(),
                CIDAREN_ORIGIN,
                issued_at + Duration::seconds(1),
            )
            .await
            .unwrap();
        assert_eq!(
            completed.exchange().state,
            BrowserBridgeExchangeState::Completed
        );
        assert_eq!(
            completed.snapshot().login_info_source(),
            Some(crate::CidarenCaptureStorageSource::LocalStorage)
        );
        let (replacement, completed_exchange) = completed.into_credential_commit_parts().unwrap();
        assert_eq!(replacement.session_kind, SessionKind::Composite);
        assert_eq!(replacement.fields.len(), 2);
        assert_eq!(
            replacement.fields[0].purpose,
            SecretPurpose::ProviderAccessToken
        );
        assert_eq!(
            replacement.fields[1].purpose,
            SecretPurpose::ProviderCompositeSession
        );
        assert_eq!(
            completed_exchange.state,
            BrowserBridgeExchangeState::Completed
        );
    }

    #[tokio::test]
    async fn typed_capture_exchange_rejects_unrepresentable_sequence_and_regressing_result() {
        let capability = bridge(true);
        let issued_at = Utc::now();
        assert_eq!(
            capability
                .capture_snapshot_exchange(
                    &context(),
                    "class-task:2002",
                    BrowserBridgeSessionId::new(),
                    "frame-1".to_owned(),
                    u64::from(u32::MAX) + 1,
                    CidarenCaptureMode::TokenOnly,
                    issued_at,
                )
                .await
                .unwrap_err()
                .kind,
            ProviderErrorKind::InvalidResponse
        );

        let session_id = BrowserBridgeSessionId::new();
        let issued = capability
            .capture_snapshot_exchange(
                &context(),
                "class-task:2002",
                session_id,
                "frame-1".to_owned(),
                1,
                CidarenCaptureMode::TokenOnly,
                issued_at,
            )
            .await
            .unwrap();
        let document = include_str!(
            "../../../fixtures/providers/cidaren/browser/capture-snapshot-token-only.json"
        )
        .replace("synthetic-session-nonce", &session_id.to_string());
        assert_eq!(
            capability
                .complete_capture_snapshot_exchange(
                    &context(),
                    "class-task:2002",
                    &issued,
                    CidarenBrowserResultDocument::try_new(document).unwrap(),
                    CIDAREN_ORIGIN,
                    issued_at - Duration::seconds(1),
                )
                .await
                .unwrap_err()
                .kind,
            ProviderErrorKind::Internal
        );
    }

    fn bridge(advertised: bool) -> CidarenBrowserBridge {
        CidarenBrowserBridge::try_new(Arc::new(FixtureDetail {
            metadata: development_metadata().unwrap(),
            advertised,
        }))
        .unwrap()
    }

    fn context() -> ProviderContext {
        ProviderContext {
            provider_id: ProviderId::new("cidaren").unwrap(),
            account_id: ProviderAccountId::new(),
            credential_refs: vec![SecretId::new()],
            correlation_id: "cidaren-browser-bridge-test".to_owned(),
        }
    }
}
