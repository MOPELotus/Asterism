use std::fmt;

use asterism_provider_api::{ProviderError, ProviderErrorKind, ProviderResult};
use asterism_secrets::SecretString;
use reqwest::Url;
use sha2::{Digest, Sha256};
use zeroize::Zeroize;

use crate::{
    ChaoxingNormalSignPreparation, ChaoxingSignActivity, ChaoxingSignDetail, ChaoxingSignVariant,
};

const ANALYSIS_BASE: &str = "https://mobilelearn.chaoxing.com/pptSign/analysis";
const ANALYSIS_CONTINUATION_BASE: &str = "https://mobilelearn.chaoxing.com/pptSign/analysis2";
const MAX_ANALYSIS_DOCUMENT_BYTES: usize = 512 * 1_024;
const MAX_ANALYSIS_CODE_BYTES: usize = 512;
const ANALYSIS_CODE_PREFIX: &str = "code='+'";

/// One immutable preparation for Ylim's separately observed `analysis` call.
///
/// The donor currently issues this auxiliary call from `preSign()` for every
/// variant, while its own comment calls the `analysis`/`analysis2` pair a
/// location-sign prerequisite. This value therefore records the exact observed
/// request without claiming it is a universal ordinary-sign requirement. It
/// exposes no plaintext query and performs no I/O.
pub struct ChaoxingPreSignAnalysisPreparation {
    remote_id: String,
    normal_preparation_digest: String,
    analysis_query: SecretString,
    request_digest: String,
}

impl ChaoxingPreSignAnalysisPreparation {
    /// Freezes the exact first auxiliary request against one ordinary-sign
    /// activity/detail/actor preparation.
    ///
    /// # Errors
    ///
    /// Returns `RemoteChanged` when any supplied snapshot or preparation no
    /// longer has the same exact binding, and `UnsupportedTask` for a non-normal
    /// sign-in. Success does not authorize this request to be sent.
    pub fn try_prepare(
        activity: &ChaoxingSignActivity,
        detail: &ChaoxingSignDetail,
        normal: &ChaoxingNormalSignPreparation,
    ) -> ProviderResult<Self> {
        if !normal.binding_matches(activity, detail) {
            return Err(remote_changed(
                "Chaoxing pre-sign analysis snapshots changed after ordinary preparation",
            ));
        }
        if activity.variant() != ChaoxingSignVariant::Normal
            || detail.variant() != ChaoxingSignVariant::Normal
        {
            return Err(ProviderError::new(
                ProviderErrorKind::UnsupportedTask,
                "Chaoxing pre-sign analysis preparation currently binds only ordinary sign-in",
            ));
        }

        let analysis_query = canonical_query(
            ANALYSIS_BASE,
            &[
                ("vs", "1"),
                ("DB_STRATEGY", "RANDOM"),
                ("aid", activity.activity_id()),
            ],
        )?;
        let request_digest = digest_parts(
            b"asterism.chaoxing.pre-sign-analysis-preparation.v1",
            &[
                normal.request_digest().as_bytes(),
                activity.fingerprint().as_bytes(),
                detail.fingerprint().as_bytes(),
                analysis_query.as_bytes(),
            ],
        );
        Ok(Self {
            remote_id: activity.remote_id().to_owned(),
            normal_preparation_digest: normal.request_digest().to_owned(),
            analysis_query: SecretString::new(analysis_query),
            request_digest,
        })
    }

    pub fn remote_id(&self) -> &str {
        &self.remote_id
    }

    pub const fn variant(&self) -> ChaoxingSignVariant {
        ChaoxingSignVariant::Normal
    }

    pub const fn analysis_route(&self) -> &'static str {
        ANALYSIS_BASE
    }

    pub fn normal_preparation_digest(&self) -> &str {
        &self.normal_preparation_digest
    }

    pub fn request_digest(&self) -> &str {
        &self.request_digest
    }

    pub fn prepared_request_count(&self) -> usize {
        usize::from(!self.analysis_query.expose_secret().is_empty())
    }
}

impl fmt::Debug for ChaoxingPreSignAnalysisPreparation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ChaoxingPreSignAnalysisPreparation")
            .field("remote_id", &self.remote_id)
            .field("normal_preparation_digest", &self.normal_preparation_digest)
            .field("analysis_query", &"[REDACTED]")
            .field("request_digest", &self.request_digest)
            .finish()
    }
}

/// One bounded raw response from the first donor-observed `analysis` call.
pub struct ChaoxingPreSignAnalysisDocument {
    remote_id: String,
    analysis_request_digest: String,
    document: String,
}

impl ChaoxingPreSignAnalysisDocument {
    /// Binds one complete response to the exact frozen `analysis` request.
    ///
    /// # Errors
    ///
    /// Rejects empty or oversized response documents.
    pub fn for_preparation(
        preparation: &ChaoxingPreSignAnalysisPreparation,
        document: impl Into<String>,
    ) -> ProviderResult<Self> {
        let mut document = document.into();
        if document.is_empty() || document.len() > MAX_ANALYSIS_DOCUMENT_BYTES {
            document.zeroize();
            return Err(invalid_response(
                "Chaoxing pre-sign analysis document is empty or oversized",
            ));
        }
        Ok(Self {
            remote_id: preparation.remote_id.clone(),
            analysis_request_digest: preparation.request_digest.clone(),
            document,
        })
    }
}

impl fmt::Debug for ChaoxingPreSignAnalysisDocument {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ChaoxingPreSignAnalysisDocument")
            .field("remote_id", &self.remote_id)
            .field("analysis_request_digest", &self.analysis_request_digest)
            .field("document", &"[REDACTED]")
            .finish()
    }
}

impl Drop for ChaoxingPreSignAnalysisDocument {
    fn drop(&mut self) {
        self.document.zeroize();
    }
}

/// Immutable preparation for the dynamic Ylim `analysis2` continuation.
///
/// The extracted `code` and complete query remain secret. This value is only a
/// donor-observed continuation template; it neither interprets the opaque
/// `analysis2` response nor grants mutation or replay authority.
pub struct ChaoxingPreSignAnalysisContinuation {
    remote_id: String,
    analysis_request_digest: String,
    analysis_response_digest: String,
    continuation_query: SecretString,
    request_digest: String,
}

impl ChaoxingPreSignAnalysisContinuation {
    pub fn remote_id(&self) -> &str {
        &self.remote_id
    }

    pub const fn variant(&self) -> ChaoxingSignVariant {
        ChaoxingSignVariant::Normal
    }

    pub const fn continuation_route(&self) -> &'static str {
        ANALYSIS_CONTINUATION_BASE
    }

    pub fn analysis_request_digest(&self) -> &str {
        &self.analysis_request_digest
    }

    pub fn analysis_response_digest(&self) -> &str {
        &self.analysis_response_digest
    }

    pub fn request_digest(&self) -> &str {
        &self.request_digest
    }

    pub fn prepared_request_count(&self) -> usize {
        usize::from(!self.continuation_query.expose_secret().is_empty())
    }
}

impl fmt::Debug for ChaoxingPreSignAnalysisContinuation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ChaoxingPreSignAnalysisContinuation")
            .field("remote_id", &self.remote_id)
            .field("analysis_request_digest", &self.analysis_request_digest)
            .field("analysis_response_digest", &self.analysis_response_digest)
            .field("continuation_query", &"[REDACTED]")
            .field("request_digest", &self.request_digest)
            .finish()
    }
}

/// Extracts the sole donor-observed dynamic code and freezes `analysis2`.
///
/// # Errors
///
/// Missing, duplicate, empty, oversized or control-character-bearing codes are
/// protocol drift. No success or completion meaning is assigned to the source
/// document or the resulting request.
pub fn parse_pre_sign_analysis_continuation(
    document: &ChaoxingPreSignAnalysisDocument,
) -> ProviderResult<ChaoxingPreSignAnalysisContinuation> {
    let mut code = extract_analysis_code(&document.document)?;
    let result = (|| {
        let continuation_query = canonical_query(
            ANALYSIS_CONTINUATION_BASE,
            &[("DB_STRATEGY", "RANDOM"), ("code", &code)],
        )?;
        let analysis_response_digest = digest_parts(
            b"asterism.chaoxing.pre-sign-analysis-response.v1",
            &[document.document.as_bytes()],
        );
        let request_digest = digest_parts(
            b"asterism.chaoxing.pre-sign-analysis-continuation.v1",
            &[
                document.analysis_request_digest.as_bytes(),
                analysis_response_digest.as_bytes(),
                continuation_query.as_bytes(),
            ],
        );
        Ok(ChaoxingPreSignAnalysisContinuation {
            remote_id: document.remote_id.clone(),
            analysis_request_digest: document.analysis_request_digest.clone(),
            analysis_response_digest,
            continuation_query: SecretString::new(continuation_query),
            request_digest,
        })
    })();
    code.zeroize();
    result
}

fn extract_analysis_code(document: &str) -> ProviderResult<String> {
    let mut markers = document.match_indices(ANALYSIS_CODE_PREFIX);
    let (start, _) = markers
        .next()
        .ok_or_else(|| protocol_drift("Chaoxing pre-sign analysis code marker is missing"))?;
    if markers.next().is_some() {
        return Err(protocol_drift(
            "Chaoxing pre-sign analysis code marker is duplicated",
        ));
    }
    let value = &document[start + ANALYSIS_CODE_PREFIX.len()..];
    let end = value
        .find('\'')
        .ok_or_else(|| protocol_drift("Chaoxing pre-sign analysis code is unterminated"))?;
    let mut code = value[..end].to_owned();
    if code.is_empty() || code.len() > MAX_ANALYSIS_CODE_BYTES || code.chars().any(char::is_control)
    {
        code.zeroize();
        return Err(protocol_drift(
            "Chaoxing pre-sign analysis code is empty or unsafe",
        ));
    }
    Ok(code)
}

fn canonical_query(base: &str, pairs: &[(&str, &str)]) -> ProviderResult<String> {
    let mut url = Url::parse(base).map_err(|_| {
        ProviderError::new(
            ProviderErrorKind::Internal,
            "Chaoxing pre-sign analysis static route is invalid",
        )
    })?;
    url.query_pairs_mut().extend_pairs(pairs.iter().copied());
    url.query().map(str::to_owned).ok_or_else(|| {
        ProviderError::new(
            ProviderErrorKind::Internal,
            "Chaoxing pre-sign analysis query preparation failed",
        )
    })
}

fn digest_parts(domain: &[u8], parts: &[&[u8]]) -> String {
    let mut digest = Sha256::new();
    digest.update(domain);
    for part in parts {
        digest.update(
            u64::try_from(part.len())
                .expect("Chaoxing pre-sign analysis digest fields fit into u64")
                .to_be_bytes(),
        );
        digest.update(part);
    }
    format!("{:x}", digest.finalize())
}

fn remote_changed(message: &'static str) -> ProviderError {
    ProviderError::new(ProviderErrorKind::RemoteChanged, message)
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

    use super::*;
    use crate::{
        ChaoxingCourseScope, ChaoxingSignActivityListDocument, ChaoxingSignDetailDocument,
        ChaoxingSignDetailRequest, parse_sign_activity_list, parse_sign_detail,
    };

    const ACTIVITIES: &str =
        include_str!("../../../fixtures/providers/chaoxing/sign/activities-mixed.json");
    const DETAIL: &str =
        include_str!("../../../fixtures/providers/chaoxing/sign/detail-normal.json");
    const ANALYSIS: &str =
        include_str!("../../../fixtures/providers/chaoxing/sign/pre-sign-analysis.html");

    #[test]
    fn freezes_exact_donor_analysis_handoff_without_sending() {
        let (activity, detail, normal) = binding();
        let preparation =
            ChaoxingPreSignAnalysisPreparation::try_prepare(&activity, &detail, &normal).unwrap();

        assert_eq!(preparation.remote_id(), activity.remote_id());
        assert_eq!(preparation.variant(), ChaoxingSignVariant::Normal);
        assert_eq!(preparation.analysis_route(), ANALYSIS_BASE);
        assert_eq!(preparation.prepared_request_count(), 1);
        assert_eq!(
            preparation.normal_preparation_digest(),
            normal.request_digest()
        );
        assert_eq!(
            query_map(preparation.analysis_query.expose_secret()),
            BTreeMap::from([
                ("DB_STRATEGY".to_owned(), "RANDOM".to_owned()),
                ("aid".to_owned(), "7001".to_owned()),
                ("vs".to_owned(), "1".to_owned()),
            ])
        );

        let document =
            ChaoxingPreSignAnalysisDocument::for_preparation(&preparation, ANALYSIS).unwrap();
        let continuation = parse_pre_sign_analysis_continuation(&document).unwrap();
        assert_eq!(continuation.remote_id(), activity.remote_id());
        assert_eq!(continuation.variant(), ChaoxingSignVariant::Normal);
        assert_eq!(
            continuation.continuation_route(),
            ANALYSIS_CONTINUATION_BASE
        );
        assert_eq!(continuation.prepared_request_count(), 1);
        assert_eq!(
            continuation.analysis_request_digest(),
            preparation.request_digest()
        );
        assert_eq!(continuation.analysis_response_digest().len(), 64);
        assert_eq!(continuation.request_digest().len(), 64);
        assert_eq!(
            query_map(continuation.continuation_query.expose_secret()),
            BTreeMap::from([
                ("DB_STRATEGY".to_owned(), "RANDOM".to_owned()),
                ("code".to_owned(), "PRIVATE_ANALYSIS_CODE".to_owned()),
            ])
        );

        for debug in [
            format!("{preparation:?}"),
            format!("{document:?}"),
            format!("{continuation:?}"),
        ] {
            assert!(!debug.contains("PRIVATE_ANALYSIS_CODE"));
            assert!(!debug.contains("9001"));
        }
    }

    #[test]
    fn analysis_handoff_is_deterministic_and_fresh_binding_bound() {
        let (activity, detail, normal) = binding();
        let first =
            ChaoxingPreSignAnalysisPreparation::try_prepare(&activity, &detail, &normal).unwrap();
        let same =
            ChaoxingPreSignAnalysisPreparation::try_prepare(&activity, &detail, &normal).unwrap();
        assert_eq!(first.request_digest(), same.request_digest());

        let first_document =
            ChaoxingPreSignAnalysisDocument::for_preparation(&first, ANALYSIS).unwrap();
        let same_document =
            ChaoxingPreSignAnalysisDocument::for_preparation(&same, ANALYSIS).unwrap();
        assert_eq!(
            parse_pre_sign_analysis_continuation(&first_document)
                .unwrap()
                .request_digest(),
            parse_pre_sign_analysis_continuation(&same_document)
                .unwrap()
                .request_digest()
        );

        let request = ChaoxingSignDetailRequest::for_test(&activity);
        let changed = DETAIL.replace("\"status\": 1", "\"status\": 2");
        let changed = ChaoxingSignDetailDocument::try_new(changed).unwrap();
        let changed = parse_sign_detail(&changed, &request).unwrap();
        assert_eq!(
            ChaoxingPreSignAnalysisPreparation::try_prepare(&activity, &changed, &normal)
                .unwrap_err()
                .kind,
            ProviderErrorKind::RemoteChanged
        );
    }

    #[test]
    fn malformed_analysis_documents_fail_closed() {
        let (activity, detail, normal) = binding();
        let preparation =
            ChaoxingPreSignAnalysisPreparation::try_prepare(&activity, &detail, &normal).unwrap();
        for document in [
            "<script>const code = 'missing-marker';</script>",
            "<script>const code='+'FIRST'; const other=\"code='+'SECOND'\";</script>",
            "<script>const code='+';</script>",
            "<script>const code='+'';</script>",
            "<script>const code='+'unterminated;</script>",
            "<script>const code='+'unsafe\ncode';</script>",
        ] {
            let document =
                ChaoxingPreSignAnalysisDocument::for_preparation(&preparation, document).unwrap();
            assert_eq!(
                parse_pre_sign_analysis_continuation(&document)
                    .unwrap_err()
                    .kind,
                ProviderErrorKind::ProtocolDrift
            );
        }
        assert_eq!(
            ChaoxingPreSignAnalysisDocument::for_preparation(
                &preparation,
                "x".repeat(MAX_ANALYSIS_DOCUMENT_BYTES + 1),
            )
            .unwrap_err()
            .kind,
            ProviderErrorKind::InvalidResponse
        );
    }

    fn binding() -> (
        ChaoxingSignActivity,
        ChaoxingSignDetail,
        ChaoxingNormalSignPreparation,
    ) {
        let scope = ChaoxingCourseScope::new("course:1001:2001", "1001", "2001").unwrap();
        let list = ChaoxingSignActivityListDocument::try_new(ACTIVITIES).unwrap();
        let activity = parse_sign_activity_list(&list, &scope).unwrap().remove(0);
        let request = ChaoxingSignDetailRequest::for_test(&activity);
        let detail = ChaoxingSignDetailDocument::try_new(DETAIL).unwrap();
        let detail = parse_sign_detail(&detail, &request).unwrap();
        let preparation = ChaoxingNormalSignPreparation::try_prepare(
            &activity,
            &detail,
            "9001",
            "4001",
            "Test Student",
        )
        .unwrap();
        (activity, detail, preparation)
    }

    fn query_map(query: &str) -> BTreeMap<String, String> {
        Url::parse(&format!("https://example.invalid/?{query}"))
            .unwrap()
            .query_pairs()
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect()
    }
}
