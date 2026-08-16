use std::{collections::BTreeMap, fmt};

use asterism_provider_api::{ProviderError, ProviderErrorKind, ProviderResult};
use reqwest::Url;
use sha2::{Digest, Sha256};
use zeroize::{Zeroize, Zeroizing};

use crate::{ChaoxingSignActivity, ChaoxingSignDetail, ChaoxingSignVariant};

const MAX_QR_DOCUMENT_BYTES: usize = 4 * 1_024;
const MAX_QR_MATERIAL_BYTES: usize = 512;
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
        let code = fields
            .get("Code")
            .map(String::as_str)
            .map(validate_material)
            .transpose()?
            .map(|value| Zeroizing::new(value.to_owned()));
        if source == ChaoxingQrSignMaterialSource::SignInPayload
            && fields.get("source").map(String::as_str) != Some("15")
        {
            return Err(invalid_response(
                "Chaoxing SIGNIN QR has an unsupported source",
            ));
        }

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

    #[cfg(test)]
    fn enc(&self) -> &str {
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
    let names = match source {
        ChaoxingQrSignMaterialSource::HttpsUrl => ["activeId", "id"].as_slice(),
        ChaoxingQrSignMaterialSource::SignInPayload => ["aid"].as_slice(),
    };
    let values = names
        .iter()
        .filter_map(|name| fields.get(*name))
        .collect::<Vec<_>>();
    if values.len() != 1 || !valid_activity_id(values[0]) {
        return Err(invalid_response(
            "Chaoxing sign-in QR activity identity is missing or ambiguous",
        ));
    }
    Ok(values[0].clone())
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
            "https://mobilelearn.chaoxing.com/path?activeId=7003&enc=one&enc=two",
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
}
