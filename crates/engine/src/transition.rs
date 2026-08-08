use asterism_domain::{Execution, ExecutionState, OrchestrationState, Timestamp};

/// Moves an execution through its guarded lifecycle and records lifecycle
/// timestamps exactly once.
///
/// # Errors
///
/// Returns [`ExecutionTransitionError`] when the edge is not legal or the
/// timestamp precedes an already recorded lifecycle timestamp.
pub fn transition_execution(
    execution: &mut Execution,
    target: ExecutionState,
    at: Timestamp,
) -> Result<(), ExecutionTransitionError> {
    let source = execution.state;
    if source == target {
        return Err(ExecutionTransitionError::NoOp(source));
    }
    if !execution_edge_allowed(source, target) {
        return Err(ExecutionTransitionError::InvalidEdge {
            from_state: source,
            to_state: target,
        });
    }
    if at < execution.created_at || execution.started_at.is_some_and(|started| at < started) {
        return Err(ExecutionTransitionError::TimestampRegression);
    }

    if target == ExecutionState::Running && execution.started_at.is_none() {
        execution.started_at = Some(at);
    }
    if target.is_terminal() {
        execution.finished_at = Some(at);
    }
    execution.state = target;
    Ok(())
}

const fn execution_edge_allowed(source: ExecutionState, target: ExecutionState) -> bool {
    use ExecutionState::{
        Cancelled, Failed, HumanRequired, Recovering, Requested, RetryWaiting, Running, Scheduled,
        Succeeded,
    };
    match source {
        Requested => matches!(target, Scheduled | Running | HumanRequired | Cancelled),
        Scheduled => matches!(target, Running | HumanRequired | Cancelled),
        Running => matches!(
            target,
            Recovering | Succeeded | Failed | RetryWaiting | HumanRequired | Cancelled
        ),
        Recovering => matches!(
            target,
            Succeeded | RetryWaiting | HumanRequired | Failed | Cancelled
        ),
        RetryWaiting => matches!(
            target,
            Scheduled | Running | Failed | HumanRequired | Cancelled
        ),
        HumanRequired => matches!(target, Scheduled | Running | Failed | Cancelled),
        Succeeded | Failed | Cancelled => false,
    }
}

/// Checks an orchestration edge without coupling the domain object to the
/// engine implementation.
///
/// # Errors
///
/// Returns [`OrchestrationTransitionError`] for a no-op or unsupported edge.
pub fn validate_orchestration_transition(
    source: OrchestrationState,
    target: OrchestrationState,
) -> Result<(), OrchestrationTransitionError> {
    if source == target {
        return Err(OrchestrationTransitionError::NoOp(source));
    }
    if orchestration_edge_allowed(source, target) {
        Ok(())
    } else {
        Err(OrchestrationTransitionError::InvalidEdge {
            from_state: source,
            to_state: target,
        })
    }
}

const fn orchestration_edge_allowed(
    source: OrchestrationState,
    target: OrchestrationState,
) -> bool {
    use OrchestrationState::{
        Cancelled, CreditBlocked, Discovered, Failed, HumanRequired, Ignored, Ready, Recovering,
        RetryWaiting, Running, Scheduled, Succeeded, WaitingApproval,
    };
    match source {
        Discovered => matches!(
            target,
            Ready | WaitingApproval | Scheduled | CreditBlocked | HumanRequired | Ignored
        ),
        Ready => matches!(
            target,
            WaitingApproval | Scheduled | CreditBlocked | HumanRequired | Running | Ignored
        ),
        WaitingApproval => matches!(target, Ready | Scheduled | Cancelled | Ignored),
        Scheduled => matches!(target, Running | CreditBlocked | HumanRequired | Cancelled),
        CreditBlocked => matches!(target, Ready | Scheduled | Cancelled | Ignored),
        HumanRequired => matches!(target, Ready | Scheduled | Cancelled | Ignored),
        Running => matches!(
            target,
            Recovering | Succeeded | Failed | RetryWaiting | HumanRequired | Cancelled
        ),
        Recovering => matches!(
            target,
            Succeeded | RetryWaiting | HumanRequired | Failed | Cancelled
        ),
        RetryWaiting => matches!(
            target,
            Scheduled | Running | Failed | HumanRequired | Cancelled
        ),
        Failed => matches!(
            target,
            Ready | Scheduled | RetryWaiting | Cancelled | Ignored
        ),
        Succeeded | Cancelled | Ignored => matches!(target, Ready),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ExecutionTransitionError {
    #[error("execution is already in state {0:?}")]
    NoOp(ExecutionState),
    #[error("execution cannot transition from {from_state:?} to {to_state:?}")]
    InvalidEdge {
        from_state: ExecutionState,
        to_state: ExecutionState,
    },
    #[error("execution transition timestamp precedes existing lifecycle state")]
    TimestampRegression,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum OrchestrationTransitionError {
    #[error("task orchestration is already in state {0:?}")]
    NoOp(OrchestrationState),
    #[error("task orchestration cannot transition from {from_state:?} to {to_state:?}")]
    InvalidEdge {
        from_state: OrchestrationState,
        to_state: OrchestrationState,
    },
}

#[cfg(test)]
mod tests {
    use asterism_domain::{ExecutionId, RequestSource, TaskId};
    use chrono::{Duration, Utc};

    use super::*;

    fn execution(now: Timestamp) -> Execution {
        Execution {
            id: ExecutionId::new(),
            task_id: TaskId::new(),
            requested_by: None,
            request_source: RequestSource::System,
            quote_id: None,
            state: ExecutionState::Requested,
            scheduled_at: None,
            started_at: None,
            finished_at: None,
            created_at: now,
        }
    }

    #[test]
    fn execution_records_start_and_finish_once() {
        let now = Utc::now();
        let mut execution = execution(now);
        transition_execution(&mut execution, ExecutionState::Running, now).unwrap();
        let finished = now + Duration::seconds(2);
        transition_execution(&mut execution, ExecutionState::Succeeded, finished).unwrap();
        assert_eq!(execution.started_at, Some(now));
        assert_eq!(execution.finished_at, Some(finished));
    }

    #[test]
    fn terminal_execution_cannot_restart() {
        let now = Utc::now();
        let mut execution = execution(now);
        transition_execution(&mut execution, ExecutionState::Running, now).unwrap();
        transition_execution(
            &mut execution,
            ExecutionState::Failed,
            now + Duration::seconds(1),
        )
        .unwrap();
        assert!(matches!(
            transition_execution(
                &mut execution,
                ExecutionState::Running,
                now + Duration::seconds(2)
            ),
            Err(ExecutionTransitionError::InvalidEdge { .. })
        ));
    }

    #[test]
    fn reopened_task_can_return_to_ready() {
        assert_eq!(
            validate_orchestration_transition(
                OrchestrationState::Succeeded,
                OrchestrationState::Ready
            ),
            Ok(())
        );
    }
}
