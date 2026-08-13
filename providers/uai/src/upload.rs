use std::fmt;

use asterism_provider_api::{ProviderContext, ProviderError, ProviderErrorKind, ProviderResult};
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
        course_resource_id: &str,
        group_id: &str,
        artifact: &UaiUploadArtifact,
    ) -> ProviderResult<UaiUploadGrant>;

    async fn upload_artifact(
        &self,
        context: &ProviderContext,
        grant: &UaiUploadGrant,
        artifact: &UaiUploadArtifact,
    ) -> ProviderResult<String>;
}

/// Short-lived CMS/object-store authorization returned by UAI.
pub struct UaiUploadGrant {
    token: Zeroizing<String>,
    file_key: String,
}

impl UaiUploadGrant {
    pub fn expose_token(&self) -> &str {
        self.token.as_str()
    }

    pub fn file_key(&self) -> &str {
        &self.file_key
    }
}

impl fmt::Debug for UaiUploadGrant {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UaiUploadGrant")
            .field("token", &"[REDACTED]")
            .field("file_key", &"[ROUTE]")
            .finish()
    }
}

impl Drop for UaiUploadGrant {
    fn drop(&mut self) {
        self.file_key.zeroize();
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
pub fn parse_upload_grant(document: &str) -> ProviderResult<UaiUploadGrant> {
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
    use super::*;

    #[test]
    fn upload_grant_is_bounded_and_redacted() {
        let grant = parse_upload_grant(
            r#"{"code":200,"upToken":"secret-upload-token","fileKey":"course/42/nothing.mp3"}"#,
        )
        .unwrap();
        assert_eq!(grant.expose_token(), "secret-upload-token");
        assert_eq!(grant.file_key(), "course/42/nothing.mp3");
        assert!(!format!("{grant:?}").contains("secret-upload-token"));
        assert!(parse_upload_grant(r#"{"code":500}"#).is_err());
    }

    #[test]
    fn donor_minimal_mp3_builds_one_secret_zeroizing_multipart_body() {
        let grant = parse_upload_grant(
            r#"{"code":200,"upToken":"secret-upload-token","fileKey":"course/42/nothing.mp3"}"#,
        )
        .unwrap();
        let artifact = UaiUploadArtifact::donor_minimal_mp3();
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
}
