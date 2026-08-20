use std::collections::{BTreeMap, BTreeSet};

use asterism_domain::{
    AnswerResolutionDecision, AnswerResolutionPlan, AnswerResolutionStatus, NormalizedAnswer,
    QuestionId, QuestionSnapshotId, TaskId, UserId,
};
use asterism_storage::{
    AnswerCandidateRecord, AnswerCandidateRepository, QuestionSnapshotRepository, StorageError,
};

type Questions<'a> = Vec<&'a asterism_domain::Question>;
type CandidatesByQuestion<'a> = BTreeMap<QuestionId, Vec<&'a AnswerCandidateRecord>>;

#[derive(Clone, Debug)]
pub struct ResolveAnswerCandidatesCommand {
    pub owner_id: UserId,
    pub task_id: TaskId,
    pub question_snapshot_id: QuestionSnapshotId,
}

#[derive(Clone, Debug)]
pub struct ConservativeAnswerResolverService<S> {
    snapshots: S,
}

impl<S> ConservativeAnswerResolverService<S> {
    pub const fn new(snapshots: S) -> Self {
        Self { snapshots }
    }
}

impl<S> ConservativeAnswerResolverService<S>
where
    S: QuestionSnapshotRepository + AnswerCandidateRepository,
{
    /// Derives a non-persisted, source-neutral resolution plan from existing
    /// evidence. Selection is emitted only when every known normalized answer
    /// for a Question agrees.
    ///
    /// # Errors
    ///
    /// Returns [`ConservativeAnswerResolverError`] for owner/Task/Snapshot
    /// mismatches, inconsistent stored evidence, invalid output, or storage
    /// failures.
    pub async fn resolve(
        &self,
        command: ResolveAnswerCandidatesCommand,
    ) -> Result<AnswerResolutionPlan, ConservativeAnswerResolverError> {
        let snapshot = self
            .snapshots
            .find_owned_question_snapshot(command.owner_id, command.question_snapshot_id)
            .await?
            .filter(|snapshot| snapshot.task_id == command.task_id)
            .ok_or(ConservativeAnswerResolverError::QuestionSnapshotNotFound)?;
        let candidates = self
            .snapshots
            .list_owned_answer_candidates(command.owner_id, snapshot.id)
            .await?;
        let (mut questions, candidates_by_question) =
            validate_and_group_evidence(&snapshot, &candidates)?;
        questions.sort_by_key(|question| question.position);

        let decisions = questions
            .into_iter()
            .map(|question| {
                resolve_question(
                    question.id,
                    candidates_by_question
                        .get(&question.id)
                        .map(Vec::as_slice)
                        .unwrap_or_default(),
                )
            })
            .collect();
        let plan = AnswerResolutionPlan {
            task_id: snapshot.task_id,
            question_snapshot_id: snapshot.id,
            decisions,
        };
        plan.validate()
            .map_err(|_| ConservativeAnswerResolverError::ResolutionInvalid)?;
        Ok(plan)
    }
}

fn validate_and_group_evidence<'s, 'a>(
    snapshot: &'s asterism_storage::QuestionSnapshot,
    candidates: &'a [AnswerCandidateRecord],
) -> Result<(Questions<'s>, CandidatesByQuestion<'a>), ConservativeAnswerResolverError> {
    let mut question_ids = BTreeSet::new();
    let mut positions = BTreeSet::new();
    for question in &snapshot.questions {
        if question.task_id != snapshot.task_id
            || question.validate().is_err()
            || !question_ids.insert(question.id)
            || !positions.insert(question.position)
        {
            return Err(ConservativeAnswerResolverError::EvidenceInvalid);
        }
    }
    let mut candidate_ids = BTreeSet::new();
    let mut by_question = BTreeMap::<QuestionId, Vec<&AnswerCandidateRecord>>::new();
    for record in candidates {
        if record.question_snapshot_id != snapshot.id
            || !question_ids.contains(&record.candidate.question_id)
            || record.candidate.validate().is_err()
            || !candidate_ids.insert(record.id)
        {
            return Err(ConservativeAnswerResolverError::EvidenceInvalid);
        }
        by_question
            .entry(record.candidate.question_id)
            .or_default()
            .push(record);
    }
    Ok((snapshot.questions.iter().collect(), by_question))
}

fn resolve_question(
    question_id: QuestionId,
    candidates: &[&AnswerCandidateRecord],
) -> AnswerResolutionDecision {
    let mut known = candidates
        .iter()
        .copied()
        .filter(|record| !matches!(record.candidate.answer, NormalizedAnswer::Unknown))
        .collect::<Vec<_>>();
    known.sort_by_key(|record| record.id);
    if known.is_empty() {
        return AnswerResolutionDecision {
            question_id,
            status: AnswerResolutionStatus::Missing,
            considered_candidate_ids: Vec::new(),
            selected_candidate_id: None,
            selected_answer: None,
        };
    }
    let consensus_answer = known[0].candidate.answer.clone();
    let considered_candidate_ids = known.iter().map(|record| record.id).collect::<Vec<_>>();
    if known
        .iter()
        .skip(1)
        .any(|record| record.candidate.answer != consensus_answer)
    {
        return AnswerResolutionDecision {
            question_id,
            status: AnswerResolutionStatus::Conflict,
            considered_candidate_ids,
            selected_candidate_id: None,
            selected_answer: None,
        };
    }

    let selected = known
        .into_iter()
        .max_by_key(|record| {
            (
                record
                    .candidate
                    .confidence
                    .map_or(0, asterism_domain::AnswerConfidence::basis_points),
                record.created_at,
                record.id,
            )
        })
        .expect("known candidate set is non-empty");
    AnswerResolutionDecision {
        question_id,
        status: AnswerResolutionStatus::Selected,
        considered_candidate_ids,
        selected_candidate_id: Some(selected.id),
        selected_answer: Some(consensus_answer),
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConservativeAnswerResolverError {
    #[error("question snapshot was not found")]
    QuestionSnapshotNotFound,
    #[error("persisted Question or AnswerCandidate evidence is inconsistent")]
    EvidenceInvalid,
    #[error("derived AnswerResolutionPlan is invalid")]
    ResolutionInvalid,
    #[error(transparent)]
    Storage(#[from] StorageError),
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use asterism_domain::{
        AnswerCandidate, AnswerCandidateId, AnswerConfidence, AnswerSource, ProviderId, Question,
        QuestionKind,
    };
    use async_trait::async_trait;
    use chrono::{Duration, Utc};

    use super::*;
    use asterism_storage::QuestionSnapshot;

    #[derive(Debug)]
    struct FakeRepository {
        owner_id: UserId,
        snapshot: QuestionSnapshot,
        candidates: Mutex<Vec<AnswerCandidateRecord>>,
    }

    #[async_trait]
    impl QuestionSnapshotRepository for FakeRepository {
        async fn save_question_snapshot(
            &self,
            _snapshot: &QuestionSnapshot,
        ) -> Result<(), StorageError> {
            unreachable!("resolution is read-only")
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
            unreachable!("resolution requires an explicit snapshot")
        }
    }

    #[async_trait]
    impl AnswerCandidateRepository for FakeRepository {
        async fn save_answer_candidate_batch(
            &self,
            _candidates: &[AnswerCandidateRecord],
        ) -> Result<(), StorageError> {
            unreachable!("resolution never persists a winner")
        }

        async fn list_owned_answer_candidates(
            &self,
            owner_id: UserId,
            question_snapshot_id: QuestionSnapshotId,
        ) -> Result<Vec<AnswerCandidateRecord>, StorageError> {
            if owner_id == self.owner_id && question_snapshot_id == self.snapshot.id {
                Ok(self.candidates.lock().unwrap().clone())
            } else {
                Ok(Vec::new())
            }
        }
    }

    #[tokio::test]
    async fn resolver_selects_only_consensus_and_surfaces_conflict_and_missing() {
        let (service, command, question_ids) = fixture();
        let plan = service.resolve(command).await.unwrap();

        assert_eq!(plan.decisions.len(), 3);
        assert_eq!(plan.decisions[0].question_id, question_ids[0]);
        assert_eq!(plan.decisions[0].status, AnswerResolutionStatus::Selected);
        let selected_id = plan.decisions[0].selected_candidate_id.unwrap();
        let records = service.snapshots.candidates.lock().unwrap();
        let selected = records
            .iter()
            .find(|record| record.id == selected_id)
            .unwrap();
        assert_eq!(
            selected.candidate.confidence,
            Some(AnswerConfidence::try_new(9_000).unwrap())
        );
        assert_eq!(plan.decisions[1].status, AnswerResolutionStatus::Conflict);
        assert_eq!(plan.decisions[1].considered_candidate_ids.len(), 2);
        assert_eq!(plan.decisions[2].status, AnswerResolutionStatus::Missing);
        assert!(plan.decisions[2].considered_candidate_ids.is_empty());
    }

    #[tokio::test]
    async fn foreign_candidate_binding_fails_the_whole_resolution() {
        let (service, command, _) = fixture();
        service.snapshots.candidates.lock().unwrap()[0].question_snapshot_id =
            QuestionSnapshotId::new();
        assert!(matches!(
            service.resolve(command).await,
            Err(ConservativeAnswerResolverError::EvidenceInvalid)
        ));
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the fixture keeps consensus, conflict and Unknown-only evidence visible together"
    )]
    fn fixture() -> (
        ConservativeAnswerResolverService<FakeRepository>,
        ResolveAnswerCandidatesCommand,
        [QuestionId; 3],
    ) {
        let owner_id = UserId::new();
        let task_id = TaskId::new();
        let snapshot_id = QuestionSnapshotId::new();
        let question_ids = [QuestionId::new(), QuestionId::new(), QuestionId::new()];
        let questions = question_ids
            .iter()
            .enumerate()
            .map(|(index, question_id)| Question {
                id: *question_id,
                task_id,
                remote_question_id: Some(format!("question-{}", index + 1)),
                kind: QuestionKind::TrueFalse,
                stem: format!("Question {}", index + 1),
                options: Vec::new(),
                attachments: Vec::new(),
                metadata_sanitized: serde_json::json!({}),
                position: u32::try_from(index + 1).unwrap(),
            })
            .collect();
        let now = Utc::now();
        let make_candidate =
            |question_id, answer, confidence: Option<u16>, created_at| AnswerCandidateRecord {
                id: AnswerCandidateId::new(),
                question_snapshot_id: snapshot_id,
                candidate: AnswerCandidate {
                    question_id,
                    source: AnswerSource::Manual,
                    answer,
                    confidence: confidence.map(|value| AnswerConfidence::try_new(value).unwrap()),
                    explanation: None,
                    provenance_sanitized: serde_json::json!({"origin": "fixture"}),
                },
                created_at,
            };
        let candidates = vec![
            make_candidate(
                question_ids[0],
                NormalizedAnswer::Boolean(true),
                Some(8_000),
                now - Duration::seconds(1),
            ),
            make_candidate(
                question_ids[0],
                NormalizedAnswer::Boolean(true),
                Some(9_000),
                now,
            ),
            make_candidate(
                question_ids[1],
                NormalizedAnswer::Boolean(true),
                Some(9_000),
                now,
            ),
            make_candidate(
                question_ids[1],
                NormalizedAnswer::Boolean(false),
                Some(9_000),
                now,
            ),
            make_candidate(question_ids[2], NormalizedAnswer::Unknown, None, now),
        ];
        (
            ConservativeAnswerResolverService::new(FakeRepository {
                owner_id,
                snapshot: QuestionSnapshot {
                    id: snapshot_id,
                    task_id,
                    provider_id: ProviderId::new("provider-alpha").unwrap(),
                    provider_version: "1.0.0".to_owned(),
                    captured_at: now,
                    questions,
                    groups: Vec::new(),
                },
                candidates: Mutex::new(candidates),
            }),
            ResolveAnswerCandidatesCommand {
                owner_id,
                task_id,
                question_snapshot_id: snapshot_id,
            },
            question_ids,
        )
    }
}
