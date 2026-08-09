use asterism_domain::{AssessmentClass, Task};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TaskAction {
    Inventory,
    ReadStatus,
    ReadDetail,
    Parse,
    Resolve,
    Build,
    Notify,
    Execute,
    Submit,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct FormalAssessmentPolicy {
    pub allow_execution: bool,
    pub allow_submission: bool,
}

/// Applies assessment-nature safeguards independently of the provider module.
///
/// # Errors
///
/// Returns [`AssessmentGuardError`] when a formal assessment action has not
/// been explicitly enabled by policy.
pub fn authorize_task_action(
    task: &Task,
    action: TaskAction,
    formal_policy: FormalAssessmentPolicy,
) -> Result<(), AssessmentGuardError> {
    if task.assessment_class != AssessmentClass::Formal {
        return Ok(());
    }

    match action {
        TaskAction::Inventory
        | TaskAction::ReadStatus
        | TaskAction::ReadDetail
        | TaskAction::Parse
        | TaskAction::Resolve
        | TaskAction::Build
        | TaskAction::Notify => Ok(()),
        TaskAction::Execute if formal_policy.allow_execution => Ok(()),
        TaskAction::Submit if formal_policy.allow_submission => Ok(()),
        TaskAction::Execute => Err(AssessmentGuardError::FormalExecutionNotAllowed),
        TaskAction::Submit => Err(AssessmentGuardError::FormalSubmissionNotAllowed),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum AssessmentGuardError {
    #[error("formal assessment execution requires an explicit policy")]
    FormalExecutionNotAllowed,
    #[error("formal assessment submission requires an explicit policy")]
    FormalSubmissionNotAllowed,
}

#[cfg(test)]
mod tests {
    use asterism_domain::{
        AssessmentClass, OrchestrationState, ProviderAccountId, RemoteState, SourceType, Task,
        TaskId,
    };
    use chrono::Utc;

    use super::*;

    fn task(source_type: SourceType, assessment_class: AssessmentClass) -> Task {
        Task {
            id: TaskId::new(),
            provider_account_id: ProviderAccountId::new(),
            course_id: None,
            remote_id: "remote".to_owned(),
            source_type,
            assessment_class,
            title: "Task".to_owned(),
            remote_state: RemoteState::Pending,
            orchestration_state: OrchestrationState::Ready,
            opens_at: None,
            due_at: None,
            closes_at: None,
            discovered_at: Utc::now(),
            updated_at: Utc::now(),
            latest_snapshot_id: None,
            capabilities: Vec::new(),
        }
    }

    #[test]
    fn routine_exam_module_is_not_blocked() {
        let task = task(SourceType::Exam, AssessmentClass::Routine);
        assert_eq!(
            authorize_task_action(&task, TaskAction::Submit, FormalAssessmentPolicy::default()),
            Ok(())
        );
    }

    #[test]
    fn formal_submission_requires_its_own_explicit_switch() {
        let task = task(SourceType::Work, AssessmentClass::Formal);
        assert_eq!(
            authorize_task_action(
                &task,
                TaskAction::Submit,
                FormalAssessmentPolicy {
                    allow_execution: true,
                    allow_submission: false,
                }
            ),
            Err(AssessmentGuardError::FormalSubmissionNotAllowed)
        );
        assert_eq!(
            authorize_task_action(
                &task,
                TaskAction::ReadDetail,
                FormalAssessmentPolicy::default()
            ),
            Ok(())
        );
    }
}
