use asterism_domain::{
    AnswerCandidate, AnswerCandidateId, AnswerConfidence, AnswerSource, NormalizedAnswer,
    QuestionId, QuestionSnapshotId, TaskId, UserId,
};
use asterism_storage::{
    AnswerCandidateRecord, AnswerCandidateRepository, QuestionSnapshotRepository, StorageError,
};
use chrono::Utc;

#[derive(Clone, Debug)]
pub struct CreateManualAnswerCandidateCommand {
    pub owner_id: UserId,
    pub task_id: TaskId,
    pub question_snapshot_id: QuestionSnapshotId,
    pub question_id: QuestionId,
    pub answer: NormalizedAnswer,
    pub confidence: Option<AnswerConfidence>,
    pub explanation: Option<String>,
}

#[derive(Clone, Debug)]
pub struct ManualAnswerCandidateService<S> {
    snapshots: S,
}

impl<S> ManualAnswerCandidateService<S> {
    pub const fn new(snapshots: S) -> Self {
        Self { snapshots }
    }
}

impl<S> ManualAnswerCandidateService<S>
where
    S: QuestionSnapshotRepository + AnswerCandidateRepository,
{
    /// Creates one Core-owned Manual answer candidate for a Question in an
    /// explicit immutable snapshot. Source and provenance are never supplied
    /// by the caller.
    ///
    /// # Errors
    ///
    /// Returns [`ManualAnswerCandidateError`] when the snapshot is not owned by
    /// the caller, its Task path is mismatched, the Question is absent, the
    /// answer is unknown or invalid, or persistence fails.
    pub async fn create(
        &self,
        command: CreateManualAnswerCandidateCommand,
    ) -> Result<AnswerCandidateRecord, ManualAnswerCandidateError> {
        let snapshot = self
            .snapshots
            .find_owned_question_snapshot(command.owner_id, command.question_snapshot_id)
            .await?
            .filter(|snapshot| snapshot.task_id == command.task_id)
            .ok_or(ManualAnswerCandidateError::QuestionSnapshotNotFound)?;
        if !snapshot
            .questions
            .iter()
            .any(|question| question.id == command.question_id)
        {
            return Err(ManualAnswerCandidateError::QuestionNotFound);
        }

        let candidate = AnswerCandidate {
            question_id: command.question_id,
            source: AnswerSource::Manual,
            answer: command.answer,
            confidence: command.confidence,
            explanation: command.explanation,
            provenance_sanitized: serde_json::json!({"origin": "manual_input"}),
        };
        if matches!(candidate.answer, NormalizedAnswer::Unknown) || candidate.validate().is_err() {
            return Err(ManualAnswerCandidateError::InvalidCandidate);
        }

        let record = AnswerCandidateRecord {
            id: AnswerCandidateId::new(),
            question_snapshot_id: snapshot.id,
            candidate,
            created_at: Utc::now(),
        };
        self.snapshots
            .save_answer_candidate_batch(std::slice::from_ref(&record))
            .await?;
        Ok(record)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ManualAnswerCandidateError {
    #[error("question snapshot was not found")]
    QuestionSnapshotNotFound,
    #[error("question was not found in the snapshot")]
    QuestionNotFound,
    #[error("manual answer candidate is unknown, malformed, or unbounded")]
    InvalidCandidate,
    #[error(transparent)]
    Storage(#[from] StorageError),
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use asterism_domain::{ProviderId, Question, QuestionKind, QuestionSnapshotId, TaskId, UserId};
    use async_trait::async_trait;
    use chrono::Utc;

    use super::*;
    use asterism_storage::QuestionSnapshot;

    #[derive(Debug)]
    struct FakeRepository {
        owner_id: UserId,
        snapshot: QuestionSnapshot,
        saved: Mutex<Vec<AnswerCandidateRecord>>,
    }

    #[async_trait]
    impl QuestionSnapshotRepository for FakeRepository {
        async fn save_question_snapshot(
            &self,
            _snapshot: &QuestionSnapshot,
        ) -> Result<(), StorageError> {
            unreachable!("manual answer creation never creates a Question snapshot")
        }

        async fn find_owned_question_snapshot(
            &self,
            owner_id: UserId,
            question_snapshot_id: QuestionSnapshotId,
        ) -> Result<Option<QuestionSnapshot>, StorageError> {
            Ok(
                (owner_id == self.owner_id && question_snapshot_id == self.snapshot.id)
                    .then(|| self.snapshot.clone()),
            )
        }

        async fn find_latest_owned_question_snapshot(
            &self,
            _owner_id: UserId,
            _task_id: TaskId,
        ) -> Result<Option<QuestionSnapshot>, StorageError> {
            unreachable!("manual answer creation requires an explicit snapshot")
        }
    }

    #[async_trait]
    impl AnswerCandidateRepository for FakeRepository {
        async fn save_answer_candidate_batch(
            &self,
            candidates: &[AnswerCandidateRecord],
        ) -> Result<(), StorageError> {
            self.saved.lock().unwrap().extend_from_slice(candidates);
            Ok(())
        }

        async fn list_owned_answer_candidates(
            &self,
            _owner_id: UserId,
            _question_snapshot_id: QuestionSnapshotId,
        ) -> Result<Vec<AnswerCandidateRecord>, StorageError> {
            unreachable!("manual answer creation does not list existing candidates")
        }
    }

    #[tokio::test]
    async fn manual_candidate_is_core_attributed_bound_and_persisted() {
        let (service, command) = fixture();
        let record = service.create(command.clone()).await.unwrap();

        assert_eq!(record.question_snapshot_id, command.question_snapshot_id);
        assert_eq!(record.candidate.question_id, command.question_id);
        assert_eq!(record.candidate.source, AnswerSource::Manual);
        assert_eq!(
            record.candidate.provenance_sanitized,
            serde_json::json!({"origin": "manual_input"})
        );
        assert_eq!(
            service.snapshots.saved.lock().unwrap().as_slice(),
            &[record]
        );
    }

    #[tokio::test]
    async fn foreign_binding_and_unknown_answer_never_persist() {
        let (service, mut command) = fixture();
        command.task_id = TaskId::new();
        assert!(matches!(
            service.create(command).await,
            Err(ManualAnswerCandidateError::QuestionSnapshotNotFound)
        ));

        let (unknown_service, mut unknown) = fixture();
        unknown.answer = NormalizedAnswer::Unknown;
        assert!(matches!(
            unknown_service.create(unknown).await,
            Err(ManualAnswerCandidateError::InvalidCandidate)
        ));
        assert!(service.snapshots.saved.lock().unwrap().is_empty());
        assert!(unknown_service.snapshots.saved.lock().unwrap().is_empty());
    }

    fn fixture() -> (
        ManualAnswerCandidateService<FakeRepository>,
        CreateManualAnswerCandidateCommand,
    ) {
        let owner_id = UserId::new();
        let task_id = TaskId::new();
        let question_id = QuestionId::new();
        let snapshot = QuestionSnapshot {
            id: QuestionSnapshotId::new(),
            task_id,
            provider_id: ProviderId::new("provider-alpha").unwrap(),
            provider_version: "1.0.0".to_owned(),
            captured_at: Utc::now(),
            questions: vec![Question {
                id: question_id,
                task_id,
                remote_question_id: Some("question-1".to_owned()),
                kind: QuestionKind::SingleChoice,
                stem: "Select one".to_owned(),
                options: Vec::new(),
                attachments: Vec::new(),
                metadata_sanitized: serde_json::json!({}),
                position: 1,
            }],
            groups: Vec::new(),
        };
        let command = CreateManualAnswerCandidateCommand {
            owner_id,
            task_id,
            question_snapshot_id: snapshot.id,
            question_id,
            answer: NormalizedAnswer::Selections(vec!["A".to_owned()]),
            confidence: Some(AnswerConfidence::try_new(9_000).unwrap()),
            explanation: Some("Entered after review".to_owned()),
        };
        (
            ManualAnswerCandidateService::new(FakeRepository {
                owner_id,
                snapshot,
                saved: Mutex::new(Vec::new()),
            }),
            command,
        )
    }
}
