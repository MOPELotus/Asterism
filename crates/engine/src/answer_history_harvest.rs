use std::{collections::BTreeSet, sync::Arc};

use asterism_domain::{
    AnswerBootstrapHarvestState, AnswerCandidate, AnswerCandidateId, AnswerEvidenceClass,
    AnswerSource, AuthState, CorpusProjectionEligibility, NormalizedAnswer, PrivateAnswerEvidence,
    PrivateAnswerEvidenceId, ProviderId, Question, QuestionGroup, QuestionGroupChild,
    QuestionSnapshotId, Timestamp, UnmatchedEvidenceReason,
};
use asterism_provider_api::{
    AnswerHistoryCursor, AnswerHistoryTaskRef, AnswerHistoryTaskRequest, ProviderContext,
    ProviderError, ProviderErrorKind, ProviderRegistry,
};
use asterism_storage::{
    AnswerBootstrapHarvestCompletion, AnswerBootstrapHarvestFailure,
    AnswerBootstrapHarvestRepository, AnswerBootstrapHarvestYield, AnswerCandidateRecord,
    AnswerHistoryIngestRequest, AnswerHistoryIngestionRepository, ClaimedAnswerBootstrapHarvest,
    ProtocolObservationRepository, ProviderAccountRuntimeRepository, QuestionSnapshot,
    StorageError, TaskRuntimeRepository,
};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::protocol_observation::{
    ProviderProtocolObservationRecordError, record_provider_protocol_observation,
};

const WATERMARK_VERSION: u32 = 1;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AnswerHistoryHarvestWorkerConfig {
    pub worker_id: String,
    pub claim_limit: u32,
    pub claim_ttl_seconds: u64,
    pub page_yield_delay_seconds: u64,
    pub retry_delay_seconds: u64,
    pub max_provider_retry_delay_seconds: u64,
}

impl AnswerHistoryHarvestWorkerConfig {
    fn validate(&self) -> Result<(), AnswerHistoryHarvestWorkerError> {
        if self.worker_id.is_empty()
            || self.worker_id.len() > 128
            || self.worker_id.trim() != self.worker_id
            || self.worker_id.chars().any(char::is_control)
            || self.claim_limit == 0
            || self.claim_limit > 100
            || self.claim_ttl_seconds == 0
            || self.page_yield_delay_seconds == 0
            || self.retry_delay_seconds == 0
            || self.max_provider_retry_delay_seconds < self.retry_delay_seconds
            || [
                self.claim_ttl_seconds,
                self.page_yield_delay_seconds,
                self.retry_delay_seconds,
                self.max_provider_retry_delay_seconds,
            ]
            .into_iter()
            .any(|seconds| i64::try_from(seconds).is_err())
        {
            return Err(AnswerHistoryHarvestWorkerError::InvalidConfig);
        }
        Ok(())
    }
}

pub struct AnswerHistoryHarvestWorker<H, A, T, I> {
    registry: Arc<ProviderRegistry>,
    harvests: H,
    accounts: A,
    tasks: T,
    imports: I,
    protocol_observations: Option<Arc<dyn ProtocolObservationRepository>>,
    config: AnswerHistoryHarvestWorkerConfig,
}

impl<H, A, T, I> AnswerHistoryHarvestWorker<H, A, T, I> {
    /// Builds the bounded one-page-per-claim history worker.
    ///
    /// # Errors
    ///
    /// Rejects unsafe worker identity, batch, lease or retry intervals.
    pub fn new(
        registry: Arc<ProviderRegistry>,
        harvests: H,
        accounts: A,
        tasks: T,
        imports: I,
        config: AnswerHistoryHarvestWorkerConfig,
    ) -> Result<Self, AnswerHistoryHarvestWorkerError> {
        config.validate()?;
        Ok(Self {
            registry,
            harvests,
            accounts,
            tasks,
            imports,
            protocol_observations: None,
            config,
        })
    }

    #[must_use]
    pub fn with_protocol_observations(
        mut self,
        observations: Arc<dyn ProtocolObservationRepository>,
    ) -> Self {
        self.protocol_observations = Some(observations);
        self
    }
}

impl<H, A, T, I> std::fmt::Debug for AnswerHistoryHarvestWorker<H, A, T, I> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AnswerHistoryHarvestWorker")
            .field("registry", &self.registry)
            .field("harvests", &"configured")
            .field("accounts", &"configured")
            .field("tasks", &"configured")
            .field("imports", &"configured")
            .field(
                "protocol_observations",
                &self.protocol_observations.is_some(),
            )
            .field("config", &self.config)
            .finish()
    }
}

impl<H, A, T, I> AnswerHistoryHarvestWorker<H, A, T, I>
where
    H: AnswerBootstrapHarvestRepository,
    A: ProviderAccountRuntimeRepository,
    T: TaskRuntimeRepository,
    I: AnswerHistoryIngestionRepository,
{
    /// Claims a bounded batch and processes at most one Provider page for each
    /// harvest before completing, yielding or recording a bounded failure.
    ///
    /// # Errors
    ///
    /// Returns only claim/lifecycle persistence and clock-boundary failures;
    /// Provider and content failures are sanitized into the harvest ledger.
    pub async fn tick_once(
        &self,
        now: Timestamp,
    ) -> Result<AnswerHistoryHarvestTickReport, AnswerHistoryHarvestWorkerError> {
        let lease_expires_at = add_seconds(now, self.config.claim_ttl_seconds)?;
        let eligible_provider_ids = self
            .registry
            .metadata()
            .filter(|metadata| {
                metadata.advertises(asterism_provider_api::ProviderCapability::AnswerHistoryHarvest)
            })
            .map(|metadata| metadata.id.clone())
            .collect::<BTreeSet<_>>();
        let claimed = self
            .harvests
            .claim_due_answer_bootstrap_harvests(
                &self.config.worker_id,
                &eligible_provider_ids,
                now,
                lease_expires_at,
                self.config.claim_limit,
            )
            .await?;
        let mut report = AnswerHistoryHarvestTickReport {
            claimed: claimed.len(),
            ..AnswerHistoryHarvestTickReport::default()
        };
        for harvest in claimed {
            match self.process_claimed(&harvest, now).await? {
                PageOutcome::Completed { imported } => {
                    report.completed += 1;
                    report.imported_tasks += imported;
                }
                PageOutcome::Yielded { imported } => {
                    report.yielded += 1;
                    report.imported_tasks += imported;
                }
                PageOutcome::RetryScheduled => report.retry_scheduled += 1,
                PageOutcome::DeadLetter => report.dead_lettered += 1,
            }
        }
        Ok(report)
    }

    async fn process_claimed(
        &self,
        claimed: &ClaimedAnswerBootstrapHarvest,
        now: Timestamp,
    ) -> Result<PageOutcome, AnswerHistoryHarvestWorkerError> {
        match self.read_and_import_page(claimed, now).await {
            Ok(PageReadOutcome::Completed { scanned, imported }) => {
                let watermark = encode_watermark(None, true)?;
                self.harvests
                    .complete_answer_bootstrap_harvest(AnswerBootstrapHarvestCompletion {
                        harvest_id: claimed.harvest.id,
                        schedule_id: claimed.harvest.schedule_id,
                        worker_id: &claimed.worker_id,
                        scanned_task_count: scanned,
                        total_task_count: scanned,
                        watermark_sanitized: &watermark,
                        at: now,
                    })
                    .await?;
                Ok(PageOutcome::Completed { imported })
            }
            Ok(PageReadOutcome::Continue {
                scanned,
                imported,
                cursor,
            }) => {
                let watermark = encode_watermark(Some(cursor), false)?;
                self.harvests
                    .yield_answer_bootstrap_harvest(AnswerBootstrapHarvestYield {
                        harvest_id: claimed.harvest.id,
                        schedule_id: claimed.harvest.schedule_id,
                        worker_id: &claimed.worker_id,
                        scanned_task_count: scanned,
                        total_task_count: None,
                        watermark_sanitized: &watermark,
                        run_at: add_seconds(now, self.config.page_yield_delay_seconds)?,
                        at: now,
                    })
                    .await?;
                Ok(PageOutcome::Yielded { imported })
            }
            Err(error) => self.record_failure(claimed, &error, now).await,
        }
    }

    async fn read_and_import_page(
        &self,
        claimed: &ClaimedAnswerBootstrapHarvest,
        now: Timestamp,
    ) -> Result<PageReadOutcome, PageError> {
        if claimed.harvest.state != AnswerBootstrapHarvestState::Running {
            return Err(PageError::InvalidBinding);
        }
        let account = self
            .accounts
            .find_runtime_provider_account(claimed.harvest.provider_account_id)
            .await
            .map_err(PageError::Storage)?
            .ok_or(PageError::AccountMissing)?;
        if account.id != claimed.harvest.provider_account_id
            || account.owner_id != claimed.harvest.owner_user_id
            || account.provider_id != claimed.harvest.provider_id
        {
            return Err(PageError::InvalidBinding);
        }
        if account.auth_state != AuthState::Authenticated {
            return Err(PageError::AuthenticationRequired);
        }
        let entry = self
            .registry
            .get(&account.provider_id)
            .ok_or(PageError::ProviderNotReady)?;
        let capability = entry
            .answer_history_harvest
            .clone()
            .ok_or(PageError::ProviderNotReady)?;
        let provider_version = entry.metadata.implementation_version.clone();
        let cursor = decode_watermark(&claimed.harvest.watermark_sanitized, &account.provider_id)?;
        let context = ProviderContext {
            provider_id: account.provider_id.clone(),
            account_id: account.id,
            credential_refs: account.credential_refs.clone(),
            correlation_id: format!("answer-bootstrap-harvest:{}", claimed.harvest.id),
        };
        let page = match capability
            .list_answer_history(&context, cursor.as_ref())
            .await
        {
            Ok(page) => page,
            Err(error) => {
                let occurrence_scope = format!(
                    "answer-history:{}:page:{}",
                    claimed.harvest.id, claimed.harvest.scanned_task_count
                );
                self.record_protocol_observation(
                    &account.provider_id,
                    &occurrence_scope,
                    &error,
                    now,
                )
                .await?;
                return Err(PageError::Provider(error));
            }
        };
        let (references, next_cursor, complete) = page.into_parts();
        let page_count =
            u32::try_from(references.len()).map_err(|_| PageError::InvalidProviderEvidence)?;
        if next_cursor.is_some() && next_cursor.as_ref() == cursor.as_ref() {
            return Err(PageError::InvalidProviderEvidence);
        }
        let mut imported = 0usize;
        for reference in references {
            imported += usize::from(
                self.read_and_import_task(
                    claimed,
                    &account,
                    capability.as_ref(),
                    &context,
                    &provider_version,
                    reference,
                    now,
                )
                .await?,
            );
        }
        let scanned = claimed
            .harvest
            .scanned_task_count
            .checked_add(page_count)
            .ok_or(PageError::InvalidProviderEvidence)?;
        if complete {
            Ok(PageReadOutcome::Completed { scanned, imported })
        } else {
            Ok(PageReadOutcome::Continue {
                scanned,
                imported,
                cursor: next_cursor.ok_or(PageError::InvalidProviderEvidence)?,
            })
        }
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "the helper keeps one fully bound Provider history read and import auditable"
    )]
    async fn read_and_import_task(
        &self,
        claimed: &ClaimedAnswerBootstrapHarvest,
        account: &asterism_domain::ProviderAccount,
        capability: &dyn asterism_provider_api::AnswerHistoryHarvestCapability,
        context: &ProviderContext,
        provider_version: &str,
        reference: AnswerHistoryTaskRef,
        now: Timestamp,
    ) -> Result<bool, PageError> {
        let task = self
            .tasks
            .find_runtime_task_by_remote_identity(account.id, &reference.remote_task_id)
            .await
            .map_err(PageError::Storage)?
            .ok_or(PageError::TaskNotMaterialized)?;
        if task.provider_account_id != account.id {
            return Err(PageError::InvalidBinding);
        }
        let request = AnswerHistoryTaskRequest {
            task_id: task.id,
            course_id: task.course_id,
            reference,
        };
        let structured_evidence = match capability
            .read_structured_answer_history_task(context, &request)
            .await
        {
            Ok(evidence) => evidence,
            Err(error)
                if matches!(
                    error.kind,
                    ProviderErrorKind::UnsupportedTask | ProviderErrorKind::RemoteChanged
                ) =>
            {
                // A completed task may legitimately have no reviewed result
                // page (video-only knowledge point, expired shell, unsupported
                // native question). It still advances the durable page cursor;
                // only transport/auth/protocol failures stop the scan.
                return Ok(false);
            }
            Err(error) => {
                let occurrence_scope =
                    format!("answer-history:{}:task:{}", claimed.harvest.id, task.id);
                self.record_protocol_observation(
                    &account.provider_id,
                    &occurrence_scope,
                    &error,
                    now,
                )
                .await?;
                return Err(PageError::Provider(error));
            }
        };
        let (provider_evidence, groups) = structured_evidence.into_parts();
        provider_evidence
            .validate(&request)
            .map_err(|_| PageError::InvalidProviderEvidence)?;
        // A Provider read can take materially longer than one worker tick (the
        // Chaoxing history adapter may enumerate a full course before reading
        // the first reviewed result). Compare its observation with a fresh
        // post-read timestamp, not the tick-start timestamp passed into this
        // method, otherwise every honest long-running read appears to come
        // from the future.
        let imported_at = Utc::now();
        if provider_evidence.observed_at > imported_at {
            return Err(PageError::InvalidProviderEvidence);
        }
        let material = build_import_material(
            &claimed.harvest,
            &task,
            provider_version,
            &request.reference,
            &provider_evidence,
            &groups,
            imported_at,
        )?;
        self.imports
            .ingest_answer_history_task(AnswerHistoryIngestRequest {
                owner_user_id: claimed.harvest.owner_user_id,
                provider_account_id: account.id,
                provider_attempt_digest: material.provider_attempt_digest,
                result_digest: material.result_digest,
                snapshot: &material.snapshot,
                candidates: &material.candidates,
                evidence: &material.evidence,
                score: material.score,
                retake: material.retake.as_ref(),
                provenance_sanitized: &material.provenance_sanitized,
                observed_at: material.observed_at,
                imported_at,
            })
            .await
            .map_err(PageError::Storage)?;
        Ok(true)
    }

    async fn record_protocol_observation(
        &self,
        provider_id: &ProviderId,
        occurrence_scope: &str,
        error: &ProviderError,
        observed_at: Timestamp,
    ) -> Result<(), PageError> {
        record_provider_protocol_observation(
            self.protocol_observations.as_deref(),
            provider_id,
            None,
            occurrence_scope,
            error,
            observed_at,
        )
        .await
        .map_err(|error| match error {
            ProviderProtocolObservationRecordError::Invalid => PageError::InvalidProviderEvidence,
            ProviderProtocolObservationRecordError::Storage(error) => PageError::Storage(error),
        })
    }

    async fn record_failure(
        &self,
        claimed: &ClaimedAnswerBootstrapHarvest,
        error: &PageError,
        now: Timestamp,
    ) -> Result<PageOutcome, AnswerHistoryHarvestWorkerError> {
        let failure = AnswerHistoryHarvestFailure::from_error(error);
        tracing::warn!(
            harvest_id = %claimed.harvest.id,
            provider_id = %claimed.harvest.provider_id,
            failure_code = failure.code,
            error = ?error,
            "answer-history scan page failed"
        );
        let retry_at = if failure.retryable {
            Some(add_seconds(
                now,
                failure
                    .retry_after_seconds
                    .unwrap_or(self.config.retry_delay_seconds)
                    .max(self.config.retry_delay_seconds)
                    .min(self.config.max_provider_retry_delay_seconds),
            )?)
        } else {
            None
        };
        let harvest = self
            .harvests
            .fail_answer_bootstrap_harvest(AnswerBootstrapHarvestFailure {
                harvest_id: claimed.harvest.id,
                schedule_id: claimed.harvest.schedule_id,
                worker_id: &claimed.worker_id,
                error_sanitized: failure.code,
                retry_at,
                at: now,
            })
            .await?;
        match harvest.state {
            AnswerBootstrapHarvestState::Paused => Ok(PageOutcome::RetryScheduled),
            AnswerBootstrapHarvestState::Failed => Ok(PageOutcome::DeadLetter),
            _ => Err(AnswerHistoryHarvestWorkerError::LifecycleMismatch),
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AnswerHistoryHarvestTickReport {
    pub claimed: usize,
    pub completed: usize,
    pub yielded: usize,
    pub retry_scheduled: usize,
    pub dead_lettered: usize,
    pub imported_tasks: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AnswerHistoryHarvestFailure {
    pub code: &'static str,
    pub retryable: bool,
    pub retry_after_seconds: Option<u64>,
}

impl AnswerHistoryHarvestFailure {
    fn from_error(error: &PageError) -> Self {
        match error {
            PageError::AccountMissing => Self::terminal("provider_account_missing"),
            PageError::AuthenticationRequired => {
                Self::retryable("provider_authentication_required")
            }
            PageError::ProviderNotReady => Self::retryable("provider_history_not_ready"),
            PageError::TaskNotMaterialized => Self::retryable("history_task_not_materialized"),
            PageError::InvalidBinding => Self::terminal("history_binding_invalid"),
            PageError::InvalidProviderEvidence => Self::terminal("provider_history_invalid"),
            PageError::Provider(provider) => match provider.kind {
                ProviderErrorKind::RateLimited => Self {
                    code: "provider_rate_limited",
                    retryable: true,
                    retry_after_seconds: provider.retry_after_seconds,
                },
                ProviderErrorKind::Network | ProviderErrorKind::ProviderUnavailable => {
                    Self::retryable("provider_unavailable")
                }
                ProviderErrorKind::Authentication | ProviderErrorKind::Authorization => {
                    Self::retryable("provider_authentication_required")
                }
                ProviderErrorKind::ProtocolDrift | ProviderErrorKind::InvalidResponse => {
                    Self::terminal("provider_history_invalid")
                }
                ProviderErrorKind::RemoteChanged
                | ProviderErrorKind::UnsupportedTask
                | ProviderErrorKind::HumanRequired => Self::terminal("provider_history_blocked"),
                ProviderErrorKind::Internal => Self::terminal("provider_history_internal"),
            },
            PageError::Storage(storage) => match storage {
                StorageError::Sqlx(_) | StorageError::Migration(_) => {
                    Self::retryable("history_storage_unavailable")
                }
                _ => Self::terminal("history_storage_invalid"),
            },
        }
    }

    const fn retryable(code: &'static str) -> Self {
        Self {
            code,
            retryable: true,
            retry_after_seconds: None,
        }
    }

    const fn terminal(code: &'static str) -> Self {
        Self {
            code,
            retryable: false,
            retry_after_seconds: None,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AnswerHistoryHarvestWorkerError {
    #[error("answer history harvest worker configuration is invalid")]
    InvalidConfig,
    #[error("answer history harvest worker timestamp is outside the supported range")]
    TimestampOverflow,
    #[error("answer history harvest lifecycle returned an unexpected state")]
    LifecycleMismatch,
    #[error(transparent)]
    Storage(#[from] StorageError),
    #[error(transparent)]
    Serialization(#[from] serde_json::Error),
}

#[derive(Debug)]
enum PageError {
    AccountMissing,
    AuthenticationRequired,
    ProviderNotReady,
    TaskNotMaterialized,
    InvalidBinding,
    InvalidProviderEvidence,
    Provider(ProviderError),
    Storage(StorageError),
}

enum PageReadOutcome {
    Completed {
        scanned: u32,
        imported: usize,
    },
    Continue {
        scanned: u32,
        imported: usize,
        cursor: AnswerHistoryCursor,
    },
}

enum PageOutcome {
    Completed { imported: usize },
    Yielded { imported: usize },
    RetryScheduled,
    DeadLetter,
}

struct ImportMaterial {
    provider_attempt_digest: [u8; 32],
    result_digest: [u8; 32],
    snapshot: QuestionSnapshot,
    candidates: Vec<AnswerCandidateRecord>,
    evidence: Vec<PrivateAnswerEvidence>,
    score: Option<asterism_domain::SubmissionScore>,
    retake: Option<asterism_provider_api::AnswerHistoryRetakeFacts>,
    provenance_sanitized: Value,
    observed_at: Timestamp,
}

fn build_import_material(
    harvest: &asterism_domain::AnswerBootstrapHarvest,
    task: &asterism_domain::Task,
    provider_version: &str,
    reference: &AnswerHistoryTaskRef,
    provider_evidence: &asterism_provider_api::ProviderAnswerHistoryTaskEvidence,
    groups: &[QuestionGroup],
    verified_at: Timestamp,
) -> Result<ImportMaterial, PageError> {
    let snapshot_id = QuestionSnapshotId::new();
    let snapshot = QuestionSnapshot {
        id: snapshot_id,
        task_id: task.id,
        provider_id: harvest.provider_id.clone(),
        provider_version: provider_version.to_owned(),
        captured_at: provider_evidence.observed_at,
        questions: provider_evidence.questions.clone(),
        groups: groups.to_vec(),
    };
    let grouped_question_ids = groups
        .iter()
        .flat_map(|group| group.children.iter())
        .filter_map(|child| match child {
            QuestionGroupChild::Question(question_id) => Some(*question_id),
            QuestionGroupChild::Group(_) => None,
        })
        .collect::<BTreeSet<_>>();
    let mut candidates = Vec::new();
    let mut evidence = Vec::new();
    for question_evidence in &provider_evidence.question_evidence {
        let question = provider_evidence
            .questions
            .iter()
            .find(|question| question.id == question_evidence.question_id)
            .ok_or(PageError::InvalidProviderEvidence)?;
        if let Some(answer) = &question_evidence.official_answer {
            push_history_answer(
                &mut candidates,
                &mut evidence,
                &HistoryAnswerInput {
                    harvest,
                    task,
                    snapshot_id,
                    question,
                    grouped: grouped_question_ids.contains(&question.id),
                    answer,
                    evidence_class: Some(AnswerEvidenceClass::Official),
                    kind: "official",
                    reference,
                    provider_evidence,
                    question_provenance: &question_evidence.provenance_sanitized,
                    verified_at,
                },
            )?;
        }
        if let Some(answer) = &question_evidence.submitted_answer {
            let evidence_class = question_evidence.submitted_answer_correct.map(|correct| {
                if correct {
                    AnswerEvidenceClass::VerifiedHistorical
                } else {
                    AnswerEvidenceClass::Negative
                }
            });
            push_history_answer(
                &mut candidates,
                &mut evidence,
                &HistoryAnswerInput {
                    harvest,
                    task,
                    snapshot_id,
                    question,
                    grouped: grouped_question_ids.contains(&question.id),
                    answer,
                    evidence_class,
                    kind: "submitted",
                    reference,
                    provider_evidence,
                    question_provenance: &question_evidence.provenance_sanitized,
                    verified_at,
                },
            )?;
        }
    }
    Ok(ImportMaterial {
        provider_attempt_digest: provider_evidence.provider_attempt_digest,
        result_digest: provider_evidence.result_digest,
        snapshot,
        candidates,
        evidence,
        score: provider_evidence.score,
        retake: provider_evidence.retake.clone(),
        provenance_sanitized: provider_evidence.provenance_sanitized.clone(),
        observed_at: provider_evidence.observed_at,
    })
}

struct HistoryAnswerInput<'a> {
    harvest: &'a asterism_domain::AnswerBootstrapHarvest,
    task: &'a asterism_domain::Task,
    snapshot_id: QuestionSnapshotId,
    question: &'a Question,
    grouped: bool,
    answer: &'a NormalizedAnswer,
    evidence_class: Option<AnswerEvidenceClass>,
    kind: &'static str,
    reference: &'a AnswerHistoryTaskRef,
    provider_evidence: &'a asterism_provider_api::ProviderAnswerHistoryTaskEvidence,
    question_provenance: &'a Value,
    verified_at: Timestamp,
}

fn push_history_answer(
    candidates: &mut Vec<AnswerCandidateRecord>,
    evidence: &mut Vec<PrivateAnswerEvidence>,
    input: &HistoryAnswerInput<'_>,
) -> Result<(), PageError> {
    let candidate_id = AnswerCandidateId::new();
    candidates.push(AnswerCandidateRecord {
        id: candidate_id,
        question_snapshot_id: input.snapshot_id,
        candidate: AnswerCandidate {
            question_id: input.question.id,
            source: AnswerSource::ProviderNative,
            answer: input.answer.clone(),
            confidence: None,
            explanation: None,
            provenance_sanitized: json!({
                "source": "answer_history_bootstrap",
                "kind": input.kind,
                "reference": {
                    "remote_task_id": input.reference.remote_task_id,
                    "course_remote_id": input.reference.course_remote_id,
                    "completed_at": input.reference.completed_at,
                    "metadata": input.reference.metadata_sanitized,
                },
                "task": input.provider_evidence.provenance_sanitized,
                "question": input.question_provenance,
                "score": input.provider_evidence.score,
                "retake": input.provider_evidence.retake,
            }),
        },
        created_at: input.provider_evidence.observed_at,
    });
    let Some(evidence_class) = input.evidence_class else {
        return Ok(());
    };
    let question_content_fingerprint = input
        .question
        .content_fingerprint()
        .map_err(|_| PageError::InvalidProviderEvidence)?;
    let record = PrivateAnswerEvidence {
        id: PrivateAnswerEvidenceId::new(),
        owner_user_id: input.harvest.owner_user_id,
        provider_id: input.harvest.provider_id.clone(),
        provider_account_id: input.harvest.provider_account_id,
        course_id: input.task.course_id,
        task_id: input.task.id,
        question_snapshot_id: input.snapshot_id,
        question_id: input.question.id,
        execution_attempt_id: None,
        provider_attempt_digest: Some(input.provider_evidence.provider_attempt_digest),
        source_candidate_id: Some(candidate_id),
        question: input.question.clone(),
        question_content_fingerprint,
        answer: input.answer.clone(),
        answer_source: AnswerSource::ProviderNative,
        evidence_class,
        result_digest: Some(input.provider_evidence.result_digest),
        provenance_sanitized: json!({
            "source": "answer_history_bootstrap",
            "kind": input.kind,
            "reference": {
                "remote_task_id": input.reference.remote_task_id,
                "course_remote_id": input.reference.course_remote_id,
                "completed_at": input.reference.completed_at,
                "metadata": input.reference.metadata_sanitized,
            },
            "task": input.provider_evidence.provenance_sanitized,
            "question": input.question_provenance,
            "score": input.provider_evidence.score,
            "retake": input.provider_evidence.retake,
        }),
        projection: if input.grouped {
            CorpusProjectionEligibility::Unmatched(UnmatchedEvidenceReason::MissingSharedContext)
        } else {
            CorpusProjectionEligibility::for_question_answer(input.question, input.answer)
        },
        observed_at: input.provider_evidence.observed_at,
        verified_at: input.verified_at,
    };
    record
        .validate()
        .map_err(|_| PageError::InvalidProviderEvidence)?;
    evidence.push(record);
    Ok(())
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
struct HarvestWatermark {
    version: u32,
    cursor: Option<AnswerHistoryCursor>,
    complete: bool,
}

fn decode_watermark(
    value: &Value,
    provider_id: &ProviderId,
) -> Result<Option<AnswerHistoryCursor>, PageError> {
    if value.as_object().is_some_and(serde_json::Map::is_empty) {
        return Ok(None);
    }
    let watermark: HarvestWatermark =
        serde_json::from_value(value.clone()).map_err(|_| PageError::InvalidProviderEvidence)?;
    if watermark.version != WATERMARK_VERSION || watermark.complete {
        return Err(PageError::InvalidProviderEvidence);
    }
    let cursor = watermark.cursor.ok_or(PageError::InvalidProviderEvidence)?;
    cursor
        .validate(provider_id)
        .map_err(|_| PageError::InvalidProviderEvidence)?;
    Ok(Some(cursor))
}

fn encode_watermark(
    cursor: Option<AnswerHistoryCursor>,
    complete: bool,
) -> Result<Value, serde_json::Error> {
    serde_json::to_value(HarvestWatermark {
        version: WATERMARK_VERSION,
        cursor,
        complete,
    })
}

fn add_seconds(at: Timestamp, seconds: u64) -> Result<Timestamp, AnswerHistoryHarvestWorkerError> {
    let seconds =
        i64::try_from(seconds).map_err(|_| AnswerHistoryHarvestWorkerError::TimestampOverflow)?;
    at.checked_add_signed(chrono::Duration::seconds(seconds))
        .ok_or(AnswerHistoryHarvestWorkerError::TimestampOverflow)
}

#[cfg(test)]
#[path = "answer_history_harvest_tests.rs"]
mod tests;
