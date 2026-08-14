use asterism_domain::RemoteState;
use asterism_provider_api::{
    ExecutionEventSink, ProviderContext, ProviderError, ProviderErrorKind, ProviderResult,
};
use async_trait::async_trait;

use crate::{
    WellearnAtomicCompletionProfile, WellearnCmiDocument, WellearnDurationProtocolMode,
    WellearnResourceCompletionCmiFormat, WellearnResourceCompletionSequence,
    WellearnResourceCompletionTimeMode, WellearnResourceCompletionWriteMode,
    WellearnResourceExecutionPlan, WellearnResourceMutationProfile, parse_cmi_snapshot,
    runtime_settings::MAX_DURATION_REPORT_SECONDS,
};

/// Sanitized proof returned after exact fresh CMI verification of an atomic
/// duration-completion result.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WellearnAtomicDurationCompletionVerification {
    profile: WellearnAtomicCompletionProfile,
    score_percent: u8,
    time_preservation_verified: Option<bool>,
}

impl WellearnAtomicDurationCompletionVerification {
    pub const fn profile(self) -> WellearnAtomicCompletionProfile {
        self.profile
    }

    pub const fn score_percent(self) -> u8 {
        self.score_percent
    }

    pub const fn time_preservation_verified(self) -> Option<bool> {
        self.time_preservation_verified
    }
}

/// Verifies one completed atomic lifecycle from its independent fresh CMI
/// evidence without entering any mutation or transport path.
///
/// # Errors
///
/// Returns a protocol error for malformed CMI and `RemoteChanged` when the
/// exact completion/progress/score goal or current-donor time preservation is
/// not visible.
pub fn verify_atomic_duration_completion(
    plan: WellearnAtomicDurationCompletionPlan,
    documents: &WellearnAtomicDurationCompletionDocuments,
) -> ProviderResult<WellearnAtomicDurationCompletionVerification> {
    documents.validate_for_plan(plan)?;
    let final_snapshot = parse_cmi_snapshot(documents.after_completion().as_str())?;
    let score_percent = plan.completion().score_percent;
    let expected_score = score_percent.to_string();
    if !final_snapshot.cmi_present()
        || final_snapshot.remote_state() != RemoteState::Completed
        || final_snapshot.percent() != Some(100)
        || final_snapshot.score_scaled_raw() != Some(expected_score.as_str())
    {
        return Err(atomic_goal_changed());
    }

    let time_preservation_verified = match plan.profile() {
        WellearnAtomicCompletionProfile::FanyuchangFreshSetSave100 => {
            let after_duration = documents
                .after_duration()
                .ok_or_else(invalid_atomic_documents)?;
            let duration_snapshot = parse_cmi_snapshot(after_duration.as_str())?;
            if !duration_snapshot.cmi_present()
                || duration_snapshot.session_time_raw().is_none()
                || duration_snapshot.total_time_raw().is_none()
                || duration_snapshot.session_time_raw() != final_snapshot.session_time_raw()
                || duration_snapshot.total_time_raw() != final_snapshot.total_time_raw()
            {
                return Err(atomic_goal_changed());
            }
            Some(true)
        }
        WellearnAtomicCompletionProfile::AutoZeroTimeSaveOnly0 => None,
    };
    Ok(WellearnAtomicDurationCompletionVerification {
        profile: plan.profile(),
        score_percent,
        time_preservation_verified,
    })
}

fn atomic_goal_changed() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::RemoteChanged,
        "WELearn atomic duration-completion goal was not visible in fresh CMI",
    )
}

/// Ordered mutation receipts emitted by one authorized atomic lifecycle.
/// Explicit rejection remains diagnostic; only fresh CMI verification can
/// prove the final goal.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WellearnAtomicDurationCompletionReceipts {
    start_accepted: bool,
    heartbeat_acceptances: Vec<bool>,
    set_accepted: Option<bool>,
    save_accepted: bool,
}

impl WellearnAtomicDurationCompletionReceipts {
    pub const fn new(
        start_accepted: bool,
        heartbeat_acceptances: Vec<bool>,
        set_accepted: Option<bool>,
        save_accepted: bool,
    ) -> Self {
        Self {
            start_accepted,
            heartbeat_acceptances,
            set_accepted,
            save_accepted,
        }
    }

    pub const fn start_accepted(&self) -> bool {
        self.start_accepted
    }

    pub fn heartbeat_acceptances(&self) -> &[bool] {
        &self.heartbeat_acceptances
    }

    pub const fn set_accepted(&self) -> Option<bool> {
        self.set_accepted
    }

    pub const fn save_accepted(&self) -> bool {
        self.save_accepted
    }
}

/// Independent CMI evidence and mutation receipts returned by one complete
/// atomic duration-completion lifecycle.
///
/// This value does not grant execution authority. The intermediate
/// `after_duration` document exists only for current Fanyuchang, whose final
/// completion CMI must carry fresh time fields from the same operation.
#[derive(Debug)]
pub struct WellearnAtomicDurationCompletionDocuments {
    initial: WellearnCmiDocument,
    after_duration: Option<WellearnCmiDocument>,
    after_completion: WellearnCmiDocument,
    receipts: WellearnAtomicDurationCompletionReceipts,
}

impl WellearnAtomicDurationCompletionDocuments {
    /// Builds and validates a complete evidence bundle against the authorized
    /// Provider wire plan.
    ///
    /// # Errors
    ///
    /// Returns an internal error when the document slots or ordered mutation
    /// receipts cannot have been produced by the selected donor lifecycle.
    pub fn try_new(
        plan: WellearnAtomicDurationCompletionPlan,
        initial: WellearnCmiDocument,
        after_duration: Option<WellearnCmiDocument>,
        after_completion: WellearnCmiDocument,
        receipts: WellearnAtomicDurationCompletionReceipts,
    ) -> ProviderResult<Self> {
        let documents = Self {
            initial,
            after_duration,
            after_completion,
            receipts,
        };
        documents.validate_for_plan(plan)?;
        Ok(documents)
    }

    pub const fn initial(&self) -> &WellearnCmiDocument {
        &self.initial
    }

    pub const fn after_duration(&self) -> Option<&WellearnCmiDocument> {
        self.after_duration.as_ref()
    }

    pub const fn after_completion(&self) -> &WellearnCmiDocument {
        &self.after_completion
    }

    pub const fn receipts(&self) -> &WellearnAtomicDurationCompletionReceipts {
        &self.receipts
    }

    /// Revalidates an evidence bundle before parsing its CMI documents.
    ///
    /// # Errors
    ///
    /// Returns an internal error for a plan/document shape mismatch.
    pub fn validate_for_plan(
        &self,
        plan: WellearnAtomicDurationCompletionPlan,
    ) -> ProviderResult<()> {
        plan.validate()?;
        let valid = match plan.profile() {
            WellearnAtomicCompletionProfile::FanyuchangFreshSetSave100 => {
                self.after_duration.is_some()
                    && self.receipts.start_accepted
                    && self.receipts.set_accepted.is_some()
                    && valid_client_counter_receipts(
                        &self.receipts.heartbeat_acceptances,
                        plan.target_seconds(),
                    )
            }
            WellearnAtomicCompletionProfile::AutoZeroTimeSaveOnly0 => {
                let expected =
                    usize::try_from(plan.target_seconds() / plan.heartbeat_interval_seconds())
                        .map_err(|_| invalid_atomic_documents())?;
                self.after_duration.is_none()
                    && self.receipts.set_accepted.is_none()
                    && self.receipts.heartbeat_acceptances.len() == expected
            }
        };
        if !valid {
            return Err(invalid_atomic_documents());
        }
        Ok(())
    }
}

fn valid_client_counter_receipts(receipts: &[bool], target_seconds: u64) -> bool {
    let Ok(expected) = usize::try_from(target_seconds) else {
        return false;
    };
    if receipts.len() > expected {
        return false;
    }
    match receipts.iter().position(|accepted| !accepted) {
        Some(rejected) => rejected + 1 == receipts.len(),
        None => receipts.len() == expected,
    }
}

fn invalid_atomic_documents() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::Internal,
        "WELearn atomic duration-completion documents do not match the frozen lifecycle",
    )
}

/// Native boundary for one Core-authorized, one-start atomic
/// duration-completion lifecycle.
///
/// Implementations may renew authentication only before the first mutation or
/// for final read-only verification. They must not replay or resume mutations
/// after the first start request has been sent.
#[async_trait]
pub trait WellearnAtomicDurationCompletionTransport: Send + Sync {
    async fn complete_duration_atomically(
        &self,
        context: &ProviderContext,
        course_id: &str,
        sco_id: &str,
        plan: WellearnAtomicDurationCompletionPlan,
        events: &(dyn ExecutionEventSink + Send + Sync),
    ) -> ProviderResult<WellearnAtomicDurationCompletionDocuments>;
}

/// Complete immutable wire plan for one donor-audited, one-start
/// duration-completion operation.
///
/// Core owns persistence, attempt authority and scheduling. This value only
/// binds all `WELearn` protocol facts that the authorized Provider operation
/// must consume together.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WellearnAtomicDurationCompletionPlan {
    profile: WellearnAtomicCompletionProfile,
    target_seconds: u64,
    heartbeat_interval_seconds: u64,
    duration_protocol_mode: WellearnDurationProtocolMode,
    completion: WellearnResourceExecutionPlan,
}

impl WellearnAtomicDurationCompletionPlan {
    /// Builds the sole complete wire plan associated with the frozen atomic
    /// completion profile and target.
    ///
    /// # Errors
    ///
    /// Returns an internal error when the target is outside the profile's
    /// evidenced bounds. Modular Auto deliberately accepts a zero-second
    /// floor allocation, while current Fanyuchang requires at least one
    /// client-counter second.
    pub fn try_new(
        profile: WellearnAtomicCompletionProfile,
        target_seconds: u64,
    ) -> ProviderResult<Self> {
        use WellearnAtomicCompletionProfile::{AutoZeroTimeSaveOnly0, FanyuchangFreshSetSave100};

        let plan = match profile {
            FanyuchangFreshSetSave100 => Self {
                profile,
                target_seconds,
                heartbeat_interval_seconds: 1,
                duration_protocol_mode: WellearnDurationProtocolMode::ClientCounter,
                completion: WellearnResourceExecutionPlan {
                    score_percent: 100,
                    sequence: WellearnResourceCompletionSequence::SelectedScore,
                    time_mode: WellearnResourceCompletionTimeMode::PreserveFreshTime,
                    cmi_format: WellearnResourceCompletionCmiFormat::Json,
                    write_mode: WellearnResourceCompletionWriteMode::SetThenSave,
                    mutation_profile: WellearnResourceMutationProfile::CurrentFullSimpleReferer,
                },
            },
            AutoZeroTimeSaveOnly0 => Self {
                profile,
                target_seconds,
                heartbeat_interval_seconds: 60,
                duration_protocol_mode: WellearnDurationProtocolMode::ImplicitServer,
                completion: WellearnResourceExecutionPlan {
                    score_percent: 0,
                    sequence: WellearnResourceCompletionSequence::SelectedScore,
                    time_mode: WellearnResourceCompletionTimeMode::ZeroTime,
                    cmi_format: WellearnResourceCompletionCmiFormat::InteractionInfoSuffix,
                    write_mode: WellearnResourceCompletionWriteMode::SaveOnly,
                    mutation_profile: WellearnResourceMutationProfile::LegacyMinimalTaskReferer,
                },
            },
        };
        plan.validate()?;
        Ok(plan)
    }

    pub const fn profile(self) -> WellearnAtomicCompletionProfile {
        self.profile
    }

    pub const fn target_seconds(self) -> u64 {
        self.target_seconds
    }

    pub const fn heartbeat_interval_seconds(self) -> u64 {
        self.heartbeat_interval_seconds
    }

    pub const fn duration_protocol_mode(self) -> WellearnDurationProtocolMode {
        self.duration_protocol_mode
    }

    pub const fn completion(self) -> WellearnResourceExecutionPlan {
        self.completion
    }

    /// Revalidates a restored plan before any fresh discovery or mutation.
    ///
    /// # Errors
    ///
    /// Returns an internal error if any independently persisted field no
    /// longer matches the selected donor profile or target bounds.
    pub fn validate(self) -> ProviderResult<()> {
        use WellearnAtomicCompletionProfile::{AutoZeroTimeSaveOnly0, FanyuchangFreshSetSave100};
        use WellearnDurationProtocolMode::{ClientCounter, ImplicitServer};
        use WellearnResourceCompletionCmiFormat::{InteractionInfoSuffix, Json};
        use WellearnResourceCompletionSequence::SelectedScore;
        use WellearnResourceCompletionTimeMode::{PreserveFreshTime, ZeroTime};
        use WellearnResourceCompletionWriteMode::{SaveOnly, SetThenSave};
        use WellearnResourceMutationProfile::{CurrentFullSimpleReferer, LegacyMinimalTaskReferer};

        if self.completion.validate().is_err() || !self.completion.requires_atomic_authority() {
            return Err(invalid_atomic_plan());
        }
        let complete_profile = (
            self.profile,
            self.target_seconds,
            self.heartbeat_interval_seconds,
            self.duration_protocol_mode,
            self.completion.score_percent,
            self.completion.sequence,
            self.completion.time_mode,
            self.completion.cmi_format,
            self.completion.write_mode,
            self.completion.mutation_profile,
        );
        let valid = matches!(
            complete_profile,
            (
                FanyuchangFreshSetSave100,
                1..=MAX_DURATION_REPORT_SECONDS,
                1,
                ClientCounter,
                100,
                SelectedScore,
                PreserveFreshTime,
                Json,
                SetThenSave,
                CurrentFullSimpleReferer,
            ) | (
                AutoZeroTimeSaveOnly0,
                0..=MAX_DURATION_REPORT_SECONDS,
                60,
                ImplicitServer,
                0,
                SelectedScore,
                ZeroTime,
                InteractionInfoSuffix,
                SaveOnly,
                LegacyMinimalTaskReferer,
            )
        );
        if !valid {
            return Err(invalid_atomic_plan());
        }
        Ok(())
    }
}

fn invalid_atomic_plan() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::Internal,
        "WELearn atomic duration-completion plan mixes incompatible donor wire facts",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constructor_freezes_each_complete_donor_profile() {
        let current = WellearnAtomicDurationCompletionPlan::try_new(
            WellearnAtomicCompletionProfile::FanyuchangFreshSetSave100,
            30,
        )
        .unwrap();
        assert_eq!(current.heartbeat_interval_seconds, 1);
        assert_eq!(
            current.duration_protocol_mode,
            WellearnDurationProtocolMode::ClientCounter
        );
        assert_eq!(current.completion.score_percent, 100);
        assert_eq!(
            current.completion.time_mode,
            WellearnResourceCompletionTimeMode::PreserveFreshTime
        );
        assert_eq!(
            current.completion.write_mode,
            WellearnResourceCompletionWriteMode::SetThenSave
        );
        assert_eq!(
            current.completion.mutation_profile,
            WellearnResourceMutationProfile::CurrentFullSimpleReferer
        );
        current.validate().unwrap();

        let auto = WellearnAtomicDurationCompletionPlan::try_new(
            WellearnAtomicCompletionProfile::AutoZeroTimeSaveOnly0,
            0,
        )
        .unwrap();
        assert_eq!(auto.heartbeat_interval_seconds, 60);
        assert_eq!(
            auto.duration_protocol_mode,
            WellearnDurationProtocolMode::ImplicitServer
        );
        assert_eq!(auto.completion.score_percent, 0);
        assert_eq!(
            auto.completion.write_mode,
            WellearnResourceCompletionWriteMode::SaveOnly
        );
        assert_eq!(
            auto.completion.mutation_profile,
            WellearnResourceMutationProfile::LegacyMinimalTaskReferer
        );
        auto.validate().unwrap();
    }

    #[test]
    fn target_bounds_preserve_auto_zero_floor_only() {
        assert!(
            WellearnAtomicDurationCompletionPlan::try_new(
                WellearnAtomicCompletionProfile::FanyuchangFreshSetSave100,
                0,
            )
            .is_err()
        );
        assert!(
            WellearnAtomicDurationCompletionPlan::try_new(
                WellearnAtomicCompletionProfile::FanyuchangFreshSetSave100,
                MAX_DURATION_REPORT_SECONDS,
            )
            .is_ok()
        );
        assert!(
            WellearnAtomicDurationCompletionPlan::try_new(
                WellearnAtomicCompletionProfile::AutoZeroTimeSaveOnly0,
                0,
            )
            .is_ok()
        );
        for profile in [
            WellearnAtomicCompletionProfile::FanyuchangFreshSetSave100,
            WellearnAtomicCompletionProfile::AutoZeroTimeSaveOnly0,
        ] {
            assert!(
                WellearnAtomicDurationCompletionPlan::try_new(
                    profile,
                    MAX_DURATION_REPORT_SECONDS + 1,
                )
                .is_err()
            );
        }
    }

    #[test]
    fn restored_plan_rejects_every_cross_donor_drift() {
        let plan = WellearnAtomicDurationCompletionPlan::try_new(
            WellearnAtomicCompletionProfile::FanyuchangFreshSetSave100,
            10,
        )
        .unwrap();

        let mut drifted = plan;
        drifted.profile = WellearnAtomicCompletionProfile::AutoZeroTimeSaveOnly0;
        assert!(drifted.validate().is_err());

        let mut drifted = plan;
        drifted.heartbeat_interval_seconds = 60;
        assert!(drifted.validate().is_err());

        let mut drifted = plan;
        drifted.duration_protocol_mode = WellearnDurationProtocolMode::ImplicitServer;
        assert!(drifted.validate().is_err());

        let mut drifted = plan;
        drifted.completion.score_percent = 0;
        assert!(drifted.validate().is_err());

        let mut drifted = plan;
        drifted.completion.cmi_format = WellearnResourceCompletionCmiFormat::InteractionInfoSuffix;
        assert!(drifted.validate().is_err());

        let mut drifted = plan;
        drifted.completion.write_mode = WellearnResourceCompletionWriteMode::SaveOnly;
        assert!(drifted.validate().is_err());
    }

    #[test]
    fn current_documents_preserve_ordered_keep_rejection_and_false_diagnostics() {
        let plan = WellearnAtomicDurationCompletionPlan::try_new(
            WellearnAtomicCompletionProfile::FanyuchangFreshSetSave100,
            3,
        )
        .unwrap();
        let documents = WellearnAtomicDurationCompletionDocuments::try_new(
            plan,
            cmi(),
            Some(cmi()),
            cmi(),
            WellearnAtomicDurationCompletionReceipts::new(
                true,
                vec![true, false],
                Some(false),
                false,
            ),
        )
        .unwrap();

        assert!(documents.after_duration().is_some());
        assert_eq!(documents.receipts().heartbeat_acceptances(), [true, false]);
        assert_eq!(documents.receipts().set_accepted(), Some(false));
        assert!(!documents.receipts().save_accepted());
        documents.validate_for_plan(plan).unwrap();
    }

    #[test]
    fn auto_documents_bind_complete_intervals_and_zero_floor() {
        let plan = WellearnAtomicDurationCompletionPlan::try_new(
            WellearnAtomicCompletionProfile::AutoZeroTimeSaveOnly0,
            125,
        )
        .unwrap();
        let documents = WellearnAtomicDurationCompletionDocuments::try_new(
            plan,
            cmi(),
            None,
            cmi(),
            WellearnAtomicDurationCompletionReceipts::new(false, vec![false, true], None, false),
        )
        .unwrap();
        assert!(documents.after_duration().is_none());
        assert_eq!(documents.receipts().heartbeat_acceptances().len(), 2);

        let zero = WellearnAtomicDurationCompletionPlan::try_new(
            WellearnAtomicCompletionProfile::AutoZeroTimeSaveOnly0,
            0,
        )
        .unwrap();
        WellearnAtomicDurationCompletionDocuments::try_new(
            zero,
            cmi(),
            None,
            cmi(),
            WellearnAtomicDurationCompletionReceipts::new(true, Vec::new(), None, true),
        )
        .unwrap();
    }

    #[test]
    fn documents_reject_impossible_current_lifecycles() {
        let plan = WellearnAtomicDurationCompletionPlan::try_new(
            WellearnAtomicCompletionProfile::FanyuchangFreshSetSave100,
            3,
        )
        .unwrap();
        for (after_duration, receipts) in [
            (
                None,
                WellearnAtomicDurationCompletionReceipts::new(
                    true,
                    vec![true, true, true],
                    Some(true),
                    true,
                ),
            ),
            (
                Some(cmi()),
                WellearnAtomicDurationCompletionReceipts::new(
                    false,
                    vec![true, true, true],
                    Some(true),
                    true,
                ),
            ),
            (
                Some(cmi()),
                WellearnAtomicDurationCompletionReceipts::new(
                    true,
                    vec![true, false, true],
                    Some(true),
                    true,
                ),
            ),
            (
                Some(cmi()),
                WellearnAtomicDurationCompletionReceipts::new(
                    true,
                    vec![true, true],
                    Some(true),
                    true,
                ),
            ),
            (
                Some(cmi()),
                WellearnAtomicDurationCompletionReceipts::new(
                    true,
                    vec![true, true, true],
                    None,
                    true,
                ),
            ),
        ] {
            assert!(
                WellearnAtomicDurationCompletionDocuments::try_new(
                    plan,
                    cmi(),
                    after_duration,
                    cmi(),
                    receipts,
                )
                .is_err()
            );
        }
    }

    #[test]
    fn documents_reject_impossible_auto_lifecycles() {
        let plan = WellearnAtomicDurationCompletionPlan::try_new(
            WellearnAtomicCompletionProfile::AutoZeroTimeSaveOnly0,
            120,
        )
        .unwrap();
        for (after_duration, receipts) in [
            (
                Some(cmi()),
                WellearnAtomicDurationCompletionReceipts::new(true, vec![true, true], None, true),
            ),
            (
                None,
                WellearnAtomicDurationCompletionReceipts::new(true, vec![true], None, true),
            ),
            (
                None,
                WellearnAtomicDurationCompletionReceipts::new(
                    true,
                    vec![true, true],
                    Some(true),
                    true,
                ),
            ),
        ] {
            assert!(
                WellearnAtomicDurationCompletionDocuments::try_new(
                    plan,
                    cmi(),
                    after_duration,
                    cmi(),
                    receipts,
                )
                .is_err()
            );
        }
    }

    #[test]
    fn verifier_proves_current_goal_and_preserved_fresh_times() {
        let plan = WellearnAtomicDurationCompletionPlan::try_new(
            WellearnAtomicCompletionProfile::FanyuchangFreshSetSave100,
            1,
        )
        .unwrap();
        let documents = WellearnAtomicDurationCompletionDocuments::try_new(
            plan,
            cmi(),
            Some(snapshot_cmi(
                "incomplete",
                "0.25",
                "20",
                Some("15"),
                Some("45"),
            )),
            snapshot_cmi("completed", "1", "100", Some("15"), Some("45")),
            WellearnAtomicDurationCompletionReceipts::new(true, vec![true], Some(false), false),
        )
        .unwrap();

        let verification = verify_atomic_duration_completion(plan, &documents).unwrap();
        assert_eq!(verification.profile(), plan.profile());
        assert_eq!(verification.score_percent(), 100);
        assert_eq!(verification.time_preservation_verified(), Some(true));
    }

    #[test]
    fn verifier_proves_auto_goal_without_inventing_a_time_predicate() {
        let plan = WellearnAtomicDurationCompletionPlan::try_new(
            WellearnAtomicCompletionProfile::AutoZeroTimeSaveOnly0,
            0,
        )
        .unwrap();
        let documents = WellearnAtomicDurationCompletionDocuments::try_new(
            plan,
            cmi(),
            None,
            snapshot_cmi("completed", "1", "0", Some("87"), Some("120")),
            WellearnAtomicDurationCompletionReceipts::new(false, Vec::new(), None, false),
        )
        .unwrap();

        let verification = verify_atomic_duration_completion(plan, &documents).unwrap();
        assert_eq!(verification.score_percent(), 0);
        assert_eq!(verification.time_preservation_verified(), None);
    }

    #[test]
    fn verifier_rejects_final_goal_or_current_time_drift() {
        let current = WellearnAtomicDurationCompletionPlan::try_new(
            WellearnAtomicCompletionProfile::FanyuchangFreshSetSave100,
            1,
        )
        .unwrap();
        for (after_duration, after_completion) in [
            (
                snapshot_cmi("incomplete", "0.25", "20", Some("15"), Some("45")),
                snapshot_cmi("completed", "1", "99", Some("15"), Some("45")),
            ),
            (
                snapshot_cmi("incomplete", "0.25", "20", Some("15"), Some("45")),
                snapshot_cmi("completed", "1", "100", Some("16"), Some("45")),
            ),
            (
                snapshot_cmi("incomplete", "0.25", "20", None, Some("45")),
                snapshot_cmi("completed", "1", "100", None, Some("45")),
            ),
        ] {
            let documents = WellearnAtomicDurationCompletionDocuments::try_new(
                current,
                cmi(),
                Some(after_duration),
                after_completion,
                WellearnAtomicDurationCompletionReceipts::new(true, vec![true], Some(true), true),
            )
            .unwrap();
            assert_eq!(
                verify_atomic_duration_completion(current, &documents)
                    .unwrap_err()
                    .kind,
                ProviderErrorKind::RemoteChanged
            );
        }
    }

    fn cmi() -> WellearnCmiDocument {
        WellearnCmiDocument::try_new("{}".to_owned()).unwrap()
    }

    fn snapshot_cmi(
        completion: &str,
        progress: &str,
        score: &str,
        session_time: Option<&str>,
        total_time: Option<&str>,
    ) -> WellearnCmiDocument {
        let mut cmi = serde_json::json!({
            "completion_status": completion,
            "progress_measure": progress,
            "score": {"scaled": score},
            "success_status": "unknown",
        });
        if let Some(session_time) = session_time {
            cmi["session_time"] = serde_json::json!(session_time);
        }
        if let Some(total_time) = total_time {
            cmi["total_time"] = serde_json::json!(total_time);
        }
        WellearnCmiDocument::try_new(
            serde_json::json!({
                "ret": 0,
                "comment": serde_json::json!({"cmi": cmi}).to_string(),
            })
            .to_string(),
        )
        .unwrap()
    }
}
