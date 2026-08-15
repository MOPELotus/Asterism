use std::collections::HashSet;

use asterism_domain::{AssessmentClass, RemoteState, SourceType, TaskCapability};
use asterism_provider_api::{ProviderError, ProviderErrorKind, ProviderResult, RemoteTask};
use asterism_secrets::SecretString;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use zeroize::Zeroize;

use crate::ChaoxingCourseScope;

const MAX_CARD_DOCUMENT_BYTES: usize = 4 * 1_024 * 1_024;
const MAX_ATTACHMENTS_PER_CARD: usize = 256;
const MAX_REMOTE_COMPONENT_BYTES: usize = 128;
const MAX_TITLE_BYTES: usize = 512;
const MAX_CARD_INDEX: u8 = 6;
const MAX_EPHEMERAL_TOKEN_BYTES: usize = 4 * 1_024;
const MAX_VIDEO_PLAY_TIME_MILLIS: u64 = 7 * 24 * 60 * 60 * 1_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ChaoxingImmediateResourceKind {
    Document,
    Read,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ChaoxingMediaKind {
    Video,
    Audio,
}

impl ChaoxingMediaKind {
    pub(crate) const fn resource_kind(self) -> &'static str {
        match self {
            Self::Video => "video",
            Self::Audio => "audio",
        }
    }

    pub(crate) const fn dtype(self) -> &'static str {
        match self {
            Self::Video => "Video",
            Self::Audio => "Audio",
        }
    }
}

pub(crate) struct ChaoxingImmediateResourceTarget {
    kind: ChaoxingImmediateResourceKind,
    remote_state: RemoteState,
    job_id: String,
    token: Option<SecretString>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ChaoxingVideoRt {
    NineTenths,
    One,
}

impl ChaoxingVideoRt {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::NineTenths => "0.9",
            Self::One => "1",
        }
    }
}

pub(crate) struct ChaoxingMediaResourceTarget {
    kind: ChaoxingMediaKind,
    remote_state: RemoteState,
    job_id: String,
    object_id: SecretString,
    other_info: SecretString,
    initial_play_time_millis: u64,
    rt: Option<ChaoxingVideoRt>,
    face_capture_enc: Option<SecretString>,
    attendance_duration: Option<SecretString>,
    attendance_duration_enc: Option<SecretString>,
}

pub(crate) struct ChaoxingLiveResourceTarget {
    remote_state: RemoteState,
    job_id: String,
    live_id: SecretString,
    stream_name: SecretString,
    video_id: SecretString,
    status_job_id: Option<SecretString>,
}

/// One fresh Chapter Work attempt target. Route credentials remain in memory,
/// are redacted from diagnostics and are zeroized when the operation ends.
#[must_use]
pub struct ChaoxingChapterWorkTarget {
    job_id: String,
    work_id: String,
    enc: SecretString,
    knowledge_token: SecretString,
}

impl ChaoxingChapterWorkTarget {
    /// Returns the fresh card job identity.
    pub fn job_id(&self) -> &str {
        &self.job_id
    }

    /// Returns the server Work route identity.
    pub fn work_id(&self) -> &str {
        &self.work_id
    }

    /// Exposes the short-lived Work route token only to an authorized transport.
    pub fn enc(&self) -> &SecretString {
        &self.enc
    }

    /// Exposes the short-lived chapter token only to an authorized transport.
    pub fn knowledge_token(&self) -> &SecretString {
        &self.knowledge_token
    }
}

impl std::fmt::Debug for ChaoxingChapterWorkTarget {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ChaoxingChapterWorkTarget")
            .field("job_id", &"[REDACTED]")
            .field("work_id", &"[REDACTED]")
            .field("enc", &"[REDACTED]")
            .field("knowledge_token", &"[REDACTED]")
            .finish()
    }
}

impl Drop for ChaoxingChapterWorkTarget {
    fn drop(&mut self) {
        self.job_id.zeroize();
        self.work_id.zeroize();
    }
}

impl ChaoxingMediaResourceTarget {
    pub(crate) const fn kind(&self) -> ChaoxingMediaKind {
        self.kind
    }

    pub(crate) const fn remote_state(&self) -> RemoteState {
        self.remote_state
    }

    pub(crate) fn job_id(&self) -> &str {
        &self.job_id
    }

    pub(crate) fn object_id(&self) -> &SecretString {
        &self.object_id
    }

    pub(crate) fn other_info(&self) -> &SecretString {
        &self.other_info
    }

    pub(crate) const fn initial_play_time_millis(&self) -> u64 {
        self.initial_play_time_millis
    }

    pub(crate) const fn rt(&self) -> Option<ChaoxingVideoRt> {
        self.rt
    }

    pub(crate) fn face_capture_enc(&self) -> Option<&SecretString> {
        self.face_capture_enc.as_ref()
    }

    pub(crate) fn attendance_duration(&self) -> Option<&SecretString> {
        self.attendance_duration.as_ref()
    }

    pub(crate) fn attendance_duration_enc(&self) -> Option<&SecretString> {
        self.attendance_duration_enc.as_ref()
    }
}

impl std::fmt::Debug for ChaoxingMediaResourceTarget {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ChaoxingMediaResourceTarget")
            .field("kind", &self.kind)
            .field("remote_state", &self.remote_state)
            .field("job_id", &"[REDACTED]")
            .field("object_id", &"[REDACTED]")
            .field("other_info", &"[REDACTED]")
            .field("initial_play_time_millis", &self.initial_play_time_millis)
            .field("rt", &self.rt)
            .field(
                "face_capture_enc",
                &self.face_capture_enc.as_ref().map(|_| "[REDACTED]"),
            )
            .field(
                "attendance_duration",
                &self.attendance_duration.as_ref().map(|_| "[REDACTED]"),
            )
            .field(
                "attendance_duration_enc",
                &self.attendance_duration_enc.as_ref().map(|_| "[REDACTED]"),
            )
            .finish()
    }
}

impl ChaoxingLiveResourceTarget {
    pub(crate) const fn remote_state(&self) -> RemoteState {
        self.remote_state
    }

    pub(crate) fn job_id(&self) -> &str {
        &self.job_id
    }

    pub(crate) fn live_id(&self) -> &SecretString {
        &self.live_id
    }

    pub(crate) fn stream_name(&self) -> &SecretString {
        &self.stream_name
    }

    pub(crate) fn video_id(&self) -> &SecretString {
        &self.video_id
    }

    pub(crate) fn status_job_id(&self) -> Option<&SecretString> {
        self.status_job_id.as_ref()
    }
}

impl std::fmt::Debug for ChaoxingLiveResourceTarget {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ChaoxingLiveResourceTarget")
            .field("remote_state", &self.remote_state)
            .field("job_id", &"[REDACTED]")
            .field("live_id", &"[REDACTED]")
            .field("stream_name", &"[REDACTED]")
            .field("video_id", &"[REDACTED]")
            .field(
                "status_job_id",
                &self.status_job_id.as_ref().map(|_| "[REDACTED]"),
            )
            .finish()
    }
}

struct SensitiveCardRoot(Map<String, Value>);

impl SensitiveCardRoot {
    fn as_map(&self) -> &Map<String, Value> {
        &self.0
    }
}

impl Drop for SensitiveCardRoot {
    fn drop(&mut self) {
        for value in self.0.values_mut() {
            zeroize_json_value(value);
        }
    }
}

impl ChaoxingImmediateResourceTarget {
    pub(crate) const fn kind(&self) -> ChaoxingImmediateResourceKind {
        self.kind
    }

    pub(crate) const fn remote_state(&self) -> RemoteState {
        self.remote_state
    }

    pub(crate) fn job_id(&self) -> &str {
        &self.job_id
    }

    pub(crate) fn token(&self) -> Option<&SecretString> {
        self.token.as_ref()
    }
}

impl std::fmt::Debug for ChaoxingImmediateResourceTarget {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ChaoxingImmediateResourceTarget")
            .field("kind", &self.kind)
            .field("remote_state", &self.remote_state)
            .field("job_id", &"[REDACTED]")
            .field("token", &self.token.as_ref().map(|_| "[REDACTED]"))
            .finish()
    }
}

/// Parses one donor-observed `knowledge/cards` response without executing its
/// script. Routing and submission fields stay outside the normalized tasks.
///
/// # Errors
///
/// Returns a protocol error for malformed `mArg`, unbounded attachment lists,
/// unstable identities, duplicate tasks, or unknown attachments marked as
/// actual jobs.
pub fn parse_chapter_resource_inventory(
    html: &str,
    scope: &ChaoxingCourseScope,
    knowledge_id: &str,
    card_index: u8,
) -> ProviderResult<Vec<RemoteTask>> {
    if html.is_empty() || html.len() > MAX_CARD_DOCUMENT_BYTES {
        return Err(invalid_response(
            "Chaoxing chapter card document is empty or exceeds the size limit",
        ));
    }
    validate_component(knowledge_id, "knowledge identity")?;
    if card_index > MAX_CARD_INDEX {
        return Err(protocol_drift(
            "Chaoxing chapter card index exceeds the donor range",
        ));
    }
    if html.contains("章节未开放") {
        return Ok(Vec::new());
    }
    let Some(root) = parse_card_root(html)? else {
        return Ok(Vec::new());
    };
    let attachments = card_attachments(root.as_map())?;

    let mut tasks = Vec::new();
    let mut seen = HashSet::new();
    for attachment in attachments {
        let Some(task) = parse_resource_attachment(attachment, scope, knowledge_id, card_index)?
        else {
            continue;
        };
        if !seen.insert(task.remote_id.clone()) {
            return Err(protocol_drift(
                "Chaoxing chapter card contains a duplicate task identity",
            ));
        }
        tasks.push(task);
    }
    Ok(tasks)
}

/// Locates one immediate Document or Read execution target without persisting
/// its short-lived token. Callers must use a freshly fetched card document.
///
/// # Errors
///
/// Returns a typed error for foreign task identities, malformed or duplicate
/// card entries, unsupported resource kinds, or missing execution fields.
pub(crate) fn locate_immediate_resource_target(
    html: &str,
    scope: &ChaoxingCourseScope,
    knowledge_id: &str,
    card_index: u8,
    remote_task_id: &str,
) -> ProviderResult<Option<ChaoxingImmediateResourceTarget>> {
    if html.is_empty() || html.len() > MAX_CARD_DOCUMENT_BYTES {
        return Err(invalid_response(
            "Chaoxing chapter card document is empty or exceeds the size limit",
        ));
    }
    validate_component(knowledge_id, "knowledge identity")?;
    if card_index > MAX_CARD_INDEX {
        return Err(protocol_drift(
            "Chaoxing chapter card index exceeds the donor range",
        ));
    }
    let prefix = format!(
        "resource:{}:{}:{knowledge_id}:",
        scope.course_id(),
        scope.class_id()
    );
    let expected_task_id = remote_task_id
        .strip_prefix(&prefix)
        .ok_or_else(|| protocol_drift("Chaoxing resource target is outside the current scope"))?;
    validate_component(expected_task_id, "resource identity")?;
    if html.contains("章节未开放") {
        return Ok(None);
    }
    let Some(root) = parse_card_root(html)? else {
        return Ok(None);
    };
    let mut found = None;
    for attachment in card_attachments(root.as_map())? {
        let Some(task) = parse_resource_attachment(attachment, scope, knowledge_id, card_index)?
        else {
            continue;
        };
        if task.remote_id != remote_task_id {
            continue;
        }
        if found.is_some() {
            return Err(protocol_drift(
                "Chaoxing chapter card contains a duplicate execution target",
            ));
        }
        let card = attachment
            .as_object()
            .ok_or_else(|| protocol_drift("Chaoxing chapter attachment is not an object"))?;
        let kind = match task.normalized.get("resource_kind").and_then(Value::as_str) {
            Some("document") => ChaoxingImmediateResourceKind::Document,
            Some("read") => ChaoxingImmediateResourceKind::Read,
            Some(_) => {
                return Err(ProviderError::new(
                    ProviderErrorKind::UnsupportedTask,
                    "Chaoxing resource kind has no immediate execution path",
                ));
            }
            None => {
                return Err(protocol_drift(
                    "Chaoxing resource target has no normalized kind",
                ));
            }
        };
        let job_id = optional_string(card, "jobid")?
            .filter(|job_id| *job_id == expected_task_id)
            .ok_or_else(|| {
                protocol_drift("Chaoxing immediate resource has no matching job identity")
            })?
            .to_owned();
        let token = if task.remote_state == RemoteState::Completed {
            None
        } else {
            let token = optional_string(card, "jtoken")?
                .filter(|token| {
                    !token.is_empty()
                        && token.len() <= MAX_EPHEMERAL_TOKEN_BYTES
                        && !token.chars().any(char::is_control)
                })
                .ok_or_else(|| {
                    protocol_drift("Chaoxing immediate resource has no valid execution token")
                })?;
            Some(SecretString::new(token))
        };
        found = Some(ChaoxingImmediateResourceTarget {
            kind,
            remote_state: task.remote_state,
            job_id,
            token,
        });
    }
    Ok(found)
}

/// Locates one fresh Video or Audio target without persisting report credentials.
///
/// # Errors
///
/// Returns a typed error for foreign identities, duplicate cards, malformed
/// media metadata, or a task which is not a supported media resource.
pub(crate) fn locate_media_resource_target(
    html: &str,
    scope: &ChaoxingCourseScope,
    knowledge_id: &str,
    card_index: u8,
    remote_task_id: &str,
) -> ProviderResult<Option<ChaoxingMediaResourceTarget>> {
    if html.is_empty() || html.len() > MAX_CARD_DOCUMENT_BYTES {
        return Err(invalid_response(
            "Chaoxing chapter card document is empty or exceeds the size limit",
        ));
    }
    validate_component(knowledge_id, "knowledge identity")?;
    if card_index > MAX_CARD_INDEX {
        return Err(protocol_drift(
            "Chaoxing chapter card index exceeds the donor range",
        ));
    }
    let prefix = format!(
        "resource:{}:{}:{knowledge_id}:",
        scope.course_id(),
        scope.class_id()
    );
    let expected_task_id = remote_task_id
        .strip_prefix(&prefix)
        .ok_or_else(|| protocol_drift("Chaoxing media target is outside the current scope"))?;
    validate_component(expected_task_id, "resource identity")?;
    if html.contains("章节未开放") {
        return Ok(None);
    }
    let Some(root) = parse_card_root(html)? else {
        return Ok(None);
    };
    let mut found = None;
    for attachment in card_attachments(root.as_map())? {
        let Some(task) = parse_resource_attachment(attachment, scope, knowledge_id, card_index)?
        else {
            continue;
        };
        if task.remote_id != remote_task_id {
            continue;
        }
        if found.is_some() {
            return Err(protocol_drift(
                "Chaoxing chapter card contains a duplicate media target",
            ));
        }
        let kind = match task.normalized.get("resource_kind").and_then(Value::as_str) {
            Some("video") => ChaoxingMediaKind::Video,
            Some("audio") => ChaoxingMediaKind::Audio,
            _ => {
                return Err(ProviderError::new(
                    ProviderErrorKind::UnsupportedTask,
                    "Chaoxing resource kind is not a supported media task",
                ));
            }
        };
        let card = attachment
            .as_object()
            .ok_or_else(|| protocol_drift("Chaoxing chapter attachment is not an object"))?;
        let property = optional_object(card, "property")?;
        let job_id = optional_string(card, "jobid")?
            .filter(|job_id| *job_id == expected_task_id)
            .ok_or_else(|| protocol_drift("Chaoxing media has no matching job identity"))?
            .to_owned();
        let object_id = required_ephemeral_string(card, "objectId", "media object identity")?;
        validate_component(object_id, "media object identity")?;
        let other_info = required_ephemeral_string(card, "otherInfo", "media report context")?
            .split('&')
            .next()
            .ok_or_else(|| protocol_drift("Chaoxing media report context is empty"))?;
        let initial_play_time_millis = optional_u64(card, "playTime")?.unwrap_or_default();
        if initial_play_time_millis > MAX_VIDEO_PLAY_TIME_MILLIS {
            return Err(protocol_drift(
                "Chaoxing media initial progress exceeds the safety limit",
            ));
        }
        let rt = parse_video_rt(optional_string(property, "rt")?, other_info)?;
        found = Some(ChaoxingMediaResourceTarget {
            kind,
            remote_state: task.remote_state,
            job_id,
            object_id: SecretString::new(object_id),
            other_info: SecretString::new(other_info),
            initial_play_time_millis,
            rt,
            face_capture_enc: optional_ephemeral_secret(card, "videoFaceCaptureEnc")?,
            attendance_duration: optional_ephemeral_secret(card, "attDuration")?,
            attendance_duration_enc: optional_ephemeral_secret(card, "attDurationEnc")?,
        });
    }
    Ok(found)
}

/// Locates one fresh Live target without persisting its route credentials.
pub(crate) fn locate_live_resource_target(
    html: &str,
    scope: &ChaoxingCourseScope,
    knowledge_id: &str,
    card_index: u8,
    remote_task_id: &str,
) -> ProviderResult<Option<ChaoxingLiveResourceTarget>> {
    if html.is_empty() || html.len() > MAX_CARD_DOCUMENT_BYTES {
        return Err(invalid_response(
            "Chaoxing chapter card document is empty or exceeds the size limit",
        ));
    }
    validate_component(knowledge_id, "knowledge identity")?;
    if card_index > MAX_CARD_INDEX {
        return Err(protocol_drift(
            "Chaoxing chapter card index exceeds the donor range",
        ));
    }
    let prefix = format!(
        "resource:{}:{}:{knowledge_id}:",
        scope.course_id(),
        scope.class_id()
    );
    let expected_task_id = remote_task_id
        .strip_prefix(&prefix)
        .ok_or_else(|| protocol_drift("Chaoxing Live target is outside the current scope"))?;
    validate_component(expected_task_id, "resource identity")?;
    if html.contains("章节未开放") {
        return Ok(None);
    }
    let Some(root) = parse_card_root(html)? else {
        return Ok(None);
    };
    let mut found = None;
    for attachment in card_attachments(root.as_map())? {
        let Some(task) = parse_resource_attachment(attachment, scope, knowledge_id, card_index)?
        else {
            continue;
        };
        if task.remote_id != remote_task_id {
            continue;
        }
        if found.is_some() {
            return Err(protocol_drift(
                "Chaoxing chapter card contains a duplicate Live target",
            ));
        }
        if task.normalized.get("resource_kind").and_then(Value::as_str) != Some("live") {
            return Err(ProviderError::new(
                ProviderErrorKind::UnsupportedTask,
                "Chaoxing resource kind is not a Live task",
            ));
        }
        let card = attachment
            .as_object()
            .ok_or_else(|| protocol_drift("Chaoxing chapter attachment is not an object"))?;
        let property = optional_object(card, "property")?;
        let job_id = optional_string(card, "jobid")?
            .filter(|job_id| *job_id == expected_task_id)
            .ok_or_else(|| protocol_drift("Chaoxing Live has no matching job identity"))?
            .to_owned();
        found = Some(ChaoxingLiveResourceTarget {
            remote_state: task.remote_state,
            job_id,
            live_id: SecretString::new(required_ephemeral_string(
                property,
                "liveId",
                "Live identity",
            )?),
            stream_name: SecretString::new(required_ephemeral_string(
                property,
                "streamName",
                "Live stream identity",
            )?),
            video_id: SecretString::new(required_ephemeral_string(
                property,
                "vdoid",
                "Live video identity",
            )?),
            status_job_id: optional_ephemeral_secret(property, "_jobid")?,
        });
    }
    Ok(found)
}

/// Locates one fresh Chapter Work route without persisting its attempt
/// credentials. Callers must scan the complete bounded card set and reject a
/// duplicate target across cards.
///
/// # Errors
///
/// Returns a typed error for foreign identities, malformed card material,
/// duplicate matches, or a target which is not a pending Chapter Work.
pub(crate) fn locate_chapter_work_target(
    html: &str,
    scope: &ChaoxingCourseScope,
    knowledge_id: &str,
    card_index: u8,
    remote_task_id: &str,
) -> ProviderResult<Option<ChaoxingChapterWorkTarget>> {
    if html.is_empty() || html.len() > MAX_CARD_DOCUMENT_BYTES {
        return Err(invalid_response(
            "Chaoxing chapter card document is empty or exceeds the size limit",
        ));
    }
    validate_component(knowledge_id, "knowledge identity")?;
    if card_index > MAX_CARD_INDEX {
        return Err(protocol_drift(
            "Chaoxing chapter card index exceeds the donor range",
        ));
    }
    let prefix = format!(
        "resource:{}:{}:{knowledge_id}:",
        scope.course_id(),
        scope.class_id()
    );
    let expected_job_id = remote_task_id
        .strip_prefix(&prefix)
        .ok_or_else(|| protocol_drift("Chaoxing Chapter Work is outside the current scope"))?;
    validate_component(expected_job_id, "Chapter Work identity")?;
    if html.contains("章节未开放") {
        return Ok(None);
    }
    let Some(root) = parse_card_root(html)? else {
        return Ok(None);
    };
    let defaults = optional_object(root.as_map(), "defaults")?;
    let mut found = None;
    for attachment in card_attachments(root.as_map())? {
        let Some(task) = parse_resource_attachment(attachment, scope, knowledge_id, card_index)?
        else {
            continue;
        };
        if task.remote_id != remote_task_id {
            continue;
        }
        if found.is_some() {
            return Err(protocol_drift(
                "Chaoxing chapter card contains a duplicate Chapter Work target",
            ));
        }
        if task.remote_state != RemoteState::Pending
            || task.normalized.get("resource_kind").and_then(Value::as_str) != Some("chapter_work")
        {
            return Err(ProviderError::new(
                ProviderErrorKind::UnsupportedTask,
                "Chaoxing Chapter Work is not pending",
            ));
        }
        let card = attachment
            .as_object()
            .ok_or_else(|| protocol_drift("Chaoxing chapter attachment is not an object"))?;
        let job_id = optional_string(card, "jobid")?
            .filter(|value| *value == expected_job_id)
            .ok_or_else(|| protocol_drift("Chaoxing Chapter Work job identity changed"))?;
        let work_id = job_id.strip_prefix("work-").unwrap_or(job_id);
        validate_component(work_id, "Chapter Work route identity")?;
        let enc = bounded_secret(card, "enc", "Chapter Work route token")?;
        let knowledge_token = bounded_secret(defaults, "ktoken", "Chapter Work knowledge token")?;
        found = Some(ChaoxingChapterWorkTarget {
            job_id: job_id.to_owned(),
            work_id: work_id.to_owned(),
            enc,
            knowledge_token,
        });
    }
    Ok(found)
}

fn bounded_secret(
    object: &Map<String, Value>,
    key: &str,
    label: &'static str,
) -> ProviderResult<SecretString> {
    optional_string(object, key)?
        .filter(|value| {
            !value.is_empty()
                && value.len() <= MAX_EPHEMERAL_TOKEN_BYTES
                && !value.chars().any(char::is_control)
        })
        .map(SecretString::new)
        .ok_or_else(|| protocol_drift(label))
}

fn required_ephemeral_string<'a>(
    object: &'a Map<String, Value>,
    key: &str,
    label: &'static str,
) -> ProviderResult<&'a str> {
    optional_string(object, key)?
        .filter(|value| {
            !value.is_empty()
                && value.len() <= MAX_EPHEMERAL_TOKEN_BYTES
                && !value.chars().any(char::is_control)
        })
        .ok_or_else(|| protocol_drift(format!("Chaoxing {label} is missing or invalid")))
}

fn optional_ephemeral_secret(
    object: &Map<String, Value>,
    key: &str,
) -> ProviderResult<Option<SecretString>> {
    optional_string(object, key)?
        .filter(|value| !value.is_empty())
        .map(|value| {
            if value.len() > MAX_EPHEMERAL_TOKEN_BYTES || value.chars().any(char::is_control) {
                Err(protocol_drift(
                    "Chaoxing optional resource credential is invalid",
                ))
            } else {
                Ok(SecretString::new(value))
            }
        })
        .transpose()
}

fn optional_u64(object: &Map<String, Value>, key: &str) -> ProviderResult<Option<u64>> {
    match object.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(value)) => value
            .as_u64()
            .map(Some)
            .ok_or_else(|| protocol_drift("Chaoxing Video progress field is invalid")),
        Some(_) => Err(protocol_drift(
            "Chaoxing Video progress field is not an integer",
        )),
    }
}

fn parse_video_rt(
    value: Option<&str>,
    other_info: &str,
) -> ProviderResult<Option<ChaoxingVideoRt>> {
    let value = value.filter(|value| !value.is_empty());
    match value {
        Some("0.9") => Ok(Some(ChaoxingVideoRt::NineTenths)),
        Some("1") => Ok(Some(ChaoxingVideoRt::One)),
        Some(_) => Err(protocol_drift("Chaoxing Video rt value is unsupported")),
        None if other_info.contains("-rt_d") => Ok(Some(ChaoxingVideoRt::NineTenths)),
        None if other_info.contains("-rt_1") => Ok(Some(ChaoxingVideoRt::One)),
        None => Ok(None),
    }
}

fn parse_card_root(html: &str) -> ProviderResult<Option<SensitiveCardRoot>> {
    let Some(object) = extract_marg_object(html)? else {
        return Ok(None);
    };
    let mut cards_data = serde_json::from_str::<Value>(object)
        .map_err(|_| protocol_drift("Chaoxing chapter card contains malformed mArg JSON"))?;
    if let Value::Object(root) = cards_data {
        Ok(Some(SensitiveCardRoot(root)))
    } else {
        zeroize_json_value(&mut cards_data);
        Err(protocol_drift(
            "Chaoxing chapter card mArg is not an object",
        ))
    }
}

fn zeroize_json_value(value: &mut Value) {
    match value {
        Value::String(value) => value.zeroize(),
        Value::Array(values) => values.iter_mut().for_each(zeroize_json_value),
        Value::Object(values) => values.values_mut().for_each(zeroize_json_value),
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

fn card_attachments(root: &Map<String, Value>) -> ProviderResult<&[Value]> {
    match root.get("attachments") {
        None | Some(Value::Null) => Ok(&[]),
        Some(Value::Array(attachments)) if attachments.len() <= MAX_ATTACHMENTS_PER_CARD => {
            Ok(attachments)
        }
        Some(Value::Array(_)) => Err(invalid_response(
            "Chaoxing chapter attachment count exceeds the size limit",
        )),
        Some(_) => Err(protocol_drift(
            "Chaoxing chapter attachments are not an array",
        )),
    }
}

fn parse_resource_attachment(
    attachment: &Value,
    scope: &ChaoxingCourseScope,
    knowledge_id: &str,
    card_index: u8,
) -> ProviderResult<Option<RemoteTask>> {
    let card = attachment
        .as_object()
        .ok_or_else(|| protocol_drift("Chaoxing chapter attachment is not an object"))?;
    let property = optional_object(card, "property")?;
    let passed = optional_bool(card, "isPassed")?.unwrap_or(false);
    let job = optional_bool(card, "job")?.unwrap_or(false);
    let card_type = optional_string(card, "type")?
        .unwrap_or_default()
        .to_ascii_lowercase();
    let read_pending =
        card_type == "read" && optional_bool(property, "read")?.is_some_and(|read| !read);
    if !passed && !job && !read_pending {
        return Ok(None);
    }
    let kind = classify_resource_kind(&card_type, property).ok_or_else(|| {
        protocol_drift("Chaoxing chapter contains an unknown job attachment type")
    })?;
    let task_id = resource_identity(card, property)?;
    validate_component(&task_id, "resource identity")?;
    let remote_id = format!(
        "resource:{}:{}:{knowledge_id}:{task_id}",
        scope.course_id(),
        scope.class_id()
    );
    let title = resource_title(kind, &task_id, property)?;
    let remote_state = if passed {
        RemoteState::Completed
    } else {
        RemoteState::Pending
    };
    let module = if kind == "chapter_work" {
        "chapter"
    } else {
        "resource"
    };
    let normalized = json!({
        "schema": "chaoxing.chapter-resource.v1",
        "module": module,
        "resource_kind": kind,
        "course_id": scope.course_id(),
        "class_id": scope.class_id(),
        "knowledge_id": knowledge_id,
        "card_index": card_index,
        "task_id": task_id,
        "title": title,
        "remote_state": remote_state,
    });
    Ok(Some(RemoteTask {
        remote_id,
        course_remote_id: Some(scope.remote_course_id().to_owned()),
        title,
        source_type: if kind == "chapter_work" {
            SourceType::Chapter
        } else {
            SourceType::Resource
        },
        assessment_class: AssessmentClass::Unknown,
        remote_state,
        opens_at: None,
        due_at: None,
        closes_at: None,
        capabilities: match (kind, remote_state) {
            ("chapter_work", RemoteState::Pending) => vec![
                TaskCapability::QuestionInventory,
                TaskCapability::QuestionParse,
                TaskCapability::SubmissionBuild,
                TaskCapability::SubmissionExecute,
                TaskCapability::SubmissionVerify,
            ],
            ("document" | "read" | "video" | "audio" | "live", RemoteState::Pending) => vec![
                TaskCapability::ProgressRead,
                TaskCapability::ResourceExecution,
            ],
            ("document" | "read" | "video" | "audio", RemoteState::Completed) => {
                vec![TaskCapability::ProgressRead]
            }
            ("live", RemoteState::Completed) => vec![TaskCapability::ProgressRead],
            _ => Vec::new(),
        },
        fingerprint: fingerprint(&normalized)?,
        normalized,
        raw_sanitized: json!({
            "resource_kind": kind,
            "card_index": card_index,
            "passed": passed,
        }),
    }))
}

fn extract_marg_object(html: &str) -> ProviderResult<Option<&str>> {
    let bytes = html.as_bytes();
    let mut search_from = 0;
    let mut found = None;
    while let Some(offset) = html[search_from..].find("mArg") {
        let token_start = search_from + offset;
        let mut cursor = token_start + "mArg".len();
        skip_ascii_whitespace(bytes, &mut cursor);
        if bytes.get(cursor) != Some(&b'=') {
            search_from = cursor;
            continue;
        }
        cursor += 1;
        skip_ascii_whitespace(bytes, &mut cursor);
        if bytes.get(cursor) != Some(&b'{') {
            return Err(protocol_drift(
                "Chaoxing chapter card mArg does not start with an object",
            ));
        }
        let end = matching_json_object_end(bytes, cursor)?;
        if found.replace(&html[cursor..end]).is_some() {
            return Err(protocol_drift(
                "Chaoxing chapter card contains multiple mArg objects",
            ));
        }
        search_from = end;
    }
    Ok(found)
}

fn matching_json_object_end(bytes: &[u8], start: usize) -> ProviderResult<usize> {
    let mut depth = 0_u32;
    let mut in_string = false;
    let mut escaped = false;
    for (offset, byte) in bytes[start..].iter().copied().enumerate() {
        if in_string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
            continue;
        }
        match byte {
            b'"' => in_string = true,
            b'{' => depth = depth.saturating_add(1),
            b'}' => {
                depth = depth.checked_sub(1).ok_or_else(|| {
                    protocol_drift("Chaoxing chapter card mArg has unbalanced braces")
                })?;
                if depth == 0 {
                    return Ok(start + offset + 1);
                }
            }
            _ => {}
        }
    }
    Err(protocol_drift(
        "Chaoxing chapter card mArg object is incomplete",
    ))
}

fn skip_ascii_whitespace(bytes: &[u8], cursor: &mut usize) {
    while bytes.get(*cursor).is_some_and(u8::is_ascii_whitespace) {
        *cursor += 1;
    }
}

fn classify_resource_kind(card_type: &str, property: &Map<String, Value>) -> Option<&'static str> {
    let property_type = optional_unchecked_string(property, "type").to_ascii_lowercase();
    let resource_type = optional_unchecked_string(property, "resourceType").to_ascii_lowercase();
    let property_module = optional_unchecked_string(property, "module").to_ascii_lowercase();
    let live = card_type.contains("live")
        || property_type.contains("live")
        || resource_type.contains("live")
        || property.contains_key("liveId")
        || property.contains_key("streamName")
        || property.contains_key("vdoid");
    if live {
        Some("live")
    } else {
        match card_type {
            "video" if property_module == "insertaudio" => Some("audio"),
            "video" => Some("video"),
            "document" => Some("document"),
            "workid" => Some("chapter_work"),
            "read" => Some("read"),
            _ => None,
        }
    }
}

fn resource_identity(
    card: &Map<String, Value>,
    property: &Map<String, Value>,
) -> ProviderResult<String> {
    for (object, key) in [
        (card, "jobid"),
        (card, "id"),
        (property, "id"),
        (card, "objectId"),
        (card, "mid"),
    ] {
        if let Some(value) = object.get(key) {
            return scalar_string(value)
                .ok_or_else(|| protocol_drift("Chaoxing chapter resource identity is not scalar"));
        }
    }
    Err(protocol_drift(
        "Chaoxing chapter resource has no stable identity",
    ))
}

fn resource_title(
    kind: &str,
    task_id: &str,
    property: &Map<String, Value>,
) -> ProviderResult<String> {
    let supplied = ["name", "title"]
        .into_iter()
        .map(|key| optional_unchecked_string(property, key))
        .find(|value| !value.trim().is_empty());
    let title = supplied.map_or_else(
        || format!("{} {task_id}", resource_label(kind)),
        |value| value.split_whitespace().collect::<Vec<_>>().join(" "),
    );
    if title.is_empty() || title.len() > MAX_TITLE_BYTES || title.chars().any(char::is_control) {
        return Err(protocol_drift(
            "Chaoxing chapter resource has an invalid title",
        ));
    }
    Ok(title)
}

fn resource_label(kind: &str) -> &'static str {
    match kind {
        "video" => "视频任务",
        "document" => "文档任务",
        "chapter_work" => "章节测验",
        "read" => "阅读任务",
        "live" => "直播任务",
        _ => "资源任务",
    }
}

fn optional_object<'a>(
    object: &'a Map<String, Value>,
    key: &str,
) -> ProviderResult<&'a Map<String, Value>> {
    static EMPTY: std::sync::LazyLock<Map<String, Value>> = std::sync::LazyLock::new(Map::new);
    match object.get(key) {
        None | Some(Value::Null) => Ok(&EMPTY),
        Some(Value::Object(value)) => Ok(value),
        Some(_) => Err(protocol_drift(
            "Chaoxing chapter attachment property is not an object",
        )),
    }
}

fn optional_bool(object: &Map<String, Value>, key: &str) -> ProviderResult<Option<bool>> {
    match object.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(value)) => Ok(Some(*value)),
        Some(_) => Err(protocol_drift(
            "Chaoxing chapter attachment boolean field is invalid",
        )),
    }
}

fn optional_string<'a>(
    object: &'a Map<String, Value>,
    key: &str,
) -> ProviderResult<Option<&'a str>> {
    match object.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value)),
        Some(_) => Err(protocol_drift(
            "Chaoxing chapter attachment string field is invalid",
        )),
    }
}

fn optional_unchecked_string<'a>(object: &'a Map<String, Value>, key: &str) -> &'a str {
    object.get(key).and_then(Value::as_str).unwrap_or_default()
}

fn scalar_string(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

fn validate_component(value: &str, label: &'static str) -> ProviderResult<()> {
    if value.is_empty()
        || value.len() > MAX_REMOTE_COMPONENT_BYTES
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(ProviderError::new(
            ProviderErrorKind::ProtocolDrift,
            format!("Chaoxing chapter {label} is invalid"),
        ));
    }
    Ok(())
}

fn fingerprint(normalized: &Value) -> ProviderResult<String> {
    let bytes = serde_json::to_vec(normalized).map_err(|_| {
        ProviderError::new(
            ProviderErrorKind::Internal,
            "failed to serialize sanitized Chaoxing resource inventory",
        )
    })?;
    Ok(format!("v1:{:x}", Sha256::digest(bytes)))
}

fn protocol_drift(message: impl Into<String>) -> ProviderError {
    ProviderError::new(ProviderErrorKind::ProtocolDrift, message)
}

fn invalid_response(message: &'static str) -> ProviderError {
    ProviderError::new(ProviderErrorKind::InvalidResponse, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CARDS_MIXED: &str =
        include_str!("../../../fixtures/providers/chaoxing/resources/cards-mixed.html");
    const CARDS_EXPECTED: &str =
        include_str!("../../../fixtures/providers/chaoxing/resources/cards-mixed.expected.json");

    #[test]
    fn card_parser_keeps_resource_kinds_states_and_chapter_work_separate() {
        let tasks = parse_chapter_resource_inventory(CARDS_MIXED, &scope(), "4001", 0).unwrap();
        let selected = Value::Array(
            tasks
                .iter()
                .map(|task| {
                    json!({
                        "remote_id": task.remote_id,
                        "title": task.title,
                        "source_type": task.source_type,
                        "remote_state": task.remote_state,
                        "resource_kind": task.normalized["resource_kind"],
                    })
                })
                .collect(),
        );
        assert_eq!(
            selected,
            serde_json::from_str::<Value>(CARDS_EXPECTED).unwrap()
        );
        for suffix in ["read", "video", "audio", "live"] {
            assert!(
                tasks
                    .iter()
                    .find(|task| task.remote_id.ends_with(&format!(":job-{suffix}")))
                    .is_some_and(|task| {
                        task.capabilities
                            == [
                                TaskCapability::ProgressRead,
                                TaskCapability::ResourceExecution,
                            ]
                    }),
                "{suffix} capability mismatch"
            );
        }
        assert!(
            tasks
                .iter()
                .find(|task| task.remote_id.ends_with(":job-document"))
                .is_some_and(|task| task.capabilities == [TaskCapability::ProgressRead])
        );
        assert!(
            tasks
                .iter()
                .find(|task| task.remote_id.ends_with(":job-work"))
                .is_some_and(|task| {
                    task.capabilities
                        == [
                            TaskCapability::QuestionInventory,
                            TaskCapability::QuestionParse,
                            TaskCapability::SubmissionBuild,
                            TaskCapability::SubmissionExecute,
                            TaskCapability::SubmissionVerify,
                        ]
                })
        );
        let serialized = serde_json::to_string(&tasks).unwrap();
        for private in [
            "PRIVATE_ENC",
            "PRIVATE_TOKEN",
            "PRIVATE_MID",
            "PRIVATE_OTHER",
            "PRIVATE_LIVE",
            "PRIVATE_READ_TOKEN",
            "SAFE_VIDEO_OBJECT",
            "SAFE_VIDEO_REPORT",
            "SAFE_AUDIO_OBJECT",
            "SAFE_AUDIO_REPORT",
            "SAFE_FACE_ENC",
        ] {
            assert!(!serialized.contains(private));
        }
    }

    #[test]
    fn marg_extractor_handles_braces_inside_json_strings() {
        let tasks = parse_chapter_resource_inventory(
            &CARDS_MIXED.replace("视频导论", "视频 {导论}"),
            &scope(),
            "4001",
            0,
        )
        .unwrap();
        assert_eq!(tasks[0].title, "视频 {导论}");
    }

    #[test]
    fn card_parser_treats_an_empty_card_slot_as_no_tasks() {
        let tasks = parse_chapter_resource_inventory(
            "<html><body>当前卡槽没有资源</body></html>",
            &scope(),
            "4001",
            6,
        )
        .unwrap();
        assert!(tasks.is_empty());
    }

    #[test]
    fn card_parser_rejects_unknown_jobs_duplicates_and_malformed_marg() {
        assert!(
            parse_chapter_resource_inventory(
                &CARDS_MIXED.replace("\"type\":\"video\"", "\"type\":\"unknown\""),
                &scope(),
                "4001",
                0,
            )
            .is_err()
        );
        assert!(
            parse_chapter_resource_inventory(
                &CARDS_MIXED.replace("job-document", "job-video"),
                &scope(),
                "4001",
                0,
            )
            .is_err()
        );
        assert!(
            parse_chapter_resource_inventory(
                "<script>mArg={broken};</script>",
                &scope(),
                "4001",
                0
            )
            .is_err()
        );
    }

    #[test]
    fn immediate_locator_keeps_tokens_ephemeral_and_handles_completed_targets() {
        let pending_document = CARDS_MIXED.replace(
            "\"jobid\":\"job-document\",\"isPassed\":true",
            "\"jobid\":\"job-document\",\"isPassed\":false",
        );
        let document = locate_immediate_resource_target(
            &pending_document,
            &scope(),
            "4001",
            0,
            "resource:100:200:4001:job-document",
        )
        .unwrap()
        .unwrap();
        assert_eq!(document.kind(), ChaoxingImmediateResourceKind::Document);
        assert_eq!(document.remote_state(), RemoteState::Pending);
        assert_eq!(document.job_id(), "job-document");
        assert_eq!(document.token().unwrap().expose_secret(), "PRIVATE_TOKEN");
        assert!(!format!("{document:?}").contains("PRIVATE_TOKEN"));

        let read = locate_immediate_resource_target(
            CARDS_MIXED,
            &scope(),
            "4001",
            0,
            "resource:100:200:4001:job-read",
        )
        .unwrap()
        .unwrap();
        assert_eq!(read.kind(), ChaoxingImmediateResourceKind::Read);
        assert_eq!(read.token().unwrap().expose_secret(), "PRIVATE_READ_TOKEN");

        let completed = locate_immediate_resource_target(
            CARDS_MIXED,
            &scope(),
            "4001",
            0,
            "resource:100:200:4001:job-document",
        )
        .unwrap()
        .unwrap();
        assert_eq!(completed.remote_state(), RemoteState::Completed);
        assert!(completed.token().is_none());
    }

    #[test]
    fn immediate_locator_rejects_foreign_unsupported_or_malformed_targets() {
        let unsupported = locate_immediate_resource_target(
            CARDS_MIXED,
            &scope(),
            "4001",
            0,
            "resource:100:200:4001:job-video",
        )
        .unwrap_err();
        assert_eq!(unsupported.kind, ProviderErrorKind::UnsupportedTask);
        assert!(
            locate_immediate_resource_target(
                CARDS_MIXED,
                &scope(),
                "4001",
                0,
                "resource:999:200:4001:job-read",
            )
            .is_err()
        );
        assert!(
            locate_immediate_resource_target(
                &CARDS_MIXED.replace("\"jtoken\":\"PRIVATE_READ_TOKEN\"", "\"jtoken\":42"),
                &scope(),
                "4001",
                0,
                "resource:100:200:4001:job-read",
            )
            .is_err()
        );
        assert!(
            locate_immediate_resource_target(
                CARDS_MIXED,
                &scope(),
                "4001",
                0,
                "resource:100:200:4001:job-missing",
            )
            .unwrap()
            .is_none()
        );
    }

    #[test]
    fn media_locator_keeps_kind_and_report_credentials_ephemeral_and_bounded() {
        let video = locate_media_resource_target(
            CARDS_MIXED,
            &scope(),
            "4001",
            0,
            "resource:100:200:4001:job-video",
        )
        .unwrap()
        .unwrap();
        assert_eq!(video.kind(), ChaoxingMediaKind::Video);
        assert_eq!(video.remote_state(), RemoteState::Pending);
        assert_eq!(video.job_id(), "job-video");
        assert_eq!(video.object_id().expose_secret(), "SAFE_VIDEO_OBJECT");
        assert_eq!(video.other_info().expose_secret(), "SAFE_VIDEO_REPORT-rt_d");
        assert_eq!(video.initial_play_time_millis(), 15_000);
        assert_eq!(video.rt(), Some(ChaoxingVideoRt::NineTenths));
        assert_eq!(
            video.face_capture_enc().unwrap().expose_secret(),
            "SAFE_FACE_ENC"
        );
        let debug = format!("{video:?}");
        assert!(!debug.contains("SAFE_VIDEO_OBJECT"));
        assert!(!debug.contains("SAFE_VIDEO_REPORT"));

        let audio = locate_media_resource_target(
            CARDS_MIXED,
            &scope(),
            "4001",
            0,
            "resource:100:200:4001:job-audio",
        )
        .unwrap()
        .unwrap();
        assert_eq!(audio.kind(), ChaoxingMediaKind::Audio);
        assert_eq!(audio.job_id(), "job-audio");
        assert_eq!(audio.object_id().expose_secret(), "SAFE_AUDIO_OBJECT");
        assert_eq!(audio.other_info().expose_secret(), "SAFE_AUDIO_REPORT-rt_1");
        assert_eq!(audio.initial_play_time_millis(), 5_000);
        assert_eq!(audio.rt(), Some(ChaoxingVideoRt::One));

        assert!(
            locate_media_resource_target(
                &CARDS_MIXED.replace("\"objectId\":\"SAFE_VIDEO_OBJECT\"", "\"objectId\":null"),
                &scope(),
                "4001",
                0,
                "resource:100:200:4001:job-video",
            )
            .is_err()
        );
        assert!(
            locate_media_resource_target(
                CARDS_MIXED,
                &scope(),
                "4001",
                0,
                "resource:100:200:4001:job-read",
            )
            .is_err()
        );
    }

    #[test]
    fn live_locator_keeps_every_route_identity_ephemeral_and_bounded() {
        let live = locate_live_resource_target(
            CARDS_MIXED,
            &scope(),
            "4001",
            0,
            "resource:100:200:4001:job-live",
        )
        .unwrap()
        .unwrap();
        assert_eq!(live.remote_state(), RemoteState::Pending);
        assert_eq!(live.job_id(), "job-live");
        assert_eq!(live.live_id().expose_secret(), "PRIVATE_LIVE");
        assert_eq!(live.stream_name().expose_secret(), "PRIVATE_STREAM");
        assert_eq!(live.video_id().expose_secret(), "PRIVATE_VDOID");
        assert_eq!(
            live.status_job_id().unwrap().expose_secret(),
            "PRIVATE_LIVE_JOB"
        );
        let debug = format!("{live:?}");
        for private in [
            "job-live",
            "PRIVATE_LIVE",
            "PRIVATE_STREAM",
            "PRIVATE_VDOID",
            "PRIVATE_LIVE_JOB",
        ] {
            assert!(!debug.contains(private));
        }

        assert!(
            locate_live_resource_target(
                &CARDS_MIXED.replace("\"streamName\":\"PRIVATE_STREAM\"", "\"streamName\":null"),
                &scope(),
                "4001",
                0,
                "resource:100:200:4001:job-live",
            )
            .is_err()
        );
        assert!(
            locate_live_resource_target(
                CARDS_MIXED,
                &scope(),
                "4001",
                0,
                "resource:100:200:4001:job-video",
            )
            .is_err()
        );
    }

    #[test]
    fn chapter_work_locator_binds_fresh_attempt_material_and_redacts_it() {
        let target = locate_chapter_work_target(
            CARDS_MIXED,
            &scope(),
            "4001",
            0,
            "resource:100:200:4001:job-work",
        )
        .unwrap()
        .unwrap();
        assert_eq!(target.job_id(), "job-work");
        assert_eq!(target.work_id(), "job-work");
        assert_eq!(target.enc().expose_secret(), "PRIVATE_ENC");
        assert_eq!(target.knowledge_token().expose_secret(), "PRIVATE_TOKEN");
        let debug = format!("{target:?}");
        for private in ["job-work", "PRIVATE_ENC", "PRIVATE_TOKEN"] {
            assert!(!debug.contains(private));
        }
    }

    #[test]
    fn chapter_work_locator_fails_closed_on_foreign_duplicate_or_missing_material() {
        assert!(
            locate_chapter_work_target(
                CARDS_MIXED,
                &scope(),
                "4001",
                0,
                "resource:999:200:4001:job-work",
            )
            .is_err()
        );
        let work = r#"{"type":"workid","job":true,"jobid":"job-work","isPassed":false,"enc":"PRIVATE_ENC","property":{"title":"章节练习"}}"#;
        let duplicate = CARDS_MIXED.replace(work, &format!("{work},{work}"));
        assert!(
            locate_chapter_work_target(
                &duplicate,
                &scope(),
                "4001",
                0,
                "resource:100:200:4001:job-work",
            )
            .is_err()
        );
        assert!(
            locate_chapter_work_target(
                &CARDS_MIXED.replace("\"enc\":\"PRIVATE_ENC\"", "\"enc\":null"),
                &scope(),
                "4001",
                0,
                "resource:100:200:4001:job-work",
            )
            .is_err()
        );
        assert!(
            locate_chapter_work_target(
                CARDS_MIXED,
                &scope(),
                "4001",
                0,
                "resource:100:200:4001:missing",
            )
            .unwrap()
            .is_none()
        );
    }

    fn scope() -> ChaoxingCourseScope {
        ChaoxingCourseScope::new("course:100:200", "100", "200").unwrap()
    }
}
