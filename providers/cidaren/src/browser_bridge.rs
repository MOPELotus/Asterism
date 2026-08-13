use std::{fmt, sync::Arc};

use asterism_domain::TaskCapability;
use asterism_provider_api::{
    BrowserBridgeCapability, BrowserSessionSpec, ProviderContext, ProviderError, ProviderErrorKind,
    ProviderIdentity, ProviderMetadata, ProviderResult, TaskDetailCapability,
};
use async_trait::async_trait;
use sha2::{Digest, Sha256};

use crate::{
    CidarenBrowserCommandEnvelope, CidarenCaptureMode, CidarenCaptureSnapshot,
    metadata::development_metadata, parse_browser_event,
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
        AssessmentClass, ProviderAccountId, ProviderId, RemoteState, SecretId, SourceType,
    };
    use asterism_provider_api::{RemoteTask, RemoteTaskDetail};

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
                include_str!(
                    "../../../fixtures/providers/cidaren/browser/capture-snapshot-token-only.json"
                ),
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
                    include_str!(
                        "../../../fixtures/providers/cidaren/browser/capture-snapshot-token-only.json"
                    ),
                    CIDAREN_ORIGIN,
                )
                .await
                .unwrap_err()
                .kind,
            ProviderErrorKind::RemoteChanged
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
