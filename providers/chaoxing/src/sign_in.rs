use std::{collections::HashSet, fmt, sync::Arc};

use asterism_provider_api::{
    ProviderContext, ProviderError, ProviderErrorKind, ProviderIdentity, ProviderMetadata,
    ProviderResult, RemoteCourse,
};
use async_trait::async_trait;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use zeroize::Zeroize;

use crate::{ChaoxingCourseRoute, ChaoxingCourseScope, metadata::development_metadata};

const SIGN_ACTIVITY_TYPE: i64 = 2;
const MAX_SIGN_DOCUMENT_BYTES: usize = 1024 * 1024;
const MAX_SIGN_ACTIVITIES: usize = 2_048;
const MAX_ACTIVITY_ID_BYTES: usize = 128;
const MAX_ACTIVITY_LABEL_BYTES: usize = 512;
const MAX_PROVIDER_CODE: i64 = i32::MAX as i64;

/// A donor-observed Chaoxing sign-in variant.
///
/// The two current sign-in donors disagree about `otherId=5`, so that code is
/// intentionally retained as ambiguous rather than being made executable.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChaoxingSignVariant {
    Normal,
    QrCode,
    Gesture,
    Location,
    Photo,
    AmbiguousCodeOrPhoto,
    Unknown(i64),
}

/// One course-bound sign-in activity discovered from `student/activelist`.
///
/// `provider_status` and `user_status` stay as raw bounded codes. Audited
/// donors assign incompatible meanings to status code `1`, so neither field is
/// promoted to a completion or eligibility decision.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChaoxingSignActivity {
    remote_id: String,
    remote_course_id: String,
    course_id: String,
    class_id: String,
    activity_id: String,
    display_label: String,
    variant: ChaoxingSignVariant,
    other_id: i64,
    provider_status: i64,
    user_status: Option<i64>,
    start_time_millis: Option<i64>,
    end_time_millis: Option<i64>,
    if_photo: Option<bool>,
    fingerprint: String,
}

impl ChaoxingSignActivity {
    pub fn remote_id(&self) -> &str {
        &self.remote_id
    }

    pub fn remote_course_id(&self) -> &str {
        &self.remote_course_id
    }

    pub fn activity_id(&self) -> &str {
        &self.activity_id
    }

    pub fn display_label(&self) -> &str {
        &self.display_label
    }

    pub const fn variant(&self) -> ChaoxingSignVariant {
        self.variant
    }

    pub const fn provider_status(&self) -> i64 {
        self.provider_status
    }

    pub const fn user_status(&self) -> Option<i64> {
        self.user_status
    }

    pub const fn start_time_millis(&self) -> Option<i64> {
        self.start_time_millis
    }

    pub const fn end_time_millis(&self) -> Option<i64> {
        self.end_time_millis
    }

    pub const fn photo_required(&self) -> Option<bool> {
        self.if_photo
    }

    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    fn belongs_to(&self, route: ChaoxingCourseRoute<'_>) -> bool {
        self.remote_course_id == route.remote_course_id()
            && self.course_id == route.course_id()
            && self.class_id == route.class_id()
    }
}

/// One immutable request for the donor-observed read-only `signDetail` route.
#[derive(Clone, Eq, PartialEq)]
pub struct ChaoxingSignDetailRequest {
    remote_id: String,
    remote_course_id: String,
    course_id: String,
    class_id: String,
    activity_id: String,
    expected_other_id: i64,
    expected_start_time_millis: Option<i64>,
    expected_end_time_millis: Option<i64>,
    discovery_fingerprint: String,
}

impl ChaoxingSignDetailRequest {
    pub(crate) fn try_new(
        route: ChaoxingCourseRoute<'_>,
        activity: &ChaoxingSignActivity,
    ) -> ProviderResult<Self> {
        if !activity.belongs_to(route) {
            return Err(remote_changed(
                "Chaoxing sign-in activity no longer belongs to the selected course",
            ));
        }
        Ok(Self {
            remote_id: activity.remote_id.clone(),
            remote_course_id: activity.remote_course_id.clone(),
            course_id: activity.course_id.clone(),
            class_id: activity.class_id.clone(),
            activity_id: activity.activity_id.clone(),
            expected_other_id: activity.other_id,
            expected_start_time_millis: activity.start_time_millis,
            expected_end_time_millis: activity.end_time_millis,
            discovery_fingerprint: activity.fingerprint.clone(),
        })
    }

    pub fn remote_id(&self) -> &str {
        &self.remote_id
    }

    pub fn remote_course_id(&self) -> &str {
        &self.remote_course_id
    }

    pub fn activity_id(&self) -> &str {
        &self.activity_id
    }

    pub fn discovery_fingerprint(&self) -> &str {
        &self.discovery_fingerprint
    }
}

impl fmt::Debug for ChaoxingSignDetailRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ChaoxingSignDetailRequest")
            .field("remote_id", &self.remote_id)
            .field("remote_course_id", &"[REDACTED]")
            .field("course_id", &"[REDACTED]")
            .field("class_id", &"[REDACTED]")
            .field("activity_id", &"[REDACTED]")
            .field("expected_other_id", &self.expected_other_id)
            .field(
                "expected_start_time_millis",
                &self.expected_start_time_millis,
            )
            .field("expected_end_time_millis", &self.expected_end_time_millis)
            .field("discovery_fingerprint", &self.discovery_fingerprint)
            .finish()
    }
}

/// One freshly rebound read-only sign-in detail snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChaoxingSignDetail {
    remote_id: String,
    variant: ChaoxingSignVariant,
    provider_status: i64,
    start_time_millis: Option<i64>,
    end_time_millis: Option<i64>,
    refreshes_qr_code: Option<bool>,
    fingerprint: String,
}

impl ChaoxingSignDetail {
    pub fn remote_id(&self) -> &str {
        &self.remote_id
    }

    pub const fn variant(&self) -> ChaoxingSignVariant {
        self.variant
    }

    pub const fn provider_status(&self) -> i64 {
        self.provider_status
    }

    pub const fn start_time_millis(&self) -> Option<i64> {
        self.start_time_millis
    }

    pub const fn end_time_millis(&self) -> Option<i64> {
        self.end_time_millis
    }

    pub const fn refreshes_qr_code(&self) -> Option<bool> {
        self.refreshes_qr_code
    }

    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }
}

/// A bounded activity-list response, redacted in diagnostics and zeroized on
/// drop because labels and account-scoped response fields may identify users.
pub struct ChaoxingSignActivityListDocument(String);

impl ChaoxingSignActivityListDocument {
    /// Wraps one complete `student/activelist` response.
    ///
    /// # Errors
    ///
    /// Returns an invalid-response error when the document is empty or exceeds
    /// the Provider-owned response limit.
    pub fn try_new(document: impl Into<String>) -> ProviderResult<Self> {
        bounded_document(document, "activity-list").map(Self)
    }

    fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ChaoxingSignActivityListDocument {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ChaoxingSignActivityListDocument([REDACTED])")
    }
}

impl Drop for ChaoxingSignActivityListDocument {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

/// A bounded `signDetail` response with the same redaction and zeroization
/// boundary as activity discovery.
pub struct ChaoxingSignDetailDocument(String);

impl ChaoxingSignDetailDocument {
    /// Wraps one complete `newsign/signDetail` response.
    ///
    /// # Errors
    ///
    /// Returns an invalid-response error when the document is empty or exceeds
    /// the Provider-owned response limit.
    pub fn try_new(document: impl Into<String>) -> ProviderResult<Self> {
        bounded_document(document, "detail").map(Self)
    }

    fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ChaoxingSignDetailDocument {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ChaoxingSignDetailDocument([REDACTED])")
    }
}

impl Drop for ChaoxingSignDetailDocument {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

/// Provider-local, read-only transport boundary for sign-in discovery.
///
/// This is deliberately not a Core capability and exposes no mutation method.
#[async_trait]
pub trait ChaoxingSignActivityReadTransport: Send + Sync {
    async fn fetch_sign_activity_list(
        &self,
        context: &ProviderContext,
        route: ChaoxingCourseRoute<'_>,
    ) -> ProviderResult<ChaoxingSignActivityListDocument>;

    async fn fetch_sign_detail(
        &self,
        context: &ProviderContext,
        request: &ChaoxingSignDetailRequest,
    ) -> ProviderResult<ChaoxingSignDetailDocument>;
}

/// Unregistered Provider-local facade for safe sign-in discovery and detail
/// rebinding. It cannot issue `preSign` or `stuSignajax`.
pub struct ChaoxingSignActivityRead {
    metadata: ProviderMetadata,
    transport: Arc<dyn ChaoxingSignActivityReadTransport>,
}

impl ChaoxingSignActivityRead {
    /// Creates the read-only boundary around one transport.
    ///
    /// # Errors
    ///
    /// Returns an internal error when compile-time Provider metadata is invalid.
    pub fn try_new(transport: Arc<dyn ChaoxingSignActivityReadTransport>) -> ProviderResult<Self> {
        Ok(Self {
            metadata: development_metadata()?,
            transport,
        })
    }

    /// Discovers sign-in activities for exactly one freshly supplied course.
    ///
    /// # Errors
    ///
    /// Fails for a foreign context, missing authentication, invalid course
    /// route, transport failure, or any ambiguous sign-in row structure.
    pub async fn list_course(
        &self,
        context: &ProviderContext,
        course: &RemoteCourse,
    ) -> ProviderResult<Vec<ChaoxingSignActivity>> {
        self.validate_context(context)?;
        let route = ChaoxingCourseRoute::from_remote_course(course)?;
        let document = self
            .transport
            .fetch_sign_activity_list(context, route)
            .await?;
        parse_sign_activity_list(&document, &route.parser_scope()?)
    }

    /// Reads one exact activity detail after rebinding it to a fresh course.
    ///
    /// The returned detail is structural evidence only. It does not prove that
    /// the current account signed in and cannot authorize a mutation replay.
    ///
    /// # Errors
    ///
    /// Fails when the activity/course binding changed or the detail conflicts
    /// with the list snapshot.
    pub async fn read_detail(
        &self,
        context: &ProviderContext,
        course: &RemoteCourse,
        activity: &ChaoxingSignActivity,
    ) -> ProviderResult<ChaoxingSignDetail> {
        self.validate_context(context)?;
        let route = ChaoxingCourseRoute::from_remote_course(course)?;
        let request = ChaoxingSignDetailRequest::try_new(route, activity)?;
        let document = self.transport.fetch_sign_detail(context, &request).await?;
        parse_sign_detail(&document, &request)
    }

    fn validate_context(&self, context: &ProviderContext) -> ProviderResult<()> {
        if context.provider_id != self.metadata.id {
            return Err(ProviderError::new(
                ProviderErrorKind::Internal,
                "Chaoxing sign-in read received a mismatched Provider context",
            ));
        }
        if context.credential_refs.is_empty() {
            return Err(ProviderError::new(
                ProviderErrorKind::Authentication,
                "Chaoxing sign-in read requires an authenticated session",
            ));
        }
        Ok(())
    }
}

impl fmt::Debug for ChaoxingSignActivityRead {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ChaoxingSignActivityRead")
            .field("metadata", &self.metadata)
            .field("transport", &"configured")
            .finish()
    }
}

impl ProviderIdentity for ChaoxingSignActivityRead {
    fn metadata(&self) -> &ProviderMetadata {
        &self.metadata
    }
}

/// Parses a bounded activity-list response without assigning completion
/// semantics to donor-conflicted status fields.
///
/// # Errors
///
/// Returns a typed error for malformed envelopes, unbounded fields, duplicate
/// sign-in identities, or rows which disagree with the requested course scope.
pub fn parse_sign_activity_list(
    document: &ChaoxingSignActivityListDocument,
    scope: &ChaoxingCourseScope,
) -> ProviderResult<Vec<ChaoxingSignActivity>> {
    let root: Value = serde_json::from_str(document.as_str())
        .map_err(|_| invalid_response("Chaoxing sign-in activity list is not valid JSON"))?;
    let root = root
        .as_object()
        .ok_or_else(|| protocol_drift("Chaoxing sign-in activity list root is not an object"))?;
    if required_code(root, "result")? != 1 {
        return Err(invalid_response(
            "Chaoxing sign-in activity list reported a failed result",
        ));
    }
    let rows = root
        .get("data")
        .and_then(Value::as_object)
        .and_then(|data| data.get("activeList"))
        .and_then(Value::as_array)
        .ok_or_else(|| protocol_drift("Chaoxing sign-in activity list has no activeList array"))?;
    if rows.len() > MAX_SIGN_ACTIVITIES {
        return Err(invalid_response(
            "Chaoxing sign-in activity count exceeds the size limit",
        ));
    }

    let mut activities = Vec::new();
    let mut seen = HashSet::new();
    for row in rows {
        let row = row
            .as_object()
            .ok_or_else(|| protocol_drift("Chaoxing sign-in activity row is not an object"))?;
        let activity_type = required_code(row, "type")?;
        if let Some(active_type) = optional_code(row, "activeType")?
            && active_type != activity_type
        {
            return Err(protocol_drift(
                "Chaoxing sign-in activity type fields disagree",
            ));
        }
        if activity_type != SIGN_ACTIVITY_TYPE {
            continue;
        }

        let activity_id = required_id(row, "id", "activity ID")?;
        if !seen.insert(activity_id.clone()) {
            return Err(protocol_drift(
                "Chaoxing sign-in activity list contains a duplicate identity",
            ));
        }
        validate_optional_scope_id(row, &["courseId"], scope.course_id(), "course")?;
        validate_optional_scope_id(row, &["classId", "clazzId"], scope.class_id(), "class")?;
        let display_label = required_display_label(row)?;
        let other_id = required_code(row, "otherId")?;
        let provider_status = required_code(row, "status")?;
        let user_status = optional_code(row, "userStatus")?;
        let start_time_millis = optional_timestamp(row, "startTime")?;
        let end_time_millis = optional_timestamp(row, "endTime")?;
        validate_time_range(start_time_millis, end_time_millis)?;
        let if_photo = optional_flag(row, "ifPhoto")?;
        let variant = classify_variant(other_id, if_photo);
        let remote_id = format!(
            "sign:{}:{}:{}",
            scope.course_id(),
            scope.class_id(),
            activity_id
        );
        let fingerprint = activity_fingerprint(
            scope,
            &activity_id,
            &display_label,
            other_id,
            provider_status,
            user_status,
            start_time_millis,
            end_time_millis,
            if_photo,
        )?;
        activities.push(ChaoxingSignActivity {
            remote_id,
            remote_course_id: scope.remote_course_id().to_owned(),
            course_id: scope.course_id().to_owned(),
            class_id: scope.class_id().to_owned(),
            activity_id,
            display_label,
            variant,
            other_id,
            provider_status,
            user_status,
            start_time_millis,
            end_time_millis,
            if_photo,
            fingerprint,
        });
    }
    Ok(activities)
}

/// Parses one read-only `signDetail` response and checks it against the exact
/// list discovery from which the request was prepared.
///
/// # Errors
///
/// Returns `RemoteChanged` for identity, variant or time drift and
/// `ProtocolDrift` for malformed or unbounded structure.
pub fn parse_sign_detail(
    document: &ChaoxingSignDetailDocument,
    request: &ChaoxingSignDetailRequest,
) -> ProviderResult<ChaoxingSignDetail> {
    let root: Value = serde_json::from_str(document.as_str())
        .map_err(|_| invalid_response("Chaoxing sign-in detail is not valid JSON"))?;
    let root = root
        .as_object()
        .ok_or_else(|| protocol_drift("Chaoxing sign-in detail root is not an object"))?;
    let activity_id = required_id(root, "id", "activity ID")?;
    if activity_id != request.activity_id {
        return Err(remote_changed(
            "Chaoxing sign-in detail activity identity changed",
        ));
    }
    validate_optional_bound_id(
        root,
        &["courseId"],
        &request.course_id,
        "Chaoxing sign-in detail course identity changed",
    )?;
    validate_optional_bound_id(
        root,
        &["classId", "clazzId"],
        &request.class_id,
        "Chaoxing sign-in detail class identity changed",
    )?;

    let active_type = match optional_code(root, "activeType")? {
        Some(active_type) => active_type,
        None => required_code(root, "type")?,
    };
    if let Some(activity_type) = optional_code(root, "type")?
        && activity_type != active_type
    {
        return Err(protocol_drift(
            "Chaoxing sign-in detail activity type fields disagree",
        ));
    }
    if active_type != SIGN_ACTIVITY_TYPE {
        return Err(remote_changed(
            "Chaoxing sign-in detail is no longer a sign-in activity",
        ));
    }

    let other_id = required_code(root, "otherId")?;
    if other_id != request.expected_other_id {
        return Err(remote_changed(
            "Chaoxing sign-in activity variant changed after discovery",
        ));
    }
    let provider_status = required_code(root, "status")?;
    let start_time_millis = optional_timestamp(root, "startTime")?;
    let end_time_millis = optional_timestamp(root, "endTime")?;
    validate_time_range(start_time_millis, end_time_millis)?;
    ensure_unchanged_optional_time(
        request.expected_start_time_millis,
        start_time_millis,
        "Chaoxing sign-in activity start time changed after discovery",
    )?;
    ensure_unchanged_optional_time(
        request.expected_end_time_millis,
        end_time_millis,
        "Chaoxing sign-in activity end time changed after discovery",
    )?;
    let if_photo = optional_flag(root, "ifPhoto")?;
    let refreshes_qr_code = optional_flag(root, "ifRefreshEwm")?;
    let variant = classify_variant(other_id, if_photo);
    let fingerprint = digest_json(&json!({
        "schema": "chaoxing.sign-detail.v1",
        "remote_course_id": request.remote_course_id,
        "course_id": request.course_id,
        "class_id": request.class_id,
        "activity_id": activity_id,
        "other_id": other_id,
        "provider_status": provider_status,
        "start_time_millis": start_time_millis,
        "end_time_millis": end_time_millis,
        "if_photo": if_photo,
        "refreshes_qr_code": refreshes_qr_code,
        "discovery_fingerprint": request.discovery_fingerprint,
    }))?;
    Ok(ChaoxingSignDetail {
        remote_id: request.remote_id.clone(),
        variant,
        provider_status,
        start_time_millis,
        end_time_millis,
        refreshes_qr_code,
        fingerprint,
    })
}

fn bounded_document(document: impl Into<String>, kind: &'static str) -> ProviderResult<String> {
    let mut document = document.into();
    if document.is_empty() || document.len() > MAX_SIGN_DOCUMENT_BYTES {
        document.zeroize();
        return Err(ProviderError::new(
            ProviderErrorKind::InvalidResponse,
            format!("Chaoxing sign-in {kind} document is empty or exceeds the size limit"),
        ));
    }
    Ok(document)
}

fn required_code(object: &Map<String, Value>, field: &'static str) -> ProviderResult<i64> {
    object
        .get(field)
        .ok_or_else(|| protocol_drift("Chaoxing sign-in response lacks a required numeric field"))
        .and_then(|value| parse_code(value, field))
}

fn optional_code(object: &Map<String, Value>, field: &'static str) -> ProviderResult<Option<i64>> {
    object
        .get(field)
        .map(|value| parse_code(value, field))
        .transpose()
}

fn parse_code(value: &Value, _field: &'static str) -> ProviderResult<i64> {
    value
        .as_i64()
        .filter(|code| (0..=MAX_PROVIDER_CODE).contains(code))
        .ok_or_else(|| protocol_drift("Chaoxing sign-in response contains an invalid numeric code"))
}

fn required_id(
    object: &Map<String, Value>,
    field: &'static str,
    label: &'static str,
) -> ProviderResult<String> {
    let value = object.get(field).ok_or_else(|| {
        protocol_drift("Chaoxing sign-in response lacks a required identity field")
    })?;
    parse_id(value, label)
}

fn parse_id(value: &Value, _label: &'static str) -> ProviderResult<String> {
    let id = match value {
        Value::String(id) => id.clone(),
        Value::Number(number) => number.as_u64().map(|id| id.to_string()).ok_or_else(|| {
            protocol_drift("Chaoxing sign-in response contains an invalid identity")
        })?,
        _ => {
            return Err(protocol_drift(
                "Chaoxing sign-in response contains an invalid identity",
            ));
        }
    };
    if id.is_empty()
        || id.len() > MAX_ACTIVITY_ID_BYTES
        || !id.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(protocol_drift(
            "Chaoxing sign-in response contains an invalid identity",
        ));
    }
    Ok(id)
}

fn validate_optional_scope_id(
    object: &Map<String, Value>,
    fields: &[&'static str],
    expected: &str,
    label: &'static str,
) -> ProviderResult<()> {
    validate_optional_bound_id(
        object,
        fields,
        expected,
        match label {
            "course" => "Chaoxing sign-in activity belongs to another course",
            _ => "Chaoxing sign-in activity belongs to another class",
        },
    )
}

fn validate_optional_bound_id(
    object: &Map<String, Value>,
    fields: &[&'static str],
    expected: &str,
    mismatch_message: &'static str,
) -> ProviderResult<()> {
    let mut found: Option<String> = None;
    for field in fields {
        let Some(value) = object.get(*field) else {
            continue;
        };
        let value = parse_id(value, field)?;
        if found.as_ref().is_some_and(|previous| previous != &value) {
            return Err(protocol_drift(
                "Chaoxing sign-in response identity aliases disagree",
            ));
        }
        found = Some(value);
    }
    if found.as_deref().is_some_and(|value| value != expected) {
        return Err(remote_changed(mismatch_message));
    }
    Ok(())
}

fn required_display_label(object: &Map<String, Value>) -> ProviderResult<String> {
    for field in ["name", "nameOne"] {
        if let Some(value) = object.get(field) {
            let label = value
                .as_str()
                .ok_or_else(|| protocol_drift("Chaoxing sign-in activity label is not text"))?;
            let label = label.trim();
            if !label.is_empty()
                && label.len() <= MAX_ACTIVITY_LABEL_BYTES
                && !label.chars().any(char::is_control)
            {
                return Ok(label.to_owned());
            }
        }
    }
    Err(protocol_drift(
        "Chaoxing sign-in activity has no bounded display label",
    ))
}

fn optional_timestamp(
    object: &Map<String, Value>,
    field: &'static str,
) -> ProviderResult<Option<i64>> {
    object
        .get(field)
        .map(|value| parse_timestamp(value, field))
        .transpose()
}

fn parse_timestamp(value: &Value, _field: &'static str) -> ProviderResult<i64> {
    let value = value
        .as_object()
        .and_then(|object| object.get("time"))
        .unwrap_or(value);
    let timestamp = match value {
        Value::Number(number) => number.as_i64(),
        Value::String(value)
            if !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()) =>
        {
            value.parse::<i64>().ok()
        }
        _ => None,
    };
    timestamp
        .filter(|value| *value >= 0)
        .ok_or_else(|| protocol_drift("Chaoxing sign-in response contains an invalid timestamp"))
}

fn optional_flag(object: &Map<String, Value>, field: &'static str) -> ProviderResult<Option<bool>> {
    object
        .get(field)
        .map(|value| match value.as_i64() {
            Some(0) => Ok(false),
            Some(1) => Ok(true),
            _ => Err(protocol_drift(
                "Chaoxing sign-in response contains an invalid binary flag",
            )),
        })
        .transpose()
}

fn validate_time_range(start: Option<i64>, end: Option<i64>) -> ProviderResult<()> {
    if start.is_some_and(|start| end.is_some_and(|end| end > 0 && start > end)) {
        return Err(protocol_drift(
            "Chaoxing sign-in activity time range is reversed",
        ));
    }
    Ok(())
}

fn ensure_unchanged_optional_time(
    discovered: Option<i64>,
    detail: Option<i64>,
    message: &'static str,
) -> ProviderResult<()> {
    if discovered.is_some() && detail.is_some() && discovered != detail {
        return Err(remote_changed(message));
    }
    Ok(())
}

const fn classify_variant(other_id: i64, if_photo: Option<bool>) -> ChaoxingSignVariant {
    match (other_id, if_photo) {
        (0, Some(true)) => ChaoxingSignVariant::Photo,
        (0, _) => ChaoxingSignVariant::Normal,
        (2, _) => ChaoxingSignVariant::QrCode,
        (3, _) => ChaoxingSignVariant::Gesture,
        (4, _) => ChaoxingSignVariant::Location,
        (5, _) => ChaoxingSignVariant::AmbiguousCodeOrPhoto,
        (other, _) => ChaoxingSignVariant::Unknown(other),
    }
}

#[allow(clippy::too_many_arguments)]
fn activity_fingerprint(
    scope: &ChaoxingCourseScope,
    activity_id: &str,
    display_label: &str,
    other_id: i64,
    provider_status: i64,
    user_status: Option<i64>,
    start_time_millis: Option<i64>,
    end_time_millis: Option<i64>,
    if_photo: Option<bool>,
) -> ProviderResult<String> {
    digest_json(&json!({
        "schema": "chaoxing.sign-activity.v1",
        "remote_course_id": scope.remote_course_id(),
        "course_id": scope.course_id(),
        "class_id": scope.class_id(),
        "activity_id": activity_id,
        "display_label": display_label,
        "activity_type": SIGN_ACTIVITY_TYPE,
        "other_id": other_id,
        "provider_status": provider_status,
        "user_status": user_status,
        "start_time_millis": start_time_millis,
        "end_time_millis": end_time_millis,
        "if_photo": if_photo,
    }))
}

fn digest_json(value: &Value) -> ProviderResult<String> {
    let encoded = serde_json::to_vec(value).map_err(|_| {
        ProviderError::new(
            ProviderErrorKind::Internal,
            "Chaoxing sign-in fingerprint serialization failed",
        )
    })?;
    Ok(format!("{:x}", Sha256::digest(encoded)))
}

fn invalid_response(message: &'static str) -> ProviderError {
    ProviderError::new(ProviderErrorKind::InvalidResponse, message)
}

fn protocol_drift(message: &'static str) -> ProviderError {
    ProviderError::new(ProviderErrorKind::ProtocolDrift, message)
}

fn remote_changed(message: &'static str) -> ProviderError {
    ProviderError::new(ProviderErrorKind::RemoteChanged, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use asterism_domain::{ProviderAccountId, ProviderId, SecretId};
    use asterism_provider_api::ProviderRouteContext;

    const LIST_FIXTURE: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fixtures/providers/chaoxing/sign/activities-mixed.json"
    ));
    const DETAIL_FIXTURE: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fixtures/providers/chaoxing/sign/detail-normal.json"
    ));

    fn scope() -> ChaoxingCourseScope {
        ChaoxingCourseScope::new("course:1001:2001", "1001", "2001").unwrap()
    }

    fn detail_request(activity: &ChaoxingSignActivity) -> ChaoxingSignDetailRequest {
        ChaoxingSignDetailRequest {
            remote_id: activity.remote_id.clone(),
            remote_course_id: activity.remote_course_id.clone(),
            course_id: activity.course_id.clone(),
            class_id: activity.class_id.clone(),
            activity_id: activity.activity_id.clone(),
            expected_other_id: activity.other_id,
            expected_start_time_millis: activity.start_time_millis,
            expected_end_time_millis: activity.end_time_millis,
            discovery_fingerprint: activity.fingerprint.clone(),
        }
    }

    struct FixtureTransport;

    #[async_trait]
    impl ChaoxingSignActivityReadTransport for FixtureTransport {
        async fn fetch_sign_activity_list(
            &self,
            _context: &ProviderContext,
            route: ChaoxingCourseRoute<'_>,
        ) -> ProviderResult<ChaoxingSignActivityListDocument> {
            assert_eq!(route.course_id(), "1001");
            assert_eq!(route.class_id(), "2001");
            ChaoxingSignActivityListDocument::try_new(LIST_FIXTURE)
        }

        async fn fetch_sign_detail(
            &self,
            _context: &ProviderContext,
            request: &ChaoxingSignDetailRequest,
        ) -> ProviderResult<ChaoxingSignDetailDocument> {
            assert_eq!(request.activity_id(), "7001");
            assert_eq!(request.discovery_fingerprint().len(), 64);
            ChaoxingSignDetailDocument::try_new(DETAIL_FIXTURE)
        }
    }

    fn context() -> ProviderContext {
        ProviderContext {
            provider_id: ProviderId::new("chaoxing").unwrap(),
            account_id: ProviderAccountId::new(),
            credential_refs: vec![SecretId::new()],
            correlation_id: "chaoxing-sign-read-test".to_owned(),
        }
    }

    fn course() -> RemoteCourse {
        RemoteCourse {
            remote_id: "course:1001:2001".to_owned(),
            title: "course".to_owned(),
            term: None,
            teacher: None,
            remote_status: None,
            metadata_sanitized: json!({"safe": true}),
            route_context: ProviderRouteContext::try_from_pairs([
                ("chaoxing.course_id".to_owned(), "1001".to_owned()),
                ("chaoxing.class_id".to_owned(), "2001".to_owned()),
                ("chaoxing.cpi".to_owned(), "3001".to_owned()),
            ])
            .unwrap(),
        }
    }

    #[test]
    fn parses_only_sign_rows_and_preserves_conflicted_codes() {
        let document = ChaoxingSignActivityListDocument::try_new(LIST_FIXTURE).unwrap();
        let activities = parse_sign_activity_list(&document, &scope()).unwrap();

        assert_eq!(activities.len(), 6);
        assert_eq!(activities[0].remote_id(), "sign:1001:2001:7001");
        assert_eq!(activities[0].display_label(), "Morning attendance");
        assert_eq!(activities[0].variant(), ChaoxingSignVariant::Normal);
        assert_eq!(activities[0].provider_status(), 1);
        assert_eq!(activities[0].user_status(), Some(0));
        assert_eq!(activities[0].start_time_millis(), Some(1_786_800_000_000));
        assert_eq!(activities[0].end_time_millis(), Some(1_786_803_600_000));
        assert_eq!(activities[1].variant(), ChaoxingSignVariant::Photo);
        assert_eq!(activities[2].variant(), ChaoxingSignVariant::QrCode);
        assert_eq!(activities[3].variant(), ChaoxingSignVariant::Gesture);
        assert_eq!(activities[4].variant(), ChaoxingSignVariant::Location);
        assert_eq!(
            activities[5].variant(),
            ChaoxingSignVariant::AmbiguousCodeOrPhoto
        );
        assert_eq!(activities[5].provider_status(), 2);
        assert_eq!(activities[5].user_status(), None);
        assert_eq!(activities[0].fingerprint().len(), 64);
    }

    #[tokio::test]
    async fn read_facade_rebinds_course_before_each_transport_call() {
        let reader = ChaoxingSignActivityRead::try_new(Arc::new(FixtureTransport)).unwrap();
        let activities = reader.list_course(&context(), &course()).await.unwrap();
        let detail = reader
            .read_detail(&context(), &course(), &activities[0])
            .await
            .unwrap();
        assert_eq!(detail.remote_id(), activities[0].remote_id());
        assert_eq!(reader.metadata(), &development_metadata().unwrap());
    }

    #[tokio::test]
    async fn read_facade_rejects_missing_auth_and_cross_course_detail() {
        let reader = ChaoxingSignActivityRead::try_new(Arc::new(FixtureTransport)).unwrap();
        let mut unauthenticated = context();
        unauthenticated.credential_refs.clear();
        let error = reader
            .list_course(&unauthenticated, &course())
            .await
            .unwrap_err();
        assert_eq!(error.kind, ProviderErrorKind::Authentication);

        let activities = reader.list_course(&context(), &course()).await.unwrap();
        let foreign = RemoteCourse {
            remote_id: "course:1001:9999".to_owned(),
            route_context: ProviderRouteContext::try_from_pairs([
                ("chaoxing.course_id".to_owned(), "1001".to_owned()),
                ("chaoxing.class_id".to_owned(), "9999".to_owned()),
                ("chaoxing.cpi".to_owned(), "3001".to_owned()),
            ])
            .unwrap(),
            ..course()
        };
        let error = reader
            .read_detail(&context(), &foreign, &activities[0])
            .await
            .unwrap_err();
        assert_eq!(error.kind, ProviderErrorKind::RemoteChanged);
    }

    #[test]
    fn parses_bound_detail_and_refines_photo_flag() {
        let list = ChaoxingSignActivityListDocument::try_new(LIST_FIXTURE).unwrap();
        let activity = parse_sign_activity_list(&list, &scope()).unwrap().remove(0);
        let request = detail_request(&activity);
        let detail = ChaoxingSignDetailDocument::try_new(DETAIL_FIXTURE).unwrap();
        let detail = parse_sign_detail(&detail, &request).unwrap();

        assert_eq!(detail.remote_id(), activity.remote_id());
        assert_eq!(detail.variant(), ChaoxingSignVariant::Normal);
        assert_eq!(detail.provider_status(), 1);
        assert_eq!(detail.start_time_millis(), activity.start_time_millis());
        assert_eq!(detail.end_time_millis(), activity.end_time_millis());
        assert_eq!(detail.refreshes_qr_code(), Some(false));
        assert_eq!(detail.fingerprint().len(), 64);
    }

    #[test]
    fn rejects_duplicate_sign_identity_atomically() {
        let fixture = LIST_FIXTURE.replace("\"id\": \"7002\"", "\"id\": \"7001\"");
        let document = ChaoxingSignActivityListDocument::try_new(fixture).unwrap();
        let error = parse_sign_activity_list(&document, &scope()).unwrap_err();
        assert_eq!(error.kind, ProviderErrorKind::ProtocolDrift);
    }

    #[test]
    fn rejects_cross_course_rows() {
        let fixture = LIST_FIXTURE.replacen("\"courseId\": \"1001\"", "\"courseId\": \"9999\"", 1);
        let document = ChaoxingSignActivityListDocument::try_new(fixture).unwrap();
        let error = parse_sign_activity_list(&document, &scope()).unwrap_err();
        assert_eq!(error.kind, ProviderErrorKind::RemoteChanged);
    }

    #[test]
    fn rejects_unknown_status_shape_instead_of_guessing() {
        let fixture = LIST_FIXTURE.replacen("\"status\": 1", "\"status\": \"active\"", 1);
        let document = ChaoxingSignActivityListDocument::try_new(fixture).unwrap();
        let error = parse_sign_activity_list(&document, &scope()).unwrap_err();
        assert_eq!(error.kind, ProviderErrorKind::ProtocolDrift);
    }

    #[test]
    fn rejects_detail_variant_or_time_drift() {
        let list = ChaoxingSignActivityListDocument::try_new(LIST_FIXTURE).unwrap();
        let activity = parse_sign_activity_list(&list, &scope()).unwrap().remove(0);
        let request = detail_request(&activity);

        for changed in [
            DETAIL_FIXTURE.replace("\"otherId\": 0", "\"otherId\": 4"),
            DETAIL_FIXTURE.replace("1786800000000", "1786800000001"),
        ] {
            let document = ChaoxingSignDetailDocument::try_new(changed).unwrap();
            let error = parse_sign_detail(&document, &request).unwrap_err();
            assert_eq!(error.kind, ProviderErrorKind::RemoteChanged);
        }
    }

    #[test]
    fn documents_are_bounded_and_redacted() {
        let oversized = "x".repeat(MAX_SIGN_DOCUMENT_BYTES + 1);
        let error = ChaoxingSignActivityListDocument::try_new(oversized).unwrap_err();
        assert_eq!(error.kind, ProviderErrorKind::InvalidResponse);
        let document = ChaoxingSignDetailDocument::try_new(DETAIL_FIXTURE).unwrap();
        assert_eq!(
            format!("{document:?}"),
            "ChaoxingSignDetailDocument([REDACTED])"
        );
    }
}
