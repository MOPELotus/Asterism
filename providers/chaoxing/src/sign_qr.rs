use std::{collections::BTreeMap, fmt};

use asterism_provider_api::{ProviderError, ProviderErrorKind, ProviderResult};
use asterism_secrets::SecretString;
use reqwest::Url;
use sha2::{Digest, Sha256};
use zeroize::{Zeroize, Zeroizing};

use crate::{ChaoxingSignActivity, ChaoxingSignDetail, ChaoxingSignVariant};

const MAX_QR_DOCUMENT_BYTES: usize = 4 * 1_024;
const MAX_QR_MATERIAL_BYTES: usize = 512;
const MAX_ACTOR_ID_BYTES: usize = 128;
const MAX_ACTOR_NAME_BYTES: usize = 256;
const QR_SUBMISSION_BASE: &str = "https://mobilelearn.chaoxing.com/pptSign/stuSignajax";
const QR_HOSTS: &[&str] = &[
    "mobilelearn.chaoxing.com",
    "www.chaoxing.com",
    "k.chaoxing.com",
];

/// The audited carrier used for one exact Chaoxing sign-in QR material.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChaoxingQrSignMaterialSource {
    HttpsUrl,
    SignInPayload,
}

/// One bounded, zeroizing QR-sign material bound to a fresh activity/detail
/// pair. It provides no send method and does not authorize a sign-in mutation.
pub struct ChaoxingQrSignMaterial {
    activity_id: String,
    source: ChaoxingQrSignMaterialSource,
    enc: Zeroizing<String>,
    code: Option<Zeroizing<String>>,
    binding_digest: [u8; 32],
    material_digest: [u8; 32],
}

impl ChaoxingQrSignMaterial {
    /// Parses one scanned QR document and binds it to a freshly rebound QR
    /// activity without exposing the dynamic `enc` or optional code.
    ///
    /// # Errors
    ///
    /// Rejects foreign activities, non-QR variants, unsafe hosts, duplicate or
    /// missing fields, unsupported sources and unbounded dynamic material.
    pub fn try_new(
        activity: &ChaoxingSignActivity,
        detail: &ChaoxingSignDetail,
        document: &str,
    ) -> ProviderResult<Self> {
        validate_binding(activity, detail)?;
        let (source, fields) = parse_document(document)?;
        let activity_id = take_activity_id(&fields, source)?;
        if activity_id != activity.activity_id() {
            return Err(remote_changed(
                "Chaoxing sign-in QR belongs to another activity",
            ));
        }
        let enc = required_material(&fields, "enc")?;
        let code = match source {
            ChaoxingQrSignMaterialSource::HttpsUrl => {
                if fields.contains_key("Code") || fields.contains_key("source") {
                    return Err(invalid_response(
                        "Chaoxing HTTPS QR mixes SIGNIN-only material fields",
                    ));
                }
                None
            }
            ChaoxingQrSignMaterialSource::SignInPayload => {
                if fields.get("source").map(String::as_str) != Some("15") {
                    return Err(invalid_response(
                        "Chaoxing SIGNIN QR has an unsupported source",
                    ));
                }
                Some(Zeroizing::new(
                    required_material(&fields, "Code")?.to_owned(),
                ))
            }
        };

        let binding_digest = digest_parts(
            b"asterism.chaoxing.sign-qr-binding.v1",
            &[
                activity.remote_id().as_bytes(),
                activity.fingerprint().as_bytes(),
                detail.fingerprint().as_bytes(),
            ],
        );
        let source_tag = match source {
            ChaoxingQrSignMaterialSource::HttpsUrl => b"https".as_slice(),
            ChaoxingQrSignMaterialSource::SignInPayload => b"signin".as_slice(),
        };
        let material_digest = digest_parts(
            b"asterism.chaoxing.sign-qr-material.v1",
            &[
                source_tag,
                activity_id.as_bytes(),
                enc.as_bytes(),
                code.as_deref().map_or(b"".as_slice(), String::as_bytes),
            ],
        );
        Ok(Self {
            activity_id,
            source,
            enc: Zeroizing::new(enc.to_owned()),
            code,
            binding_digest,
            material_digest,
        })
    }

    pub const fn source(&self) -> ChaoxingQrSignMaterialSource {
        self.source
    }

    pub const fn binding_digest(&self) -> [u8; 32] {
        self.binding_digest
    }

    pub const fn material_digest(&self) -> [u8; 32] {
        self.material_digest
    }

    pub const fn has_refresh_code(&self) -> bool {
        self.code.is_some()
    }

    pub(crate) fn enc(&self) -> &str {
        self.enc.as_str()
    }
}

impl fmt::Debug for ChaoxingQrSignMaterial {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ChaoxingQrSignMaterial")
            .field("activity_id", &"[REDACTED]")
            .field("source", &self.source)
            .field("enc", &"[REDACTED]")
            .field("code", &self.code.as_ref().map(|_| "[REDACTED]"))
            .field("binding_digest", &self.binding_digest)
            .field("material_digest", &self.material_digest)
            .finish()
    }
}

impl Drop for ChaoxingQrSignMaterial {
    fn drop(&mut self) {
        self.activity_id.zeroize();
        self.enc.zeroize();
    }
}

/// One immutable preparation for Ylim's exact QR `stuSignajax` step.
///
/// This is deliberately not a complete sign-in sequence. It freezes only the
/// independently evidenced QR submission query and exposes no plaintext query
/// or send method. The donor-conflicted pre-sign step and fresh completion
/// readback remain separate blockers.
pub struct ChaoxingQrSignSubmissionPreparation {
    remote_id: String,
    material_digest: [u8; 32],
    submission_query: SecretString,
    request_digest: [u8; 32],
}

impl ChaoxingQrSignSubmissionPreparation {
    /// Freezes one exact QR submission step from freshly rebound snapshots,
    /// scanned material and explicit actor identity.
    ///
    /// # Errors
    ///
    /// Rejects stale material/activity bindings, non-QR variants and malformed
    /// actor fields. Success still grants no remote mutation authority.
    pub fn try_prepare(
        activity: &ChaoxingSignActivity,
        detail: &ChaoxingSignDetail,
        material: &ChaoxingQrSignMaterial,
        uid: impl Into<String>,
        fid: impl Into<String>,
        name: impl Into<String>,
    ) -> ProviderResult<Self> {
        validate_binding(activity, detail)?;
        let expected_binding = digest_parts(
            b"asterism.chaoxing.sign-qr-binding.v1",
            &[
                activity.remote_id().as_bytes(),
                activity.fingerprint().as_bytes(),
                detail.fingerprint().as_bytes(),
            ],
        );
        if material.activity_id != activity.activity_id()
            || material.binding_digest != expected_binding
        {
            return Err(remote_changed(
                "Chaoxing sign-in QR material changed before submission preparation",
            ));
        }

        let mut uid = uid.into();
        let mut fid = fid.into();
        let mut name = name.into();
        let result = (|| {
            validate_actor_id(&uid)?;
            validate_actor_id(&fid)?;
            validate_actor_name(&name)?;
            let submission_query = canonical_query(
                QR_SUBMISSION_BASE,
                &[
                    ("enc", material.enc()),
                    ("name", &name),
                    ("activeId", activity.activity_id()),
                    ("uid", &uid),
                    ("clientip", ""),
                    ("useragent", ""),
                    ("latitude", "-1"),
                    ("longitude", "-1"),
                    ("fid", &fid),
                    ("appType", "15"),
                ],
            )?;
            let request_digest = digest_parts(
                b"asterism.chaoxing.sign-qr-submission-preparation.v1",
                &[
                    activity.remote_id().as_bytes(),
                    activity.fingerprint().as_bytes(),
                    detail.fingerprint().as_bytes(),
                    material.material_digest.as_slice(),
                    submission_query.as_bytes(),
                ],
            );
            Ok(Self {
                remote_id: activity.remote_id().to_owned(),
                material_digest: material.material_digest,
                submission_query: SecretString::new(submission_query),
                request_digest,
            })
        })();
        uid.zeroize();
        fid.zeroize();
        name.zeroize();
        result
    }

    pub fn remote_id(&self) -> &str {
        &self.remote_id
    }

    pub const fn variant(&self) -> ChaoxingSignVariant {
        ChaoxingSignVariant::QrCode
    }

    pub const fn submission_route(&self) -> &'static str {
        QR_SUBMISSION_BASE
    }

    pub const fn material_digest(&self) -> [u8; 32] {
        self.material_digest
    }

    pub const fn request_digest(&self) -> [u8; 32] {
        self.request_digest
    }

    pub fn prepared_request_count(&self) -> usize {
        usize::from(!self.submission_query.expose_secret().is_empty())
    }
}

impl fmt::Debug for ChaoxingQrSignSubmissionPreparation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ChaoxingQrSignSubmissionPreparation")
            .field("remote_id", &self.remote_id)
            .field("material_digest", &self.material_digest)
            .field("submission_query", &"[REDACTED]")
            .field("request_digest", &self.request_digest)
            .finish()
    }
}

fn validate_actor_id(value: &str) -> ProviderResult<()> {
    if value.is_empty()
        || value.len() > MAX_ACTOR_ID_BYTES
        || !value.bytes().all(|byte| byte.is_ascii_digit())
    {
        Err(invalid_response(
            "Chaoxing QR sign-in actor identity is invalid",
        ))
    } else {
        Ok(())
    }
}

fn validate_actor_name(value: &str) -> ProviderResult<()> {
    if value.trim().is_empty()
        || value.trim() != value
        || value.len() > MAX_ACTOR_NAME_BYTES
        || value.chars().any(char::is_control)
    {
        Err(invalid_response(
            "Chaoxing QR sign-in actor name is invalid",
        ))
    } else {
        Ok(())
    }
}

fn canonical_query(base: &str, pairs: &[(&str, &str)]) -> ProviderResult<String> {
    let mut url = Url::parse(base).map_err(|_| {
        ProviderError::new(
            ProviderErrorKind::Internal,
            "Chaoxing QR sign-in static route is invalid",
        )
    })?;
    url.query_pairs_mut().extend_pairs(pairs.iter().copied());
    url.query().map(str::to_owned).ok_or_else(|| {
        ProviderError::new(
            ProviderErrorKind::Internal,
            "Chaoxing QR sign-in query preparation failed",
        )
    })
}

fn validate_binding(
    activity: &ChaoxingSignActivity,
    detail: &ChaoxingSignDetail,
) -> ProviderResult<()> {
    if activity.variant() != ChaoxingSignVariant::QrCode
        || detail.variant() != ChaoxingSignVariant::QrCode
    {
        return Err(ProviderError::new(
            ProviderErrorKind::UnsupportedTask,
            "Chaoxing sign-in QR material requires a QR activity",
        ));
    }
    if activity.remote_id() != detail.remote_id()
        || activity.fingerprint() != detail.discovery_fingerprint()
    {
        return Err(remote_changed(
            "Chaoxing sign-in QR activity changed before material binding",
        ));
    }
    Ok(())
}

fn parse_document(
    document: &str,
) -> ProviderResult<(ChaoxingQrSignMaterialSource, BTreeMap<String, String>)> {
    if document.is_empty()
        || document.len() > MAX_QR_DOCUMENT_BYTES
        || document.trim() != document
        || document.chars().any(char::is_control)
    {
        return Err(invalid_response(
            "Chaoxing sign-in QR document is empty, unsafe or oversized",
        ));
    }
    let decoded = document.replace("&amp;", "&");
    if let Some(query) = decoded.strip_prefix("SIGNIN:") {
        let url = Url::parse(&format!("https://mobilelearn.chaoxing.com/?{query}"))
            .map_err(|_| invalid_response("Chaoxing SIGNIN QR payload is malformed"))?;
        return parse_query(&url)
            .map(|fields| (ChaoxingQrSignMaterialSource::SignInPayload, fields));
    }
    let url = Url::parse(&decoded)
        .map_err(|_| invalid_response("Chaoxing sign-in QR URL is malformed"))?;
    if url.scheme() != "https"
        || url.host_str().is_none_or(|host| !QR_HOSTS.contains(&host))
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
    {
        return Err(invalid_response(
            "Chaoxing sign-in QR URL is outside the audited HTTPS hosts",
        ));
    }
    parse_query(&url).map(|fields| (ChaoxingQrSignMaterialSource::HttpsUrl, fields))
}

fn parse_query(url: &Url) -> ProviderResult<BTreeMap<String, String>> {
    let mut fields = BTreeMap::new();
    for (key, value) in url.query_pairs() {
        if !matches!(
            key.as_ref(),
            "activeId" | "id" | "aid" | "enc" | "Code" | "source"
        ) {
            continue;
        }
        if fields
            .insert(key.into_owned(), value.into_owned())
            .is_some()
        {
            return Err(invalid_response(
                "Chaoxing sign-in QR contains duplicate material fields",
            ));
        }
    }
    Ok(fields)
}

fn take_activity_id(
    fields: &BTreeMap<String, String>,
    source: ChaoxingQrSignMaterialSource,
) -> ProviderResult<String> {
    let aliases = ["activeId", "id", "aid"];
    let values = aliases
        .iter()
        .filter_map(|name| fields.get(*name).map(|value| (*name, value)))
        .collect::<Vec<_>>();
    let allowed_alias = match source {
        ChaoxingQrSignMaterialSource::HttpsUrl => {
            matches!(values.first(), Some(&("activeId" | "id", _)))
        }
        ChaoxingQrSignMaterialSource::SignInPayload => {
            matches!(values.first(), Some(&("aid", _)))
        }
    };
    if values.len() != 1 || !allowed_alias || !valid_activity_id(values[0].1) {
        return Err(invalid_response(
            "Chaoxing sign-in QR activity identity is missing or ambiguous",
        ));
    }
    Ok(values[0].1.clone())
}

fn required_material<'a>(
    fields: &'a BTreeMap<String, String>,
    name: &str,
) -> ProviderResult<&'a str> {
    fields
        .get(name)
        .map(String::as_str)
        .map(validate_material)
        .transpose()?
        .ok_or_else(|| invalid_response("Chaoxing sign-in QR material is missing"))
}

fn validate_material(value: &str) -> ProviderResult<&str> {
    if value.is_empty()
        || value.len() > MAX_QR_MATERIAL_BYTES
        || !value.is_ascii()
        || value
            .bytes()
            .any(|byte| byte.is_ascii_whitespace() || byte.is_ascii_control())
    {
        Err(invalid_response(
            "Chaoxing sign-in QR material is unsafe or oversized",
        ))
    } else {
        Ok(value)
    }
}

fn valid_activity_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn digest_parts(domain: &[u8], parts: &[&[u8]]) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(domain);
    for part in parts {
        digest.update(u64::try_from(part.len()).unwrap_or(u64::MAX).to_be_bytes());
        digest.update(part);
    }
    digest.finalize().into()
}

fn invalid_response(message: &'static str) -> ProviderError {
    ProviderError::new(ProviderErrorKind::InvalidResponse, message)
}

fn remote_changed(message: &'static str) -> ProviderError {
    ProviderError::new(ProviderErrorKind::RemoteChanged, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ChaoxingCourseScope, ChaoxingSignActivityListDocument, ChaoxingSignDetailDocument,
        ChaoxingSignDetailRequest, parse_sign_activity_list, parse_sign_detail,
    };
    use std::collections::BTreeMap;

    const ACTIVITIES: &str =
        include_str!("../../../fixtures/providers/chaoxing/sign/activities-mixed.json");
    const QR_DETAIL: &str =
        include_str!("../../../fixtures/providers/chaoxing/sign/detail-qr.json");
    const QR_URL: &str = include_str!("../../../fixtures/providers/chaoxing/sign/qr-url.txt");
    const SIGNIN_PAYLOAD: &str =
        include_str!("../../../fixtures/providers/chaoxing/sign/qr-signin.txt");

    #[test]
    fn binds_https_and_signin_qr_material_without_exposing_secrets() {
        let (activity, detail) = qr_binding();
        let https = ChaoxingQrSignMaterial::try_new(&activity, &detail, QR_URL.trim()).unwrap();
        assert_eq!(https.source(), ChaoxingQrSignMaterialSource::HttpsUrl);
        assert_eq!(https.enc(), "PRIVATE_QR_ENC");
        assert!(!https.has_refresh_code());

        let signin =
            ChaoxingQrSignMaterial::try_new(&activity, &detail, SIGNIN_PAYLOAD.trim()).unwrap();
        assert_eq!(signin.source(), ChaoxingQrSignMaterialSource::SignInPayload);
        assert!(signin.has_refresh_code());
        assert_eq!(signin.enc(), "PRIVATE_QR_ENC");
        assert_eq!(https.binding_digest(), signin.binding_digest());
        assert_ne!(https.material_digest(), signin.material_digest());

        for debug in [format!("{https:?}"), format!("{signin:?}")] {
            assert!(!debug.contains("7003"));
            assert!(!debug.contains("PRIVATE_QR_ENC"));
            assert!(!debug.contains("PRIVATE_QR_CODE"));
        }
    }

    #[test]
    fn foreign_duplicate_and_unsupported_qr_material_fail_closed() {
        let (activity, detail) = qr_binding();
        for document in [
            "https://evil.example/path?activeId=7003&enc=PRIVATE_QR_ENC",
            "https://mobilelearn.chaoxing.com/path?activeId=7003&id=7003&enc=PRIVATE_QR_ENC",
            "https://mobilelearn.chaoxing.com/path?activeId=7003&aid=foreign&enc=PRIVATE_QR_ENC",
            "https://mobilelearn.chaoxing.com/path?activeId=7003&enc=one&enc=two",
            "https://mobilelearn.chaoxing.com/path?activeId=7003&enc=PRIVATE_QR_ENC&Code=PRIVATE_QR_CODE",
            "https://mobilelearn.chaoxing.com/path?activeId=7003&enc=PRIVATE_QR_ENC&source=15",
            "SIGNIN:aid=7003&activeId=foreign&source=15&Code=PRIVATE_QR_CODE&enc=PRIVATE_QR_ENC",
            "SIGNIN:aid=7003&source=15&enc=PRIVATE_QR_ENC",
            "SIGNIN:aid=7003&source=16&Code=PRIVATE_QR_CODE&enc=PRIVATE_QR_ENC",
        ] {
            assert_eq!(
                ChaoxingQrSignMaterial::try_new(&activity, &detail, document)
                    .unwrap_err()
                    .kind,
                ProviderErrorKind::InvalidResponse
            );
        }
        assert_eq!(
            ChaoxingQrSignMaterial::try_new(
                &activity,
                &detail,
                "https://mobilelearn.chaoxing.com/path?activeId=foreign&enc=PRIVATE_QR_ENC",
            )
            .unwrap_err()
            .kind,
            ProviderErrorKind::RemoteChanged
        );

        let scope = ChaoxingCourseScope::new("course:1001:2001", "1001", "2001").unwrap();
        let document = ChaoxingSignActivityListDocument::try_new(ACTIVITIES).unwrap();
        let activities = parse_sign_activity_list(&document, &scope).unwrap();
        assert_eq!(
            ChaoxingQrSignMaterial::try_new(&activities[0], &detail, QR_URL.trim())
                .unwrap_err()
                .kind,
            ProviderErrorKind::UnsupportedTask
        );
    }

    #[test]
    fn freezes_exact_qr_submission_step_without_claiming_a_sequence() {
        let (activity, detail) = qr_binding();
        let material =
            ChaoxingQrSignMaterial::try_new(&activity, &detail, SIGNIN_PAYLOAD.trim()).unwrap();
        let preparation = ChaoxingQrSignSubmissionPreparation::try_prepare(
            &activity,
            &detail,
            &material,
            "9001",
            "4001",
            "Test Student",
        )
        .unwrap();

        assert_eq!(preparation.remote_id(), activity.remote_id());
        assert_eq!(preparation.variant(), ChaoxingSignVariant::QrCode);
        assert_eq!(preparation.submission_route(), QR_SUBMISSION_BASE);
        assert_eq!(preparation.prepared_request_count(), 1);
        assert_eq!(preparation.material_digest(), material.material_digest());
        assert_ne!(preparation.request_digest(), [0; 32]);
        assert_eq!(
            query_map(preparation.submission_query.expose_secret()),
            BTreeMap::from([
                ("activeId".to_owned(), "7003".to_owned()),
                ("appType".to_owned(), "15".to_owned()),
                ("clientip".to_owned(), String::new()),
                ("enc".to_owned(), "PRIVATE_QR_ENC".to_owned()),
                ("fid".to_owned(), "4001".to_owned()),
                ("latitude".to_owned(), "-1".to_owned()),
                ("longitude".to_owned(), "-1".to_owned()),
                ("name".to_owned(), "Test Student".to_owned()),
                ("uid".to_owned(), "9001".to_owned()),
                ("useragent".to_owned(), String::new()),
            ])
        );
        let debug = format!("{preparation:?}");
        assert!(!debug.contains("PRIVATE_QR_ENC"));
        assert!(!debug.contains("9001"));
        assert!(!debug.contains("Test Student"));
    }

    #[test]
    fn qr_submission_step_is_actor_and_fresh_binding_bound() {
        let (activity, detail) = qr_binding();
        let material = ChaoxingQrSignMaterial::try_new(&activity, &detail, QR_URL.trim()).unwrap();
        let first = ChaoxingQrSignSubmissionPreparation::try_prepare(
            &activity, &detail, &material, "9001", "4001", "Student",
        )
        .unwrap();
        let same = ChaoxingQrSignSubmissionPreparation::try_prepare(
            &activity, &detail, &material, "9001", "4001", "Student",
        )
        .unwrap();
        let other = ChaoxingQrSignSubmissionPreparation::try_prepare(
            &activity, &detail, &material, "9002", "4001", "Student",
        )
        .unwrap();
        assert_eq!(first.request_digest(), same.request_digest());
        assert_ne!(first.request_digest(), other.request_digest());

        for (uid, fid, name) in [
            ("", "4001", "Student"),
            ("not-a-uid", "4001", "Student"),
            ("9001", "", "Student"),
            ("9001", "4001", "\n"),
        ] {
            assert_eq!(
                ChaoxingQrSignSubmissionPreparation::try_prepare(
                    &activity, &detail, &material, uid, fid, name,
                )
                .unwrap_err()
                .kind,
                ProviderErrorKind::InvalidResponse
            );
        }

        let request = ChaoxingSignDetailRequest::for_test(&activity);
        let changed = QR_DETAIL.replace("\"status\": 1", "\"status\": 2");
        let changed = ChaoxingSignDetailDocument::try_new(changed).unwrap();
        let changed = parse_sign_detail(&changed, &request).unwrap();
        assert_eq!(
            ChaoxingQrSignSubmissionPreparation::try_prepare(
                &activity, &changed, &material, "9001", "4001", "Student",
            )
            .unwrap_err()
            .kind,
            ProviderErrorKind::RemoteChanged
        );
    }

    fn qr_binding() -> (ChaoxingSignActivity, ChaoxingSignDetail) {
        let scope = ChaoxingCourseScope::new("course:1001:2001", "1001", "2001").unwrap();
        let document = ChaoxingSignActivityListDocument::try_new(ACTIVITIES).unwrap();
        let activities = parse_sign_activity_list(&document, &scope).unwrap();
        let activity = activities
            .into_iter()
            .find(|activity| activity.activity_id() == "7003")
            .unwrap();
        let request = ChaoxingSignDetailRequest::for_test(&activity);
        let document = ChaoxingSignDetailDocument::try_new(QR_DETAIL).unwrap();
        let detail = parse_sign_detail(&document, &request).unwrap();
        (activity, detail)
    }

    fn query_map(query: &str) -> BTreeMap<String, String> {
        Url::parse(&format!("https://example.invalid/?{query}"))
            .unwrap()
            .query_pairs()
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect()
    }
}
