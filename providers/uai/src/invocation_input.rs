use std::fmt;

use asterism_provider_api::{ProviderError, ProviderErrorKind, ProviderResult};
use asterism_secrets::SecretValue;
use zeroize::Zeroizing;

use crate::UaiUploadArtifact;

/// Public `input_type` for one exact discussion reply.
pub const UAI_DISCUSSION_REPLY_INPUT_TYPE: &str = "uai.discussion.reply-input.v1";
/// Public `input_type` for one MP3 artifact upload.
pub const UAI_ARTIFACT_UPLOAD_INPUT_TYPE: &str = "uai.artifact-upload.mp3-input.v1";
/// Public `input_type` for explicit authorization of the donor-derived oral
/// evidence paired with an immutable ordinary Submission Draft.
pub const UAI_COMPOUND_ORAL_INPUT_TYPE: &str = "uai.compound-oral.authorization.v1";

const DISCUSSION_MAGIC: &[u8] = b"uai.discussion.reply-input.v1\0";
const UPLOAD_MAGIC: &[u8] = b"uai.artifact-upload.mp3-input.v1\0";
const ORAL_MAGIC: &[u8] = b"uai.compound-oral.authorization.v1\0";
const MAX_DISCUSSION_CONTENT_BYTES: usize = 8 * 1_024;
const MAX_UPLOAD_FILENAME_BYTES: usize = 128;
const MAX_UPLOAD_CODEC_BYTES: usize = 64 * 1_024 * 1_024;

/// Encodes one exact bounded discussion reply for
/// [`UAI_DISCUSSION_REPLY_INPUT_TYPE`].
///
/// # Errors
///
/// Rejects empty, untrimmed, control-bearing or oversized content.
pub fn encode_discussion_reply_input(content: &str) -> ProviderResult<SecretValue> {
    if content.is_empty()
        || content.len() > MAX_DISCUSSION_CONTENT_BYTES
        || content.trim() != content
        || content
            .chars()
            .any(|character| character.is_control() && !matches!(character, '\n' | '\r' | '\t'))
    {
        return Err(invalid_input());
    }
    let length = u32::try_from(content.len()).map_err(|_| invalid_input())?;
    let mut encoded = Vec::with_capacity(DISCUSSION_MAGIC.len() + 4 + content.len());
    encoded.extend_from_slice(DISCUSSION_MAGIC);
    encoded.extend_from_slice(&length.to_be_bytes());
    encoded.extend_from_slice(content.as_bytes());
    Ok(SecretValue::new(encoded))
}

/// Encodes one exact MP3 artifact for [`UAI_ARTIFACT_UPLOAD_INPUT_TYPE`].
///
/// # Errors
///
/// Rejects an artifact whose complete public invocation codec would exceed
/// Core's 64 MiB private-input boundary.
pub fn encode_artifact_upload_input(artifact: &UaiUploadArtifact) -> ProviderResult<SecretValue> {
    let filename_length = u16::try_from(artifact.filename().len()).map_err(|_| invalid_input())?;
    let artifact_length =
        u32::try_from(artifact.expose_bytes().len()).map_err(|_| invalid_input())?;
    let encoded_length = UPLOAD_MAGIC
        .len()
        .checked_add(2)
        .and_then(|length| length.checked_add(artifact.filename().len()))
        .and_then(|length| length.checked_add(4))
        .and_then(|length| length.checked_add(artifact.expose_bytes().len()))
        .filter(|length| *length <= MAX_UPLOAD_CODEC_BYTES)
        .ok_or_else(invalid_input)?;
    let mut encoded = Vec::with_capacity(encoded_length);
    encoded.extend_from_slice(UPLOAD_MAGIC);
    encoded.extend_from_slice(&filename_length.to_be_bytes());
    encoded.extend_from_slice(artifact.filename().as_bytes());
    encoded.extend_from_slice(&artifact_length.to_be_bytes());
    encoded.extend_from_slice(artifact.expose_bytes());
    Ok(SecretValue::new(encoded))
}

/// Encodes the exact explicit authorization marker for
/// [`UAI_COMPOUND_ORAL_INPUT_TYPE`].
#[must_use]
pub fn encode_compound_oral_authorization() -> SecretValue {
    SecretValue::new(ORAL_MAGIC.to_vec())
}

/// Decoded discussion content. Debug never exposes caller text.
pub(crate) struct UaiDiscussionInvocationInput {
    content: Zeroizing<String>,
}

impl UaiDiscussionInvocationInput {
    pub(crate) fn decode(value: &SecretValue) -> ProviderResult<Self> {
        let mut reader = Reader::new(value.expose_secret());
        reader.expect_magic(DISCUSSION_MAGIC)?;
        let length = reader.read_u32_as_usize()?;
        if length == 0 || length > MAX_DISCUSSION_CONTENT_BYTES {
            return Err(invalid_input());
        }
        let content = std::str::from_utf8(reader.take(length)?)
            .map_err(|_| invalid_input())?
            .to_owned();
        if !reader.finished()
            || content.trim() != content
            || content
                .chars()
                .any(|character| character.is_control() && !matches!(character, '\n' | '\r' | '\t'))
        {
            return Err(invalid_input());
        }
        Ok(Self {
            content: Zeroizing::new(content),
        })
    }

    pub(crate) fn content(&self) -> &str {
        self.content.as_str()
    }
}

impl fmt::Debug for UaiDiscussionInvocationInput {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UaiDiscussionInvocationInput")
            .field("content", &"[REDACTED]")
            .finish()
    }
}

/// Decoded bounded MP3 artifact. Debug never exposes bytes or filename.
pub(crate) struct UaiArtifactUploadInvocationInput {
    artifact: UaiUploadArtifact,
}

impl UaiArtifactUploadInvocationInput {
    pub(crate) fn decode(value: &SecretValue) -> ProviderResult<Self> {
        let bytes = value.expose_secret();
        if bytes.len() > MAX_UPLOAD_CODEC_BYTES {
            return Err(invalid_input());
        }
        let mut reader = Reader::new(bytes);
        reader.expect_magic(UPLOAD_MAGIC)?;
        let filename_length = usize::from(reader.read_u16()?);
        if filename_length == 0 || filename_length > MAX_UPLOAD_FILENAME_BYTES {
            return Err(invalid_input());
        }
        let filename = Zeroizing::new(
            std::str::from_utf8(reader.take(filename_length)?)
                .map_err(|_| invalid_input())?
                .to_owned(),
        );
        let artifact_length = reader.read_u32_as_usize()?;
        if artifact_length == 0 || !reader.remaining_is(artifact_length) {
            return Err(invalid_input());
        }
        let artifact_bytes = reader.take(artifact_length)?.to_vec();
        if !reader.finished() {
            return Err(invalid_input());
        }
        let artifact = UaiUploadArtifact::try_new(filename.as_str(), "audio/mpeg", artifact_bytes)
            .map_err(|_| invalid_input())?;
        Ok(Self { artifact })
    }

    pub(crate) const fn artifact(&self) -> &UaiUploadArtifact {
        &self.artifact
    }
}

impl fmt::Debug for UaiArtifactUploadInvocationInput {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UaiArtifactUploadInvocationInput")
            .field("artifact", &"[REDACTED]")
            .finish()
    }
}

pub(crate) fn validate_compound_oral_authorization(value: &SecretValue) -> ProviderResult<()> {
    if value.expose_secret() == ORAL_MAGIC {
        Ok(())
    } else {
        Err(invalid_input())
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> Reader<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }

    fn expect_magic(&mut self, magic: &[u8]) -> ProviderResult<()> {
        if self.take(magic.len())? == magic {
            Ok(())
        } else {
            Err(invalid_input())
        }
    }

    fn read_u16(&mut self) -> ProviderResult<u16> {
        let bytes: [u8; 2] = self.take(2)?.try_into().map_err(|_| invalid_input())?;
        Ok(u16::from_be_bytes(bytes))
    }

    fn read_u32_as_usize(&mut self) -> ProviderResult<usize> {
        let bytes: [u8; 4] = self.take(4)?.try_into().map_err(|_| invalid_input())?;
        usize::try_from(u32::from_be_bytes(bytes)).map_err(|_| invalid_input())
    }

    fn take(&mut self, length: usize) -> ProviderResult<&'a [u8]> {
        let end = self
            .position
            .checked_add(length)
            .filter(|end| *end <= self.bytes.len())
            .ok_or_else(invalid_input)?;
        let value = &self.bytes[self.position..end];
        self.position = end;
        Ok(value)
    }

    const fn finished(&self) -> bool {
        self.position == self.bytes.len()
    }

    fn remaining_is(&self, length: usize) -> bool {
        self.bytes.len().saturating_sub(self.position) == length
    }
}

fn invalid_input() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::InvalidResponse,
        "UAI private invocation input is invalid",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discussion_codec_is_exact_bounded_and_redacted() {
        let content = "bounded reply";
        let encoded = encode_discussion_reply_input(content).unwrap();
        let input = UaiDiscussionInvocationInput::decode(&encoded).unwrap();
        assert_eq!(input.content(), content);
        assert!(!format!("{input:?}").contains(content));
        assert!(encode_discussion_reply_input(" padded ").is_err());

        let mut trailing = DISCUSSION_MAGIC.to_vec();
        trailing.extend_from_slice(&1_u32.to_be_bytes());
        trailing.extend_from_slice(b"x!");
        assert!(UaiDiscussionInvocationInput::decode(&SecretValue::new(trailing)).is_err());
    }

    #[test]
    fn upload_codec_fixes_media_type_and_rejects_trailing_bytes() {
        let filename = "answer.mp3";
        let artifact = vec![7_u8; 4_096];
        let upload = UaiUploadArtifact::try_new(filename, "audio/mpeg", artifact.clone()).unwrap();
        let encoded = encode_artifact_upload_input(&upload).unwrap();
        let input = UaiArtifactUploadInvocationInput::decode(&encoded).unwrap();
        assert_eq!(input.artifact().filename(), filename);
        assert_eq!(input.artifact().media_type(), "audio/mpeg");
        assert_eq!(input.artifact().expose_bytes(), artifact);
        assert!(!format!("{input:?}").contains(filename));

        let mut encoded = encoded.expose_secret().to_vec();
        encoded.push(0);
        assert!(UaiArtifactUploadInvocationInput::decode(&SecretValue::new(encoded)).is_err());
    }

    #[test]
    fn oral_authorization_is_magic_only() {
        assert!(
            validate_compound_oral_authorization(&encode_compound_oral_authorization()).is_ok()
        );
        let mut changed = ORAL_MAGIC.to_vec();
        changed.push(0);
        assert!(validate_compound_oral_authorization(&SecretValue::new(changed)).is_err());
    }
}
