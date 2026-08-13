use asterism_provider_api::{ProviderContext, ProviderResult, RemoteTaskDetail};
use async_trait::async_trait;

use crate::{
    CidarenAnswerEvidenceBinding, CidarenWordEvidence, CidarenWordInventory, CidarenWordLookup,
};

/// Provider-private native boundary for the donor-observed Cidaren answer
/// evidence lifecycle.
///
/// Binding always re-reads the complete `StudyTask/List` for the freshly bound
/// Task Course. Inventory and word-info calls then accept only typed bindings
/// produced by that read, preventing arbitrary cross-account/Course routing.
#[async_trait]
pub trait CidarenAnswerEvidenceTransport: Send + Sync {
    async fn bind_answer_evidence(
        &self,
        context: &ProviderContext,
        remote_task_id: &str,
        detail: &RemoteTaskDetail,
    ) -> ProviderResult<CidarenAnswerEvidenceBinding>;

    async fn fetch_word_inventory(
        &self,
        context: &ProviderContext,
        binding: &CidarenAnswerEvidenceBinding,
    ) -> ProviderResult<CidarenWordInventory>;

    async fn fetch_word_evidence(
        &self,
        context: &ProviderContext,
        lookup: &CidarenWordLookup,
    ) -> ProviderResult<CidarenWordEvidence>;

    async fn resolve_word_prototype(
        &self,
        context: &ProviderContext,
        word: &str,
    ) -> ProviderResult<Option<String>>;
}
