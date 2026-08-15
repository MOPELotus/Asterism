use std::fmt;

use asterism_provider_api::{ProviderError, ProviderErrorKind, ProviderResult};
use asterism_secrets::SecretString;
use reqwest::Url;
use sha2::{Digest, Sha256};
use zeroize::Zeroize;

use crate::{ChaoxingSignActivity, ChaoxingSignDetail, ChaoxingSignVariant};

const PRE_SIGN_BASE: &str = "https://mobilelearn.chaoxing.com/newsign/preSign";
const SUBMISSION_BASE: &str = "https://mobilelearn.chaoxing.com/pptSign/stuSignajax";
const MAX_ACTOR_ID_BYTES: usize = 128;
const MAX_ACTOR_NAME_BYTES: usize = 256;
const REQUEST_DIGEST_DOMAIN: &[u8] = b"asterism.chaoxing.normal-sign-preparation.v1\0";

/// An immutable, unregistered preparation for the donor-corroborated ordinary
/// sign-in request shape.
///
/// This value is not durable mutation authority. It exposes no send method,
/// performs no HTTP request, assigns no meaning to Provider status codes, and
/// must not be interpreted as a receipt or completion result.
pub struct ChaoxingNormalSignPreparation {
    remote_id: String,
    discovery_fingerprint: String,
    detail_fingerprint: String,
    pre_sign_query: SecretString,
    submission_query: SecretString,
    request_digest: String,
}

impl ChaoxingNormalSignPreparation {
    /// Freezes one ordinary sign-in request pair from an exact activity/detail
    /// binding and explicit actor identity.
    ///
    /// # Errors
    ///
    /// Returns `RemoteChanged` when the activity and detail snapshots are not
    /// the same discovery, `UnsupportedTask` for every non-normal variant, and
    /// `InvalidResponse` for malformed actor fields. A successful preparation
    /// still does not authorize either request to be sent.
    pub fn try_prepare(
        activity: &ChaoxingSignActivity,
        detail: &ChaoxingSignDetail,
        uid: impl Into<String>,
        fid: impl Into<String>,
        name: impl Into<String>,
    ) -> ProviderResult<Self> {
        if activity.remote_id() != detail.remote_id()
            || activity.fingerprint() != detail.discovery_fingerprint()
        {
            return Err(ProviderError::new(
                ProviderErrorKind::RemoteChanged,
                "Chaoxing sign-in activity and detail snapshots are not bound",
            ));
        }
        if activity.variant() != ChaoxingSignVariant::Normal
            || detail.variant() != ChaoxingSignVariant::Normal
        {
            return Err(ProviderError::new(
                ProviderErrorKind::UnsupportedTask,
                "Chaoxing sign-in variant has no unambiguous immutable preparation",
            ));
        }

        let mut uid = uid.into();
        let mut fid = fid.into();
        let mut name = name.into();
        let result = (|| {
            validate_actor_id(&uid)?;
            validate_actor_id(&fid)?;
            validate_actor_name(&name)?;

            let pre_sign_query = canonical_query(
                PRE_SIGN_BASE,
                &[
                    ("courseId", activity.course_id()),
                    ("classId", activity.class_id()),
                    ("activePrimaryId", activity.activity_id()),
                    ("general", "1"),
                    ("sys", "1"),
                    ("ls", "1"),
                    ("appType", "15"),
                    ("tid", ""),
                    ("uid", &uid),
                    ("ut", "s"),
                    ("isTeacherViewOpen", "0"),
                ],
            )?;
            let submission_query = canonical_query(
                SUBMISSION_BASE,
                &[
                    ("activeId", activity.activity_id()),
                    ("uid", &uid),
                    ("clientip", ""),
                    ("latitude", "-1"),
                    ("longitude", "-1"),
                    ("appType", "15"),
                    ("fid", &fid),
                    ("name", &name),
                ],
            )?;
            let request_digest = request_digest(
                activity,
                detail,
                pre_sign_query.as_bytes(),
                submission_query.as_bytes(),
            );
            Ok(Self {
                remote_id: activity.remote_id().to_owned(),
                discovery_fingerprint: activity.fingerprint().to_owned(),
                detail_fingerprint: detail.fingerprint().to_owned(),
                pre_sign_query: SecretString::new(pre_sign_query),
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
        ChaoxingSignVariant::Normal
    }

    pub const fn pre_sign_route(&self) -> &'static str {
        PRE_SIGN_BASE
    }

    pub const fn submission_route(&self) -> &'static str {
        SUBMISSION_BASE
    }

    pub fn request_digest(&self) -> &str {
        &self.request_digest
    }

    /// Returns the number of frozen request templates. A valid ordinary sign
    /// preparation always contains separate pre-sign and submission templates.
    pub fn prepared_request_count(&self) -> usize {
        [
            self.pre_sign_query.expose_secret(),
            self.submission_query.expose_secret(),
        ]
        .into_iter()
        .filter(|query| !query.is_empty())
        .count()
    }
}

impl fmt::Debug for ChaoxingNormalSignPreparation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ChaoxingNormalSignPreparation")
            .field("remote_id", &self.remote_id)
            .field("discovery_fingerprint", &self.discovery_fingerprint)
            .field("detail_fingerprint", &self.detail_fingerprint)
            .field("pre_sign_query", &"[REDACTED]")
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
        return Err(ProviderError::new(
            ProviderErrorKind::InvalidResponse,
            "Chaoxing ordinary sign-in actor identity is invalid",
        ));
    }
    Ok(())
}

fn validate_actor_name(value: &str) -> ProviderResult<()> {
    if value.trim().is_empty()
        || value.trim() != value
        || value.len() > MAX_ACTOR_NAME_BYTES
        || value.chars().any(char::is_control)
    {
        return Err(ProviderError::new(
            ProviderErrorKind::InvalidResponse,
            "Chaoxing ordinary sign-in actor name is invalid",
        ));
    }
    Ok(())
}

fn canonical_query(base: &str, pairs: &[(&str, &str)]) -> ProviderResult<String> {
    let mut url = Url::parse(base).map_err(|_| {
        ProviderError::new(
            ProviderErrorKind::Internal,
            "Chaoxing ordinary sign-in static route is invalid",
        )
    })?;
    url.query_pairs_mut().extend_pairs(pairs.iter().copied());
    url.query().map(str::to_owned).ok_or_else(|| {
        ProviderError::new(
            ProviderErrorKind::Internal,
            "Chaoxing ordinary sign-in query preparation failed",
        )
    })
}

fn request_digest(
    activity: &ChaoxingSignActivity,
    detail: &ChaoxingSignDetail,
    pre_sign_query: &[u8],
    submission_query: &[u8],
) -> String {
    let mut digest = Sha256::new();
    digest.update(REQUEST_DIGEST_DOMAIN);
    update_digest_field(&mut digest, activity.remote_id().as_bytes());
    update_digest_field(&mut digest, activity.fingerprint().as_bytes());
    update_digest_field(&mut digest, detail.fingerprint().as_bytes());
    update_digest_field(&mut digest, pre_sign_query);
    update_digest_field(&mut digest, submission_query);
    format!("{:x}", digest.finalize())
}

fn update_digest_field(digest: &mut Sha256, value: &[u8]) {
    digest.update(
        u64::try_from(value.len())
            .expect("sign-in preparation fields fit into u64")
            .to_be_bytes(),
    );
    digest.update(value);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ChaoxingCourseScope, ChaoxingSignActivityListDocument, ChaoxingSignDetailDocument,
        ChaoxingSignDetailRequest, parse_sign_activity_list, parse_sign_detail,
    };
    use std::collections::BTreeMap;

    const LIST_FIXTURE: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fixtures/providers/chaoxing/sign/activities-mixed.json"
    ));
    const DETAIL_FIXTURE: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fixtures/providers/chaoxing/sign/detail-normal.json"
    ));

    #[test]
    fn freezes_exact_normal_queries_without_sending() {
        let (activity, detail) = normal_snapshots();
        let preparation = ChaoxingNormalSignPreparation::try_prepare(
            &activity,
            &detail,
            "9001",
            "4001",
            "Test Student",
        )
        .unwrap();

        assert_eq!(preparation.remote_id(), activity.remote_id());
        assert_eq!(preparation.variant(), ChaoxingSignVariant::Normal);
        assert_eq!(preparation.pre_sign_route(), PRE_SIGN_BASE);
        assert_eq!(preparation.submission_route(), SUBMISSION_BASE);
        assert_eq!(preparation.prepared_request_count(), 2);
        assert_eq!(preparation.request_digest().len(), 64);
        assert_eq!(
            query_map(preparation.pre_sign_query.expose_secret()),
            BTreeMap::from([
                ("activePrimaryId".to_owned(), "7001".to_owned()),
                ("appType".to_owned(), "15".to_owned()),
                ("classId".to_owned(), "2001".to_owned()),
                ("courseId".to_owned(), "1001".to_owned()),
                ("general".to_owned(), "1".to_owned()),
                ("isTeacherViewOpen".to_owned(), "0".to_owned()),
                ("ls".to_owned(), "1".to_owned()),
                ("sys".to_owned(), "1".to_owned()),
                ("tid".to_owned(), String::new()),
                ("uid".to_owned(), "9001".to_owned()),
                ("ut".to_owned(), "s".to_owned()),
            ])
        );
        assert_eq!(
            query_map(preparation.submission_query.expose_secret()),
            BTreeMap::from([
                ("activeId".to_owned(), "7001".to_owned()),
                ("appType".to_owned(), "15".to_owned()),
                ("clientip".to_owned(), String::new()),
                ("fid".to_owned(), "4001".to_owned()),
                ("latitude".to_owned(), "-1".to_owned()),
                ("longitude".to_owned(), "-1".to_owned()),
                ("name".to_owned(), "Test Student".to_owned()),
                ("uid".to_owned(), "9001".to_owned()),
            ])
        );
        let debug = format!("{preparation:?}");
        assert!(!debug.contains("9001"));
        assert!(!debug.contains("Test Student"));
    }

    #[test]
    fn preparation_is_deterministic_and_actor_bound() {
        let (activity, detail) = normal_snapshots();
        let first = ChaoxingNormalSignPreparation::try_prepare(
            &activity, &detail, "9001", "4001", "Student",
        )
        .unwrap();
        let same = ChaoxingNormalSignPreparation::try_prepare(
            &activity, &detail, "9001", "4001", "Student",
        )
        .unwrap();
        let other = ChaoxingNormalSignPreparation::try_prepare(
            &activity, &detail, "9002", "4001", "Student",
        )
        .unwrap();
        assert_eq!(first.request_digest(), same.request_digest());
        assert_ne!(first.request_digest(), other.request_digest());
    }

    #[test]
    fn rejects_variant_snapshot_and_actor_drift() {
        let (normal, detail) = normal_snapshots();
        for (uid, fid, name) in [
            ("", "4001", "Student"),
            ("not-a-uid", "4001", "Student"),
            ("9001", "", "Student"),
            ("9001", "4001", "\n"),
        ] {
            let error =
                ChaoxingNormalSignPreparation::try_prepare(&normal, &detail, uid, fid, name)
                    .unwrap_err();
            assert_eq!(error.kind, ProviderErrorKind::InvalidResponse);
        }

        let scope = scope();
        let document = ChaoxingSignActivityListDocument::try_new(LIST_FIXTURE).unwrap();
        let activities = parse_sign_activity_list(&document, &scope).unwrap();
        let error = ChaoxingNormalSignPreparation::try_prepare(
            &activities[2],
            &detail,
            "9001",
            "4001",
            "Student",
        )
        .unwrap_err();
        assert_eq!(error.kind, ProviderErrorKind::RemoteChanged);
    }

    fn normal_snapshots() -> (ChaoxingSignActivity, ChaoxingSignDetail) {
        let scope = scope();
        let list = ChaoxingSignActivityListDocument::try_new(LIST_FIXTURE).unwrap();
        let activity = parse_sign_activity_list(&list, &scope).unwrap().remove(0);
        let request = ChaoxingSignDetailRequest::for_test(&activity);
        let detail = ChaoxingSignDetailDocument::try_new(DETAIL_FIXTURE).unwrap();
        let detail = parse_sign_detail(&detail, &request).unwrap();
        (activity, detail)
    }

    fn scope() -> ChaoxingCourseScope {
        ChaoxingCourseScope::new("course:1001:2001", "1001", "2001").unwrap()
    }

    fn query_map(query: &str) -> BTreeMap<String, String> {
        Url::parse(&format!("https://example.invalid/?{query}"))
            .unwrap()
            .query_pairs()
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect()
    }
}
