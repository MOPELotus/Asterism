//! Strongly typed, time-sortable identifiers.

use std::{fmt, str::FromStr};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

macro_rules! entity_id {
    ($name:ident) => {
        #[doc = concat!("Identifier for a `", stringify!($name), "` entity.")]
        #[derive(
            Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize,
        )]
        #[serde(transparent)]
        pub struct $name(Uuid);

        impl $name {
            /// Generates a monotonically sortable `UUIDv7` identifier.
            pub fn new() -> Self {
                Self(Uuid::now_v7())
            }

            pub const fn from_uuid(value: Uuid) -> Self {
                Self(value)
            }

            pub const fn into_uuid(self) -> Uuid {
                self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(formatter)
            }
        }

        impl FromStr for $name {
            type Err = uuid::Error;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Uuid::parse_str(value).map(Self)
            }
        }
    };
}

entity_id!(UserId);
entity_id!(ProviderAccountId);
entity_id!(ProviderRuntimeSettingsId);
entity_id!(CourseId);
entity_id!(CourseEnrollmentDraftId);
entity_id!(CourseEnrollmentAttemptId);
entity_id!(TaskId);
entity_id!(TaskSnapshotId);
entity_id!(TaskDiffId);
entity_id!(TaskActionReceiptId);
entity_id!(QuestionId);
entity_id!(QuestionGroupId);
entity_id!(QuestionSnapshotId);
entity_id!(QuestionReadAttemptId);
entity_id!(QuestionSessionId);
entity_id!(AnswerCandidateId);
entity_id!(PrivateAnswerEvidenceId);
entity_id!(GlobalAnswerCorpusEntryId);
entity_id!(AnswerBootstrapHarvestId);
entity_id!(AnswerHistoryImportId);
entity_id!(StrictCompletionWorkflowId);
entity_id!(ScoreImprovementWorkflowId);
entity_id!(ProtocolObservationId);
entity_id!(SubmissionDraftId);
entity_id!(SubmissionResultId);
entity_id!(ExecutionId);
entity_id!(ExecutionAttemptId);
entity_id!(ExecutionInvocationDraftId);
entity_id!(BatchExecutionId);
entity_id!(BatchExecutionAttemptId);
entity_id!(ScheduleId);
entity_id!(AutomationPlanId);
entity_id!(PriceQuoteId);
entity_id!(CreditGrantReceiptId);
entity_id!(CreditTransactionId);
entity_id!(CreditReservationId);
entity_id!(AuthSessionId);
entity_id!(AuthBootstrapSessionId);
entity_id!(BrowserBridgeSessionId);
entity_id!(WebSessionId);
entity_id!(ServiceTokenId);
entity_id!(SecretId);
entity_id!(EventId);
entity_id!(NotificationId);
entity_id!(AuditRecordId);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_id_round_trips_through_text_and_json() {
        let id = TaskId::new();
        let text = id.to_string();
        assert_eq!(text.parse::<TaskId>().unwrap(), id);

        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(serde_json::from_str::<TaskId>(&json).unwrap(), id);
    }
}
