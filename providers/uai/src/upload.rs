use std::{fmt, sync::Arc};

use asterism_provider_api::{
    ProviderContext, ProviderError, ProviderErrorKind, ProviderResult, TaskDetailCapability,
};
use async_trait::async_trait;
use serde_json::Value;
use sha2::{Digest, Sha256};
use zeroize::{Zeroize, Zeroizing};

use crate::encrypted::ZeroizingJsonValue;

const MAX_UPLOAD_GRANT_RESPONSE_BYTES: usize = 64 * 1_024;
const MAX_UPLOAD_RESPONSE_BYTES: usize = 64 * 1_024;
const MAX_UPLOAD_TOKEN_BYTES: usize = 8 * 1_024;
const MAX_UPLOAD_KEY_BYTES: usize = 1_024;
const MAX_UPLOAD_ARTIFACT_BYTES: usize = 64 * 1_024 * 1_024;
const MINIMAL_MP3_BYTES: usize = 4_096;

/// Provider-private boundary for the first two stages of the audited upload
/// flow. Shared Core still owns the durable artifact/attempt state machine.
#[async_trait]
pub trait UaiUploadTransport: Send + Sync {
    async fn request_upload_grant(
        &self,
        context: &ProviderContext,
        intent: &UaiUploadIntent,
        artifact: &UaiUploadArtifact,
    ) -> ProviderResult<UaiUploadGrant>;

    async fn upload_artifact(
        &self,
        context: &ProviderContext,
        grant: &UaiUploadGrant,
        artifact: &UaiUploadArtifact,
    ) -> ProviderResult<UaiUploadedArtifact>;
}

/// Short-lived CMS/object-store authorization returned by UAI.
pub struct UaiUploadGrant {
    token: Zeroizing<String>,
    file_key: String,
    intent_fingerprint: String,
    artifact_digest: String,
    remote_task_id: String,
    course_resource_id: String,
    group_id: String,
    upload_position: u32,
}

impl UaiUploadGrant {
    pub fn expose_token(&self) -> &str {
        self.token.as_str()
    }

    pub fn file_key(&self) -> &str {
        &self.file_key
    }

    fn validate_for_artifact(&self, artifact: &UaiUploadArtifact) -> ProviderResult<()> {
        if self.artifact_digest == artifact.digest() {
            Ok(())
        } else {
            Err(ProviderError::new(
                ProviderErrorKind::RemoteChanged,
                "UAI upload grant is foreign to the selected artifact",
            ))
        }
    }
}

impl fmt::Debug for UaiUploadGrant {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UaiUploadGrant")
            .field("token", &"[REDACTED]")
            .field("file_key", &"[ROUTE]")
            .field("intent_fingerprint", &self.intent_fingerprint)
            .field("artifact_digest", &self.artifact_digest)
            .field("remote_task_id", &self.remote_task_id)
            .field("course_resource_id", &self.course_resource_id)
            .field("group_id", &self.group_id)
            .field("upload_position", &self.upload_position)
            .finish()
    }
}

impl Drop for UaiUploadGrant {
    fn drop(&mut self) {
        self.file_key.zeroize();
        self.intent_fingerprint.zeroize();
        self.artifact_digest.zeroize();
        self.remote_task_id.zeroize();
        self.course_resource_id.zeroize();
        self.group_id.zeroize();
    }
}

/// Exact object-store result retaining the immutable Task/module/artifact
/// binding required by a future upload-answer Draft.
pub struct UaiUploadedArtifact {
    remote_task_id: String,
    course_resource_id: String,
    group_id: String,
    upload_position: u32,
    file_key: String,
    artifact_digest: String,
    intent_fingerprint: String,
}

impl UaiUploadedArtifact {
    pub fn remote_task_id(&self) -> &str {
        &self.remote_task_id
    }

    pub fn group_id(&self) -> &str {
        &self.group_id
    }

    pub const fn upload_position(&self) -> u32 {
        self.upload_position
    }

    pub fn file_key(&self) -> &str {
        &self.file_key
    }

    pub fn artifact_digest(&self) -> &str {
        &self.artifact_digest
    }

    pub fn intent_fingerprint(&self) -> &str {
        &self.intent_fingerprint
    }

    pub(crate) fn from_grant(
        grant: &UaiUploadGrant,
        returned_file_key: String,
    ) -> ProviderResult<Self> {
        if returned_file_key != grant.file_key {
            return Err(ProviderError::new(
                ProviderErrorKind::RemoteChanged,
                "UAI uploaded artifact changed the granted object key",
            ));
        }
        Ok(Self {
            remote_task_id: grant.remote_task_id.clone(),
            course_resource_id: grant.course_resource_id.clone(),
            group_id: grant.group_id.clone(),
            upload_position: grant.upload_position,
            file_key: returned_file_key,
            artifact_digest: grant.artifact_digest.clone(),
            intent_fingerprint: grant.intent_fingerprint.clone(),
        })
    }
}

impl fmt::Debug for UaiUploadedArtifact {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UaiUploadedArtifact")
            .field("remote_task_id", &self.remote_task_id)
            .field("course_resource_id", &self.course_resource_id)
            .field("group_id", &self.group_id)
            .field("upload_position", &self.upload_position)
            .field("file_key", &"[ROUTE]")
            .field("artifact_digest", &self.artifact_digest)
            .field("intent_fingerprint", &self.intent_fingerprint)
            .finish()
    }
}

impl Drop for UaiUploadedArtifact {
    fn drop(&mut self) {
        self.remote_task_id.zeroize();
        self.course_resource_id.zeroize();
        self.group_id.zeroize();
        self.file_key.zeroize();
        self.artifact_digest.zeroize();
        self.intent_fingerprint.zeroize();
    }
}

/// Fresh Task-bound intent for exactly one audited upload module.
#[derive(Clone, Eq, PartialEq)]
pub struct UaiUploadIntent {
    remote_task_id: String,
    task_fingerprint: String,
    course_resource_id: String,
    group_id: String,
    upload_position: u32,
    artifact_digest: String,
    fingerprint: String,
}

impl UaiUploadIntent {
    pub fn remote_task_id(&self) -> &str {
        &self.remote_task_id
    }

    pub fn course_resource_id(&self) -> &str {
        &self.course_resource_id
    }

    pub fn group_id(&self) -> &str {
        &self.group_id
    }

    pub const fn upload_position(&self) -> u32 {
        self.upload_position
    }

    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    pub(crate) fn matches_artifact(&self, artifact: &UaiUploadArtifact) -> bool {
        self.artifact_digest == artifact.digest()
    }
}

impl fmt::Debug for UaiUploadIntent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UaiUploadIntent")
            .field("remote_task_id", &self.remote_task_id)
            .field("task_fingerprint", &self.task_fingerprint)
            .field("course_resource_id", &self.course_resource_id)
            .field("group_id", &self.group_id)
            .field("upload_position", &self.upload_position)
            .field("artifact_digest", &self.artifact_digest)
            .field("fingerprint", &self.fingerprint)
            .finish()
    }
}

/// Provider-private fresh Task gate for upload grant acquisition.
#[derive(Clone)]
pub struct UaiUploadPreparation {
    details: Arc<dyn TaskDetailCapability>,
}

impl UaiUploadPreparation {
    pub fn new(details: Arc<dyn TaskDetailCapability>) -> Self {
        Self { details }
    }

    /// Re-discovers the exact Group and freezes its unique upload module plus
    /// the selected artifact digest before either remote upload stage.
    ///
    /// # Errors
    ///
    /// Returns a typed error when the Task changes, lacks a unique audited
    /// upload position, or no longer matches its stable identity.
    pub async fn prepare_intent(
        &self,
        context: &ProviderContext,
        remote_task_id: &str,
        artifact: &UaiUploadArtifact,
    ) -> ProviderResult<UaiUploadIntent> {
        let detail = self.details.task_detail(context, remote_task_id).await?;
        build_upload_intent(&detail, remote_task_id, artifact)
    }
}

impl fmt::Debug for UaiUploadPreparation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UaiUploadPreparation")
            .field("details", &"configured")
            .finish()
    }
}

/// Bounded Provider-owned artifact bytes. Core must eventually supply these
/// through an owner/account/Task/Draft-bound artifact handle.
pub struct UaiUploadArtifact {
    filename: String,
    media_type: String,
    bytes: Zeroizing<Vec<u8>>,
}

impl UaiUploadArtifact {
    /// Freezes one bounded artifact accepted by the audited upload route.
    ///
    /// # Errors
    ///
    /// Rejects empty/oversized bytes and unsafe file metadata.
    pub fn try_new(filename: &str, media_type: &str, bytes: Vec<u8>) -> ProviderResult<Self> {
        if bytes.is_empty() || bytes.len() > MAX_UPLOAD_ARTIFACT_BYTES {
            return Err(invalid_input(
                "UAI upload artifact has an invalid bounded size",
            ));
        }
        if filename.is_empty()
            || filename.len() > 128
            || !filename
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        {
            return Err(invalid_input("UAI upload artifact filename is invalid"));
        }
        if media_type != "audio/mpeg" {
            return Err(invalid_input(
                "UAI audited upload artifact must use audio/mpeg",
            ));
        }
        Ok(Self {
            filename: filename.to_owned(),
            media_type: media_type.to_owned(),
            bytes: Zeroizing::new(bytes),
        })
    }

    /// Builds the exact donor placeholder MP3 used by the optional empty-file
    /// upload capability.
    pub fn donor_minimal_mp3() -> Self {
        let mut bytes = vec![0_u8; MINIMAL_MP3_BYTES];
        bytes[..4].copy_from_slice(&[0xff, 0xfb, 0x90, 0x00]);
        Self {
            filename: "nothing.mp3".to_owned(),
            media_type: "audio/mpeg".to_owned(),
            bytes: Zeroizing::new(bytes),
        }
    }

    pub fn filename(&self) -> &str {
        &self.filename
    }

    pub fn media_type(&self) -> &str {
        &self.media_type
    }

    pub fn expose_bytes(&self) -> &[u8] {
        self.bytes.as_slice()
    }

    pub fn digest(&self) -> String {
        let mut digest = Sha256::new();
        digest.update(b"asterism:uai:upload-artifact:v1\0");
        digest.update(self.filename.as_bytes());
        digest.update(b"\0");
        digest.update(self.media_type.as_bytes());
        digest.update(b"\0");
        digest.update(self.bytes.as_slice());
        format!("sha256:{:x}", digest.finalize())
    }
}

impl fmt::Debug for UaiUploadArtifact {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UaiUploadArtifact")
            .field("filename", &self.filename)
            .field("media_type", &self.media_type)
            .field("byte_length", &self.bytes.len())
            .finish()
    }
}

impl Drop for UaiUploadArtifact {
    fn drop(&mut self) {
        self.filename.zeroize();
        self.media_type.zeroize();
    }
}

/// Complete zeroizing multipart body for the fixed Qiniu upload route.
pub struct UaiMultipartUpload {
    content_type: String,
    body: Zeroizing<Vec<u8>>,
}

impl UaiMultipartUpload {
    pub fn content_type(&self) -> &str {
        &self.content_type
    }

    pub fn expose_body(&self) -> &[u8] {
        self.body.as_slice()
    }
}

impl fmt::Debug for UaiMultipartUpload {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UaiMultipartUpload")
            .field("content_type", &self.content_type)
            .field("body", &"[REDACTED]")
            .field("byte_length", &self.body.len())
            .finish()
    }
}

impl Drop for UaiMultipartUpload {
    fn drop(&mut self) {
        self.content_type.zeroize();
    }
}

/// Parses one CMS upload grant and retains the token only in zeroizing memory.
///
/// # Errors
///
/// Returns a typed invalid-response or protocol-drift error for rejected,
/// malformed or unbounded responses.
pub fn parse_upload_grant(
    document: &str,
    intent: &UaiUploadIntent,
) -> ProviderResult<UaiUploadGrant> {
    if document.is_empty() || document.len() > MAX_UPLOAD_GRANT_RESPONSE_BYTES {
        return Err(invalid_response(
            "UAI upload grant response is empty or oversized",
        ));
    }
    let root = ZeroizingJsonValue::new(
        serde_json::from_str::<Value>(document)
            .map_err(|_| invalid_response("UAI upload grant response is not valid JSON"))?,
    );
    let root = root
        .as_value()
        .as_object()
        .ok_or_else(|| protocol_drift("UAI upload grant response is not an object"))?;
    if root.get("code").and_then(Value::as_i64) != Some(200) {
        return Err(invalid_response("UAI rejected the upload grant request"));
    }
    let token = required_string(root.get("upToken"), MAX_UPLOAD_TOKEN_BYTES, "upload token")?;
    let file_key = required_string(root.get("fileKey"), MAX_UPLOAD_KEY_BYTES, "upload file key")?;
    if file_key.chars().any(char::is_control) {
        return Err(protocol_drift("UAI upload file key is unsafe"));
    }
    Ok(UaiUploadGrant {
        token: Zeroizing::new(token),
        file_key,
        intent_fingerprint: intent.fingerprint.clone(),
        artifact_digest: intent.artifact_digest.clone(),
        remote_task_id: intent.remote_task_id.clone(),
        course_resource_id: intent.course_resource_id.clone(),
        group_id: intent.group_id.clone(),
        upload_position: intent.upload_position,
    })
}

/// Builds a bounded multipart body containing only token, exact key and the
/// selected artifact.
///
/// # Errors
///
/// Returns an invalid-input error if the composed multipart body would exceed
/// the artifact bound plus fixed framing overhead.
pub fn build_upload_multipart(
    grant: &UaiUploadGrant,
    artifact: &UaiUploadArtifact,
) -> ProviderResult<UaiMultipartUpload> {
    grant.validate_for_artifact(artifact)?;
    let boundary = upload_boundary(grant.file_key(), artifact.expose_bytes());
    let mut body = Zeroizing::new(Vec::with_capacity(
        artifact.expose_bytes().len().saturating_add(16 * 1_024),
    ));
    append_text_part(&mut body, &boundary, "token", grant.expose_token());
    append_text_part(&mut body, &boundary, "key", grant.file_key());
    body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    body.extend_from_slice(
        format!(
            "Content-Disposition: form-data; name=\"file\"; filename=\"{}\"\r\n",
            artifact.filename()
        )
        .as_bytes(),
    );
    body.extend_from_slice(format!("Content-Type: {}\r\n\r\n", artifact.media_type()).as_bytes());
    body.extend_from_slice(artifact.expose_bytes());
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    if body.len() > MAX_UPLOAD_ARTIFACT_BYTES.saturating_add(16 * 1_024) {
        return Err(invalid_input("UAI multipart upload exceeds the size limit"));
    }
    Ok(UaiMultipartUpload {
        content_type: format!("multipart/form-data; boundary={boundary}"),
        body,
    })
}

/// Parses the object-store response and requires the exact granted file key.
///
/// # Errors
///
/// Returns a typed invalid-response or remote-changed error for malformed,
/// oversized or mismatched responses.
pub fn parse_upload_result(document: &str, expected_file_key: &str) -> ProviderResult<String> {
    if document.is_empty() || document.len() > MAX_UPLOAD_RESPONSE_BYTES {
        return Err(invalid_response(
            "UAI object upload response is empty or oversized",
        ));
    }
    let root = ZeroizingJsonValue::new(
        serde_json::from_str::<Value>(document)
            .map_err(|_| invalid_response("UAI object upload response is not valid JSON"))?,
    );
    let key = root
        .as_value()
        .as_object()
        .and_then(|root| root.get("key"))
        .and_then(Value::as_str)
        .filter(|key| !key.is_empty() && key.len() <= MAX_UPLOAD_KEY_BYTES)
        .ok_or_else(|| protocol_drift("UAI object upload response has no file key"))?;
    if key != expected_file_key {
        return Err(ProviderError::new(
            ProviderErrorKind::RemoteChanged,
            "UAI object upload response changed the granted file key",
        ));
    }
    Ok(key.to_owned())
}

fn build_upload_intent(
    detail: &asterism_provider_api::RemoteTaskDetail,
    remote_task_id: &str,
    artifact: &UaiUploadArtifact,
) -> ProviderResult<UaiUploadIntent> {
    if detail.task.remote_id != remote_task_id {
        return Err(ProviderError::new(
            ProviderErrorKind::RemoteChanged,
            "UAI fresh upload Task identity changed",
        ));
    }
    let task = detail
        .normalized_detail
        .get("task")
        .and_then(Value::as_object)
        .ok_or_else(|| protocol_drift("UAI fresh upload Task has no normalized detail"))?;
    if task.get("schema").and_then(Value::as_str) != Some("uai.group-task.v1") {
        return Err(protocol_drift(
            "UAI fresh upload Task has an unknown normalized schema",
        ));
    }
    let course_resource_id = safe_remote_component(task.get("course_resource_id"), "Course")?;
    let group_id = safe_remote_component(task.get("group_id"), "Group")?;
    let expected_remote_task_id = format!(
        "group:{course_resource_id}:{}:{group_id}",
        task.get("unit")
            .and_then(Value::as_object)
            .and_then(|unit| unit.get("id"))
            .and_then(Value::as_str)
            .filter(|value| is_remote_component(value))
            .ok_or_else(|| protocol_drift("UAI fresh upload Task has no valid Unit identity"))?
    );
    if expected_remote_task_id != remote_task_id {
        return Err(ProviderError::new(
            ProviderErrorKind::RemoteChanged,
            "UAI fresh upload hierarchy does not match the requested Task",
        ));
    }
    let task_types = task
        .get("task_types")
        .and_then(Value::as_array)
        .ok_or_else(|| protocol_drift("UAI fresh upload Task has no type array"))?;
    let question_count = task
        .get("question_count")
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .filter(|value| *value == task_types.len() && *value > 0)
        .ok_or_else(|| protocol_drift("UAI fresh upload Task type/count shape changed"))?;
    let positions = task_types
        .iter()
        .enumerate()
        .filter_map(|(index, value)| (value.as_str() == Some("multiFileUpload")).then_some(index))
        .collect::<Vec<_>>();
    if positions.len() != 1
        || task_types
            .iter()
            .any(|value| value.as_str().is_none_or(str::is_empty))
    {
        return Err(ProviderError::new(
            ProviderErrorKind::UnsupportedTask,
            "UAI upload requires exactly one audited upload module",
        ));
    }
    let upload_position = u32::try_from(positions[0] + 1)
        .map_err(|_| protocol_drift("UAI upload module position exceeds its bound"))?;
    let artifact_digest = artifact.digest();
    let mut digest = Sha256::new();
    digest.update(b"asterism:uai:upload-intent:v1\0");
    digest.update(remote_task_id.as_bytes());
    digest.update(b"\0");
    digest.update(detail.task.fingerprint.as_bytes());
    digest.update(b"\0");
    digest.update(upload_position.to_be_bytes());
    digest.update(b"\0");
    digest.update(artifact_digest.as_bytes());
    let fingerprint = format!("uai-upload-v1:{:x}", digest.finalize());
    let _ = question_count;
    Ok(UaiUploadIntent {
        remote_task_id: remote_task_id.to_owned(),
        task_fingerprint: detail.task.fingerprint.clone(),
        course_resource_id,
        group_id,
        upload_position,
        artifact_digest,
        fingerprint,
    })
}

fn safe_remote_component(value: Option<&Value>, label: &'static str) -> ProviderResult<String> {
    value
        .and_then(Value::as_str)
        .filter(|value| is_remote_component(value))
        .map(str::to_owned)
        .ok_or_else(|| {
            protocol_drift(format!(
                "UAI fresh upload Task has no valid {label} identity"
            ))
        })
}

fn is_remote_component(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn upload_boundary(file_key: &str, bytes: &[u8]) -> String {
    let mut digest = Sha256::new();
    digest.update(b"asterism:uai:upload-boundary:v1\0");
    digest.update(file_key.as_bytes());
    digest.update(bytes.len().to_le_bytes());
    format!("----asterism-uai-{:x}", digest.finalize())
}

fn append_text_part(body: &mut Vec<u8>, boundary: &str, name: &str, value: &str) {
    body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    body.extend_from_slice(
        format!("Content-Disposition: form-data; name=\"{name}\"\r\n\r\n").as_bytes(),
    );
    body.extend_from_slice(value.as_bytes());
    body.extend_from_slice(b"\r\n");
}

fn required_string(
    value: Option<&Value>,
    maximum_bytes: usize,
    label: &'static str,
) -> ProviderResult<String> {
    value
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty() && value.len() <= maximum_bytes)
        .map(str::to_owned)
        .ok_or_else(|| protocol_drift(format!("UAI upload grant has no valid {label}")))
}

fn invalid_input(message: impl Into<String>) -> ProviderError {
    ProviderError::new(ProviderErrorKind::InvalidResponse, message)
}

fn invalid_response(message: impl Into<String>) -> ProviderError {
    ProviderError::new(ProviderErrorKind::InvalidResponse, message)
}

fn protocol_drift(message: impl Into<String>) -> ProviderError {
    ProviderError::new(ProviderErrorKind::ProtocolDrift, message)
}

#[cfg(test)]
mod tests {
    use asterism_domain::{AssessmentClass, RemoteState, SourceType, TaskCapability};
    use asterism_provider_api::{RemoteTask, RemoteTaskDetail};

    use super::*;

    #[test]
    fn upload_intent_is_fresh_task_position_and_artifact_bound() {
        let artifact = UaiUploadArtifact::donor_minimal_mp3();
        let detail = upload_detail(&["multichoice", "multiFileUpload"]);
        let intent =
            build_upload_intent(&detail, "group:2001:unit-1:group-upload", &artifact).unwrap();
        assert_eq!(intent.upload_position(), 2);
        assert_eq!(intent.course_resource_id(), "2001");
        assert_eq!(intent.group_id(), "group-upload");
        assert!(intent.matches_artifact(&artifact));

        let duplicate = upload_detail(&["multiFileUpload", "multiFileUpload"]);
        assert!(
            build_upload_intent(&duplicate, "group:2001:unit-1:group-upload", &artifact,).is_err()
        );
        assert!(
            build_upload_intent(&detail, "group:other:unit-1:group-upload", &artifact).is_err()
        );
    }

    #[test]
    fn upload_grant_is_bounded_and_redacted() {
        let artifact = UaiUploadArtifact::donor_minimal_mp3();
        let intent = fixture_intent(&artifact);
        let grant = parse_upload_grant(
            r#"{"code":200,"upToken":"secret-upload-token","fileKey":"course/42/nothing.mp3"}"#,
            &intent,
        )
        .unwrap();
        assert_eq!(grant.expose_token(), "secret-upload-token");
        assert_eq!(grant.file_key(), "course/42/nothing.mp3");
        assert!(!format!("{grant:?}").contains("secret-upload-token"));
        assert!(parse_upload_grant(r#"{"code":500}"#, &intent).is_err());
    }

    #[test]
    fn donor_minimal_mp3_builds_one_secret_zeroizing_multipart_body() {
        let artifact = UaiUploadArtifact::donor_minimal_mp3();
        let intent = fixture_intent(&artifact);
        let grant = parse_upload_grant(
            r#"{"code":200,"upToken":"secret-upload-token","fileKey":"course/42/nothing.mp3"}"#,
            &intent,
        )
        .unwrap();
        assert_eq!(artifact.expose_bytes().len(), MINIMAL_MP3_BYTES);
        assert_eq!(&artifact.expose_bytes()[..4], &[0xff, 0xfb, 0x90, 0x00]);
        let multipart = build_upload_multipart(&grant, &artifact).unwrap();
        assert!(
            multipart
                .content_type()
                .starts_with("multipart/form-data; boundary=")
        );
        let body = multipart.expose_body();
        assert!(
            body.windows(b"secret-upload-token".len())
                .any(|window| window == b"secret-upload-token")
        );
        assert!(
            body.windows(grant.file_key().len())
                .any(|window| window == grant.file_key().as_bytes())
        );
        assert!(!format!("{multipart:?}").contains("secret-upload-token"));

        let foreign_artifact = UaiUploadArtifact::try_new(
            "different.mp3",
            "audio/mpeg",
            vec![7_u8; MINIMAL_MP3_BYTES],
        )
        .unwrap();
        assert!(build_upload_multipart(&grant, &foreign_artifact).is_err());
    }

    #[test]
    fn object_upload_result_must_repeat_the_exact_granted_key() {
        assert_eq!(
            parse_upload_result(
                r#"{"key":"course/42/nothing.mp3"}"#,
                "course/42/nothing.mp3"
            )
            .unwrap(),
            "course/42/nothing.mp3"
        );
        assert!(
            parse_upload_result(r#"{"key":"other/file.mp3"}"#, "course/42/nothing.mp3").is_err()
        );
    }

    #[test]
    fn uploaded_artifact_retains_task_module_and_artifact_binding() {
        let artifact = UaiUploadArtifact::donor_minimal_mp3();
        let intent = fixture_intent(&artifact);
        let grant = parse_upload_grant(
            r#"{"code":200,"upToken":"secret-upload-token","fileKey":"course/42/nothing.mp3"}"#,
            &intent,
        )
        .unwrap();
        let uploaded =
            UaiUploadedArtifact::from_grant(&grant, "course/42/nothing.mp3".to_owned()).unwrap();
        assert_eq!(uploaded.remote_task_id(), intent.remote_task_id());
        assert_eq!(uploaded.group_id(), intent.group_id());
        assert_eq!(uploaded.upload_position(), intent.upload_position());
        assert_eq!(uploaded.file_key(), grant.file_key());
        assert_eq!(uploaded.artifact_digest(), artifact.digest());
        assert_eq!(uploaded.intent_fingerprint(), intent.fingerprint());
        assert!(!format!("{uploaded:?}").contains("course/42/nothing.mp3"));
        assert!(UaiUploadedArtifact::from_grant(&grant, "other/key.mp3".to_owned()).is_err());
    }

    fn fixture_intent(artifact: &UaiUploadArtifact) -> UaiUploadIntent {
        let artifact_digest = artifact.digest();
        UaiUploadIntent {
            remote_task_id: "group:2001:unit-1:group-upload".to_owned(),
            task_fingerprint: "v1:upload".to_owned(),
            course_resource_id: "2001".to_owned(),
            group_id: "group-upload".to_owned(),
            upload_position: 1,
            artifact_digest,
            fingerprint: "uai-upload-v1:fixture".to_owned(),
        }
    }

    fn upload_detail(task_types: &[&str]) -> RemoteTaskDetail {
        let normalized = serde_json::json!({
            "schema": "uai.group-task.v1",
            "course_resource_id": "2001",
            "unit": {"id": "unit-1", "title": "Unit 1"},
            "section": {"id": "section-1", "title": "Section 1"},
            "micro": {"id": "micro-1", "title": "Speaking"},
            "group_id": "group-upload",
            "task_types": task_types,
            "question_count": task_types.len(),
        });
        RemoteTaskDetail {
            task: RemoteTask {
                remote_id: "group:2001:unit-1:group-upload".to_owned(),
                course_remote_id: Some("course-resource:2001".to_owned()),
                title: "Upload recording".to_owned(),
                source_type: SourceType::Resource,
                assessment_class: AssessmentClass::Routine,
                remote_state: RemoteState::Unknown,
                opens_at: None,
                due_at: None,
                closes_at: None,
                capabilities: vec![TaskCapability::BrowserBridge],
                fingerprint: "v1:upload".to_owned(),
                normalized: normalized.clone(),
                raw_sanitized: serde_json::json!({"schema":"uai.group-task.raw.v1"}),
            },
            normalized_detail: serde_json::json!({
                "schema": "uai.group-task-detail.v1",
                "task": normalized,
            }),
        }
    }
}
