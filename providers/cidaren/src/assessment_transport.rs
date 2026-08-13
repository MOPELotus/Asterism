use crate::{
    CidarenAssessmentBinding, CidarenAssessmentResponse, CidarenWireAnswer,
    CidarenWordSelectionPlan,
};
use asterism_provider_api::{ProviderContext, ProviderResult};
use async_trait::async_trait;

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
        binding: &CidarenAssessmentBinding,
    ) -> ProviderResult<CidarenAssessmentResponse>;

    async fn verify_answer(
        &self,
        context: &ProviderContext,
        binding: &CidarenAssessmentBinding,
        topic_code: &str,
        answer: &CidarenWireAnswer,
    ) -> ProviderResult<CidarenAssessmentResponse>;

    async fn submit_answer_and_save(
        &self,
        context: &ProviderContext,
        binding: &CidarenAssessmentBinding,
        topic_code: &str,
        time_spent_millis: u64,
    ) -> ProviderResult<CidarenAssessmentResponse>;

    async fn skip_answer(
        &self,
        context: &ProviderContext,
        binding: &CidarenAssessmentBinding,
        topic_code: &str,
        time_spent_millis: u64,
    ) -> ProviderResult<CidarenAssessmentResponse>;

    async fn submit_chose_word(
        &self,
        context: &ProviderContext,
        binding: &CidarenAssessmentBinding,
        plan: &CidarenWordSelectionPlan,
    ) -> ProviderResult<CidarenAssessmentResponse>;
}
