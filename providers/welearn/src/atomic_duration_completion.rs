use asterism_provider_api::{ProviderError, ProviderErrorKind, ProviderResult};

use crate::{
    WellearnAtomicCompletionProfile, WellearnDurationProtocolMode,
    WellearnResourceCompletionCmiFormat, WellearnResourceCompletionSequence,
    WellearnResourceCompletionTimeMode, WellearnResourceCompletionWriteMode,
    WellearnResourceExecutionPlan, WellearnResourceMutationProfile,
    runtime_settings::MAX_DURATION_REPORT_SECONDS,
};

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
}
