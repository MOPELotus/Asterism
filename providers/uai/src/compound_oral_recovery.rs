use std::{fmt, sync::Arc};

use asterism_domain::SubmissionDraft;
use asterism_provider_api::{
    ExecutionMutationRecoveryRecord, ExecutionMutationVerification, ProviderContext, ProviderError,
    ProviderErrorKind, ProviderResult, TaskDetailCapability,
};

use crate::{
    UaiAnswerTransport, UaiCompoundOralAttemptScope, UaiCompoundOralDraftAttemptBinding,
    UaiCompoundOralPlanState, UaiCompoundOralPreparation, UaiCompoundOralResultState,
    UaiCompoundOralSubmission, UaiCompoundOralSubmissionSequence, UaiCompoundOralVerification,
    build_compound_oral_submission_request,
};

/// Complete private plan owner that can only be created after current Task,
/// encrypted oral evidence and the exact issue-time request have all been
/// rebound. It grants readback only and never returns a reconstructed mutation
/// request.
pub struct UaiRecoveredCompoundOralPlan {
    plan_state: UaiCompoundOralPlanState,
}

impl UaiRecoveredCompoundOralPlan {
    pub const fn request_digest(&self) -> [u8; 32] {
        self.plan_state.request_digest()
    }

    pub fn remote_task_id(&self) -> &str {
        self.plan_state.submission().remote_task_id()
    }

    /// Parses exact receipt-versioned readback through the fully recovered
    /// fresh Task/oral/request chain.
    ///
    /// # Errors
    ///
    /// Rejects a foreign accepted result or changed matching/oral readback
    /// before producing mutation verification evidence.
    pub fn verify_readback(
        &self,
        result: &UaiCompoundOralResultState,
        document: &str,
    ) -> ProviderResult<(UaiCompoundOralVerification, ExecutionMutationVerification)> {
        result.verify_plan_state_readback(document, &self.plan_state)
    }
}

impl fmt::Debug for UaiRecoveredCompoundOralPlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UaiRecoveredCompoundOralPlan")
            .field("plan_state", &"[REDACTED]")
            .finish()
    }
}

/// Fully owner/account/Attempt-bound read-only compound-oral recovery owner.
/// It contains no transport sink or mutation request.
pub struct UaiRecoveredBoundCompoundOralPlan {
    binding: UaiCompoundOralDraftAttemptBinding,
    recovered: UaiRecoveredCompoundOralPlan,
    result: UaiCompoundOralResultState,
}

impl UaiRecoveredBoundCompoundOralPlan {
    pub const fn binding(&self) -> &UaiCompoundOralDraftAttemptBinding {
        &self.binding
    }

    pub const fn request_digest(&self) -> [u8; 32] {
        self.recovered.request_digest()
    }

    pub fn remote_task_id(&self) -> &str {
        self.recovered.remote_task_id()
    }

    /// Revalidates the exact receipt-versioned readback without dispatching a
    /// mutation.
    ///
    /// # Errors
    ///
    /// Rejects a foreign result or changed readback document.
    pub fn verify_readback(
        &self,
        document: &str,
    ) -> ProviderResult<(UaiCompoundOralVerification, ExecutionMutationVerification)> {
        self.recovered.verify_readback(&self.result, document)
    }
}

impl fmt::Debug for UaiRecoveredBoundCompoundOralPlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UaiRecoveredBoundCompoundOralPlan")
            .field("binding", &self.binding)
            .field("recovered", &self.recovered)
            .field("result", &"[REDACTED]")
            .finish()
    }
}

/// Provider-owned read-only recovery adapter. It freshly rebuilds the exact
/// semantic plan and issue-time request, but never dispatches either mutation.
#[derive(Clone)]
pub struct UaiCompoundOralRecovery {
    preparation: UaiCompoundOralPreparation,
}

impl UaiCompoundOralRecovery {
    pub fn new(
        details: Arc<dyn TaskDetailCapability>,
        answers: Arc<dyn UaiAnswerTransport>,
    ) -> Self {
        Self {
            preparation: UaiCompoundOralPreparation::new(details, answers),
        }
    }

    /// Freshly rediscovers the exact two-module Task and oral evidence before
    /// rebinding the recovered private plan to the original issue request.
    ///
    /// # Errors
    ///
    /// Rejects stale Task/Draft/answer material or another valid Course/account
    /// request. The rebuilt request is dropped without dispatch.
    pub async fn bind_plan_state(
        &self,
        context: &ProviderContext,
        ordinary_draft: &SubmissionDraft,
        plan_state: UaiCompoundOralPlanState,
        course_instance_id: &str,
        account_openid: &str,
    ) -> ProviderResult<UaiRecoveredCompoundOralPlan> {
        let fresh = self
            .preparation
            .prepare_submission(
                context,
                plan_state.submission().remote_task_id(),
                ordinary_draft,
            )
            .await?;
        bind_fresh_plan(plan_state, &fresh, course_instance_id, account_openid)
    }

    /// Validates the complete credential-free Draft/Attempt projection before
    /// any fresh Task/oral read, then performs the same read-only plan rebind.
    ///
    /// # Errors
    ///
    /// Rejects Core scope, state handle, issue/receipt/readback, result-version,
    /// Task or exact request drift. No mutation is dispatched.
    #[allow(
        clippy::too_many_arguments,
        reason = "recovery receives every independently persisted authority"
    )]
    pub async fn bind_verified_plan_state(
        &self,
        context: &ProviderContext,
        scope: &UaiCompoundOralAttemptScope,
        binding: &UaiCompoundOralDraftAttemptBinding,
        ordinary_draft: &SubmissionDraft,
        plan_state: UaiCompoundOralPlanState,
        sequence: &UaiCompoundOralSubmissionSequence,
        record: &ExecutionMutationRecoveryRecord,
        plan_state_digest: [u8; 32],
        course_instance_id: &str,
        account_openid: &str,
    ) -> ProviderResult<UaiRecoveredBoundCompoundOralPlan> {
        if scope.provider_account_id() != context.account_id {
            return Err(foreign_recovery_chain());
        }
        let result_state = UaiCompoundOralResultState::decode_recovery_record(sequence, record)?;
        let result_state_digest = record
            .stage_output()
            .map(asterism_provider_api::ExecutionMutationStageOutput::output_digest)
            .ok_or_else(foreign_recovery_chain)?;
        binding.validate_before_recovery(
            scope,
            sequence,
            &plan_state,
            &result_state,
            record,
            plan_state_digest,
            result_state_digest,
        )?;
        let recovered = self
            .bind_plan_state(
                context,
                ordinary_draft,
                plan_state,
                course_instance_id,
                account_openid,
            )
            .await?;
        Ok(UaiRecoveredBoundCompoundOralPlan {
            binding: binding.clone(),
            recovered,
            result: result_state,
        })
    }
}

impl fmt::Debug for UaiCompoundOralRecovery {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UaiCompoundOralRecovery")
            .field("preparation", &"configured; mutation replay forbidden")
            .finish()
    }
}

fn bind_fresh_plan(
    plan_state: UaiCompoundOralPlanState,
    fresh: &UaiCompoundOralSubmission,
    course_instance_id: &str,
    account_openid: &str,
) -> ProviderResult<UaiRecoveredCompoundOralPlan> {
    if fresh.plan_binding_digest()? != plan_state.submission().plan_binding_digest()? {
        return Err(foreign_recovery_chain());
    }
    let request =
        build_compound_oral_submission_request(fresh, course_instance_id, account_openid)?;
    plan_state.validate_request(&request)?;
    Ok(UaiRecoveredCompoundOralPlan { plan_state })
}

fn foreign_recovery_chain() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::ProtocolDrift,
        "UAI recovered compound oral chain is stale or foreign",
    )
}

#[cfg(test)]
mod tests {
    use asterism_domain::SubmissionDraftId;
    use serde_json::json;

    use super::*;
    use crate::{
        EncodedUaiCompoundOralPlanState, UaiCompoundOralSubmissionSequence,
        build_compound_oral_submission_request,
    };

    #[test]
    fn recovered_plan_rebinds_fresh_semantics_and_exact_account_request() {
        let draft_id = SubmissionDraftId::new();
        let original = UaiCompoundOralSubmission::fixture(
            draft_id,
            "right",
            json!(["spoken"]),
            Some(json!({"slot": 1})),
        );
        let sequence = UaiCompoundOralSubmissionSequence::try_new(&original).unwrap();
        let request =
            build_compound_oral_submission_request(&original, "course-instance-1", "openid-1")
                .unwrap();
        let encoded = EncodedUaiCompoundOralPlanState::try_new(&original, &request).unwrap();
        let digest = encoded.digest();
        let value = encoded.into_secret_value();
        let state = UaiCompoundOralPlanState::decode_bound(
            &value,
            digest,
            &sequence,
            request.request_digest(),
        )
        .unwrap();
        let fresh = UaiCompoundOralSubmission::fixture(
            draft_id,
            "right",
            json!(["spoken"]),
            Some(json!({"slot": 1})),
        );
        let recovered = bind_fresh_plan(state, &fresh, "course-instance-1", "openid-1").unwrap();
        assert_eq!(recovered.request_digest(), request.request_digest());
        assert_eq!(recovered.remote_task_id(), "group:2001:unit-1:group-oral");
        assert!(!format!("{recovered:?}").contains("spoken"));

        let encoded = EncodedUaiCompoundOralPlanState::try_new(&original, &request).unwrap();
        let digest = encoded.digest();
        let value = encoded.into_secret_value();
        let state = UaiCompoundOralPlanState::decode_bound(
            &value,
            digest,
            &sequence,
            request.request_digest(),
        )
        .unwrap();
        let changed = UaiCompoundOralSubmission::fixture(
            draft_id,
            "changed",
            json!(["spoken"]),
            Some(json!({"slot": 1})),
        );
        assert!(bind_fresh_plan(state, &changed, "course-instance-1", "openid-1").is_err());

        let encoded = EncodedUaiCompoundOralPlanState::try_new(&original, &request).unwrap();
        let digest = encoded.digest();
        let value = encoded.into_secret_value();
        let state = UaiCompoundOralPlanState::decode_bound(
            &value,
            digest,
            &sequence,
            request.request_digest(),
        )
        .unwrap();
        assert!(bind_fresh_plan(state, &fresh, "course-instance-2", "openid-1").is_err());
    }
}
