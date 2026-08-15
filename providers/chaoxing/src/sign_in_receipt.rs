use std::fmt;

use asterism_provider_api::{ProviderError, ProviderErrorKind, ProviderResult};
use scraper::{Html, Selector};
use sha2::{Digest, Sha256};
use zeroize::Zeroize;

use crate::ChaoxingNormalSignPreparation;

const MAX_PRE_SIGN_DOCUMENT_BYTES: usize = 512 * 1_024;
const MAX_SIGN_RECEIPT_BYTES: usize = 8 * 1_024;

/// Exact pre-sign evidence supported by the audited `#statuscontent` element.
///
/// `NoCompletionMarker` is deliberately not called ready or eligible. It only
/// means the pre-sign page did not contain the donor-observed completed marker.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChaoxingPreSignEvidenceKind {
    NoCompletionMarker,
    AlreadySigned,
}

/// One preparation-bound pre-sign observation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChaoxingPreSignEvidence {
    remote_id: String,
    preparation_digest: String,
    document_digest: String,
    kind: ChaoxingPreSignEvidenceKind,
}

impl ChaoxingPreSignEvidence {
    pub fn remote_id(&self) -> &str {
        &self.remote_id
    }

    pub fn preparation_digest(&self) -> &str {
        &self.preparation_digest
    }

    pub fn document_digest(&self) -> &str {
        &self.document_digest
    }

    pub const fn kind(&self) -> ChaoxingPreSignEvidenceKind {
        self.kind
    }
}

/// Donor-observed `stuSignajax` response classes.
///
/// These are transport receipts only. In particular, `Accepted` does not prove
/// completion and must be followed by an independent account-bound readback.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChaoxingSignReceiptKind {
    Accepted,
    AlreadySigned,
    WindowClosed,
}

/// One immutable response receipt bound to the exact prepared request digest.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChaoxingSignReceipt {
    remote_id: String,
    preparation_digest: String,
    response_digest: String,
    kind: ChaoxingSignReceiptKind,
}

impl ChaoxingSignReceipt {
    pub fn remote_id(&self) -> &str {
        &self.remote_id
    }

    pub fn preparation_digest(&self) -> &str {
        &self.preparation_digest
    }

    pub fn response_digest(&self) -> &str {
        &self.response_digest
    }

    pub const fn kind(&self) -> ChaoxingSignReceiptKind {
        self.kind
    }
}

/// A bounded pre-sign HTML response tied to one immutable preparation.
pub struct ChaoxingPreSignDocument {
    remote_id: String,
    preparation_digest: String,
    document: String,
}

impl ChaoxingPreSignDocument {
    /// Binds one complete pre-sign response to the request preparation that
    /// would have produced it.
    ///
    /// # Errors
    ///
    /// Returns an invalid-response error for an empty or oversized document.
    pub fn for_preparation(
        preparation: &ChaoxingNormalSignPreparation,
        document: impl Into<String>,
    ) -> ProviderResult<Self> {
        let document = bounded_document(
            document,
            MAX_PRE_SIGN_DOCUMENT_BYTES,
            "Chaoxing pre-sign document is empty or exceeds the size limit",
        )?;
        Ok(Self {
            remote_id: preparation.remote_id().to_owned(),
            preparation_digest: preparation.request_digest().to_owned(),
            document,
        })
    }
}

impl fmt::Debug for ChaoxingPreSignDocument {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ChaoxingPreSignDocument")
            .field("remote_id", &self.remote_id)
            .field("preparation_digest", &self.preparation_digest)
            .field("document", &"[REDACTED]")
            .finish()
    }
}

impl Drop for ChaoxingPreSignDocument {
    fn drop(&mut self) {
        self.document.zeroize();
    }
}

/// A bounded `stuSignajax` body tied to one immutable preparation.
pub struct ChaoxingSignReceiptDocument {
    remote_id: String,
    preparation_digest: String,
    response: String,
}

impl ChaoxingSignReceiptDocument {
    /// Binds one complete response body to the prepared mutation.
    ///
    /// # Errors
    ///
    /// Returns an invalid-response error for an empty or oversized body.
    pub fn for_preparation(
        preparation: &ChaoxingNormalSignPreparation,
        response: impl Into<String>,
    ) -> ProviderResult<Self> {
        let response = bounded_document(
            response,
            MAX_SIGN_RECEIPT_BYTES,
            "Chaoxing sign-in receipt is empty or exceeds the size limit",
        )?;
        Ok(Self {
            remote_id: preparation.remote_id().to_owned(),
            preparation_digest: preparation.request_digest().to_owned(),
            response,
        })
    }
}

impl fmt::Debug for ChaoxingSignReceiptDocument {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ChaoxingSignReceiptDocument")
            .field("remote_id", &self.remote_id)
            .field("preparation_digest", &self.preparation_digest)
            .field("response", &"[REDACTED]")
            .finish()
    }
}

impl Drop for ChaoxingSignReceiptDocument {
    fn drop(&mut self) {
        self.response.zeroize();
    }
}

/// Parses the sole donor-observed pre-sign status element.
///
/// # Errors
///
/// Fails closed if `#statuscontent` is missing, duplicated, or contains any
/// non-empty value other than the exact completed marker.
pub fn parse_pre_sign_evidence(
    document: &ChaoxingPreSignDocument,
) -> ProviderResult<ChaoxingPreSignEvidence> {
    let html = Html::parse_document(&document.document);
    let selector = Selector::parse("#statuscontent").map_err(|_| {
        ProviderError::new(
            ProviderErrorKind::Internal,
            "Chaoxing pre-sign selector initialization failed",
        )
    })?;
    let mut elements = html.select(&selector);
    let element = elements
        .next()
        .ok_or_else(|| protocol_drift("Chaoxing pre-sign document has no statuscontent element"))?;
    if elements.next().is_some() {
        return Err(protocol_drift(
            "Chaoxing pre-sign document has duplicate statuscontent elements",
        ));
    }
    let mut status = element
        .text()
        .flat_map(str::chars)
        .filter(|character| !character.is_whitespace())
        .collect::<String>();
    let kind = match status.as_str() {
        "" => ChaoxingPreSignEvidenceKind::NoCompletionMarker,
        "签到成功" => ChaoxingPreSignEvidenceKind::AlreadySigned,
        _ => {
            status.zeroize();
            return Err(protocol_drift(
                "Chaoxing pre-sign statuscontent has an unknown value",
            ));
        }
    };
    status.zeroize();
    Ok(ChaoxingPreSignEvidence {
        remote_id: document.remote_id.clone(),
        preparation_digest: document.preparation_digest.clone(),
        document_digest: digest(document.document.as_bytes()),
        kind,
    })
}

/// Classifies only the exact donor-observed `stuSignajax` response strings.
///
/// # Errors
///
/// Any other body is protocol drift rather than an inferred rejection or
/// success. No receipt class is promoted to verified completion.
pub fn parse_sign_receipt(
    document: &ChaoxingSignReceiptDocument,
) -> ProviderResult<ChaoxingSignReceipt> {
    let kind = match document.response.trim() {
        "success" => ChaoxingSignReceiptKind::Accepted,
        "您已签到过了" => ChaoxingSignReceiptKind::AlreadySigned,
        "success2" => ChaoxingSignReceiptKind::WindowClosed,
        _ => {
            return Err(protocol_drift(
                "Chaoxing sign-in receipt has an unknown response value",
            ));
        }
    };
    Ok(ChaoxingSignReceipt {
        remote_id: document.remote_id.clone(),
        preparation_digest: document.preparation_digest.clone(),
        response_digest: digest(document.response.as_bytes()),
        kind,
    })
}

fn bounded_document(
    document: impl Into<String>,
    maximum_bytes: usize,
    message: &'static str,
) -> ProviderResult<String> {
    let mut document = document.into();
    if document.is_empty() || document.len() > maximum_bytes {
        document.zeroize();
        return Err(ProviderError::new(
            ProviderErrorKind::InvalidResponse,
            message,
        ));
    }
    Ok(document)
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
    use crate::{
        ChaoxingCourseScope, ChaoxingSignActivityListDocument, ChaoxingSignDetailDocument,
        ChaoxingSignDetailRequest, parse_sign_activity_list, parse_sign_detail,
    };

    const LIST_FIXTURE: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fixtures/providers/chaoxing/sign/activities-mixed.json"
    ));
    const DETAIL_FIXTURE: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fixtures/providers/chaoxing/sign/detail-normal.json"
    ));
    const PRE_SIGN_EMPTY: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fixtures/providers/chaoxing/sign/pre-sign-no-completion.html"
    ));
    const PRE_SIGN_COMPLETED: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fixtures/providers/chaoxing/sign/pre-sign-completed.html"
    ));

    #[test]
    fn pre_sign_evidence_is_bound_and_never_infers_eligibility() {
        let preparation = preparation();
        for (fixture, expected) in [
            (
                PRE_SIGN_EMPTY,
                ChaoxingPreSignEvidenceKind::NoCompletionMarker,
            ),
            (
                PRE_SIGN_COMPLETED,
                ChaoxingPreSignEvidenceKind::AlreadySigned,
            ),
        ] {
            let document = ChaoxingPreSignDocument::for_preparation(&preparation, fixture).unwrap();
            let evidence = parse_pre_sign_evidence(&document).unwrap();
            assert_eq!(evidence.remote_id(), preparation.remote_id());
            assert_eq!(evidence.preparation_digest(), preparation.request_digest());
            assert_eq!(evidence.document_digest().len(), 64);
            assert_eq!(evidence.kind(), expected);
        }
    }

    #[test]
    fn receipt_classes_are_bound_but_not_completion() {
        let preparation = preparation();
        for (fixture, expected) in [
            (
                include_str!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../../fixtures/providers/chaoxing/sign/receipt-accepted.txt"
                )),
                ChaoxingSignReceiptKind::Accepted,
            ),
            (
                include_str!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../../fixtures/providers/chaoxing/sign/receipt-already-signed.txt"
                )),
                ChaoxingSignReceiptKind::AlreadySigned,
            ),
            (
                include_str!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../../fixtures/providers/chaoxing/sign/receipt-window-closed.txt"
                )),
                ChaoxingSignReceiptKind::WindowClosed,
            ),
        ] {
            let document =
                ChaoxingSignReceiptDocument::for_preparation(&preparation, fixture).unwrap();
            let receipt = parse_sign_receipt(&document).unwrap();
            assert_eq!(receipt.remote_id(), preparation.remote_id());
            assert_eq!(receipt.preparation_digest(), preparation.request_digest());
            assert_eq!(receipt.response_digest().len(), 64);
            assert_eq!(receipt.kind(), expected);
        }
    }

    #[test]
    fn unknown_pre_sign_or_receipt_values_fail_closed() {
        let preparation = preparation();
        for html in [
            "<html><body></body></html>",
            "<div id='statuscontent'>unexpected</div>",
            "<div id='statuscontent'></div><div id='statuscontent'></div>",
        ] {
            let document = ChaoxingPreSignDocument::for_preparation(&preparation, html).unwrap();
            let error = parse_pre_sign_evidence(&document).unwrap_err();
            assert_eq!(error.kind, ProviderErrorKind::ProtocolDrift);
        }
        for response in ["ok", "failed", "success PRIVATE_DETAIL"] {
            let document =
                ChaoxingSignReceiptDocument::for_preparation(&preparation, response).unwrap();
            let error = parse_sign_receipt(&document).unwrap_err();
            assert_eq!(error.kind, ProviderErrorKind::ProtocolDrift);
        }
    }

    #[test]
    fn documents_are_bounded_zeroizing_and_redacted() {
        let preparation = preparation();
        let error = ChaoxingPreSignDocument::for_preparation(
            &preparation,
            "x".repeat(MAX_PRE_SIGN_DOCUMENT_BYTES + 1),
        )
        .unwrap_err();
        assert_eq!(error.kind, ProviderErrorKind::InvalidResponse);
        let document =
            ChaoxingSignReceiptDocument::for_preparation(&preparation, "PRIVATE_RESPONSE").unwrap();
        assert!(!format!("{document:?}").contains("PRIVATE_RESPONSE"));
    }

    fn preparation() -> ChaoxingNormalSignPreparation {
        let scope = ChaoxingCourseScope::new("course:1001:2001", "1001", "2001").unwrap();
        let list = ChaoxingSignActivityListDocument::try_new(LIST_FIXTURE).unwrap();
        let activity = parse_sign_activity_list(&list, &scope).unwrap().remove(0);
        let request = ChaoxingSignDetailRequest::for_test(&activity);
        let detail = ChaoxingSignDetailDocument::try_new(DETAIL_FIXTURE).unwrap();
        let detail = parse_sign_detail(&detail, &request).unwrap();
        ChaoxingNormalSignPreparation::try_prepare(
            &activity,
            &detail,
            "9001",
            "4001",
            "Test Student",
        )
        .unwrap()
    }
}
