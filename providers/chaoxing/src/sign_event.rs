use std::{fmt, sync::Arc};

use asterism_provider_api::{
    ProviderContext, ProviderError, ProviderErrorKind, ProviderIdentity, ProviderMetadata,
    ProviderResult,
};
use asterism_secrets::SecretString;
use async_trait::async_trait;
use scraper::{Html, Selector};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use zeroize::Zeroize;

use crate::metadata::development_metadata;

const MAX_BOOTSTRAP_DOCUMENT_BYTES: usize = 512 * 1_024;
const MAX_EVENT_DOCUMENT_BYTES: usize = 256 * 1_024;
const MAX_IM_TOKEN_BYTES: usize = 16 * 1_024;
const MAX_IM_UID_BYTES: usize = 128;
const MAX_IM_NAME_BYTES: usize = 256;
const MAX_EVENT_ID_BYTES: usize = 128;
const MAX_COURSE_NAME_BYTES: usize = 512;
const BINDING_DIGEST_DOMAIN: &[u8] = b"asterism.chaoxing.sign-event-binding.v1\0";
const CREDENTIAL_DIGEST_DOMAIN: &[u8] = b"asterism.chaoxing.sign-event-credential.v1\0";

/// One context-bound `WebIM` bootstrap document. Its token-bearing body is
/// redacted in diagnostics and zeroized on drop.
pub struct ChaoxingSignEventBootstrapDocument {
    binding_digest: String,
    document: String,
}

impl ChaoxingSignEventBootstrapDocument {
    /// Binds one complete `im.chaoxing.com/webim/me` response to the exact
    /// Provider account and correlation which requested it.
    ///
    /// # Errors
    ///
    /// Returns a typed error for a foreign/unauthenticated context or an empty
    /// or oversized document.
    pub fn for_context(
        context: &ProviderContext,
        document: impl Into<String>,
    ) -> ProviderResult<Self> {
        validate_context(context)?;
        let mut document = document.into();
        if document.is_empty() || document.len() > MAX_BOOTSTRAP_DOCUMENT_BYTES {
            document.zeroize();
            return Err(ProviderError::new(
                ProviderErrorKind::InvalidResponse,
                "Chaoxing sign-event bootstrap document is empty or exceeds the size limit",
            ));
        }
        Ok(Self {
            binding_digest: context_binding_digest(context),
            document,
        })
    }
}

impl fmt::Debug for ChaoxingSignEventBootstrapDocument {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ChaoxingSignEventBootstrapDocument")
            .field("binding_digest", &self.binding_digest)
            .field("document", &"[REDACTED]")
            .finish()
    }
}

impl Drop for ChaoxingSignEventBootstrapDocument {
    fn drop(&mut self) {
        self.document.zeroize();
    }
}

/// Parsed `WebIM` credentials retained only behind zeroizing secret wrappers.
///
/// There is intentionally no public plaintext accessor. A future connection
/// transport must remain inside the Provider and use durable Core subscription
/// and recovery authority.
pub struct ChaoxingSignEventBootstrap {
    binding_digest: String,
    credential_digest: String,
    token: SecretString,
    uid: SecretString,
    name: SecretString,
}

impl ChaoxingSignEventBootstrap {
    pub fn binding_digest(&self) -> &str {
        &self.binding_digest
    }

    pub fn credential_digest(&self) -> &str {
        &self.credential_digest
    }

    /// Reports structural completeness without exposing any credential value.
    pub fn credential_count(&self) -> usize {
        [
            self.token.expose_secret(),
            self.uid.expose_secret(),
            self.name.expose_secret(),
        ]
        .into_iter()
        .filter(|value| !value.is_empty())
        .count()
    }
}

impl fmt::Debug for ChaoxingSignEventBootstrap {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ChaoxingSignEventBootstrap")
            .field("binding_digest", &self.binding_digest)
            .field("credential_digest", &self.credential_digest)
            .field("token", &"[REDACTED]")
            .field("uid", &"[REDACTED]")
            .field("name", &"[REDACTED]")
            .finish()
    }
}

/// One bounded IM message bound to the credentials which observed it.
pub struct ChaoxingSignEventDocument {
    binding_digest: String,
    credential_digest: String,
    document: String,
}

impl ChaoxingSignEventDocument {
    /// Binds one already-delivered IM message to a parsed bootstrap.
    ///
    /// This constructor does not open a WebSocket or acknowledge a message.
    ///
    /// # Errors
    ///
    /// Returns an invalid-response error for an empty or oversized message.
    pub fn for_bootstrap(
        bootstrap: &ChaoxingSignEventBootstrap,
        document: impl Into<String>,
    ) -> ProviderResult<Self> {
        let mut document = document.into();
        if document.is_empty() || document.len() > MAX_EVENT_DOCUMENT_BYTES {
            document.zeroize();
            return Err(ProviderError::new(
                ProviderErrorKind::InvalidResponse,
                "Chaoxing sign-event message is empty or exceeds the size limit",
            ));
        }
        Ok(Self {
            binding_digest: bootstrap.binding_digest.clone(),
            credential_digest: bootstrap.credential_digest.clone(),
            document,
        })
    }
}

impl fmt::Debug for ChaoxingSignEventDocument {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ChaoxingSignEventDocument")
            .field("binding_digest", &self.binding_digest)
            .field("credential_digest", &self.credential_digest)
            .field("document", &"[REDACTED]")
            .finish()
    }
}

impl Drop for ChaoxingSignEventDocument {
    fn drop(&mut self) {
        self.document.zeroize();
    }
}

/// Course/activity identity discovered from a donor-observed IM attachment.
/// It is discovery evidence only and must be rebound through `signDetail`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChaoxingSignEvent {
    remote_id: String,
    activity_id: String,
    course_id: String,
    class_id: String,
    course_name: String,
    binding_digest: String,
    credential_digest: String,
    message_digest: String,
}

impl ChaoxingSignEvent {
    pub fn remote_id(&self) -> &str {
        &self.remote_id
    }

    pub fn activity_id(&self) -> &str {
        &self.activity_id
    }

    pub fn course_id(&self) -> &str {
        &self.course_id
    }

    pub fn class_id(&self) -> &str {
        &self.class_id
    }

    pub fn course_name(&self) -> &str {
        &self.course_name
    }

    pub fn binding_digest(&self) -> &str {
        &self.binding_digest
    }

    pub fn credential_digest(&self) -> &str {
        &self.credential_digest
    }

    pub fn message_digest(&self) -> &str {
        &self.message_digest
    }
}

/// Read-only transport for the HTTP bootstrap page only. WebSocket connection
/// and event acknowledgement are deliberately absent.
#[async_trait]
pub trait ChaoxingSignEventReadTransport: Send + Sync {
    async fn fetch_sign_event_bootstrap(
        &self,
        context: &ProviderContext,
    ) -> ProviderResult<ChaoxingSignEventBootstrapDocument>;
}

/// Unregistered Provider-local facade for `WebIM` bootstrap discovery.
pub struct ChaoxingSignEventRead {
    metadata: ProviderMetadata,
    transport: Arc<dyn ChaoxingSignEventReadTransport>,
}

impl ChaoxingSignEventRead {
    /// Creates the read-only `WebIM` bootstrap facade.
    ///
    /// # Errors
    ///
    /// Returns an internal error when compile-time metadata is invalid.
    pub fn try_new(transport: Arc<dyn ChaoxingSignEventReadTransport>) -> ProviderResult<Self> {
        Ok(Self {
            metadata: development_metadata()?,
            transport,
        })
    }

    /// Fetches and parses one context-bound `WebIM` bootstrap page.
    ///
    /// # Errors
    ///
    /// Fails before transport for a foreign or unauthenticated context and
    /// otherwise propagates bounded transport/parser failures.
    pub async fn bootstrap(
        &self,
        context: &ProviderContext,
    ) -> ProviderResult<ChaoxingSignEventBootstrap> {
        validate_context(context)?;
        let document = self.transport.fetch_sign_event_bootstrap(context).await?;
        parse_sign_event_bootstrap(&document)
    }
}

impl fmt::Debug for ChaoxingSignEventRead {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ChaoxingSignEventRead")
            .field("metadata", &self.metadata)
            .field("transport", &"configured")
            .finish()
    }
}

impl ProviderIdentity for ChaoxingSignEventRead {
    fn metadata(&self) -> &ProviderMetadata {
        &self.metadata
    }
}

/// Parses exact donor-observed `#myToken`, `#myTuid`, and `#myName` fields.
///
/// # Errors
///
/// Missing, duplicate, empty, malformed or oversized fields fail closed.
pub fn parse_sign_event_bootstrap(
    document: &ChaoxingSignEventBootstrapDocument,
) -> ProviderResult<ChaoxingSignEventBootstrap> {
    let html = Html::parse_document(&document.document);
    let mut token = unique_element_text(&html, "#myToken", "WebIM token")?;
    let mut uid = unique_element_text(&html, "#myTuid", "WebIM UID")?;
    let mut name = unique_element_text(&html, "#myName", "WebIM name")?;
    let result = (|| {
        validate_secret(&token, MAX_IM_TOKEN_BYTES, "WebIM token")?;
        validate_decimal_id(&uid, MAX_IM_UID_BYTES)?;
        validate_secret(&name, MAX_IM_NAME_BYTES, "WebIM name")?;
        let credential_digest = credential_digest(&document.binding_digest, &token, &uid, &name);
        Ok(ChaoxingSignEventBootstrap {
            binding_digest: document.binding_digest.clone(),
            credential_digest,
            token: SecretString::new(std::mem::take(&mut token)),
            uid: SecretString::new(std::mem::take(&mut uid)),
            name: SecretString::new(std::mem::take(&mut name)),
        })
    })();
    token.zeroize();
    uid.zeroize();
    name.zeroize();
    result
}

/// Parses one delivered IM message and returns only sign-in (`atype=2`)
/// attachments. Other activity types are ignored.
///
/// # Errors
///
/// A sign-like attachment with malformed or unbounded identity fields fails
/// closed instead of being partially accepted.
pub fn parse_sign_event(
    document: &ChaoxingSignEventDocument,
) -> ProviderResult<Option<ChaoxingSignEvent>> {
    let root: Value = serde_json::from_str(&document.document).map_err(|_| {
        ProviderError::new(
            ProviderErrorKind::InvalidResponse,
            "Chaoxing sign-event message is not valid JSON",
        )
    })?;
    let Some(attachment) = root
        .get("ext")
        .and_then(Value::as_object)
        .and_then(|ext| ext.get("attachment"))
        .and_then(Value::as_object)
        .and_then(|attachment| attachment.get("att_chat_course"))
    else {
        return Ok(None);
    };
    let attachment = attachment
        .as_object()
        .ok_or_else(|| protocol_drift("Chaoxing sign-event course attachment is not an object"))?;
    if required_code(attachment, "atype")? != 2 {
        return Ok(None);
    }

    let activity_id = required_id(attachment, "aid")?;
    let course = attachment
        .get("courseInfo")
        .and_then(Value::as_object)
        .ok_or_else(|| protocol_drift("Chaoxing sign-event has no courseInfo object"))?;
    let course_id = required_id(course, "courseid")?;
    let class_id = required_id(course, "classid")?;
    let course_name = required_text(course, "coursename", MAX_COURSE_NAME_BYTES)?;
    Ok(Some(ChaoxingSignEvent {
        remote_id: format!("sign:event:{course_id}:{class_id}:{activity_id}"),
        activity_id,
        course_id,
        class_id,
        course_name,
        binding_digest: document.binding_digest.clone(),
        credential_digest: document.credential_digest.clone(),
        message_digest: digest(document.document.as_bytes()),
    }))
}

fn unique_element_text(
    html: &Html,
    selector_text: &'static str,
    label: &'static str,
) -> ProviderResult<String> {
    let selector = Selector::parse(selector_text).map_err(|_| {
        ProviderError::new(
            ProviderErrorKind::Internal,
            "Chaoxing sign-event selector initialization failed",
        )
    })?;
    let mut elements = html.select(&selector);
    let element = elements.next().ok_or_else(|| {
        protocol_drift("Chaoxing sign-event bootstrap is missing a required field")
    })?;
    if elements.next().is_some() {
        return Err(protocol_drift(
            "Chaoxing sign-event bootstrap has a duplicate required field",
        ));
    }
    let value = element.text().collect::<String>().trim().to_owned();
    if value.is_empty() {
        return Err(protocol_drift(match label {
            "WebIM token" => "Chaoxing sign-event bootstrap token is empty",
            "WebIM UID" => "Chaoxing sign-event bootstrap UID is empty",
            _ => "Chaoxing sign-event bootstrap name is empty",
        }));
    }
    Ok(value)
}

fn validate_secret(value: &str, maximum_bytes: usize, label: &'static str) -> ProviderResult<()> {
    if value.is_empty()
        || value.len() > maximum_bytes
        || value.chars().any(char::is_control)
        || value.trim() != value
    {
        return Err(protocol_drift(match label {
            "WebIM token" => "Chaoxing sign-event bootstrap token is invalid",
            _ => "Chaoxing sign-event bootstrap name is invalid",
        }));
    }
    Ok(())
}

fn validate_decimal_id(value: &str, maximum_bytes: usize) -> ProviderResult<()> {
    if value.is_empty()
        || value.len() > maximum_bytes
        || !value.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(protocol_drift(
            "Chaoxing sign-event bootstrap identity is invalid",
        ));
    }
    Ok(())
}

fn required_code(object: &Map<String, Value>, field: &'static str) -> ProviderResult<i64> {
    object
        .get(field)
        .and_then(Value::as_i64)
        .filter(|code| (0..=i64::from(i32::MAX)).contains(code))
        .ok_or_else(|| protocol_drift("Chaoxing sign-event has an invalid activity type"))
}

fn required_id(object: &Map<String, Value>, field: &'static str) -> ProviderResult<String> {
    let value = object
        .get(field)
        .ok_or_else(|| protocol_drift("Chaoxing sign-event lacks a required identity"))?;
    let value = match value {
        Value::String(value) => value.clone(),
        Value::Number(value) => value
            .as_u64()
            .map(|value| value.to_string())
            .ok_or_else(|| protocol_drift("Chaoxing sign-event contains an invalid identity"))?,
        _ => {
            return Err(protocol_drift(
                "Chaoxing sign-event contains an invalid identity",
            ));
        }
    };
    if value.is_empty()
        || value.len() > MAX_EVENT_ID_BYTES
        || !value.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(protocol_drift(
            "Chaoxing sign-event contains an invalid identity",
        ));
    }
    Ok(value)
}

fn required_text(
    object: &Map<String, Value>,
    field: &'static str,
    maximum_bytes: usize,
) -> ProviderResult<String> {
    let value = object
        .get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| {
            !value.is_empty()
                && value.len() <= maximum_bytes
                && !value.chars().any(char::is_control)
        })
        .ok_or_else(|| protocol_drift("Chaoxing sign-event contains invalid course text"))?;
    Ok(value.to_owned())
}

fn validate_context(context: &ProviderContext) -> ProviderResult<()> {
    if context.provider_id.as_str() != "chaoxing" {
        return Err(ProviderError::new(
            ProviderErrorKind::Internal,
            "Chaoxing sign-event read received a mismatched Provider context",
        ));
    }
    if context.credential_refs.is_empty() {
        return Err(ProviderError::new(
            ProviderErrorKind::Authentication,
            "Chaoxing sign-event read requires an authenticated session",
        ));
    }
    Ok(())
}

fn context_binding_digest(context: &ProviderContext) -> String {
    let account_id = context.account_id.to_string();
    digest_fields(
        BINDING_DIGEST_DOMAIN,
        &[
            context.provider_id.as_str(),
            &account_id,
            &context.correlation_id,
        ],
    )
}

fn credential_digest(binding_digest: &str, token: &str, uid: &str, name: &str) -> String {
    digest_fields(
        CREDENTIAL_DIGEST_DOMAIN,
        &[binding_digest, token, uid, name],
    )
}

fn digest_fields(domain: &[u8], fields: &[&str]) -> String {
    let mut digest = Sha256::new();
    digest.update(domain);
    for field in fields {
        digest.update(
            u64::try_from(field.len())
                .expect("sign-event fields fit into u64")
                .to_be_bytes(),
        );
        digest.update(field.as_bytes());
    }
    format!("{:x}", digest.finalize())
}

fn digest(value: &[u8]) -> String {
    format!("{:x}", Sha256::digest(value))
}

fn protocol_drift(message: &'static str) -> ProviderError {
    ProviderError::new(ProviderErrorKind::ProtocolDrift, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use asterism_domain::{ProviderAccountId, ProviderId, SecretId};

    const BOOTSTRAP_FIXTURE: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fixtures/providers/chaoxing/sign/webim-me.html"
    ));
    const SIGN_EVENT_FIXTURE: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fixtures/providers/chaoxing/sign/webim-sign-event.json"
    ));
    const NON_SIGN_EVENT_FIXTURE: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fixtures/providers/chaoxing/sign/webim-non-sign-event.json"
    ));

    struct FixtureTransport;

    #[async_trait]
    impl ChaoxingSignEventReadTransport for FixtureTransport {
        async fn fetch_sign_event_bootstrap(
            &self,
            context: &ProviderContext,
        ) -> ProviderResult<ChaoxingSignEventBootstrapDocument> {
            ChaoxingSignEventBootstrapDocument::for_context(context, BOOTSTRAP_FIXTURE)
        }
    }

    #[test]
    fn bootstrap_secrets_are_context_bound_and_redacted() {
        let document =
            ChaoxingSignEventBootstrapDocument::for_context(&context(), BOOTSTRAP_FIXTURE).unwrap();
        let bootstrap = parse_sign_event_bootstrap(&document).unwrap();
        assert_eq!(bootstrap.binding_digest().len(), 64);
        assert_eq!(bootstrap.credential_digest().len(), 64);
        assert_eq!(bootstrap.credential_count(), 3);
        let debug = format!("{bootstrap:?}");
        assert!(!debug.contains("PRIVATE_IM_TOKEN"));
        assert!(!debug.contains("PRIVATE_STUDENT_NAME"));
    }

    #[tokio::test]
    async fn facade_requires_authentication_before_bootstrap_transport() {
        let reader = ChaoxingSignEventRead::try_new(Arc::new(FixtureTransport)).unwrap();
        let bootstrap = reader.bootstrap(&context()).await.unwrap();
        assert_eq!(bootstrap.credential_count(), 3);
        assert_eq!(reader.metadata(), &development_metadata().unwrap());

        let mut unauthenticated = context();
        unauthenticated.credential_refs.clear();
        let error = reader.bootstrap(&unauthenticated).await.unwrap_err();
        assert_eq!(error.kind, ProviderErrorKind::Authentication);
    }

    #[test]
    fn sign_event_retains_only_rebindable_identity() {
        let bootstrap = bootstrap();
        let document =
            ChaoxingSignEventDocument::for_bootstrap(&bootstrap, SIGN_EVENT_FIXTURE).unwrap();
        let event = parse_sign_event(&document).unwrap().unwrap();
        assert_eq!(event.remote_id(), "sign:event:1001:2001:7001");
        assert_eq!(event.activity_id(), "7001");
        assert_eq!(event.course_id(), "1001");
        assert_eq!(event.class_id(), "2001");
        assert_eq!(event.course_name(), "Calculus");
        assert_eq!(event.binding_digest(), bootstrap.binding_digest());
        assert_eq!(event.credential_digest(), bootstrap.credential_digest());
        assert_eq!(event.message_digest().len(), 64);
    }

    #[test]
    fn non_sign_messages_are_ignored_and_malformed_sign_messages_fail() {
        let bootstrap = bootstrap();
        let document =
            ChaoxingSignEventDocument::for_bootstrap(&bootstrap, NON_SIGN_EVENT_FIXTURE).unwrap();
        assert_eq!(parse_sign_event(&document).unwrap(), None);

        for changed in [
            SIGN_EVENT_FIXTURE.replace("\"aid\": 7001", "\"aid\": \"bad\""),
            SIGN_EVENT_FIXTURE.replace("\"classid\": 2001", "\"classid\": -1"),
            SIGN_EVENT_FIXTURE.replace("\"courseInfo\"", "\"missingCourseInfo\""),
        ] {
            let document = ChaoxingSignEventDocument::for_bootstrap(&bootstrap, changed).unwrap();
            let error = parse_sign_event(&document).unwrap_err();
            assert_eq!(error.kind, ProviderErrorKind::ProtocolDrift);
        }
    }

    #[test]
    fn bootstrap_structure_and_document_bounds_fail_closed() {
        for html in [
            "<div id='myTuid'>9001</div><div id='myName'>name</div>",
            "<div id='myToken'>one</div><div id='myToken'>two</div><div id='myTuid'>9001</div><div id='myName'>name</div>",
            "<div id='myToken'>token</div><div id='myTuid'>bad</div><div id='myName'>name</div>",
        ] {
            let document =
                ChaoxingSignEventBootstrapDocument::for_context(&context(), html).unwrap();
            let error = parse_sign_event_bootstrap(&document).unwrap_err();
            assert_eq!(error.kind, ProviderErrorKind::ProtocolDrift);
        }
        let error = ChaoxingSignEventBootstrapDocument::for_context(
            &context(),
            "x".repeat(MAX_BOOTSTRAP_DOCUMENT_BYTES + 1),
        )
        .unwrap_err();
        assert_eq!(error.kind, ProviderErrorKind::InvalidResponse);
    }

    fn bootstrap() -> ChaoxingSignEventBootstrap {
        let document =
            ChaoxingSignEventBootstrapDocument::for_context(&context(), BOOTSTRAP_FIXTURE).unwrap();
        parse_sign_event_bootstrap(&document).unwrap()
    }

    fn context() -> ProviderContext {
        ProviderContext {
            provider_id: ProviderId::new("chaoxing").unwrap(),
            account_id: ProviderAccountId::new(),
            credential_refs: vec![SecretId::new()],
            correlation_id: "chaoxing-sign-event-test".to_owned(),
        }
    }
}
