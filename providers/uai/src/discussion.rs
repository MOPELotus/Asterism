use std::fmt;

use asterism_domain::SubmissionReceipt;
use asterism_provider_api::{ProviderContext, ProviderError, ProviderErrorKind, ProviderResult};
use async_trait::async_trait;
use chrono::Utc;
use serde_json::{Map, Value};
use zeroize::{Zeroize, Zeroizing};

use crate::{
    course_inventory::parse_course_context_for_resource_id, encrypted::ZeroizingJsonValue,
    user_identity::parse_user_identity,
};

const MAX_DISCUSSION_RESPONSE_BYTES: usize = 1_024 * 1_024;
const MAX_DISCUSSION_CONTENT_BYTES: usize = 8 * 1_024;
const MAX_DISCUSSION_ID_BYTES: usize = 128;
const MAX_DISCUSSION_PAGE_SIZE: u32 = 20;

/// Provider-private native boundary for the donor-audited discussion flow.
/// A shared immutable reply Draft capability is still required before this is
/// registered as a cross-Provider capability.
#[async_trait]
pub trait UaiDiscussionTransport: Send + Sync {
    async fn resolve_discussion_binding(
        &self,
        context: &ProviderContext,
        course_resource_id: &str,
        group_id: &str,
    ) -> ProviderResult<UaiDiscussionBinding>;

    async fn find_discussion_topic(
        &self,
        context: &ProviderContext,
        binding: &UaiDiscussionBinding,
    ) -> ProviderResult<Option<u64>>;

    async fn read_discussion_replies(
        &self,
        context: &ProviderContext,
        topic_id: u64,
        page_number: u32,
        page_size: u32,
    ) -> ProviderResult<UaiDiscussionReplyPage>;

    async fn submit_discussion_reply(
        &self,
        context: &ProviderContext,
        draft: &UaiDiscussionReplyDraft,
    ) -> ProviderResult<SubmissionReceipt>;
}

/// Fresh route and account facts required by the UAI discussion endpoints.
///
/// The object is Provider-private until Core gains an immutable discussion
/// reply Draft. It deliberately carries no credential or browser material.
#[derive(Clone, Eq, PartialEq)]
pub struct UaiDiscussionBinding {
    course_instance: String,
    group: String,
    class: String,
    curricula: String,
    current_user: String,
}

impl UaiDiscussionBinding {
    /// Builds one fresh, exact Course/Group/account discussion binding.
    ///
    /// # Errors
    ///
    /// Rejects unbounded or unsafe route and identity values.
    pub fn try_new(
        course_instance_id: &str,
        group_id: &str,
        class_id: &str,
        curricula_id: &str,
        current_user_id: &str,
    ) -> ProviderResult<Self> {
        Ok(Self {
            course_instance: required_route_value(course_instance_id, "Course instance")?,
            group: required_identifier(group_id, "Group")?,
            class: optional_identifier(class_id, "class")?,
            curricula: required_identifier(curricula_id, "curricula")?,
            current_user: required_identifier(current_user_id, "current user")?,
        })
    }

    pub fn group_id(&self) -> &str {
        &self.group
    }

    pub fn current_user_id(&self) -> &str {
        &self.current_user
    }
}

impl fmt::Debug for UaiDiscussionBinding {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UaiDiscussionBinding")
            .field("course_instance", &"[ROUTE]")
            .field("group", &self.group)
            .field("class", &"[ROUTE]")
            .field("curricula", &"[ROUTE]")
            .field("current_user", &"[ACCOUNT]")
            .finish()
    }
}

impl Drop for UaiDiscussionBinding {
    fn drop(&mut self) {
        self.course_instance.zeroize();
        self.group.zeroize();
        self.class.zeroize();
        self.curricula.zeroize();
        self.current_user.zeroize();
    }
}

/// Provider-private immutable intent for one UAI discussion reply.
pub struct UaiDiscussionReplyDraft {
    binding: UaiDiscussionBinding,
    topic_id: u64,
    content: Zeroizing<String>,
}

impl UaiDiscussionReplyDraft {
    /// Freezes one exact topic and bounded text reply.
    ///
    /// # Errors
    ///
    /// Rejects missing topics and blank, unsafe or oversized reply content.
    pub fn try_new(
        binding: UaiDiscussionBinding,
        topic_id: u64,
        content: &str,
    ) -> ProviderResult<Self> {
        if topic_id == 0 {
            return Err(invalid_input("UAI discussion topic ID is invalid"));
        }
        let content = content.trim();
        if content.is_empty()
            || content.len() > MAX_DISCUSSION_CONTENT_BYTES
            || content
                .chars()
                .any(|character| character.is_control() && !matches!(character, '\n' | '\r' | '\t'))
        {
            return Err(invalid_input("UAI discussion reply content is invalid"));
        }
        Ok(Self {
            binding,
            topic_id,
            content: Zeroizing::new(content.to_owned()),
        })
    }

    pub fn binding(&self) -> &UaiDiscussionBinding {
        &self.binding
    }

    pub const fn topic_id(&self) -> u64 {
        self.topic_id
    }

    pub fn expose_content(&self) -> &str {
        self.content.as_str()
    }
}

impl fmt::Debug for UaiDiscussionReplyDraft {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UaiDiscussionReplyDraft")
            .field("binding", &self.binding)
            .field("topic_id", &self.topic_id)
            .field("content", &"[REDACTED]")
            .finish()
    }
}

#[derive(Clone, Eq, PartialEq)]
pub struct UaiDiscussionReply {
    remote_reply_id: Option<String>,
    create_id: String,
    content: String,
}

impl UaiDiscussionReply {
    pub fn remote_reply_id(&self) -> Option<&str> {
        self.remote_reply_id.as_deref()
    }

    pub fn create_id(&self) -> &str {
        &self.create_id
    }

    pub fn content(&self) -> &str {
        &self.content
    }
}

impl fmt::Debug for UaiDiscussionReply {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UaiDiscussionReply")
            .field("remote_reply_id", &self.remote_reply_id)
            .field("create_id", &"[ACCOUNT]")
            .field("content", &"[REDACTED]")
            .finish()
    }
}

impl Drop for UaiDiscussionReply {
    fn drop(&mut self) {
        if let Some(remote_reply_id) = self.remote_reply_id.as_mut() {
            remote_reply_id.zeroize();
        }
        self.create_id.zeroize();
        self.content.zeroize();
    }
}

#[derive(Clone, Eq, PartialEq)]
pub struct UaiDiscussionReplyPage {
    replies: Vec<UaiDiscussionReply>,
    has_more: bool,
}

impl fmt::Debug for UaiDiscussionReplyPage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UaiDiscussionReplyPage")
            .field("reply_count", &self.replies.len())
            .field("has_more", &self.has_more)
            .finish()
    }
}

impl UaiDiscussionReplyPage {
    pub fn replies(&self) -> &[UaiDiscussionReply] {
        &self.replies
    }

    pub const fn has_more(&self) -> bool {
        self.has_more
    }

    pub fn contains_exact_reply(&self, draft: &UaiDiscussionReplyDraft) -> bool {
        self.replies.iter().any(|reply| {
            reply.create_id == draft.binding.current_user && reply.content == draft.content.as_str()
        })
    }
}

/// Derives the donor-required discussion route facts from fresh Course-list,
/// Course-resource detail and user-info reads.
///
/// # Errors
///
/// Fails closed when the selected Course-resource is missing/duplicated or any
/// Course, class, instance, Group or account identity is malformed.
pub fn parse_discussion_binding(
    course_document: &str,
    course_detail_document: &str,
    user_info_document: &[u8],
    expected_course_resource_id: &str,
    expected_group_id: &str,
) -> ProviderResult<UaiDiscussionBinding> {
    let expected_course_resource_id =
        required_identifier(expected_course_resource_id, "Course resource")?;
    let root = parse_object(course_document, "Course list response")?;
    let root = root
        .as_value()
        .as_object()
        .ok_or_else(|| protocol_drift("UAI discussion Course list is not an object"))?;
    if root.get("code").and_then(Value::as_i64) != Some(1)
        || root.get("success").and_then(Value::as_bool) != Some(true)
    {
        return Err(invalid_response(
            "UAI discussion Course list did not succeed",
        ));
    }
    let courses = root
        .get("value")
        .and_then(Value::as_object)
        .and_then(|value| value.get("courseList"))
        .and_then(Value::as_array)
        .ok_or_else(|| protocol_drift("UAI discussion Course list has no Course array"))?;
    let mut selected: Option<(String, String)> = None;
    for course in courses {
        let course = course
            .as_object()
            .ok_or_else(|| protocol_drift("UAI discussion Course row is not an object"))?;
        let curricula_id = required_scalar_identifier(course.get("id"), "curricula ID")?;
        let class_id = course
            .get("classId")
            .map(|value| optional_scalar_identifier(value, "class ID"))
            .transpose()?
            .unwrap_or_default();
        let resources = course
            .get("courseResourceList")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                protocol_drift("UAI discussion Course row has no Course-resource array")
            })?;
        for resource in resources {
            let resource = resource.as_object().ok_or_else(|| {
                protocol_drift("UAI discussion Course-resource row is not an object")
            })?;
            let resource_id = required_scalar_identifier(resource.get("id"), "Course-resource ID")?;
            if resource_id == expected_course_resource_id
                && selected
                    .replace((class_id.clone(), curricula_id.clone()))
                    .is_some()
            {
                return Err(protocol_drift(
                    "UAI discussion Course list contains a duplicate selected resource",
                ));
            }
        }
    }
    let (class_id, curricula_id) = selected.ok_or_else(|| {
        ProviderError::new(
            ProviderErrorKind::RemoteChanged,
            "UAI discussion Course-resource disappeared before execution",
        )
    })?;
    let route =
        parse_course_context_for_resource_id(expected_course_resource_id, course_detail_document)?;
    let identity = parse_user_identity(user_info_document)?;
    UaiDiscussionBinding::try_new(
        route.course_instance_id(),
        expected_group_id,
        &class_id,
        &curricula_id,
        identity.app_user_id(),
    )
}

/// Builds the donor-audited first-page topic lookup body.
///
/// # Errors
///
/// Returns an invalid-input error if the bounded JSON request cannot be
/// encoded.
pub fn build_discussion_topic_request(
    binding: &UaiDiscussionBinding,
) -> ProviderResult<Zeroizing<String>> {
    encode_request(&serde_json::json!({
        "order": "desc",
        "pageNum": 1,
        "pageSize": 1,
        "courseId": binding.course_instance,
        "groupId": binding.group,
        "type": 2,
        "classId": binding.class,
        "curriculaId": binding.curricula,
    }))
}

/// Parses one first-page topic lookup without guessing a missing topic.
///
/// # Errors
///
/// Returns a typed invalid-response or protocol-drift error for malformed,
/// rejected, oversized or ambiguous documents.
pub fn parse_discussion_topic(document: &str) -> ProviderResult<Option<u64>> {
    let root = parse_object(document, "topic response")?;
    let root = root
        .as_value()
        .as_object()
        .ok_or_else(|| protocol_drift("UAI discussion topic response is not an object"))?;
    require_success(root, "topic response")?;
    let topics = root
        .get("value")
        .and_then(Value::as_object)
        .and_then(|value| value.get("utopics"))
        .and_then(Value::as_array)
        .ok_or_else(|| protocol_drift("UAI discussion topic response has no topic array"))?;
    match topics.as_slice() {
        [] => Ok(None),
        [topic] => topic
            .as_object()
            .and_then(|topic| topic.get("topicId"))
            .and_then(unsigned_scalar)
            .filter(|topic_id| *topic_id > 0)
            .map(Some)
            .ok_or_else(|| protocol_drift("UAI discussion topic response has an invalid topic ID")),
        _ => Err(protocol_drift(
            "UAI discussion single-topic lookup returned multiple topics",
        )),
    }
}

/// Builds one bounded page request for current-user reply readback.
///
/// # Errors
///
/// Returns an invalid-input error for zero/unbounded page values or JSON
/// encoding failure.
pub fn build_discussion_reply_page_request(
    topic_id: u64,
    page_number: u32,
    page_size: u32,
) -> ProviderResult<Zeroizing<String>> {
    if topic_id == 0 || page_number == 0 || page_size == 0 || page_size > MAX_DISCUSSION_PAGE_SIZE {
        return Err(invalid_input(
            "UAI discussion reply page request is outside the bounded range",
        ));
    }
    encode_request(&serde_json::json!({
        "topicId": topic_id,
        "parentId": "",
        "order": "desc",
        "pageNum": page_number,
        "pageSize": page_size,
    }))
}

/// Parses one bounded reply page. Pagination authority remains with Core/the
/// future discussion capability and must stop at its own maximum page count.
///
/// # Errors
///
/// Returns a typed invalid-response or protocol-drift error for rejected,
/// oversized or structurally ambiguous reply pages.
pub fn parse_discussion_reply_page(
    document: &str,
    requested_page_size: u32,
) -> ProviderResult<UaiDiscussionReplyPage> {
    if requested_page_size == 0 || requested_page_size > MAX_DISCUSSION_PAGE_SIZE {
        return Err(invalid_input(
            "UAI discussion reply page size is outside the bounded range",
        ));
    }
    let root = parse_object(document, "reply page response")?;
    let root = root
        .as_value()
        .as_object()
        .ok_or_else(|| protocol_drift("UAI discussion reply page response is not an object"))?;
    require_success(root, "reply page response")?;
    let rows = root
        .get("value")
        .and_then(Value::as_object)
        .and_then(|value| value.get("replyContents"))
        .and_then(Value::as_array)
        .ok_or_else(|| protocol_drift("UAI discussion reply response has no reply array"))?;
    if rows.len() > requested_page_size as usize {
        return Err(protocol_drift(
            "UAI discussion reply response exceeds its requested page size",
        ));
    }
    let mut replies = Vec::with_capacity(rows.len());
    for row in rows {
        let row = row
            .as_object()
            .ok_or_else(|| protocol_drift("UAI discussion reply row is not an object"))?;
        let create_id = required_scalar_identifier(row.get("createId"), "reply creator")?;
        let content = row
            .get("content")
            .and_then(Value::as_str)
            .filter(|content| {
                !content.is_empty()
                    && content.len() <= MAX_DISCUSSION_CONTENT_BYTES
                    && !content.chars().any(|character| {
                        character.is_control() && !matches!(character, '\n' | '\r' | '\t')
                    })
            })
            .ok_or_else(|| protocol_drift("UAI discussion reply has invalid content"))?
            .to_owned();
        let remote_reply_id = row
            .get("replyId")
            .or_else(|| row.get("id"))
            .map(|value| required_scalar_identifier(Some(value), "reply ID"))
            .transpose()?;
        replies.push(UaiDiscussionReply {
            remote_reply_id,
            create_id,
            content,
        });
    }
    Ok(UaiDiscussionReplyPage {
        has_more: rows.len() == requested_page_size as usize,
        replies,
    })
}

/// Builds the donor-audited plain-text reply mutation body.
///
/// # Errors
///
/// Returns an invalid-input error if the bounded JSON body cannot be encoded.
pub fn build_discussion_reply_request(
    draft: &UaiDiscussionReplyDraft,
) -> ProviderResult<Zeroizing<String>> {
    encode_request(&serde_json::json!({
        "type": 2,
        "content": draft.content.as_str(),
        "topicId": draft.topic_id,
        "parentId": "",
        "topReplyId": "",
        "contentType": "text",
    }))
}

/// Parses a reply acknowledgement as a receipt only. Exact current-user
/// content readback remains the independent success authority.
///
/// # Errors
///
/// Returns a typed invalid-response or protocol-drift error for rejected,
/// malformed or unbounded acknowledgement documents.
pub fn parse_discussion_reply_receipt(document: &str) -> ProviderResult<SubmissionReceipt> {
    let root = parse_object(document, "reply acknowledgement")?;
    let root = root
        .as_value()
        .as_object()
        .ok_or_else(|| protocol_drift("UAI discussion reply acknowledgement is not an object"))?;
    let accepted = root.get("success").and_then(Value::as_bool) == Some(true)
        || root.get("code").and_then(Value::as_i64) == Some(200);
    if !accepted {
        return Err(invalid_response(
            "UAI rejected the discussion reply mutation",
        ));
    }
    let provider_trace_id = root
        .get("value")
        .and_then(Value::as_object)
        .and_then(|value| value.get("replyId").or_else(|| value.get("id")))
        .or_else(|| root.get("replyId"))
        .map(|value| required_scalar_identifier(Some(value), "reply receipt ID"))
        .transpose()?;
    let receipt = SubmissionReceipt {
        remote_status: "accepted".to_owned(),
        message_sanitized: Some(
            "UAI accepted the discussion reply for independent readback".to_owned(),
        ),
        provider_trace_id,
        received_at: Utc::now(),
    };
    receipt
        .validate()
        .map_err(|_| invalid_response("UAI discussion reply receipt is invalid"))?;
    Ok(receipt)
}

fn encode_request(value: &Value) -> ProviderResult<Zeroizing<String>> {
    let encoded = serde_json::to_string(value)
        .map_err(|_| invalid_input("UAI discussion request cannot be encoded"))?;
    if encoded.is_empty() || encoded.len() > MAX_DISCUSSION_CONTENT_BYTES * 2 {
        return Err(invalid_input(
            "UAI discussion request exceeds the size limit",
        ));
    }
    Ok(Zeroizing::new(encoded))
}

fn parse_object(document: &str, label: &'static str) -> ProviderResult<ZeroizingJsonValue> {
    if document.is_empty() || document.len() > MAX_DISCUSSION_RESPONSE_BYTES {
        return Err(invalid_response(format!(
            "UAI discussion {label} is empty or exceeds the size limit"
        )));
    }
    let value = serde_json::from_str::<Value>(document)
        .map_err(|_| invalid_response(format!("UAI discussion {label} is not valid JSON")))?;
    if !value.is_object() {
        return Err(protocol_drift(format!(
            "UAI discussion {label} is not an object"
        )));
    }
    Ok(ZeroizingJsonValue::new(value))
}

fn require_success(root: &Map<String, Value>, label: &'static str) -> ProviderResult<()> {
    if root.get("success").and_then(Value::as_bool) != Some(true) {
        return Err(invalid_response(format!(
            "UAI discussion {label} did not succeed"
        )));
    }
    Ok(())
}

fn required_scalar_identifier(
    value: Option<&Value>,
    label: &'static str,
) -> ProviderResult<String> {
    let value = value
        .and_then(scalar_text)
        .ok_or_else(|| protocol_drift(format!("UAI discussion response has no {label}")))?;
    required_identifier(&value, label)
}

fn optional_scalar_identifier(value: &Value, label: &'static str) -> ProviderResult<String> {
    match value {
        Value::Null => Ok(String::new()),
        Value::String(value) if value.is_empty() => Ok(String::new()),
        _ => required_scalar_identifier(Some(value), label),
    }
}

fn scalar_text(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => Some(value.clone()),
        Value::Number(value) if value.is_u64() || value.is_i64() => Some(value.to_string()),
        _ => None,
    }
}

fn unsigned_scalar(value: &Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_str()?.parse::<u64>().ok())
}

fn required_route_value(value: &str, label: &'static str) -> ProviderResult<String> {
    let value = value.trim();
    if value.is_empty()
        || value.len() > MAX_DISCUSSION_ID_BYTES * 4
        || value.chars().any(char::is_control)
    {
        return Err(invalid_input(format!(
            "UAI discussion {label} route is invalid"
        )));
    }
    Ok(value.to_owned())
}

fn required_identifier(value: &str, label: &'static str) -> ProviderResult<String> {
    let value = value.trim();
    if value.is_empty()
        || value.len() > MAX_DISCUSSION_ID_BYTES
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(invalid_input(format!(
            "UAI discussion {label} identity is invalid"
        )));
    }
    Ok(value.to_owned())
}

fn optional_identifier(value: &str, label: &'static str) -> ProviderResult<String> {
    if value.is_empty() {
        Ok(String::new())
    } else {
        required_identifier(value, label)
    }
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

    const COURSES: &str = include_str!("../../../fixtures/providers/uai/courses/list-mixed.json");
    const DETAIL: &str =
        include_str!("../../../fixtures/providers/uai/courses/resource-detail.json");
    const USER_INFO: &[u8] =
        include_bytes!("../../../fixtures/providers/uai/auth/user-info-valid.json");

    #[test]
    fn fresh_documents_derive_the_exact_discussion_binding() {
        let binding =
            parse_discussion_binding(COURSES, DETAIL, USER_INFO, "2001", "group-1").unwrap();
        let body: Value =
            serde_json::from_str(&build_discussion_topic_request(&binding).unwrap()).unwrap();
        assert_eq!(body["courseId"], "course-v2:synthetic+rw+20260809");
        assert_eq!(body["groupId"], "group-1");
        assert_eq!(body["classId"], "class-must-drop");
        assert_eq!(body["curriculaId"], "1001");
        assert!(
            parse_discussion_binding(COURSES, DETAIL, USER_INFO, "missing", "group-1").is_err()
        );
    }

    #[test]
    fn topic_request_and_parser_preserve_exact_course_group_binding() {
        let binding = binding();
        let body: Value =
            serde_json::from_str(&build_discussion_topic_request(&binding).unwrap()).unwrap();
        assert_eq!(body["courseId"], "course-v2:synthetic+rw");
        assert_eq!(body["groupId"], "group-1");
        assert_eq!(body["classId"], "class-7");
        assert_eq!(body["curriculaId"], "1001");
        assert_eq!(
            parse_discussion_topic(r#"{"success":true,"value":{"utopics":[{"topicId":42}]}}"#)
                .unwrap(),
            Some(42)
        );
        assert_eq!(
            parse_discussion_topic(r#"{"success":true,"value":{"utopics":[]}}"#).unwrap(),
            None
        );
    }

    #[test]
    fn reply_draft_request_and_exact_readback_are_bound_and_redacted() {
        let draft = UaiDiscussionReplyDraft::try_new(binding(), 42, "  bounded reply  ").unwrap();
        assert!(!format!("{draft:?}").contains("bounded reply"));
        let body: Value =
            serde_json::from_str(&build_discussion_reply_request(&draft).unwrap()).unwrap();
        assert_eq!(body["topicId"], 42);
        assert_eq!(body["content"], "bounded reply");
        assert_eq!(body["contentType"], "text");

        let page = parse_discussion_reply_page(
            r#"{"success":true,"value":{"replyContents":[{"replyId":9,"createId":"user-9","content":"bounded reply"}]}}"#,
            20,
        )
        .unwrap();
        assert!(page.contains_exact_reply(&draft));
        assert!(!page.has_more());
        assert_eq!(page.replies()[0].remote_reply_id(), Some("9"));
    }

    #[test]
    fn bounded_pagination_and_receipt_do_not_claim_verified_success() {
        let request = build_discussion_reply_page_request(42, 3, 20).unwrap();
        let request: Value = serde_json::from_str(&request).unwrap();
        assert_eq!(request["pageNum"], 3);
        assert_eq!(request["pageSize"], 20);
        assert!(build_discussion_reply_page_request(42, 0, 20).is_err());
        assert!(build_discussion_reply_page_request(42, 1, 21).is_err());

        let receipt = parse_discussion_reply_receipt(
            r#"{"code":200,"success":false,"value":{"replyId":"reply-9"}}"#,
        )
        .unwrap();
        assert_eq!(receipt.remote_status, "accepted");
        assert_eq!(receipt.provider_trace_id.as_deref(), Some("reply-9"));
    }

    #[test]
    fn malformed_or_ambiguous_discussion_documents_fail_closed() {
        assert!(
            parse_discussion_topic(
                r#"{"success":true,"value":{"utopics":[{"topicId":1},{"topicId":2}]}}"#
            )
            .is_err()
        );
        assert!(
            parse_discussion_reply_page(
                r#"{"success":true,"value":{"replyContents":[{"createId":"user-9","content":""}]}}"#,
                20,
            )
            .is_err()
        );
        assert!(parse_discussion_reply_receipt(r#"{"code":500,"success":false}"#).is_err());
        assert!(UaiDiscussionReplyDraft::try_new(binding(), 0, "reply").is_err());
    }

    fn binding() -> UaiDiscussionBinding {
        UaiDiscussionBinding::try_new(
            "course-v2:synthetic+rw",
            "group-1",
            "class-7",
            "1001",
            "user-9",
        )
        .unwrap()
    }
}
