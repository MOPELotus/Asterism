use crate::{CidarenAssessmentResponse, CidarenMutationRequest, CidarenStartAnswerRequest};
use asterism_domain::Timestamp;
use asterism_provider_api::{ProviderContext, ProviderError, ProviderErrorKind, ProviderResult};
use async_trait::async_trait;

/// Exact bounded result of one prepared assessment request. The digest covers
/// the raw response bytes before parser sanitization and the timestamp records
/// when those bytes were received, not when Core later accepts the outcome.
pub struct CidarenAssessmentTransportOutcome {
    response: CidarenAssessmentResponse,
    response_digest: [u8; 32],
    received_at: Timestamp,
}

impl CidarenAssessmentTransportOutcome {
    /// Creates a transport outcome only when a non-zero raw response digest is
    /// available for Core's durable operation ledger.
    ///
    /// # Errors
    ///
    /// Returns `InvalidResponse` for a zero digest.
    pub fn try_new(
        response: CidarenAssessmentResponse,
        response_digest: [u8; 32],
        received_at: Timestamp,
    ) -> ProviderResult<Self> {
        if response_digest == [0; 32] {
            return Err(ProviderError::new(
                ProviderErrorKind::InvalidResponse,
                "Cidaren assessment response digest is empty",
            ));
        }
        Ok(Self {
            response,
            response_digest,
            received_at,
        })
    }

    pub(crate) fn into_parts(self) -> (CidarenAssessmentResponse, [u8; 32], Timestamp) {
        (self.response, self.response_digest, self.received_at)
    }
}

impl std::fmt::Debug for CidarenAssessmentTransportOutcome {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CidarenAssessmentTransportOutcome")
            .field("response", &self.response)
            .field("response_digest", &self.response_digest)
            .field("received_at", &self.received_at)
            .finish()
    }
}

/// Provider-private native boundary for the donor-observed Cidaren assessment
/// lifecycle.
///
/// Every method may mutate remote state. Core must persist the matching
/// attempt/operation intent before calling it and must never automatically
/// replay an ambiguous outcome. This trait is intentionally not substituted
/// for the read-only Question capability contracts.
#[async_trait]
pub trait CidarenAssessmentTransport: Send + Sync {
    async fn start_answer(
        &self,
        context: &ProviderContext,
        request: &CidarenStartAnswerRequest,
    ) -> ProviderResult<CidarenAssessmentTransportOutcome>;

    async fn verify_answer(
        &self,
        context: &ProviderContext,
        request: &CidarenMutationRequest,
    ) -> ProviderResult<CidarenAssessmentTransportOutcome>;

    async fn submit_answer_and_save(
        &self,
        context: &ProviderContext,
        request: &CidarenMutationRequest,
    ) -> ProviderResult<CidarenAssessmentTransportOutcome>;

    async fn skip_answer(
        &self,
        context: &ProviderContext,
        request: &CidarenMutationRequest,
    ) -> ProviderResult<CidarenAssessmentTransportOutcome>;

    async fn submit_chose_word(
        &self,
        context: &ProviderContext,
        request: &CidarenMutationRequest,
    ) -> ProviderResult<CidarenAssessmentTransportOutcome>;
}
