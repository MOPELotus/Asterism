use asterism_domain::Timestamp;
use asterism_events::{EventBus, EventEnvelope};
use asterism_storage::{FailureDisposition, OutboxRepository, StorageError};
use async_trait::async_trait;
use chrono::Duration;

#[async_trait]
pub trait EventSink: Send + Sync {
    async fn deliver(&self, event: &EventEnvelope) -> Result<(), DeliveryError>;
}

#[async_trait]
impl EventSink for EventBus {
    async fn deliver(&self, event: &EventEnvelope) -> Result<(), DeliveryError> {
        // The transactional outbox is the durable source of truth. A live bus
        // with no current subscribers is therefore a successful ephemeral
        // delivery; new subscribers resynchronize through bounded read APIs.
        let _ = self.publish(event.clone());
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error("event delivery failed: {message_sanitized}")]
pub struct DeliveryError {
    pub message_sanitized: String,
}

impl DeliveryError {
    pub fn sanitized(message: impl Into<String>) -> Self {
        Self {
            message_sanitized: message.into(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DispatchConfig {
    pub worker_id: String,
    pub batch_size: u32,
    pub claim_ttl_seconds: u64,
    pub max_attempts: u32,
}

impl DispatchConfig {
    /// Checks that claims and retry limits are bounded and non-empty.
    ///
    /// # Errors
    ///
    /// Returns [`DispatchError::InvalidConfig`] when a required value is zero,
    /// the worker identifier is empty, or the TTL does not fit `i64`.
    pub fn validate(&self) -> Result<(), DispatchError> {
        if self.worker_id.is_empty()
            || self.batch_size == 0
            || self.claim_ttl_seconds == 0
            || self.max_attempts == 0
            || i64::try_from(self.claim_ttl_seconds).is_err()
        {
            Err(DispatchError::InvalidConfig)
        } else {
            Ok(())
        }
    }
}

#[derive(Debug)]
pub struct OutboxDispatcher<R, S> {
    repository: R,
    sink: S,
    config: DispatchConfig,
}

impl<R, S> OutboxDispatcher<R, S>
where
    R: OutboxRepository,
    S: EventSink,
{
    /// Constructs a dispatcher after validating its claim policy.
    ///
    /// # Errors
    ///
    /// Returns [`DispatchError::InvalidConfig`] for an invalid configuration.
    pub fn new(repository: R, sink: S, config: DispatchConfig) -> Result<Self, DispatchError> {
        config.validate()?;
        Ok(Self {
            repository,
            sink,
            config,
        })
    }

    /// Claims and delivers one bounded batch with at-least-once semantics.
    ///
    /// # Errors
    ///
    /// Returns [`DispatchError`] when claim bookkeeping cannot be completed.
    /// Individual sink failures are persisted for retry/dead-letter handling and
    /// summarized in the successful report.
    pub async fn dispatch_once(&self, now: Timestamp) -> Result<DispatchReport, DispatchError> {
        let ttl_seconds = i64::try_from(self.config.claim_ttl_seconds)
            .map_err(|_| DispatchError::InvalidConfig)?;
        let lease_expires_at = now + Duration::seconds(ttl_seconds);
        let records = self
            .repository
            .claim_batch(
                &self.config.worker_id,
                now,
                lease_expires_at,
                self.config.batch_size,
            )
            .await?;
        let mut report = DispatchReport {
            claimed: records.len(),
            ..DispatchReport::default()
        };

        for record in records {
            match self.sink.deliver(&record.event).await {
                Ok(()) => {
                    self.repository
                        .mark_delivered(record.event.id, &self.config.worker_id, now)
                        .await?;
                    report.delivered += 1;
                }
                Err(error) => {
                    match self
                        .repository
                        .mark_failed(
                            record.event.id,
                            &self.config.worker_id,
                            &error.message_sanitized,
                            self.config.max_attempts,
                        )
                        .await?
                    {
                        FailureDisposition::RetryPending => report.retry_pending += 1,
                        FailureDisposition::DeadLetter => report.dead_lettered += 1,
                    }
                }
            }
        }
        Ok(report)
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DispatchReport {
    pub claimed: usize,
    pub delivered: usize,
    pub retry_pending: usize,
    pub dead_lettered: usize,
}

#[derive(Debug, thiserror::Error)]
pub enum DispatchError {
    #[error("outbox dispatch configuration is invalid")]
    InvalidConfig,
    #[error("outbox storage operation failed: {0}")]
    Storage(#[from] StorageError),
}

#[cfg(test)]
mod tests {
    use asterism_domain::{TaskDiffKind, TaskId};
    use asterism_events::DomainEvent;
    use asterism_storage::{Database, OutboxRepository, SqliteOutboxRepository};
    use chrono::Utc;

    use super::*;

    #[derive(Debug)]
    struct TestSink {
        fail: bool,
    }

    #[async_trait]
    impl EventSink for TestSink {
        async fn deliver(&self, _event: &EventEnvelope) -> Result<(), DeliveryError> {
            if self.fail {
                Err(DeliveryError::sanitized("test sink unavailable"))
            } else {
                Ok(())
            }
        }
    }

    fn config() -> DispatchConfig {
        DispatchConfig {
            worker_id: "outbox-test".to_owned(),
            batch_size: 10,
            claim_ttl_seconds: 30,
            max_attempts: 1,
        }
    }

    async fn repository_with_event() -> (SqliteOutboxRepository, Timestamp) {
        let database = Database::connect("sqlite::memory:").await.unwrap();
        database.migrate().await.unwrap();
        let repository = SqliteOutboxRepository::new(database);
        let now = Utc::now();
        repository
            .enqueue(&EventEnvelope::at(
                "dispatch-test",
                DomainEvent::TaskChanged {
                    task_id: TaskId::new(),
                    changes: vec![TaskDiffKind::Created],
                },
                now,
            ))
            .await
            .unwrap();
        (repository, now)
    }

    #[tokio::test]
    async fn successful_delivery_is_marked_once() {
        let (repository, now) = repository_with_event().await;
        let dispatcher =
            OutboxDispatcher::new(repository, TestSink { fail: false }, config()).unwrap();
        assert_eq!(
            dispatcher.dispatch_once(now).await.unwrap(),
            DispatchReport {
                claimed: 1,
                delivered: 1,
                retry_pending: 0,
                dead_lettered: 0,
            }
        );
        assert_eq!(dispatcher.dispatch_once(now).await.unwrap().claimed, 0);
    }

    #[tokio::test]
    async fn exhausted_delivery_is_dead_lettered() {
        let (repository, now) = repository_with_event().await;
        let dispatcher =
            OutboxDispatcher::new(repository, TestSink { fail: true }, config()).unwrap();
        assert_eq!(
            dispatcher.dispatch_once(now).await.unwrap().dead_lettered,
            1
        );
        assert_eq!(dispatcher.dispatch_once(now).await.unwrap().claimed, 0);
    }

    #[tokio::test]
    async fn event_bus_delivery_is_live_and_does_not_require_a_subscriber() {
        let (repository, now) = repository_with_event().await;
        let bus = EventBus::new(8);
        let mut receiver = bus.subscribe();
        let dispatcher = OutboxDispatcher::new(repository, bus, config()).unwrap();

        assert_eq!(dispatcher.dispatch_once(now).await.unwrap().delivered, 1);
        assert!(matches!(
            receiver.recv().await.unwrap().event,
            DomainEvent::TaskChanged { .. }
        ));

        let (repository, now) = repository_with_event().await;
        let dispatcher = OutboxDispatcher::new(repository, EventBus::new(8), config()).unwrap();
        assert_eq!(dispatcher.dispatch_once(now).await.unwrap().delivered, 1);
    }
}
