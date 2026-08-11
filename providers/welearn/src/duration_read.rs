use std::{fmt, sync::Arc};

use asterism_provider_api::{
    DurationReadCapability, ProviderContext, ProviderError, ProviderErrorKind, ProviderIdentity,
    ProviderMetadata, ProviderResult, RemoteDuration,
};
use async_trait::async_trait;
use chrono::Utc;

use crate::{
    WellearnCmiTransport,
    cmi::{parse_cmi_snapshot, parse_sco_identity},
    metadata::development_metadata,
};

const MAX_DURATION_SECONDS: u64 = 10 * 365 * 24 * 60 * 60;

/// Reads the donor-observed integer `total_time` independently from progress
/// and every duration-report mutation.
pub struct WellearnDurationRead {
    metadata: ProviderMetadata,
    transport: Arc<dyn WellearnCmiTransport>,
}

impl WellearnDurationRead {
    /// Creates the fresh CMI-backed duration reader.
    ///
    /// # Errors
    ///
    /// Returns an internal error if compile-time metadata is invalid.
    pub fn try_new(transport: Arc<dyn WellearnCmiTransport>) -> ProviderResult<Self> {
        Ok(Self {
            metadata: development_metadata()?,
            transport,
        })
    }
}

impl fmt::Debug for WellearnDurationRead {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WellearnDurationRead")
            .field("metadata", &self.metadata)
            .field("transport", &"configured")
            .finish()
    }
}

impl ProviderIdentity for WellearnDurationRead {
    fn metadata(&self) -> &ProviderMetadata {
        &self.metadata
    }
}

#[async_trait]
impl DurationReadCapability for WellearnDurationRead {
    async fn read_duration(
        &self,
        context: &ProviderContext,
        remote_task_id: &str,
    ) -> ProviderResult<RemoteDuration> {
        validate_context(context, &self.metadata)?;
        let (course_id, sco_id) = parse_sco_identity(remote_task_id)?;
        let document = self
            .transport
            .fetch_cmi(context, &course_id, &sco_id)
            .await?;
        let snapshot = parse_cmi_snapshot(document.as_str())?;
        let duration_seconds = if snapshot.cmi_present() {
            parse_duration_seconds(snapshot.total_time_raw().ok_or_else(|| {
                ProviderError::new(
                    ProviderErrorKind::ProtocolDrift,
                    "WELearn CMI has no total_time duration observation",
                )
            })?)?
        } else {
            0
        };
        Ok(RemoteDuration {
            duration_seconds,
            updated_at: Utc::now(),
        })
    }
}

fn parse_duration_seconds(value: &str) -> ProviderResult<u64> {
    if value.is_empty()
        || (value.len() > 1 && value.starts_with('0'))
        || !value.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(duration_drift());
    }
    value
        .parse::<u64>()
        .ok()
        .filter(|seconds| *seconds <= MAX_DURATION_SECONDS)
        .ok_or_else(duration_drift)
}

fn validate_context(context: &ProviderContext, metadata: &ProviderMetadata) -> ProviderResult<()> {
    if context.provider_id != metadata.id {
        return Err(ProviderError::new(
            ProviderErrorKind::Internal,
            "WELearn duration read received a mismatched Provider context",
        ));
    }
    if context.credential_refs.is_empty() {
        return Err(ProviderError::new(
            ProviderErrorKind::Authentication,
            "WELearn duration read requires an authenticated session",
        ));
    }
    Ok(())
}

fn duration_drift() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::ProtocolDrift,
        "WELearn total_time does not use the audited bounded integer-second grammar",
    )
}

#[cfg(test)]
mod tests {
    use asterism_domain::{ProviderAccountId, ProviderId, SecretId};

    use super::*;
    use crate::WellearnCmiDocument;

    #[derive(Debug)]
    struct FixtureTransport(&'static str);

    #[async_trait]
    impl WellearnCmiTransport for FixtureTransport {
        async fn fetch_cmi(
            &self,
            _context: &ProviderContext,
            course_id: &str,
            sco_id: &str,
        ) -> ProviderResult<WellearnCmiDocument> {
            assert_eq!(course_id, "1001");
            assert_eq!(sco_id, "301");
            WellearnCmiDocument::try_new(self.0)
        }
    }

    #[tokio::test]
    async fn duration_read_normalizes_total_time_seconds_and_unstarted_zero() {
        let present = WellearnDurationRead::try_new(Arc::new(FixtureTransport(
            r#"{"ret":0,"comment":"{\"cmi\":{\"completion_status\":\"incomplete\",\"progress_measure\":\"0.25\",\"session_time\":\"15\",\"total_time\":\"45\",\"score\":{\"scaled\":\"20\"},\"success_status\":\"unknown\"}}"}"#,
        )))
        .unwrap();
        assert_eq!(
            present
                .read_duration(&context(), "sco:1001:301")
                .await
                .unwrap()
                .duration_seconds,
            45
        );

        let absent = WellearnDurationRead::try_new(Arc::new(FixtureTransport(
            r#"{"ret":0,"comment":"{}"}"#,
        )))
        .unwrap();
        assert_eq!(
            absent
                .read_duration(&context(), "sco:1001:301")
                .await
                .unwrap()
                .duration_seconds,
            0
        );
    }

    #[test]
    fn duration_grammar_is_canonical_and_bounded() {
        assert_eq!(parse_duration_seconds("0").unwrap(), 0);
        assert_eq!(
            parse_duration_seconds("315360000").unwrap(),
            MAX_DURATION_SECONDS
        );
        for invalid in ["", "00", "01", "-1", "1.0", "PT1S", "315360001"] {
            assert_eq!(
                parse_duration_seconds(invalid).unwrap_err().kind,
                ProviderErrorKind::ProtocolDrift
            );
        }
    }

    fn context() -> ProviderContext {
        ProviderContext {
            provider_id: ProviderId::new("welearn").unwrap(),
            account_id: ProviderAccountId::new(),
            credential_refs: vec![SecretId::new()],
            correlation_id: "welearn-duration-read".to_owned(),
        }
    }
}
