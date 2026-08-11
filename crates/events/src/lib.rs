//! Structured domain events shared by the scheduler, API, notifications, and
//! integrations.

use asterism_domain::{
    AuthSessionId, AuthState, CreditAmount, EventId, ExecutionId, ExecutionLogEvent,
    ExecutionProgress, HumanRequiredReason, OrchestrationState, TaskDiffKind, TaskId,
    TaskLifecycleAction, Timestamp, UserId, UserStatus,
};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct EventEnvelope {
    pub id: EventId,
    pub occurred_at: Timestamp,
    pub correlation_id: String,
    pub event: DomainEvent,
}

impl EventEnvelope {
    pub fn new(correlation_id: impl Into<String>, event: DomainEvent) -> Self {
        Self::at(correlation_id, event, Utc::now())
    }

    pub fn at(
        correlation_id: impl Into<String>,
        event: DomainEvent,
        occurred_at: Timestamp,
    ) -> Self {
        Self {
            id: EventId::new(),
            occurred_at,
            correlation_id: correlation_id.into(),
            event,
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "type", content = "payload")]
pub enum DomainEvent {
    TaskChanged {
        task_id: TaskId,
        changes: Vec<TaskDiffKind>,
    },
    TaskLifecycleActionApplied {
        task_id: TaskId,
        action: TaskLifecycleAction,
        state: OrchestrationState,
        delayed_until: Option<Timestamp>,
        affected_execution_id: Option<ExecutionId>,
    },
    UserChanged {
        user_id: UserId,
        status: UserStatus,
    },
    ExecutionStateChanged {
        execution_id: ExecutionId,
        state: asterism_domain::ExecutionState,
    },
    ExecutionProgressed(ExecutionProgress),
    ExecutionLogged(ExecutionLogEvent),
    AuthStateChanged {
        auth_session_id: AuthSessionId,
        state: AuthState,
    },
    HumanRequired {
        task_id: Option<TaskId>,
        execution_id: Option<ExecutionId>,
        reason: HumanRequiredReason,
    },
    CreditReserved {
        user_id: UserId,
        execution_id: ExecutionId,
        amount: CreditAmount,
    },
    CreditGranted {
        user_id: UserId,
        operator_id: UserId,
        amount: CreditAmount,
    },
    CreditCommitted {
        user_id: UserId,
        execution_id: ExecutionId,
        amount: CreditAmount,
    },
    CreditReleased {
        user_id: UserId,
        execution_id: ExecutionId,
        amount: CreditAmount,
    },
    ExecutionRecoveryRequired {
        execution_id: ExecutionId,
        task_id: TaskId,
    },
}

/// A bounded, in-process fan-out bus. Durable consumers use the transactional
/// outbox in `asterism-storage`; this bus is only the live delivery path.
#[derive(Clone, Debug)]
pub struct EventBus {
    sender: broadcast::Sender<EventEnvelope>,
}

impl EventBus {
    pub fn new(capacity: usize) -> Self {
        let (sender, _) = broadcast::channel(capacity.max(1));
        Self { sender }
    }

    /// Publishes one event to every current live subscriber.
    ///
    /// # Errors
    ///
    /// Returns [`EventBusError::NoSubscribers`] when there are no active live
    /// subscribers. Durable delivery remains the responsibility of the outbox.
    pub fn publish(&self, event: EventEnvelope) -> Result<usize, EventBusError> {
        self.sender
            .send(event)
            .map_err(|_| EventBusError::NoSubscribers)
    }

    pub fn subscribe(&self) -> broadcast::Receiver<EventEnvelope> {
        self.sender.subscribe()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum EventBusError {
    #[error("the live event bus has no subscribers")]
    NoSubscribers,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn subscribers_receive_structured_events() {
        let bus = EventBus::new(8);
        let mut receiver = bus.subscribe();
        let expected = EventEnvelope::new(
            "scan-1",
            DomainEvent::TaskChanged {
                task_id: TaskId::new(),
                changes: vec![TaskDiffKind::Created],
            },
        );
        bus.publish(expected.clone()).unwrap();
        assert_eq!(receiver.recv().await.unwrap(), expected);
    }
}
