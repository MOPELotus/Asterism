use std::{collections::BTreeSet, fmt};

use asterism_domain::ProviderAccountId;
use asterism_provider_api::{
    ExecutionMutationIssue, ExecutionMutationPlan, ExecutionMutationPlanStep,
    ExecutionMutationReceipt, ExecutionMutationRecoveryRecord, ExecutionMutationVerification,
    ProviderContext, ProviderCourseEnrollmentDraft, ProviderError, ProviderErrorKind,
    ProviderExecutionPlanArtifact, ProviderResult, RemoteCourse,
};
use asterism_secrets::{SecretString, SecretValue};
use async_trait::async_trait;
use reqwest::Url;
use scraper::{Html, Selector};
use serde::de::{IgnoredAny, MapAccess, Visitor};
use serde::{Deserialize, Deserializer};
use sha2::{Digest, Sha256};
use zeroize::Zeroize;

const PREVIEW_BASE: &str = "https://mooc1.chaoxing.com/addcourse/pcqrcodemiddleview";
const INVITE_API_BASE: &str = "https://i.chaoxing.com/base/getInviteCode";
const JOIN_BASE: &str =
    "https://mooc1.chaoxing.com/mooc-ans/teachingClassPhoneManage/phone/participateCls";
const MAX_INVITE_CODE_BYTES: usize = 64;
const MAX_COMPONENT_BYTES: usize = 128;
const MAX_ENC_BYTES: usize = 512;
const MAX_PREVIEW_DOCUMENT_BYTES: usize = 512 * 1_024;
const MAX_RECEIPT_BYTES: usize = 16 * 1_024;
pub(crate) const COURSE_ENROLLMENT_ARTIFACT_TYPE: &str = "chaoxing.course-enrollment.v1";
pub(crate) const COURSE_ENROLLMENT_OPERATION_TYPE: &str =
    "chaoxing.course-enrollment.participate-cls";
const COURSE_ENROLLMENT_REQUEST_PREFIX: &[u8] = b"GET\0";

/// An immutable, transport-free preparation for the donor-observed invite
/// preview. The invite code and complete query remain redacted.
pub struct ChaoxingCourseInvitePreviewPreparation {
    invite_code: SecretString,
    query: SecretString,
    request_digest: String,
}

impl ChaoxingCourseInvitePreviewPreparation {
    /// Freezes the exact `pcqrcodemiddleview` request without issuing it.
    ///
    /// # Errors
    ///
    /// Rejects empty, padded, non-decimal or oversized invite codes.
    pub fn try_prepare(mut invite_code: String) -> ProviderResult<Self> {
        if !valid_decimal(&invite_code, MAX_INVITE_CODE_BYTES) {
            invite_code.zeroize();
            return Err(invalid_response("Chaoxing Course invite code is invalid"));
        }
        let query = canonical_query(
            PREVIEW_BASE,
            &[("inviteCode", invite_code.as_str()), ("checkEnc", "1")],
            "Chaoxing Course invite preview route is invalid",
        )?;
        let request_digest = digest_parts(
            b"asterism.chaoxing.course-invite-preview.v1\0",
            &[invite_code.as_bytes(), query.as_bytes()],
        );
        Ok(Self {
            invite_code: SecretString::new(invite_code),
            query: SecretString::new(query),
            request_digest,
        })
    }

    pub const fn route(&self) -> &'static str {
        PREVIEW_BASE
    }

    pub fn request_digest(&self) -> &str {
        &self.request_digest
    }

    pub fn prepared_request_count(&self) -> usize {
        usize::from(!self.query.expose_secret().is_empty())
    }

    pub(crate) fn query(&self) -> &str {
        self.query.expose_secret()
    }
}

impl fmt::Debug for ChaoxingCourseInvitePreviewPreparation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ChaoxingCourseInvitePreviewPreparation")
            .field("invite_code", &"[REDACTED]")
            .field("query", &"[REDACTED]")
            .field("request_digest", &self.request_digest)
            .finish()
    }
}

/// Immutable fallback preparation for the donor-observed i-site invite lookup.
/// The form body remains redacted and no request is issued.
pub struct ChaoxingCourseInviteApiPreparation {
    invite_code: SecretString,
    form_body: SecretString,
    request_digest: String,
}

impl ChaoxingCourseInviteApiPreparation {
    /// Freezes `base/getInviteCode` with a caller-supplied current timestamp.
    ///
    /// # Errors
    ///
    /// Rejects malformed invite codes and zero timestamps.
    pub fn try_prepare(mut invite_code: String, timestamp_millis: u64) -> ProviderResult<Self> {
        if !valid_decimal(&invite_code, MAX_INVITE_CODE_BYTES) || timestamp_millis == 0 {
            invite_code.zeroize();
            return Err(invalid_response(
                "Chaoxing Course invite API input is invalid",
            ));
        }
        let timestamp = timestamp_millis.to_string();
        let form_body = canonical_query(
            INVITE_API_BASE,
            &[("invitecode", invite_code.as_str()), ("_t", &timestamp)],
            "Chaoxing Course invite API route is invalid",
        )?;
        let request_digest = digest_parts(
            b"asterism.chaoxing.course-invite-api.v1\0",
            &[
                invite_code.as_bytes(),
                timestamp.as_bytes(),
                form_body.as_bytes(),
            ],
        );
        Ok(Self {
            invite_code: SecretString::new(invite_code),
            form_body: SecretString::new(form_body),
            request_digest,
        })
    }

    pub const fn route(&self) -> &'static str {
        INVITE_API_BASE
    }

    pub fn request_digest(&self) -> &str {
        &self.request_digest
    }

    pub fn prepared_request_count(&self) -> usize {
        usize::from(!self.form_body.expose_secret().is_empty())
    }

    pub(crate) fn form_body(&self) -> &str {
        self.form_body.expose_secret()
    }
}

impl fmt::Debug for ChaoxingCourseInviteApiPreparation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ChaoxingCourseInviteApiPreparation")
            .field("invite_code", &"[REDACTED]")
            .field("form_body", &"[REDACTED]")
            .field("request_digest", &self.request_digest)
            .finish()
    }
}

/// Bounded JSON response tied to the exact i-site fallback request.
pub struct ChaoxingCourseInviteApiDocument {
    request_digest: String,
    invite_code: SecretString,
    response: String,
}

impl ChaoxingCourseInviteApiDocument {
    /// # Errors
    ///
    /// Rejects empty or oversized response bodies.
    pub fn for_preparation(
        preparation: &ChaoxingCourseInviteApiPreparation,
        mut response: String,
    ) -> ProviderResult<Self> {
        if response.is_empty() || response.len() > MAX_RECEIPT_BYTES {
            response.zeroize();
            return Err(invalid_response(
                "Chaoxing Course invite API response is empty or oversized",
            ));
        }
        Ok(Self {
            request_digest: preparation.request_digest.clone(),
            invite_code: SecretString::new(preparation.invite_code.expose_secret().to_owned()),
            response,
        })
    }
}

impl fmt::Debug for ChaoxingCourseInviteApiDocument {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ChaoxingCourseInviteApiDocument")
            .field("request_digest", &self.request_digest)
            .field("invite_code", &"[REDACTED]")
            .field("response", &"[REDACTED]")
            .finish()
    }
}

impl Drop for ChaoxingCourseInviteApiDocument {
    fn drop(&mut self) {
        self.response.zeroize();
    }
}

/// Redacted, exact same-origin middle-page handoff returned by the i-site API.
pub struct ChaoxingCourseInvitePreviewRedirect {
    invite_code: SecretString,
    query: SecretString,
    request_digest: String,
}

impl ChaoxingCourseInvitePreviewRedirect {
    pub const fn route(&self) -> &'static str {
        PREVIEW_BASE
    }

    pub fn request_digest(&self) -> &str {
        &self.request_digest
    }

    pub fn prepared_request_count(&self) -> usize {
        usize::from(!self.query.expose_secret().is_empty())
    }

    pub(crate) fn query(&self) -> &str {
        self.query.expose_secret()
    }
}

impl fmt::Debug for ChaoxingCourseInvitePreviewRedirect {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ChaoxingCourseInvitePreviewRedirect")
            .field("invite_code", &"[REDACTED]")
            .field("query", &"[REDACTED]")
            .field("request_digest", &self.request_digest)
            .finish()
    }
}

/// Parses the duplicate-safe `getInviteCode` success shape and validates its
/// exact Chaoxing middle-page handoff. Donor-observed plaintext URLs are
/// upgraded to HTTPS before any future transport may consume them.
///
/// # Errors
///
/// Rejected invites return `RemoteChanged`; malformed, duplicate, application
/// invite or foreign redirect shapes fail closed.
pub fn parse_course_invite_api_redirect(
    document: &ChaoxingCourseInviteApiDocument,
) -> ProviderResult<ChaoxingCourseInvitePreviewRedirect> {
    let wire: InviteApiWire = serde_json::from_str(&document.response)
        .map_err(|_| protocol_drift("Chaoxing Course invite API JSON changed"))?;
    if wire.status != Some(true) {
        return Err(ProviderError::new(
            ProviderErrorKind::RemoteChanged,
            "Chaoxing Course invite was rejected or expired",
        ));
    }
    if wire.flag != Some(0) {
        return Err(ProviderError::new(
            ProviderErrorKind::UnsupportedTask,
            "Chaoxing invite resolves to an unsupported application family",
        ));
    }
    let mut raw_url = wire
        .url
        .ok_or_else(|| protocol_drift("Chaoxing Course invite API redirect is missing"))?;
    let result = (|| {
        let mut url = Url::parse(&raw_url)
            .map_err(|_| protocol_drift("Chaoxing Course invite API redirect is invalid"))?;
        if url.scheme() == "http" {
            url.set_scheme("https")
                .map_err(|()| protocol_drift("Chaoxing Course invite redirect cannot use HTTPS"))?;
        }
        if url.scheme() != "https"
            || url.host_str() != Some("mooc1.chaoxing.com")
            || url.port().is_some()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
            || url.path() != "/addcourse/pcqrcodemiddleview"
        {
            return Err(protocol_drift(
                "Chaoxing Course invite API redirect is foreign",
            ));
        }
        let mut invite = None;
        let mut check_enc = None;
        for (key, value) in url.query_pairs() {
            match key.as_ref() {
                "inviteCode" => set_unique(&mut invite, value.into_owned())?,
                "checkEnc" => set_unique(&mut check_enc, value.into_owned())?,
                _ => {}
            }
        }
        if invite.as_deref() != Some(document.invite_code.expose_secret())
            || check_enc.as_deref() != Some("1")
        {
            return Err(ProviderError::new(
                ProviderErrorKind::RemoteChanged,
                "Chaoxing Course invite API redirect binding changed",
            ));
        }
        let query = url
            .query()
            .ok_or_else(|| protocol_drift("Chaoxing Course invite redirect query is missing"))?
            .to_owned();
        if query.len() > 2_048 {
            return Err(protocol_drift(
                "Chaoxing Course invite redirect query is oversized",
            ));
        }
        let request_digest = digest_parts(
            b"asterism.chaoxing.course-invite-api-redirect.v1\0",
            &[
                document.request_digest.as_bytes(),
                document.response.as_bytes(),
                query.as_bytes(),
            ],
        );
        Ok(ChaoxingCourseInvitePreviewRedirect {
            invite_code: SecretString::new(document.invite_code.expose_secret().to_owned()),
            query: SecretString::new(query),
            request_digest,
        })
    })();
    raw_url.zeroize();
    result
}

/// One bounded invite middle-page document tied to its exact read request.
pub struct ChaoxingCourseInvitePreviewDocument {
    preparation_digest: String,
    invite_code: SecretString,
    source_url: SecretString,
    document: String,
}

impl ChaoxingCourseInvitePreviewDocument {
    /// # Errors
    ///
    /// Rejects empty or oversized response documents.
    pub fn for_preparation(
        preparation: &ChaoxingCourseInvitePreviewPreparation,
        document: String,
    ) -> ProviderResult<Self> {
        Self::for_preparation_at(
            preparation,
            format!("{}?{}", preparation.route(), preparation.query()),
            document,
        )
    }

    pub(crate) fn for_preparation_at(
        preparation: &ChaoxingCourseInvitePreviewPreparation,
        source_url: String,
        document: String,
    ) -> ProviderResult<Self> {
        Self::try_new(
            preparation.request_digest.clone(),
            preparation.invite_code.expose_secret(),
            source_url,
            document,
        )
    }

    /// Binds a middle-page response to the exact validated i-site redirect.
    ///
    /// # Errors
    ///
    /// Rejects empty or oversized response documents.
    pub fn for_redirect(
        redirect: &ChaoxingCourseInvitePreviewRedirect,
        document: String,
    ) -> ProviderResult<Self> {
        Self::for_redirect_at(
            redirect,
            format!("{}?{}", redirect.route(), redirect.query()),
            document,
        )
    }

    pub(crate) fn for_redirect_at(
        redirect: &ChaoxingCourseInvitePreviewRedirect,
        source_url: String,
        document: String,
    ) -> ProviderResult<Self> {
        Self::try_new(
            redirect.request_digest.clone(),
            redirect.invite_code.expose_secret(),
            source_url,
            document,
        )
    }

    fn try_new(
        preparation_digest: String,
        invite_code: &str,
        mut source_url: String,
        mut document: String,
    ) -> ProviderResult<Self> {
        if document.is_empty() || document.len() > MAX_PREVIEW_DOCUMENT_BYTES {
            source_url.zeroize();
            document.zeroize();
            return Err(invalid_response(
                "Chaoxing Course invite preview document is empty or oversized",
            ));
        }
        if let Err(error) = validate_course_invite_source_url(&source_url, invite_code) {
            source_url.zeroize();
            document.zeroize();
            return Err(error);
        }
        Ok(Self {
            preparation_digest,
            invite_code: SecretString::new(invite_code.to_owned()),
            source_url: SecretString::new(source_url),
            document,
        })
    }
}

impl fmt::Debug for ChaoxingCourseInvitePreviewDocument {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ChaoxingCourseInvitePreviewDocument")
            .field("preparation_digest", &self.preparation_digest)
            .field("invite_code", &"[REDACTED]")
            .field("source_url", &"[REDACTED]")
            .field("document", &"[REDACTED]")
            .finish()
    }
}

impl Drop for ChaoxingCourseInvitePreviewDocument {
    fn drop(&mut self) {
        self.document.zeroize();
    }
}

/// Exact, credential-free Course/Class identity plus redacted route material
/// recovered from a single invite middle page.
pub struct ChaoxingCourseInvitePreview {
    course_id: String,
    class_id: String,
    preview_request_digest: String,
    preview_document_digest: String,
    invite_code: SecretString,
    add_class_enc: SecretString,
    add_class_timestamp: SecretString,
}

impl ChaoxingCourseInvitePreview {
    pub fn course_id(&self) -> &str {
        &self.course_id
    }

    pub fn class_id(&self) -> &str {
        &self.class_id
    }

    pub fn preview_request_digest(&self) -> &str {
        &self.preview_request_digest
    }

    pub fn preview_document_digest(&self) -> &str {
        &self.preview_document_digest
    }
}

impl fmt::Debug for ChaoxingCourseInvitePreview {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ChaoxingCourseInvitePreview")
            .field("course_id", &self.course_id)
            .field("class_id", &self.class_id)
            .field("preview_request_digest", &self.preview_request_digest)
            .field("preview_document_digest", &self.preview_document_digest)
            .field("invite_code", &"[REDACTED]")
            .field("add_class_enc", &"[REDACTED]")
            .field("add_class_timestamp", &"[REDACTED]")
            .finish()
    }
}

/// Parses the exact hidden-input and numeric script/attribute subsets observed
/// in the current donor.
///
/// # Errors
///
/// Missing, duplicate, padded, conflicting or malformed Course/Class/enc/time
/// facts fail closed. Script fallback is limited to exact scalar assignment
/// statements and attribute fallback to exact route-named attributes; arbitrary
/// page text is never searched. Response-URL fallback is accepted only from the
/// transport-preserved, strictly bound final middle-page URL.
pub fn parse_course_invite_preview(
    document: &ChaoxingCourseInvitePreviewDocument,
) -> ProviderResult<ChaoxingCourseInvitePreview> {
    let html = Html::parse_document(&document.document);
    let selector = Selector::parse("input[name], input[id]").map_err(|_| {
        ProviderError::new(
            ProviderErrorKind::Internal,
            "Chaoxing Course invite selector is invalid",
        )
    })?;
    let mut fields = html
        .select(&selector)
        .flat_map(|node| {
            let value = node.value().attr("value").map(str::to_owned);
            let name = node.value().attr("name");
            let id = node.value().attr("id");
            let mut identities = Vec::with_capacity(2);
            if let (Some(name), Some(value)) = (name, value.as_ref()) {
                identities.push((name.to_owned(), value.clone()));
            }
            if let (Some(id), Some(value)) = (id, value.as_ref())
                && name.is_none_or(|name| !name.eq_ignore_ascii_case(id))
            {
                identities.push((id.to_owned(), value.clone()));
            }
            identities
        })
        .collect::<Vec<_>>();
    fields.extend(extract_invite_route_attributes(&html)?);
    fields.extend(extract_invite_route_script_assignments(&html)?);
    let source_fields = course_invite_source_url_fields(
        document.source_url.expose_secret(),
        document.invite_code.expose_secret(),
    )?;
    append_missing_route_aliases(&mut fields, &source_fields, &["courseId", "courseid"]);
    append_missing_route_aliases(
        &mut fields,
        &source_fields,
        &["clazzId", "classId", "clazzid"],
    );
    append_missing_route_aliases(&mut fields, &source_fields, &["addclzenc"]);
    let course_id = unique_alias(&fields, &["courseId", "courseid"], "Course")?;
    let class_id = unique_alias(&fields, &["clazzId", "classId", "clazzid"], "Class")?;
    let add_class_enc = unique_alias(&fields, &["addclzenc"], "enc")?;
    let add_class_timestamp =
        unique_alias(&fields, &["addclztimeStamp", "timeStamp"], "timestamp")?;
    if !valid_decimal(&course_id, MAX_COMPONENT_BYTES)
        || !valid_decimal(&class_id, MAX_COMPONENT_BYTES)
        || !valid_route_material(&add_class_enc, MAX_ENC_BYTES)
        || !valid_decimal(&add_class_timestamp, MAX_COMPONENT_BYTES)
    {
        return Err(protocol_drift(
            "Chaoxing Course invite preview contains malformed route material",
        ));
    }
    let preview_document_digest = digest_parts(
        b"asterism.chaoxing.course-invite-preview-document.v2\0",
        &[
            document.source_url.expose_secret().as_bytes(),
            document.document.as_bytes(),
        ],
    );
    Ok(ChaoxingCourseInvitePreview {
        course_id,
        class_id,
        preview_request_digest: document.preparation_digest.clone(),
        preview_document_digest,
        invite_code: SecretString::new(document.invite_code.expose_secret().to_owned()),
        add_class_enc: SecretString::new(add_class_enc),
        add_class_timestamp: SecretString::new(add_class_timestamp),
    })
}

fn append_missing_route_aliases(
    fields: &mut Vec<(String, String)>,
    source_fields: &[(String, String)],
    aliases: &[&str],
) {
    if fields
        .iter()
        .any(|(name, _)| aliases.iter().any(|alias| name.eq_ignore_ascii_case(alias)))
    {
        return;
    }
    fields.extend(
        source_fields
            .iter()
            .filter(|(name, _)| aliases.iter().any(|alias| name.eq_ignore_ascii_case(alias)))
            .cloned(),
    );
}

fn validate_course_invite_source_url(source_url: &str, invite_code: &str) -> ProviderResult<()> {
    course_invite_source_url_fields(source_url, invite_code).map(|_| ())
}

fn course_invite_source_url_fields(
    source_url: &str,
    invite_code: &str,
) -> ProviderResult<Vec<(String, String)>> {
    let url = Url::parse(source_url)
        .map_err(|_| protocol_drift("Chaoxing Course invite source URL is invalid"))?;
    if url.as_str() != source_url
        || source_url.len() > 2_048
        || url.scheme() != "https"
        || url.host_str() != Some("mooc1.chaoxing.com")
        || url.port().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || url.path() != "/addcourse/pcqrcodemiddleview"
    {
        return Err(protocol_drift(
            "Chaoxing Course invite source URL route changed",
        ));
    }
    let mut seen = BTreeSet::new();
    let mut bound_invite = None;
    let mut check_enc = None;
    let mut fields = Vec::new();
    for (key, value) in url.query_pairs() {
        let semantic = if key == "inviteCode" {
            bound_invite = Some(value.as_ref().to_owned());
            "invite"
        } else if key == "checkEnc" {
            check_enc = Some(value.as_ref().to_owned());
            "check"
        } else if ["courseId", "courseid"]
            .into_iter()
            .any(|alias| key.eq_ignore_ascii_case(alias))
        {
            if !valid_decimal(value.as_ref(), MAX_COMPONENT_BYTES) {
                return Err(protocol_drift(
                    "Chaoxing Course invite source URL Course identity is malformed",
                ));
            }
            fields.push((key.into_owned(), value.into_owned()));
            "course"
        } else if ["clazzId", "classId", "clazzid"]
            .into_iter()
            .any(|alias| key.eq_ignore_ascii_case(alias))
        {
            if !valid_decimal(value.as_ref(), MAX_COMPONENT_BYTES) {
                return Err(protocol_drift(
                    "Chaoxing Course invite source URL Class identity is malformed",
                ));
            }
            fields.push((key.into_owned(), value.into_owned()));
            "class"
        } else if key == "enc" {
            if !valid_route_material(value.as_ref(), MAX_ENC_BYTES) {
                return Err(protocol_drift(
                    "Chaoxing Course invite source URL enc is malformed",
                ));
            }
            fields.push(("addclzenc".to_owned(), value.into_owned()));
            "enc"
        } else {
            return Err(protocol_drift(
                "Chaoxing Course invite source URL contains an unknown field",
            ));
        };
        if !seen.insert(semantic) {
            return Err(protocol_drift(
                "Chaoxing Course invite source URL contains duplicate binding fields",
            ));
        }
    }
    if bound_invite.as_deref() != Some(invite_code) || check_enc.as_deref() != Some("1") {
        return Err(ProviderError::new(
            ProviderErrorKind::RemoteChanged,
            "Chaoxing Course invite source URL binding changed",
        ));
    }
    Ok(fields)
}

fn extract_invite_route_attributes(html: &Html) -> ProviderResult<Vec<(String, String)>> {
    let selector = Selector::parse("*").map_err(|_| {
        ProviderError::new(
            ProviderErrorKind::Internal,
            "Chaoxing Course invite attribute selector is invalid",
        )
    })?;
    Ok(html
        .select(&selector)
        .flat_map(|node| node.value().attrs())
        .filter(|(name, value)| invite_route_alias(name) && valid_donor_numeric_fallback(value))
        .map(|(name, value)| (name.to_owned(), value.to_owned()))
        .collect())
}

fn extract_invite_route_script_assignments(html: &Html) -> ProviderResult<Vec<(String, String)>> {
    let selector = Selector::parse("script:not([src])").map_err(|_| {
        ProviderError::new(
            ProviderErrorKind::Internal,
            "Chaoxing Course invite script selector is invalid",
        )
    })?;
    Ok(html
        .select(&selector)
        .flat_map(|script| script.text())
        .flat_map(|script| script.split([';', '\n', '\r']))
        .filter_map(exact_invite_route_assignment)
        .map(|(name, value)| (name.to_owned(), value.to_owned()))
        .collect())
}

fn exact_invite_route_assignment(statement: &str) -> Option<(&str, &str)> {
    let statement = statement.trim();
    let statement = ["var ", "let ", "const "]
        .into_iter()
        .find_map(|prefix| statement.strip_prefix(prefix))
        .unwrap_or(statement)
        .trim();
    let separator = statement
        .find('=')
        .into_iter()
        .chain(statement.find(':'))
        .min()?;
    let name = strip_matching_quotes(statement[..separator].trim())?;
    let value = strip_matching_quotes(statement[separator + 1..].trim())?;
    (invite_route_alias(name) && valid_donor_numeric_fallback(value)).then_some((name, value))
}

fn strip_matching_quotes(value: &str) -> Option<&str> {
    match (value.as_bytes().first(), value.as_bytes().last()) {
        (Some(first), Some(last)) if first == last && matches!(first, b'\'' | b'"') => {
            value.get(1..value.len().checked_sub(1)?)
        }
        (Some(_), Some(_)) => Some(value),
        _ => None,
    }
}

fn invite_route_alias(name: &str) -> bool {
    [
        "courseId",
        "courseid",
        "clazzId",
        "classId",
        "clazzid",
        "addclzenc",
        "addclztimeStamp",
        "timeStamp",
    ]
    .into_iter()
    .any(|alias| name.eq_ignore_ascii_case(alias))
}

fn valid_donor_numeric_fallback(value: &str) -> bool {
    value.len() >= 5 && valid_decimal(value, MAX_ENC_BYTES)
}

/// Immutable, unregistered preparation for the donor-observed enrollment GET.
/// It is not durable issue authority and exposes no plaintext query.
pub struct ChaoxingCourseJoinPreparation {
    course_id: String,
    class_id: String,
    preview_request_digest: String,
    preview_document_digest: String,
    query: SecretString,
    request_digest: String,
    request_digest_bytes: [u8; 32],
}

impl ChaoxingCourseJoinPreparation {
    /// Freezes `participateCls` against one exact invite preview.
    ///
    /// # Errors
    ///
    /// Returns an internal error only when the compile-time route is invalid.
    pub fn try_prepare(preview: &ChaoxingCourseInvitePreview) -> ProviderResult<Self> {
        let query = canonical_query(
            JOIN_BASE,
            &[
                ("courseId", preview.course_id.as_str()),
                ("classId", preview.class_id.as_str()),
                ("enc", preview.add_class_enc.expose_secret()),
                ("timeStamp", preview.add_class_timestamp.expose_secret()),
                ("inviteCode", preview.invite_code.expose_secret()),
            ],
            "Chaoxing Course join route is invalid",
        )?;
        let request_digest_bytes = digest_parts_bytes(
            b"asterism.chaoxing.course-join-preparation.v1\0",
            &[
                preview.preview_request_digest.as_bytes(),
                preview.preview_document_digest.as_bytes(),
                query.as_bytes(),
            ],
        );
        let request_digest = encode_digest(request_digest_bytes);
        Ok(Self {
            course_id: preview.course_id.clone(),
            class_id: preview.class_id.clone(),
            preview_request_digest: preview.preview_request_digest.clone(),
            preview_document_digest: preview.preview_document_digest.clone(),
            query: SecretString::new(query),
            request_digest,
            request_digest_bytes,
        })
    }

    pub const fn route(&self) -> &'static str {
        JOIN_BASE
    }

    pub fn course_id(&self) -> &str {
        &self.course_id
    }

    pub fn class_id(&self) -> &str {
        &self.class_id
    }

    pub fn request_digest(&self) -> &str {
        &self.request_digest
    }

    pub(crate) const fn request_digest_bytes(&self) -> [u8; 32] {
        self.request_digest_bytes
    }

    pub fn prepared_request_count(&self) -> usize {
        usize::from(!self.query.expose_secret().is_empty())
    }

    pub(crate) fn query(&self) -> &str {
        self.query.expose_secret()
    }

    pub(crate) fn frozen_request(&self) -> SecretValue {
        let mut request = Vec::with_capacity(
            COURSE_ENROLLMENT_REQUEST_PREFIX.len() + self.route().len() + 1 + self.query().len(),
        );
        request.extend_from_slice(COURSE_ENROLLMENT_REQUEST_PREFIX);
        request.extend_from_slice(self.route().as_bytes());
        request.push(b'?');
        request.extend_from_slice(self.query().as_bytes());
        SecretValue::new(request)
    }

    /// Performs the independent readback boundary over one fresh Course list.
    ///
    /// # Errors
    ///
    /// Duplicate exact Course/Class identities are protocol drift.
    pub fn verify_fresh_membership(&self, courses: &[RemoteCourse]) -> ProviderResult<bool> {
        let expected = format!("course:{}:{}", self.course_id, self.class_id);
        let matches = courses
            .iter()
            .filter(|course| course.remote_id == expected)
            .count();
        match matches {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(protocol_drift(
                "Chaoxing Course join readback returned duplicate membership",
            )),
        }
    }
}

impl fmt::Debug for ChaoxingCourseJoinPreparation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ChaoxingCourseJoinPreparation")
            .field("course_id", &self.course_id)
            .field("class_id", &self.class_id)
            .field("preview_request_digest", &self.preview_request_digest)
            .field("preview_document_digest", &self.preview_document_digest)
            .field("query", &"[REDACTED]")
            .field("request_digest", &self.request_digest)
            .field("request_digest_bytes", &"[HASHED]")
            .finish()
    }
}

/// Provider-private, credential-free `CourseEnrollment` authority proposal. It
/// binds the exact Provider account and remote Course/Class to the frozen
/// preview and join request. Core cannot persist this artifact through the
/// current Task-owned `ExecutionMutationSink`, so this type grants no send or
/// replay authority by itself.
#[derive(Clone, Eq, PartialEq)]
pub struct ChaoxingCourseEnrollmentCommand {
    provider_account_id: ProviderAccountId,
    course_id: String,
    class_id: String,
    artifact: ProviderExecutionPlanArtifact,
    mutation_plan: ExecutionMutationPlan,
    mutation_issue: ExecutionMutationIssue,
}

impl ChaoxingCourseEnrollmentCommand {
    /// Freezes the Provider-private artifact, one-step mutation plan and exact
    /// issue identity for a future `CourseEnrollment` Core boundary.
    ///
    /// # Errors
    ///
    /// Rejects a foreign Provider context or a context without authenticated
    /// credential references.
    pub fn try_prepare(
        context: &ProviderContext,
        preparation: &ChaoxingCourseJoinPreparation,
    ) -> ProviderResult<Self> {
        if context.provider_id.as_str() != "chaoxing" {
            return Err(invalid_response(
                "Chaoxing Course enrollment received a foreign Provider context",
            ));
        }
        if context.credential_refs.is_empty() {
            return Err(ProviderError::new(
                ProviderErrorKind::Authentication,
                "Chaoxing Course enrollment requires an authenticated account",
            ));
        }
        let account = context.account_id.to_string();
        let account_binding = digest_parts(
            b"asterism.chaoxing.course-enrollment-account.v1\0",
            &[context.provider_id.as_str().as_bytes(), account.as_bytes()],
        );
        let artifact = ProviderExecutionPlanArtifact::try_new(
            context.provider_id.clone(),
            COURSE_ENROLLMENT_ARTIFACT_TYPE,
            serde_json::json!({
                "account_binding_digest": account_binding,
                "remote_course_id": preparation.course_id,
                "remote_class_id": preparation.class_id,
                "preview_request_digest": preparation.preview_request_digest,
                "preview_document_digest": preparation.preview_document_digest,
                "join_request_digest": preparation.request_digest,
                "verification": "fresh_course_inventory",
            }),
        )?;
        let step = ExecutionMutationPlanStep::try_new(
            1,
            COURSE_ENROLLMENT_OPERATION_TYPE,
            Some(preparation.request_digest_bytes),
            Vec::new(),
        )?;
        let mutation_plan = ExecutionMutationPlan::try_new(artifact.artifact_digest(), vec![step])?;
        let mutation_issue = ExecutionMutationIssue::new(
            1,
            COURSE_ENROLLMENT_OPERATION_TYPE,
            preparation.request_digest_bytes,
        )?;
        Ok(Self {
            provider_account_id: context.account_id,
            course_id: preparation.course_id.clone(),
            class_id: preparation.class_id.clone(),
            artifact,
            mutation_plan,
            mutation_issue,
        })
    }

    pub const fn artifact(&self) -> &ProviderExecutionPlanArtifact {
        &self.artifact
    }

    pub const fn mutation_plan(&self) -> &ExecutionMutationPlan {
        &self.mutation_plan
    }

    pub const fn mutation_issue(&self) -> &ExecutionMutationIssue {
        &self.mutation_issue
    }

    pub fn course_id(&self) -> &str {
        &self.course_id
    }

    pub fn class_id(&self) -> &str {
        &self.class_id
    }

    /// Produces exact read-only recovery evidence from one freshly fetched and
    /// parsed Course inventory. Absence is only a negative observation; it
    /// never authorizes replay of an ambiguous issue.
    ///
    /// # Errors
    ///
    /// Rejects account/Provider drift, malformed or duplicate inventory
    /// identities, and duplicate exact memberships.
    pub fn observe_fresh_membership(
        &self,
        context: &ProviderContext,
        courses: &[RemoteCourse],
    ) -> ProviderResult<ChaoxingCourseMembershipObservation> {
        self.validate_context(context)?;
        let expected = format!("course:{}:{}", self.course_id, self.class_id);
        let mut identities = BTreeSet::new();
        for course in courses {
            if course.remote_id.is_empty()
                || course.remote_id.len() > 512
                || course.remote_id.trim() != course.remote_id
                || course.remote_id.chars().any(char::is_control)
                || !identities.insert(course.remote_id.clone())
            {
                return Err(protocol_drift(
                    "Chaoxing Course enrollment readback identities are ambiguous",
                ));
            }
        }
        let present = identities.contains(&expected);
        let observation_digest = course_membership_observation_digest(
            self.artifact.artifact_digest(),
            &identities,
            present,
        );
        Ok(ChaoxingCourseMembershipObservation {
            provider_account_id: self.provider_account_id,
            course_id: self.course_id.clone(),
            class_id: self.class_id.clone(),
            observation_digest,
            present,
        })
    }

    /// Classifies a durable issue/receipt record using only independently fresh
    /// Course inventory. An ambiguous issue with no receipt stays consumed even
    /// when membership is not yet visible.
    ///
    /// # Errors
    ///
    /// Rejects foreign issue, account, Course/Class or persisted verification
    /// evidence.
    pub fn recover(
        &self,
        record: &ExecutionMutationRecoveryRecord,
        observation: &ChaoxingCourseMembershipObservation,
    ) -> ProviderResult<ChaoxingCourseEnrollmentRecoveryOutcome> {
        if record.issue() != &self.mutation_issue
            || observation.provider_account_id != self.provider_account_id
            || observation.course_id != self.course_id
            || observation.class_id != self.class_id
            || record.verification().is_some_and(|verification| {
                verification.ordinal() != self.mutation_issue.ordinal()
                    || verification.observation_digest() != observation.observation_digest
                    || verification.verified() != observation.present
            })
        {
            return Err(invalid_response(
                "Chaoxing Course enrollment recovery evidence is foreign",
            ));
        }
        Ok(match (record.receipt(), observation.present) {
            (None, true) => {
                ChaoxingCourseEnrollmentRecoveryOutcome::AmbiguousVerifiedByFreshInventory
            }
            (None, false) => ChaoxingCourseEnrollmentRecoveryOutcome::AmbiguousLocked,
            (Some(receipt), true) if receipt.accepted() => {
                ChaoxingCourseEnrollmentRecoveryOutcome::AcceptedAndVerified
            }
            (Some(receipt), false) if receipt.accepted() => {
                ChaoxingCourseEnrollmentRecoveryOutcome::AcceptedAwaitingFreshMembership
            }
            (Some(_), true) => {
                ChaoxingCourseEnrollmentRecoveryOutcome::MembershipPresentAfterRejectedReceipt
            }
            (Some(_), false) => ChaoxingCourseEnrollmentRecoveryOutcome::Rejected,
        })
    }

    /// Maps one parsed transport receipt into Core's hash-only receipt shape.
    /// This is usable only after the matching issue has already been persisted.
    ///
    /// # Errors
    ///
    /// Rejects a receipt for any other prepared request.
    pub fn bind_receipt(
        &self,
        receipt: &ChaoxingCourseJoinReceipt,
    ) -> ProviderResult<ExecutionMutationReceipt> {
        if receipt.request_digest != encode_digest(self.mutation_issue.request_digest()) {
            return Err(invalid_response(
                "Chaoxing Course enrollment receipt is foreign",
            ));
        }
        ExecutionMutationReceipt::new(
            self.mutation_issue.ordinal(),
            receipt.response_digest_bytes,
            receipt.kind == ChaoxingCourseJoinReceiptKind::Accepted,
        )
    }

    /// Builds the hash-only verification proposed for a future Course mutation
    /// lifecycle.
    ///
    /// # Errors
    ///
    /// Rejects an observation bound to another account or Course/Class.
    pub fn verification(
        &self,
        observation: &ChaoxingCourseMembershipObservation,
    ) -> ProviderResult<ExecutionMutationVerification> {
        if observation.provider_account_id != self.provider_account_id
            || observation.course_id != self.course_id
            || observation.class_id != self.class_id
        {
            return Err(invalid_response(
                "Chaoxing Course enrollment observation is foreign",
            ));
        }
        ExecutionMutationVerification::new(
            self.mutation_issue.ordinal(),
            observation.observation_digest,
            observation.present,
        )
    }

    fn validate_context(&self, context: &ProviderContext) -> ProviderResult<()> {
        if context.provider_id.as_str() != "chaoxing"
            || context.account_id != self.provider_account_id
            || context.credential_refs.is_empty()
        {
            return Err(invalid_response(
                "Chaoxing Course enrollment readback context changed",
            ));
        }
        Ok(())
    }

    #[allow(
        dead_code,
        reason = "the marker remains unreachable until Core adds CourseEnrollment issue authority"
    )]
    pub(crate) fn bind_issued<'a>(
        &self,
        preparation: &'a ChaoxingCourseJoinPreparation,
        persisted_issue: &ExecutionMutationIssue,
    ) -> ProviderResult<ChaoxingIssuedCourseJoin<'a>> {
        if persisted_issue != &self.mutation_issue
            || preparation.course_id != self.course_id
            || preparation.class_id != self.class_id
            || preparation.request_digest_bytes != self.mutation_issue.request_digest()
        {
            return Err(invalid_response(
                "Chaoxing Course enrollment issue binding is foreign",
            ));
        }
        Ok(ChaoxingIssuedCourseJoin { preparation })
    }
}

impl fmt::Debug for ChaoxingCourseEnrollmentCommand {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ChaoxingCourseEnrollmentCommand")
            .field("provider_account_id", &"[BOUND]")
            .field("course_id", &self.course_id)
            .field("class_id", &self.class_id)
            .field("artifact", &self.artifact)
            .field("mutation_plan", &self.mutation_plan)
            .field("mutation_issue", &self.mutation_issue)
            .finish()
    }
}

#[derive(Clone, Eq, PartialEq)]
pub struct ChaoxingCourseMembershipObservation {
    provider_account_id: ProviderAccountId,
    course_id: String,
    class_id: String,
    observation_digest: [u8; 32],
    present: bool,
}

impl ChaoxingCourseMembershipObservation {
    pub const fn observation_digest(&self) -> [u8; 32] {
        self.observation_digest
    }

    pub const fn present(&self) -> bool {
        self.present
    }
}

impl fmt::Debug for ChaoxingCourseMembershipObservation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ChaoxingCourseMembershipObservation")
            .field("provider_account_id", &"[BOUND]")
            .field("course_id", &self.course_id)
            .field("class_id", &self.class_id)
            .field("observation_digest", &"[HASHED]")
            .field("present", &self.present)
            .finish()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChaoxingCourseEnrollmentRecoveryOutcome {
    AmbiguousVerifiedByFreshInventory,
    AmbiguousLocked,
    AcceptedAndVerified,
    AcceptedAwaitingFreshMembership,
    MembershipPresentAfterRejectedReceipt,
    Rejected,
}

/// Opaque proof that the exact join issue was matched after durable issuance.
/// Only a future `CourseEnrollment` coordinator in this module can construct it.
pub struct ChaoxingIssuedCourseJoin<'a> {
    preparation: &'a ChaoxingCourseJoinPreparation,
}

impl<'a> ChaoxingIssuedCourseJoin<'a> {
    pub(crate) const fn preparation(&self) -> &'a ChaoxingCourseJoinPreparation {
        self.preparation
    }
}

impl fmt::Debug for ChaoxingIssuedCourseJoin<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ChaoxingIssuedCourseJoin")
            .field("course_id", &self.preparation.course_id)
            .field("class_id", &self.preparation.class_id)
            .field("request_digest", &self.preparation.request_digest)
            .finish_non_exhaustive()
    }
}

/// Opaque shared-draft marker constructed only after the exact mutation issue
/// has been durably accepted by Core.
pub struct ChaoxingIssuedCourseEnrollment<'a> {
    draft: &'a ProviderCourseEnrollmentDraft,
}

impl<'a> ChaoxingIssuedCourseEnrollment<'a> {
    pub(crate) fn try_bind(
        draft: &'a ProviderCourseEnrollmentDraft,
        issue: &ExecutionMutationIssue,
    ) -> ProviderResult<Self> {
        validate_shared_course_enrollment_draft(draft)?;
        if issue.ordinal() != 1
            || issue.operation_type() != COURSE_ENROLLMENT_OPERATION_TYPE
            || issue.request_digest() != draft.request_digest()
        {
            return Err(invalid_response(
                "Chaoxing shared Course enrollment issue is foreign",
            ));
        }
        Ok(Self { draft })
    }

    pub(crate) const fn draft(&self) -> &'a ProviderCourseEnrollmentDraft {
        self.draft
    }
}

impl fmt::Debug for ChaoxingIssuedCourseEnrollment<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ChaoxingIssuedCourseEnrollment")
            .field("provider_id", self.draft.provider_id())
            .field("remote_course_id", &self.draft.remote_course_id())
            .field("remote_class_id", &self.draft.remote_class_id())
            .field("request_digest", &"[HASHED]")
            .finish_non_exhaustive()
    }
}

/// Provider-private transport contract. Preview and fallback reads are safe to
/// retry. Join request construction must finish before durable issue; sending
/// consumes that prepared transport exactly once and requires the opaque issued
/// marker. No generic Execution coordinator is exposed while Course identity
/// and ambiguous verification persistence are missing from Core.
#[async_trait]
pub trait ChaoxingCourseEnrollmentTransport: Send + Sync {
    type PreparedJoin: Send;

    async fn fetch_direct_preview(
        &self,
        context: &ProviderContext,
        preparation: &ChaoxingCourseInvitePreviewPreparation,
    ) -> ProviderResult<ChaoxingCourseInvitePreviewDocument>;

    async fn fetch_invite_api(
        &self,
        context: &ProviderContext,
        preparation: &ChaoxingCourseInviteApiPreparation,
    ) -> ProviderResult<ChaoxingCourseInviteApiDocument>;

    async fn fetch_redirect_preview(
        &self,
        context: &ProviderContext,
        redirect: &ChaoxingCourseInvitePreviewRedirect,
    ) -> ProviderResult<ChaoxingCourseInvitePreviewDocument>;

    async fn prepare_join_transport(
        &self,
        context: &ProviderContext,
        preparation: &ChaoxingCourseJoinPreparation,
    ) -> ProviderResult<Self::PreparedJoin>;

    async fn send_issued_join(
        &self,
        prepared: Self::PreparedJoin,
        issued: ChaoxingIssuedCourseJoin<'_>,
    ) -> ProviderResult<ChaoxingCourseJoinReceiptDocument>;

    async fn prepare_frozen_join_transport(
        &self,
        context: &ProviderContext,
        draft: &ProviderCourseEnrollmentDraft,
    ) -> ProviderResult<Self::PreparedJoin>;

    async fn send_issued_frozen_join(
        &self,
        prepared: Self::PreparedJoin,
        issued: ChaoxingIssuedCourseEnrollment<'_>,
    ) -> ProviderResult<ChaoxingCourseJoinReceiptDocument>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChaoxingCourseJoinReceiptKind {
    Accepted,
    Rejected,
}

/// One transport receipt bound to the exact prepared mutation. `Accepted`
/// does not prove membership; only a fresh Course inventory can do that.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChaoxingCourseJoinReceipt {
    request_digest: String,
    response_digest: String,
    response_digest_bytes: [u8; 32],
    kind: ChaoxingCourseJoinReceiptKind,
}

impl ChaoxingCourseJoinReceipt {
    pub fn request_digest(&self) -> &str {
        &self.request_digest
    }

    pub fn response_digest(&self) -> &str {
        &self.response_digest
    }

    pub const fn kind(&self) -> ChaoxingCourseJoinReceiptKind {
        self.kind
    }

    pub(crate) fn bind_shared_draft(
        &self,
        draft: &ProviderCourseEnrollmentDraft,
    ) -> ProviderResult<ExecutionMutationReceipt> {
        validate_shared_course_enrollment_draft(draft)?;
        if self.request_digest != encode_digest(draft.request_digest()) {
            return Err(invalid_response(
                "Chaoxing shared Course enrollment receipt is foreign",
            ));
        }
        ExecutionMutationReceipt::new(
            1,
            self.response_digest_bytes,
            self.kind == ChaoxingCourseJoinReceiptKind::Accepted,
        )
    }
}

/// Bounded response body tied to the exact prepared join request.
pub struct ChaoxingCourseJoinReceiptDocument {
    request_digest: String,
    response: String,
}

impl ChaoxingCourseJoinReceiptDocument {
    /// # Errors
    ///
    /// Rejects empty or oversized response bodies.
    pub fn for_preparation(
        preparation: &ChaoxingCourseJoinPreparation,
        mut response: String,
    ) -> ProviderResult<Self> {
        if response.is_empty() || response.len() > MAX_RECEIPT_BYTES {
            response.zeroize();
            return Err(invalid_response(
                "Chaoxing Course join receipt is empty or oversized",
            ));
        }
        Ok(Self {
            request_digest: preparation.request_digest.clone(),
            response,
        })
    }

    pub(crate) fn for_shared_draft(
        draft: &ProviderCourseEnrollmentDraft,
        mut response: String,
    ) -> ProviderResult<Self> {
        validate_shared_course_enrollment_draft(draft)?;
        if response.is_empty() || response.len() > MAX_RECEIPT_BYTES {
            response.zeroize();
            return Err(invalid_response(
                "Chaoxing Course join receipt is empty or oversized",
            ));
        }
        Ok(Self {
            request_digest: encode_digest(draft.request_digest()),
            response,
        })
    }
}

impl fmt::Debug for ChaoxingCourseJoinReceiptDocument {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ChaoxingCourseJoinReceiptDocument")
            .field("request_digest", &self.request_digest)
            .field("response", &"[REDACTED]")
            .finish()
    }
}

impl Drop for ChaoxingCourseJoinReceiptDocument {
    fn drop(&mut self) {
        self.response.zeroize();
    }
}

/// Parses only the donor-observed numeric `result` receipt. Duplicate result
/// or message fields are rejected rather than resolved by last-write wins.
///
/// # Errors
///
/// Malformed, duplicate or unsupported response shapes are protocol drift.
pub fn parse_course_join_receipt(
    document: &ChaoxingCourseJoinReceiptDocument,
) -> ProviderResult<ChaoxingCourseJoinReceipt> {
    let wire: JoinReceiptWire = serde_json::from_str(&document.response)
        .map_err(|_| protocol_drift("Chaoxing Course join receipt JSON changed"))?;
    let result = wire
        .result
        .ok_or_else(|| protocol_drift("Chaoxing Course join receipt result is missing"))?;
    if let Some(message) = wire.message
        && (message.trim() != message
            || message.len() > 2_048
            || message.chars().any(char::is_control))
    {
        return Err(protocol_drift(
            "Chaoxing Course join receipt message is unsafe",
        ));
    }
    let kind = if result == 1 {
        ChaoxingCourseJoinReceiptKind::Accepted
    } else {
        ChaoxingCourseJoinReceiptKind::Rejected
    };
    let response_digest_bytes = digest_parts_bytes(
        b"asterism.chaoxing.course-join-receipt.v1\0",
        &[document.response.as_bytes()],
    );
    Ok(ChaoxingCourseJoinReceipt {
        request_digest: document.request_digest.clone(),
        response_digest: encode_digest(response_digest_bytes),
        response_digest_bytes,
        kind,
    })
}

struct JoinReceiptWire {
    result: Option<i64>,
    message: Option<String>,
}

struct InviteApiWire {
    status: Option<bool>,
    flag: Option<i64>,
    url: Option<String>,
}

impl<'de> Deserialize<'de> for InviteApiWire {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct InviteApiVisitor;

        impl<'de> Visitor<'de> for InviteApiVisitor {
            type Value = InviteApiWire;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a Chaoxing Course invite API object")
            }

            fn visit_map<M>(self, mut map: M) -> Result<Self::Value, M::Error>
            where
                M: MapAccess<'de>,
            {
                let mut status = None;
                let mut flag = None;
                let mut url = None;
                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "status" => {
                            if status.is_some() {
                                return Err(serde::de::Error::duplicate_field("status"));
                            }
                            status = Some(map.next_value::<bool>()?);
                        }
                        "flag" => {
                            if flag.is_some() {
                                return Err(serde::de::Error::duplicate_field("flag"));
                            }
                            flag = Some(map.next_value::<i64>()?);
                        }
                        "url" => {
                            if url.is_some() {
                                return Err(serde::de::Error::duplicate_field("url"));
                            }
                            url = Some(map.next_value::<String>()?);
                        }
                        _ => {
                            map.next_value::<IgnoredAny>()?;
                        }
                    }
                }
                Ok(InviteApiWire { status, flag, url })
            }
        }

        deserializer.deserialize_map(InviteApiVisitor)
    }
}

impl<'de> Deserialize<'de> for JoinReceiptWire {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct JoinReceiptVisitor;

        impl<'de> Visitor<'de> for JoinReceiptVisitor {
            type Value = JoinReceiptWire;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a Chaoxing Course join receipt object")
            }

            fn visit_map<M>(self, mut map: M) -> Result<Self::Value, M::Error>
            where
                M: MapAccess<'de>,
            {
                let mut result = None;
                let mut message = None;
                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "result" => {
                            if result.is_some() {
                                return Err(serde::de::Error::duplicate_field("result"));
                            }
                            result = Some(map.next_value::<i64>()?);
                        }
                        "errorMsg" | "msg" => {
                            if message.is_some() {
                                return Err(serde::de::Error::duplicate_field("message"));
                            }
                            message = Some(map.next_value::<String>()?);
                        }
                        _ => {
                            map.next_value::<IgnoredAny>()?;
                        }
                    }
                }
                Ok(JoinReceiptWire { result, message })
            }
        }

        deserializer.deserialize_map(JoinReceiptVisitor)
    }
}

fn unique_alias(
    fields: &[(String, String)],
    aliases: &[&str],
    label: &'static str,
) -> ProviderResult<String> {
    let mut values = fields
        .iter()
        .filter(|(name, _)| aliases.iter().any(|alias| name.eq_ignore_ascii_case(alias)))
        .map(|(_, value)| value.as_str());
    let Some(value) = values.next() else {
        return Err(protocol_drift(match label {
            "Course" => "Chaoxing Course invite preview Course identity is missing",
            "Class" => "Chaoxing Course invite preview Class identity is missing",
            "enc" => "Chaoxing Course invite preview enc is missing",
            _ => "Chaoxing Course invite preview timestamp is missing",
        }));
    };
    if values.next().is_some() {
        return Err(protocol_drift(
            "Chaoxing Course invite preview contains duplicate route material",
        ));
    }
    Ok(value.to_owned())
}

fn set_unique(slot: &mut Option<String>, value: String) -> ProviderResult<()> {
    if slot.replace(value).is_some() {
        return Err(protocol_drift(
            "Chaoxing Course invite redirect contains duplicate binding fields",
        ));
    }
    Ok(())
}

pub(crate) fn validate_shared_course_enrollment_draft(
    draft: &ProviderCourseEnrollmentDraft,
) -> ProviderResult<()> {
    shared_course_join_url(draft).map(|_| ())
}

pub(crate) fn shared_course_join_url(draft: &ProviderCourseEnrollmentDraft) -> ProviderResult<Url> {
    if draft.provider_id().as_str() != "chaoxing"
        || draft.artifact_type() != COURSE_ENROLLMENT_ARTIFACT_TYPE
    {
        return Err(invalid_response(
            "Chaoxing shared Course enrollment draft is foreign",
        ));
    }
    let request = draft.request().expose_secret();
    let raw_url = request
        .strip_prefix(COURSE_ENROLLMENT_REQUEST_PREFIX)
        .ok_or_else(|| {
            invalid_response("Chaoxing shared Course enrollment request method changed")
        })?;
    let raw_url = std::str::from_utf8(raw_url)
        .map_err(|_| invalid_response("Chaoxing shared Course enrollment request is not UTF-8"))?;
    let url = Url::parse(raw_url)
        .map_err(|_| invalid_response("Chaoxing shared Course enrollment request is invalid"))?;
    if url.as_str() != raw_url
        || url.scheme() != "https"
        || url.host_str() != Some("mooc1.chaoxing.com")
        || url.port().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || url.path() != "/mooc-ans/teachingClassPhoneManage/phone/participateCls"
    {
        return Err(invalid_response(
            "Chaoxing shared Course enrollment request route changed",
        ));
    }
    let mut course_id = None;
    let mut class_id = None;
    let mut enc = None;
    let mut timestamp = None;
    let mut invitation = None;
    let mut count = 0_usize;
    for (key, value) in url.query_pairs() {
        count += 1;
        let slot = match key.as_ref() {
            "courseId" => &mut course_id,
            "classId" => &mut class_id,
            "enc" => &mut enc,
            "timeStamp" => &mut timestamp,
            "inviteCode" => &mut invitation,
            _ => {
                return Err(invalid_response(
                    "Chaoxing shared Course enrollment request contains an unknown field",
                ));
            }
        };
        if slot.replace(value.into_owned()).is_some() {
            return Err(invalid_response(
                "Chaoxing shared Course enrollment request contains duplicate fields",
            ));
        }
    }
    if count != 5
        || course_id.as_deref() != Some(draft.remote_course_id())
        || class_id.as_deref() != Some(draft.remote_class_id())
        || enc
            .as_deref()
            .is_none_or(|value| !valid_route_material(value, MAX_ENC_BYTES))
        || timestamp
            .as_deref()
            .is_none_or(|value| !valid_decimal(value, MAX_COMPONENT_BYTES))
        || invitation
            .as_deref()
            .is_none_or(|value| !valid_decimal(value, MAX_INVITE_CODE_BYTES))
    {
        return Err(invalid_response(
            "Chaoxing shared Course enrollment request binding changed",
        ));
    }
    Ok(url)
}

fn canonical_query(
    base: &str,
    pairs: &[(&str, &str)],
    message: &'static str,
) -> ProviderResult<String> {
    let mut url =
        Url::parse(base).map_err(|_| ProviderError::new(ProviderErrorKind::Internal, message))?;
    url.query_pairs_mut().extend_pairs(pairs.iter().copied());
    url.query()
        .map(str::to_owned)
        .ok_or_else(|| ProviderError::new(ProviderErrorKind::Internal, message))
}

fn valid_decimal(value: &str, maximum: usize) -> bool {
    !value.is_empty()
        && value.len() <= maximum
        && value.trim() == value
        && value.bytes().all(|byte| byte.is_ascii_digit())
}

fn valid_route_material(value: &str, maximum: usize) -> bool {
    !value.is_empty()
        && value.len() <= maximum
        && value.trim() == value
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn digest_parts(domain: &[u8], parts: &[&[u8]]) -> String {
    encode_digest(digest_parts_bytes(domain, parts))
}

fn digest_parts_bytes(domain: &[u8], parts: &[&[u8]]) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(domain);
    for part in parts {
        digest.update(
            u64::try_from(part.len())
                .expect("Chaoxing Course invite digest fields fit into u64")
                .to_be_bytes(),
        );
        digest.update(part);
    }
    digest.finalize().into()
}

fn encode_digest(digest: [u8; 32]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(64);
    for byte in digest {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    encoded
}

fn course_membership_observation_digest(
    artifact_digest: [u8; 32],
    identities: &BTreeSet<String>,
    present: bool,
) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"asterism.chaoxing.course-membership-observation.v1\0");
    digest.update(artifact_digest);
    digest.update([u8::from(present)]);
    digest.update(
        u32::try_from(identities.len())
            .expect("Chaoxing Course inventory is bounded")
            .to_be_bytes(),
    );
    for identity in identities {
        digest.update(
            u32::try_from(identity.len())
                .expect("Chaoxing Course identity is bounded")
                .to_be_bytes(),
        );
        digest.update(identity.as_bytes());
    }
    digest.finalize().into()
}

fn invalid_response(message: &'static str) -> ProviderError {
    ProviderError::new(ProviderErrorKind::InvalidResponse, message)
}

fn protocol_drift(message: &'static str) -> ProviderError {
    ProviderError::new(ProviderErrorKind::ProtocolDrift, message)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use asterism_domain::{ProviderId, SecretId};
    use serde_json::json;

    use super::*;
    use asterism_provider_api::ProviderRouteContext;

    const PREVIEW: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fixtures/providers/chaoxing/course-invite/middle-view.html"
    ));
    const SCRIPT_PREVIEW: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fixtures/providers/chaoxing/course-invite/middle-view-script.html"
    ));
    const ATTRIBUTE_PREVIEW: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fixtures/providers/chaoxing/course-invite/middle-view-attributes.html"
    ));
    const URL_PREVIEW: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fixtures/providers/chaoxing/course-invite/middle-view-url.html"
    ));
    const ACCEPTED: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fixtures/providers/chaoxing/course-invite/participate-accepted.json"
    ));
    const REJECTED: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fixtures/providers/chaoxing/course-invite/participate-rejected.json"
    ));
    const API_SUCCESS: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fixtures/providers/chaoxing/course-invite/invite-api-success.json"
    ));

    #[test]
    fn freezes_preview_and_join_without_exposing_dynamic_material() {
        let preview_request =
            ChaoxingCourseInvitePreviewPreparation::try_prepare("12345678".to_owned()).unwrap();
        assert_eq!(preview_request.route(), PREVIEW_BASE);
        assert_eq!(preview_request.prepared_request_count(), 1);
        assert_eq!(
            query_map(preview_request.query.expose_secret()),
            BTreeMap::from([
                ("checkEnc".to_owned(), "1".to_owned()),
                ("inviteCode".to_owned(), "12345678".to_owned()),
            ])
        );
        let document = ChaoxingCourseInvitePreviewDocument::for_preparation(
            &preview_request,
            PREVIEW.to_owned(),
        )
        .unwrap();
        let preview = parse_course_invite_preview(&document).unwrap();
        assert_eq!(preview.course_id(), "1001");
        assert_eq!(preview.class_id(), "2001");
        assert_eq!(
            preview.preview_request_digest(),
            preview_request.request_digest()
        );
        let join = ChaoxingCourseJoinPreparation::try_prepare(&preview).unwrap();
        assert_eq!(join.route(), JOIN_BASE);
        assert_eq!(join.course_id(), "1001");
        assert_eq!(join.class_id(), "2001");
        assert_eq!(join.prepared_request_count(), 1);
        assert_eq!(
            query_map(join.query.expose_secret()),
            BTreeMap::from([
                ("classId".to_owned(), "2001".to_owned()),
                ("courseId".to_owned(), "1001".to_owned()),
                ("enc".to_owned(), "PRIVATE_ADD_CLASS_ENC".to_owned()),
                ("inviteCode".to_owned(), "12345678".to_owned()),
                ("timeStamp".to_owned(), "1720000000000".to_owned()),
            ])
        );
        for debug in [
            format!("{preview_request:?}"),
            format!("{document:?}"),
            format!("{preview:?}"),
            format!("{join:?}"),
        ] {
            assert!(!debug.contains("12345678"));
            assert!(!debug.contains("PRIVATE_ADD_CLASS_ENC"));
        }
    }

    #[test]
    fn parses_only_exact_donor_script_and_attribute_fallbacks() {
        let request =
            ChaoxingCourseInvitePreviewPreparation::try_prepare("12345678".to_owned()).unwrap();
        for (html, course_id, class_id, enc, timestamp) in [
            (
                SCRIPT_PREVIEW,
                "100001",
                "200001",
                "837462910",
                "1720000000000",
            ),
            (
                ATTRIBUTE_PREVIEW,
                "100002",
                "200002",
                "837462911",
                "1720000000001",
            ),
        ] {
            let document =
                ChaoxingCourseInvitePreviewDocument::for_preparation(&request, html.to_owned())
                    .unwrap();
            let preview = parse_course_invite_preview(&document).unwrap();
            assert_eq!(preview.course_id(), course_id);
            assert_eq!(preview.class_id(), class_id);
            let join = ChaoxingCourseJoinPreparation::try_prepare(&preview).unwrap();
            assert_eq!(
                query_map(join.query.expose_secret()),
                BTreeMap::from([
                    ("classId".to_owned(), class_id.to_owned()),
                    ("courseId".to_owned(), course_id.to_owned()),
                    ("enc".to_owned(), enc.to_owned()),
                    ("inviteCode".to_owned(), "12345678".to_owned()),
                    ("timeStamp".to_owned(), timestamp.to_owned()),
                ])
            );
        }

        let duplicate =
            format!("{PREVIEW}<script>var courseId = \"100001\"; clazzId = \"200001\";</script>");
        let document =
            ChaoxingCourseInvitePreviewDocument::for_preparation(&request, duplicate).unwrap();
        assert_eq!(
            parse_course_invite_preview(&document).unwrap_err().kind,
            ProviderErrorKind::ProtocolDrift
        );

        let arbitrary_text = PREVIEW
            .replace("id=\"courseId\"", "id=\"missingCourse\"")
            .replace("</body>", "courseId = \"100001\";</body>");
        let document =
            ChaoxingCourseInvitePreviewDocument::for_preparation(&request, arbitrary_text).unwrap();
        assert_eq!(
            parse_course_invite_preview(&document).unwrap_err().kind,
            ProviderErrorKind::ProtocolDrift
        );
    }

    #[test]
    fn response_url_fallback_is_strictly_bound_and_kept_secret() {
        let request =
            ChaoxingCourseInvitePreviewPreparation::try_prepare("12345678".to_owned()).unwrap();
        let source_url = concat!(
            "https://mooc1.chaoxing.com/addcourse/pcqrcodemiddleview?",
            "inviteCode=12345678&checkEnc=1&courseId=100003&clazzId=200003&",
            "enc=837462912"
        );
        let document = ChaoxingCourseInvitePreviewDocument::for_preparation_at(
            &request,
            source_url.to_owned(),
            URL_PREVIEW.to_owned(),
        )
        .unwrap();
        let preview = parse_course_invite_preview(&document).unwrap();
        assert_eq!(preview.course_id(), "100003");
        assert_eq!(preview.class_id(), "200003");
        let join = ChaoxingCourseJoinPreparation::try_prepare(&preview).unwrap();
        assert_eq!(
            query_map(join.query.expose_secret()),
            BTreeMap::from([
                ("classId".to_owned(), "200003".to_owned()),
                ("courseId".to_owned(), "100003".to_owned()),
                ("enc".to_owned(), "837462912".to_owned()),
                ("inviteCode".to_owned(), "12345678".to_owned()),
                ("timeStamp".to_owned(), "1720000000002".to_owned()),
            ])
        );
        assert!(!format!("{document:?}").contains(source_url));

        for (source_url, kind) in [
            (
                "http://mooc1.chaoxing.com/addcourse/pcqrcodemiddleview?inviteCode=12345678&checkEnc=1",
                ProviderErrorKind::ProtocolDrift,
            ),
            (
                "https://foreign.example/addcourse/pcqrcodemiddleview?inviteCode=12345678&checkEnc=1",
                ProviderErrorKind::ProtocolDrift,
            ),
            (
                "https://mooc1.chaoxing.com/addcourse/pcqrcodemiddleview?inviteCode=87654321&checkEnc=1",
                ProviderErrorKind::RemoteChanged,
            ),
            (
                "https://mooc1.chaoxing.com/addcourse/pcqrcodemiddleview?inviteCode=12345678&checkEnc=1&courseId=100003&courseid=100003",
                ProviderErrorKind::ProtocolDrift,
            ),
            (
                "https://mooc1.chaoxing.com/addcourse/pcqrcodemiddleview?inviteCode=12345678&checkEnc=1&unknown=1",
                ProviderErrorKind::ProtocolDrift,
            ),
        ] {
            assert_eq!(
                ChaoxingCourseInvitePreviewDocument::for_preparation_at(
                    &request,
                    source_url.to_owned(),
                    URL_PREVIEW.to_owned(),
                )
                .unwrap_err()
                .kind,
                kind
            );
        }
    }

    #[test]
    fn api_fallback_upgrades_and_binds_the_exact_middle_page() {
        let api = ChaoxingCourseInviteApiPreparation::try_prepare(
            "12345678".to_owned(),
            1_720_000_000_000,
        )
        .unwrap();
        assert_eq!(api.route(), INVITE_API_BASE);
        assert_eq!(api.prepared_request_count(), 1);
        assert_eq!(
            query_map(api.form_body.expose_secret()),
            BTreeMap::from([
                ("_t".to_owned(), "1720000000000".to_owned()),
                ("invitecode".to_owned(), "12345678".to_owned()),
            ])
        );
        let response =
            ChaoxingCourseInviteApiDocument::for_preparation(&api, API_SUCCESS.to_owned()).unwrap();
        let redirect = parse_course_invite_api_redirect(&response).unwrap();
        assert_eq!(redirect.route(), PREVIEW_BASE);
        assert_eq!(redirect.prepared_request_count(), 1);
        assert_eq!(
            query_map(redirect.query.expose_secret()),
            BTreeMap::from([
                ("checkEnc".to_owned(), "1".to_owned()),
                ("enc".to_owned(), "PRIVATE_PREVIEW_ENC".to_owned()),
                ("inviteCode".to_owned(), "12345678".to_owned()),
            ])
        );
        let document =
            ChaoxingCourseInvitePreviewDocument::for_redirect(&redirect, PREVIEW.to_owned())
                .unwrap();
        let preview = parse_course_invite_preview(&document).unwrap();
        assert_eq!(preview.course_id(), "1001");
        assert_eq!(preview.class_id(), "2001");
        for debug in [
            format!("{api:?}"),
            format!("{response:?}"),
            format!("{redirect:?}"),
        ] {
            assert!(!debug.contains("12345678"));
            assert!(!debug.contains("PRIVATE_PREVIEW_ENC"));
        }
    }

    #[test]
    fn receipt_is_not_membership_and_fresh_inventory_is_independent_proof() {
        let join = join_preparation();
        let document =
            ChaoxingCourseJoinReceiptDocument::for_preparation(&join, ACCEPTED.to_owned()).unwrap();
        let receipt = parse_course_join_receipt(&document).unwrap();
        assert_eq!(receipt.kind(), ChaoxingCourseJoinReceiptKind::Accepted);
        assert_eq!(receipt.request_digest(), join.request_digest());
        assert_eq!(receipt.response_digest().len(), 64);
        assert!(!join.verify_fresh_membership(&[]).unwrap());
        assert!(
            join.verify_fresh_membership(&[course("course:1001:2001")])
                .unwrap()
        );
        assert_eq!(
            join.verify_fresh_membership(
                &[course("course:1001:2001"), course("course:1001:2001"),]
            )
            .unwrap_err()
            .kind,
            ProviderErrorKind::ProtocolDrift
        );
    }

    #[test]
    fn course_enrollment_contract_binds_account_plan_and_exact_issue() {
        let context = context();
        let join = join_preparation();
        let command = ChaoxingCourseEnrollmentCommand::try_prepare(&context, &join).unwrap();
        assert_eq!(command.course_id(), "1001");
        assert_eq!(command.class_id(), "2001");
        assert_eq!(command.artifact().provider_id(), &context.provider_id);
        assert_eq!(
            command.artifact().artifact_type(),
            COURSE_ENROLLMENT_ARTIFACT_TYPE
        );
        assert_eq!(
            command.mutation_plan().artifact_digest(),
            command.artifact().artifact_digest()
        );
        assert_eq!(command.mutation_plan().steps().len(), 1);
        assert_eq!(
            command.mutation_plan().steps()[0].request_digest(),
            Some(command.mutation_issue().request_digest())
        );
        assert_eq!(command.mutation_issue().ordinal(), 1);
        assert_eq!(
            command.mutation_issue().operation_type(),
            COURSE_ENROLLMENT_OPERATION_TYPE
        );
        assert!(command.bind_issued(&join, command.mutation_issue()).is_ok());

        let receipt_document =
            ChaoxingCourseJoinReceiptDocument::for_preparation(&join, ACCEPTED.to_owned()).unwrap();
        let receipt = parse_course_join_receipt(&receipt_document).unwrap();
        let bound = command.bind_receipt(&receipt).unwrap();
        assert_eq!(bound.ordinal(), 1);
        assert!(bound.accepted());

        let debug = format!("{command:?}");
        assert!(!debug.contains(&context.account_id.to_string()));
        assert!(!debug.contains("12345678"));
        assert!(!debug.contains("PRIVATE_ADD_CLASS_ENC"));
    }

    #[test]
    fn ambiguous_join_recovers_only_from_fresh_inventory_without_replay() {
        let context = context();
        let join = join_preparation();
        let command = ChaoxingCourseEnrollmentCommand::try_prepare(&context, &join).unwrap();
        let ambiguous =
            ExecutionMutationRecoveryRecord::try_new(command.mutation_issue().clone(), None, None)
                .unwrap();
        let absent = command.observe_fresh_membership(&context, &[]).unwrap();
        assert_eq!(
            command.recover(&ambiguous, &absent).unwrap(),
            ChaoxingCourseEnrollmentRecoveryOutcome::AmbiguousLocked
        );
        let present = command
            .observe_fresh_membership(&context, &[course("course:1001:2001")])
            .unwrap();
        assert_eq!(
            command.recover(&ambiguous, &present).unwrap(),
            ChaoxingCourseEnrollmentRecoveryOutcome::AmbiguousVerifiedByFreshInventory
        );

        // The generic Task recovery shape cannot persist the required
        // verification on an issue that has no receipt. This is the exact Core
        // gap which prevents wiring CourseEnrollment into ExecutionMutationSink.
        assert!(
            ExecutionMutationRecoveryRecord::try_new(
                command.mutation_issue().clone(),
                None,
                Some(command.verification(&present).unwrap()),
            )
            .is_err()
        );
        assert_eq!(ambiguous.issue(), command.mutation_issue());
    }

    #[test]
    fn receipt_and_readback_outcomes_remain_separate() {
        let context = context();
        let join = join_preparation();
        let command = ChaoxingCourseEnrollmentCommand::try_prepare(&context, &join).unwrap();
        let present = command
            .observe_fresh_membership(&context, &[course("course:1001:2001")])
            .unwrap();
        let absent = command.observe_fresh_membership(&context, &[]).unwrap();

        let accepted_document =
            ChaoxingCourseJoinReceiptDocument::for_preparation(&join, ACCEPTED.to_owned()).unwrap();
        let accepted = parse_course_join_receipt(&accepted_document).unwrap();
        let accepted_record = ExecutionMutationRecoveryRecord::try_new(
            command.mutation_issue().clone(),
            Some(command.bind_receipt(&accepted).unwrap()),
            None,
        )
        .unwrap();
        assert_eq!(
            command.recover(&accepted_record, &present).unwrap(),
            ChaoxingCourseEnrollmentRecoveryOutcome::AcceptedAndVerified
        );
        assert_eq!(
            command.recover(&accepted_record, &absent).unwrap(),
            ChaoxingCourseEnrollmentRecoveryOutcome::AcceptedAwaitingFreshMembership
        );

        let rejected_document =
            ChaoxingCourseJoinReceiptDocument::for_preparation(&join, REJECTED.to_owned()).unwrap();
        let rejected = parse_course_join_receipt(&rejected_document).unwrap();
        let rejected_record = ExecutionMutationRecoveryRecord::try_new(
            command.mutation_issue().clone(),
            Some(command.bind_receipt(&rejected).unwrap()),
            None,
        )
        .unwrap();
        assert_eq!(
            command.recover(&rejected_record, &absent).unwrap(),
            ChaoxingCourseEnrollmentRecoveryOutcome::Rejected
        );
        assert_eq!(
            command.recover(&rejected_record, &present).unwrap(),
            ChaoxingCourseEnrollmentRecoveryOutcome::MembershipPresentAfterRejectedReceipt
        );
    }

    #[test]
    fn enrollment_recovery_rejects_foreign_account_and_inventory_identity() {
        let context = context();
        let join = join_preparation();
        let command = ChaoxingCourseEnrollmentCommand::try_prepare(&context, &join).unwrap();
        let mut foreign = context.clone();
        foreign.account_id = asterism_domain::ProviderAccountId::new();
        assert_eq!(
            command
                .observe_fresh_membership(&foreign, &[course("course:1001:2001")])
                .unwrap_err()
                .kind,
            ProviderErrorKind::InvalidResponse
        );
        assert_eq!(
            command
                .observe_fresh_membership(
                    &context,
                    &[course("course:1001:2001"), course("course:1001:2001"),],
                )
                .unwrap_err()
                .kind,
            ProviderErrorKind::ProtocolDrift
        );
    }

    #[test]
    fn drifted_preview_and_receipt_shapes_fail_closed() {
        for invite in ["", " 12345678", "invite-code", &"1".repeat(65)] {
            assert_eq!(
                ChaoxingCourseInvitePreviewPreparation::try_prepare(invite.to_owned())
                    .unwrap_err()
                    .kind,
                ProviderErrorKind::InvalidResponse
            );
        }
        let request =
            ChaoxingCourseInvitePreviewPreparation::try_prepare("12345678".to_owned()).unwrap();
        for html in [
            PREVIEW.replace("id=\"courseId\"", "id=\"missingCourse\""),
            PREVIEW.replace(
                "<input id=\"courseId\" value=\"1001\">",
                "<input id=\"courseId\" value=\"1001\"><input name=\"courseid\" value=\"1001\">",
            ),
            PREVIEW.replace("value=\"1001\"", "value=\" 1001\""),
            PREVIEW.replace("PRIVATE_ADD_CLASS_ENC", "unsafe/enc"),
        ] {
            let document =
                ChaoxingCourseInvitePreviewDocument::for_preparation(&request, html).unwrap();
            assert_eq!(
                parse_course_invite_preview(&document).unwrap_err().kind,
                ProviderErrorKind::ProtocolDrift
            );
        }
        let join = join_preparation();
        for response in [
            r"{}",
            r#"{"result":1,"result":0}"#,
            r#"{"result":"1"}"#,
            r#"{"result":1,"msg":"ok","errorMsg":"duplicate"}"#,
        ] {
            let document =
                ChaoxingCourseJoinReceiptDocument::for_preparation(&join, response.to_owned())
                    .unwrap();
            assert_eq!(
                parse_course_join_receipt(&document).unwrap_err().kind,
                ProviderErrorKind::ProtocolDrift
            );
        }

        let api = ChaoxingCourseInviteApiPreparation::try_prepare(
            "12345678".to_owned(),
            1_720_000_000_000,
        )
        .unwrap();
        for (response, kind) in [
            (
                r#"{"status":true,"flag":0}"#,
                ProviderErrorKind::ProtocolDrift,
            ),
            (
                r#"{"status":true,"status":false,"flag":0,"url":"https://mooc1.chaoxing.com/addcourse/pcqrcodemiddleview?inviteCode=12345678&checkEnc=1"}"#,
                ProviderErrorKind::ProtocolDrift,
            ),
            (
                r#"{"status":true,"flag":1,"url":"https://mooc1.chaoxing.com/addcourse/pcqrcodemiddleview?inviteCode=12345678&checkEnc=1"}"#,
                ProviderErrorKind::UnsupportedTask,
            ),
            (
                r#"{"status":true,"flag":0,"url":"https://foreign.example/addcourse/pcqrcodemiddleview?inviteCode=12345678&checkEnc=1"}"#,
                ProviderErrorKind::ProtocolDrift,
            ),
            (
                r#"{"status":true,"flag":0,"url":"https://mooc1.chaoxing.com/addcourse/pcqrcodemiddleview?inviteCode=87654321&checkEnc=1"}"#,
                ProviderErrorKind::RemoteChanged,
            ),
        ] {
            let document =
                ChaoxingCourseInviteApiDocument::for_preparation(&api, response.to_owned())
                    .unwrap();
            assert_eq!(
                parse_course_invite_api_redirect(&document)
                    .unwrap_err()
                    .kind,
                kind
            );
        }
    }

    fn join_preparation() -> ChaoxingCourseJoinPreparation {
        let request =
            ChaoxingCourseInvitePreviewPreparation::try_prepare("12345678".to_owned()).unwrap();
        let document =
            ChaoxingCourseInvitePreviewDocument::for_preparation(&request, PREVIEW.to_owned())
                .unwrap();
        let preview = parse_course_invite_preview(&document).unwrap();
        ChaoxingCourseJoinPreparation::try_prepare(&preview).unwrap()
    }

    fn context() -> ProviderContext {
        ProviderContext {
            provider_id: ProviderId::new("chaoxing").unwrap(),
            account_id: ProviderAccountId::new(),
            credential_refs: vec![SecretId::new()],
            correlation_id: "course-enrollment-fixture".to_owned(),
        }
    }

    fn course(remote_id: &str) -> RemoteCourse {
        RemoteCourse {
            remote_id: remote_id.to_owned(),
            title: "Synthetic Course".to_owned(),
            term: None,
            teacher: None,
            remote_status: None,
            metadata_sanitized: json!({"fixture": true}),
            route_context: ProviderRouteContext::default(),
        }
    }

    fn query_map(query: &str) -> BTreeMap<String, String> {
        Url::parse(&format!("https://example.invalid/?{query}"))
            .unwrap()
            .query_pairs()
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect()
    }
}
