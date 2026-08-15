use std::{fmt, sync::Arc};

use asterism_domain::{ProtocolObservationKind, ProtocolSurface, RemoteState};
use asterism_provider_api::{
    ProviderContext, ProviderError, ProviderErrorKind, ProviderIdentity, ProviderMetadata,
    ProviderResult, RemoteProgress, TaskProgressCapability,
};
use async_trait::async_trait;
use chrono::Utc;
use serde_json::{Map, Value};
use zeroize::Zeroize;

use crate::{
    course_inventory::{protocol_drift, required_remote_component},
    metadata::development_metadata,
    protocol_observation::{json_value_kind, protocol_drift_with_observation},
};

const MAX_CMI_DOCUMENT_BYTES: usize = 1_024 * 1_024;
const MAX_CMI_SCALAR_BYTES: usize = 128;
pub(crate) const UNINITIALIZED_CMI_MARKER: &str = "学习数据不正确";

/// One bounded CMI response whose body is redacted and zeroized on drop.
pub struct WellearnCmiDocument(String);

impl WellearnCmiDocument {
    /// Wraps one complete native response before CMI parsing.
    ///
    /// # Errors
    ///
    /// Returns an invalid-response error for an empty or oversized body.
    pub fn try_new(document: impl Into<String>) -> ProviderResult<Self> {
        let mut document = document.into();
        if document.is_empty() || document.len() > MAX_CMI_DOCUMENT_BYTES {
            document.zeroize();
            return Err(ProviderError::new(
                ProviderErrorKind::InvalidResponse,
                "WELearn CMI response is empty or exceeds the size limit",
            ));
        }
        Ok(Self(document))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for WellearnCmiDocument {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("WellearnCmiDocument([REDACTED])")
    }
}

impl Drop for WellearnCmiDocument {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

/// Sanitized read-only CMI facts. Time fields remain opaque until live
/// evidence establishes their unit and grammar.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WellearnCmiSnapshot {
    cmi_present: bool,
    remote_state: RemoteState,
    percent: Option<u8>,
    completion_raw: Option<String>,
    progress_raw: Option<String>,
    session_time_raw: Option<String>,
    total_time_raw: Option<String>,
    score_scaled_raw: Option<String>,
    success_status_raw: Option<String>,
}

impl WellearnCmiSnapshot {
    pub const fn cmi_present(&self) -> bool {
        self.cmi_present
    }

    pub const fn remote_state(&self) -> RemoteState {
        self.remote_state
    }

    pub const fn percent(&self) -> Option<u8> {
        self.percent
    }

    pub fn completion_raw(&self) -> Option<&str> {
        self.completion_raw.as_deref()
    }

    pub fn progress_raw(&self) -> Option<&str> {
        self.progress_raw.as_deref()
    }

    pub fn session_time_raw(&self) -> Option<&str> {
        self.session_time_raw.as_deref()
    }

    pub fn total_time_raw(&self) -> Option<&str> {
        self.total_time_raw.as_deref()
    }

    pub fn score_scaled_raw(&self) -> Option<&str> {
        self.score_scaled_raw.as_deref()
    }

    pub fn success_status_raw(&self) -> Option<&str> {
        self.success_status_raw.as_deref()
    }

    fn into_remote_progress(self) -> RemoteProgress {
        RemoteProgress {
            remote_state: self.remote_state,
            percent: self.percent,
            duration_seconds: None,
            updated_at: Utc::now(),
        }
    }
}

impl Drop for WellearnCmiSnapshot {
    fn drop(&mut self) {
        for value in [
            &mut self.completion_raw,
            &mut self.progress_raw,
            &mut self.session_time_raw,
            &mut self.total_time_raw,
            &mut self.score_scaled_raw,
            &mut self.success_status_raw,
        ] {
            if let Some(value) = value.as_mut() {
                value.zeroize();
            }
        }
    }
}

/// Native boundary for one fresh `getscoinfo_v7` read.
#[async_trait]
pub trait WellearnCmiTransport: Send + Sync {
    async fn fetch_cmi(
        &self,
        context: &ProviderContext,
        course_id: &str,
        sco_id: &str,
    ) -> ProviderResult<WellearnCmiDocument>;
}

/// Fresh task progress backed by the read-only CMI route.
pub struct WellearnTaskProgress {
    metadata: ProviderMetadata,
    transport: Arc<dyn WellearnCmiTransport>,
}

impl WellearnTaskProgress {
    /// Creates the capability around one injected CMI transport.
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

impl fmt::Debug for WellearnTaskProgress {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WellearnTaskProgress")
            .field("metadata", &self.metadata)
            .field("transport", &"configured")
            .finish()
    }
}

impl ProviderIdentity for WellearnTaskProgress {
    fn metadata(&self) -> &ProviderMetadata {
        &self.metadata
    }
}

#[async_trait]
impl TaskProgressCapability for WellearnTaskProgress {
    async fn read_progress(
        &self,
        context: &ProviderContext,
        remote_task_id: &str,
    ) -> ProviderResult<RemoteProgress> {
        validate_context(context, &self.metadata)?;
        let (course_id, sco_id) = parse_sco_identity(remote_task_id)?;
        let document = self
            .transport
            .fetch_cmi(context, &course_id, &sco_id)
            .await?;
        parse_cmi_snapshot(document.as_str()).map(WellearnCmiSnapshot::into_remote_progress)
    }
}

/// Parses the nested `comment` CMI response without assigning a duration unit.
///
/// # Errors
///
/// Returns an invalid-response or protocol-drift error for malformed, unsafe or
/// unsupported response shapes.
pub fn parse_cmi_snapshot(document: &str) -> ProviderResult<WellearnCmiSnapshot> {
    if document.is_empty() || document.len() > MAX_CMI_DOCUMENT_BYTES {
        return Err(ProviderError::new(
            ProviderErrorKind::InvalidResponse,
            "WELearn CMI response is empty or exceeds the size limit",
        ));
    }
    let outer: Value = serde_json::from_str(document).map_err(|_| {
        ProviderError::new(
            ProviderErrorKind::InvalidResponse,
            "WELearn CMI response is not valid JSON",
        )
    })?;
    let Some(outer_object) = outer.as_object() else {
        return Err(cmi_result_shape_drift(
            "WELearn CMI response is not an object",
            &outer,
        ));
    };
    if outer_object.get("ret").and_then(Value::as_i64) != Some(0) {
        return Err(cmi_result_shape_drift(
            "WELearn CMI read did not succeed",
            &outer,
        ));
    }
    let comment = outer_object
        .get("comment")
        .and_then(Value::as_str)
        .ok_or_else(|| protocol_drift("WELearn CMI response has no comment document"))?;
    if comment.is_empty() || comment.len() > MAX_CMI_DOCUMENT_BYTES {
        return Err(ProviderError::new(
            ProviderErrorKind::InvalidResponse,
            "WELearn nested CMI document is empty or exceeds the size limit",
        ));
    }
    let nested: Value = serde_json::from_str(comment).map_err(|_| {
        ProviderError::new(
            ProviderErrorKind::InvalidResponse,
            "WELearn nested CMI document is not valid JSON",
        )
    })?;
    let nested = nested
        .as_object()
        .ok_or_else(|| protocol_drift("WELearn nested CMI document is not an object"))?;
    let Some(cmi) = nested.get("cmi") else {
        return Ok(WellearnCmiSnapshot {
            cmi_present: false,
            remote_state: RemoteState::Pending,
            percent: Some(0),
            completion_raw: None,
            progress_raw: None,
            session_time_raw: None,
            total_time_raw: None,
            score_scaled_raw: None,
            success_status_raw: None,
        });
    };
    let cmi = cmi
        .as_object()
        .ok_or_else(|| protocol_drift("WELearn CMI field is not an object"))?;
    let completion_raw = optional_scalar(cmi, "completion_status")?;
    let progress_raw = optional_scalar(cmi, "progress_measure")?;
    let percent = progress_raw
        .as_deref()
        .map(parse_progress_percent)
        .transpose()?;
    let score_scaled_raw = cmi
        .get("score")
        .map(|score| {
            score
                .as_object()
                .ok_or_else(|| protocol_drift("WELearn CMI score is not an object"))
                .and_then(|score| optional_scalar(score, "scaled"))
        })
        .transpose()?
        .flatten();
    Ok(WellearnCmiSnapshot {
        cmi_present: true,
        remote_state: completion_state(completion_raw.as_deref()),
        percent,
        completion_raw,
        progress_raw,
        session_time_raw: optional_scalar(cmi, "session_time")?,
        total_time_raw: optional_scalar(cmi, "total_time")?,
        score_scaled_raw,
        success_status_raw: optional_scalar(cmi, "success_status")?,
    })
}

fn cmi_result_shape_drift(message: &'static str, root: &Value) -> ProviderError {
    let result = root.get("ret");
    let result_state = match result.and_then(Value::as_i64) {
        Some(0) => "accepted",
        Some(_) => "integer_nonzero",
        None if result.is_none() => "missing",
        None => "not_integer",
    };
    protocol_drift_with_observation(
        message,
        ProtocolSurface::TaskProgress,
        ProtocolObservationKind::UnknownResultShape,
        serde_json::json!({
            "document": "cmi_outer",
            "root_type": json_value_kind(Some(root)),
            "result_field": "ret",
            "result_type": json_value_kind(result),
            "result_state": result_state,
        }),
    )
}

/// Parses a pre-mutation CMI read while retaining the donor's exact explicit
/// uninitialized response as an absent baseline. Read-only capabilities must
/// continue to call [`parse_cmi_snapshot`] directly.
///
/// # Errors
///
/// Returns the same typed errors as [`parse_cmi_snapshot`] for every response
/// other than the audited uninitialized marker.
pub(crate) fn parse_mutation_cmi_baseline(
    document: &str,
) -> ProviderResult<Option<WellearnCmiSnapshot>> {
    if document.contains(UNINITIALIZED_CMI_MARKER) {
        return Ok(None);
    }
    parse_cmi_snapshot(document).map(Some)
}

fn optional_scalar(cmi: &Map<String, Value>, key: &'static str) -> ProviderResult<Option<String>> {
    let Some(value) = cmi.get(key) else {
        return Ok(None);
    };
    let value = match value {
        Value::String(value) => value.trim().to_owned(),
        Value::Number(value) => value.to_string(),
        _ => return Err(protocol_drift(format!("WELearn CMI {key} is not scalar"))),
    };
    if value.is_empty() || value.len() > MAX_CMI_SCALAR_BYTES || value.chars().any(char::is_control)
    {
        return Err(protocol_drift(format!(
            "WELearn CMI {key} has an invalid shape"
        )));
    }
    Ok(Some(value))
}

fn parse_progress_percent(value: &str) -> ProviderResult<u8> {
    if value == "0" {
        return Ok(0);
    }
    if value == "1" {
        return Ok(100);
    }
    let Some((whole, fraction)) = value.split_once('.') else {
        return Err(protocol_drift(
            "WELearn CMI progress_measure is outside the supported grammar",
        ));
    };
    if fraction.is_empty()
        || fraction.len() > 9
        || !fraction.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(protocol_drift(
            "WELearn CMI progress_measure is outside the supported grammar",
        ));
    }
    if whole == "1" && fraction.bytes().all(|byte| byte == b'0') {
        return Ok(100);
    }
    if whole != "0" {
        return Err(protocol_drift(
            "WELearn CMI progress_measure is outside the supported range",
        ));
    }
    let mut digits = fraction.bytes().chain(std::iter::repeat(b'0'));
    let tens = digits.next().expect("padded progress digit") - b'0';
    let ones = digits.next().expect("padded progress digit") - b'0';
    let round = u8::from(digits.next().expect("padded progress digit") >= b'5');
    Ok((tens * 10 + ones + round).min(100))
}

fn completion_state(value: Option<&str>) -> RemoteState {
    let Some(value) = value else {
        return RemoteState::Unknown;
    };
    match value.trim().to_ascii_lowercase().as_str() {
        "completed" | "passed" => RemoteState::Completed,
        "incomplete" | "failed" => RemoteState::InProgress,
        "not_attempted" | "not attempted" => RemoteState::Pending,
        _ => RemoteState::Unknown,
    }
}

pub(crate) fn parse_sco_identity(value: &str) -> ProviderResult<(String, String)> {
    let mut components = value.split(':');
    if components.next() != Some("sco") {
        return Err(protocol_drift(
            "WELearn progress received an unsupported Task identity",
        ));
    }
    let course_id = components
        .next()
        .ok_or_else(|| protocol_drift("WELearn progress Task identity has no Course ID"))?;
    let sco_id = components
        .next()
        .ok_or_else(|| protocol_drift("WELearn progress Task identity has no SCO ID"))?;
    if components.next().is_some() {
        return Err(protocol_drift(
            "WELearn progress Task identity has extra components",
        ));
    }
    Ok((
        required_remote_component(
            Some(&Value::String(course_id.to_owned())),
            "progress Course ID",
        )?,
        required_remote_component(Some(&Value::String(sco_id.to_owned())), "progress SCO ID")?,
    ))
}

fn validate_context(context: &ProviderContext, metadata: &ProviderMetadata) -> ProviderResult<()> {
    if context.provider_id != metadata.id {
        return Err(ProviderError::new(
            ProviderErrorKind::Internal,
            "WELearn progress received a mismatched Provider context",
        ));
    }
    if context.credential_refs.is_empty() {
        return Err(ProviderError::new(
            ProviderErrorKind::Authentication,
            "WELearn progress requires an authenticated session",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use asterism_domain::{ProviderAccountId, ProviderId, SecretId};

    use super::*;

    const CMI: &str = include_str!("../../../fixtures/providers/welearn/cmi/progress-mixed.json");

    #[derive(Debug)]
    struct FixtureTransport {
        calls: AtomicUsize,
    }

    #[async_trait]
    impl WellearnCmiTransport for FixtureTransport {
        async fn fetch_cmi(
            &self,
            _context: &ProviderContext,
            course_id: &str,
            sco_id: &str,
        ) -> ProviderResult<WellearnCmiDocument> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            assert_eq!(course_id, "1001");
            assert_eq!(sco_id, "sco-1");
            WellearnCmiDocument::try_new(CMI)
        }
    }

    #[test]
    fn parser_keeps_completion_progress_and_times_independent() {
        let snapshot = parse_cmi_snapshot(CMI).unwrap();
        assert_eq!(snapshot.remote_state(), RemoteState::InProgress);
        assert_eq!(snapshot.percent(), Some(42));
        assert_eq!(snapshot.completion_raw(), Some("incomplete"));
        assert_eq!(snapshot.progress_raw(), Some("0.421"));
        assert_eq!(snapshot.session_time_raw(), Some("PT5M"));
        assert_eq!(snapshot.total_time_raw(), Some("300"));
        assert_eq!(snapshot.score_scaled_raw(), Some("0.8"));
        assert_eq!(snapshot.success_status_raw(), Some("unknown"));
        assert!(snapshot.cmi_present());

        let completed = parse_cmi_snapshot(
            r#"{"ret":0,"comment":"{\"cmi\":{\"completion_status\":\"completed\",\"progress_measure\":\"0.25\",\"total_time\":\"900\"}}"}"#,
        )
        .unwrap();
        assert_eq!(completed.remote_state(), RemoteState::Completed);
        assert_eq!(completed.percent(), Some(25));
        assert_eq!(completed.total_time_raw(), Some("900"));
    }

    #[test]
    fn parser_rejects_malformed_nested_or_out_of_range_progress() {
        for document in [
            r"{}",
            r#"{"comment":{}}"#,
            r#"{"comment":"{}"}"#,
            r#"{"ret":"0","comment":"{}"}"#,
            r#"{"comment":"not-json"}"#,
            r#"{"ret":1,"comment":"{}"}"#,
            r#"{"comment":"{\"cmi\":{\"progress_measure\":\"1.1\"}}"}"#,
            r#"{"comment":"{\"cmi\":{\"session_time\":[]}}"}"#,
        ] {
            assert!(parse_cmi_snapshot(document).is_err());
        }
        let absent = parse_cmi_snapshot(r#"{"ret":0,"comment":"{}"}"#).unwrap();
        assert_eq!(absent.remote_state(), RemoteState::Pending);
        assert_eq!(absent.percent(), Some(0));
        assert!(!absent.cmi_present());
    }

    #[test]
    fn outer_result_drift_attaches_type_and_state_without_the_result_value() {
        let string_error =
            parse_cmi_snapshot(r#"{"ret":"must-not-cross","comment":"{}"}"#).unwrap_err();
        let string_observation = string_error.protocol_observation.unwrap();
        assert_eq!(string_observation.surface, ProtocolSurface::TaskProgress);
        assert_eq!(
            string_observation.kind,
            ProtocolObservationKind::UnknownResultShape
        );
        assert_eq!(
            string_observation.shape_sanitized,
            serde_json::json!({
                "document": "cmi_outer",
                "root_type": "object",
                "result_field": "ret",
                "result_type": "string",
                "result_state": "not_integer",
            })
        );
        assert!(
            !string_observation
                .shape_sanitized
                .to_string()
                .contains("must-not-cross")
        );

        let nonzero = parse_cmi_snapshot(r#"{"ret":7,"comment":"{}"}"#).unwrap_err();
        assert_eq!(
            nonzero.protocol_observation.unwrap().shape_sanitized["result_state"],
            "integer_nonzero"
        );
        let non_object = parse_cmi_snapshot(r"[]").unwrap_err();
        assert_eq!(
            non_object.protocol_observation.unwrap().shape_sanitized,
            serde_json::json!({
                "document": "cmi_outer",
                "root_type": "array",
                "result_field": "ret",
                "result_type": "missing",
                "result_state": "missing",
            })
        );
    }

    #[tokio::test]
    async fn capability_binds_identity_and_does_not_claim_duration_seconds() {
        let transport = Arc::new(FixtureTransport {
            calls: AtomicUsize::new(0),
        });
        let capability = WellearnTaskProgress::try_new(transport.clone()).unwrap();
        let progress = capability
            .read_progress(&context(), "sco:1001:sco-1")
            .await
            .unwrap();
        assert_eq!(progress.remote_state, RemoteState::InProgress);
        assert_eq!(progress.percent, Some(42));
        assert_eq!(progress.duration_seconds, None);
        assert_eq!(transport.calls.load(Ordering::SeqCst), 1);

        assert!(
            capability
                .read_progress(&context(), "sco:1001:sco-1:extra")
                .await
                .is_err()
        );
        assert_eq!(transport.calls.load(Ordering::SeqCst), 1);
    }

    fn context() -> ProviderContext {
        ProviderContext {
            provider_id: ProviderId::new("welearn").unwrap(),
            account_id: ProviderAccountId::new(),
            credential_refs: vec![SecretId::new()],
            correlation_id: "welearn-cmi-test".to_owned(),
        }
    }
}
