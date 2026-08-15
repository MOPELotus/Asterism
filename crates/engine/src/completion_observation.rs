use asterism_domain::{
    CompletionDiagnosis, CompletionOutcome, RemoteState, SubmissionVerificationSnapshot,
    SubmissionVerificationStatus,
};
use asterism_provider_api::ExecutionOutcome;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CompletionObservation {
    pub outcome: Option<CompletionOutcome>,
    pub diagnosis: Option<CompletionDiagnosis>,
}

impl CompletionObservation {
    fn completed() -> Self {
        Self {
            outcome: Some(CompletionOutcome::Completed),
            diagnosis: None,
        }
    }

    fn incomplete(diagnosis: CompletionDiagnosis) -> Self {
        Self {
            outcome: None,
            diagnosis: Some(diagnosis),
        }
    }
}

/// Converts a Provider execution result into one conservative shared
/// completion observation.
///
/// # Errors
///
/// Returns [`CompletionObservationError`] when the Provider result itself is
/// malformed.
pub fn observe_execution_completion(
    outcome: &ExecutionOutcome,
    provider_diagnosis: Option<CompletionDiagnosis>,
) -> Result<CompletionObservation, CompletionObservationError> {
    outcome.validate().map_err(|_| CompletionObservationError)?;
    if outcome.verified && outcome.remote_state == RemoteState::Completed {
        Ok(CompletionObservation::completed())
    } else {
        Ok(CompletionObservation::incomplete(
            provider_diagnosis.unwrap_or(CompletionDiagnosis::RemoteUnknown),
        ))
    }
}

/// Converts a fresh submission readback into one conservative shared
/// completion observation.
///
/// # Errors
///
/// Returns [`CompletionObservationError`] when the verification snapshot is
/// malformed.
pub fn observe_submission_completion(
    verification: &SubmissionVerificationSnapshot,
    provider_diagnosis: Option<CompletionDiagnosis>,
) -> Result<CompletionObservation, CompletionObservationError> {
    verification
        .validate()
        .map_err(|_| CompletionObservationError)?;
    if verification.remote_state == Some(RemoteState::Completed) {
        return Ok(CompletionObservation::completed());
    }
    let diagnosis = provider_diagnosis.unwrap_or(match verification.status {
        SubmissionVerificationStatus::Rejected => CompletionDiagnosis::ScoreBelowThreshold,
        SubmissionVerificationStatus::Confirmed
        | SubmissionVerificationStatus::Pending
        | SubmissionVerificationStatus::Inconclusive => CompletionDiagnosis::RemoteUnknown,
    });
    Ok(CompletionObservation::incomplete(diagnosis))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[error("Provider completion observation is invalid")]
pub struct CompletionObservationError;

#[cfg(test)]
mod tests {
    use asterism_domain::{SubmissionScore, SubmissionVerificationStatus};
    use chrono::Utc;

    use super::*;

    #[test]
    fn execution_requires_fresh_verified_completion() {
        let completed = ExecutionOutcome {
            remote_state: RemoteState::Completed,
            verified: true,
            result_sanitized: serde_json::json!({"status": "completed"}),
        };
        assert_eq!(
            observe_execution_completion(&completed, None).unwrap(),
            CompletionObservation::completed()
        );

        let unverified = ExecutionOutcome {
            verified: false,
            ..completed
        };
        assert_eq!(
            observe_execution_completion(&unverified, None).unwrap(),
            CompletionObservation::incomplete(CompletionDiagnosis::RemoteUnknown)
        );
    }

    #[test]
    fn provider_diagnosis_is_used_only_while_incomplete() {
        let pending = ExecutionOutcome {
            remote_state: RemoteState::InProgress,
            verified: true,
            result_sanitized: serde_json::json!({}),
        };
        assert_eq!(
            observe_execution_completion(&pending, Some(CompletionDiagnosis::DurationInsufficient))
                .unwrap(),
            CompletionObservation::incomplete(CompletionDiagnosis::DurationInsufficient)
        );

        let completed = ExecutionOutcome {
            remote_state: RemoteState::Completed,
            ..pending
        };
        assert_eq!(
            observe_execution_completion(
                &completed,
                Some(CompletionDiagnosis::DurationInsufficient)
            )
            .unwrap(),
            CompletionObservation::completed()
        );
    }

    #[test]
    fn rejected_submission_defaults_to_score_diagnosis_but_completion_wins() {
        let mut verification = SubmissionVerificationSnapshot {
            status: SubmissionVerificationStatus::Rejected,
            remote_state: Some(RemoteState::InProgress),
            score: Some(SubmissionScore {
                earned_milli_points: 60_000,
                possible_milli_points: 100_000,
            }),
            progress_percent: Some(60),
            questions: Vec::new(),
            verified_at: Utc::now(),
        };
        assert_eq!(
            observe_submission_completion(&verification, None).unwrap(),
            CompletionObservation::incomplete(CompletionDiagnosis::ScoreBelowThreshold)
        );

        verification.remote_state = Some(RemoteState::Completed);
        assert_eq!(
            observe_submission_completion(
                &verification,
                Some(CompletionDiagnosis::ScoreBelowThreshold)
            )
            .unwrap(),
            CompletionObservation::completed()
        );
    }
}
