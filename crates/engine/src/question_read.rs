use std::{collections::BTreeSet, sync::Arc};

use asterism_domain::{
    AuditActor, AuthState, MAX_QUESTION_READ_ATTEMPT_TTL_SECONDS, MAX_QUESTION_SESSION_TTL_SECONDS,
    ProviderAccount, ProviderId, Question, QuestionKind, QuestionReadAttempt,
    QuestionReadAttemptId, QuestionReadAttemptState, QuestionSession, QuestionSnapshotId, Task,
    TaskCapability, TaskId, Timestamp, UserId,
};
use asterism_provider_api::{
    ProviderContext, ProviderEntry, ProviderError, ProviderQuestionReadStepOutcome,
    ProviderRegistry, QuestionInventoryCapability, RemoteQuestionRef,
    ResolvedProviderQuestionReadContinuation, ResolvedProviderRuntimeSettings,
};
use asterism_secrets::{SecretAccess, SecretActor, SecretStoreError};
use asterism_storage::{
    ProtocolObservationRepository, ProviderAccountRuntimeRepository,
    ProviderRuntimeSettingsRepository, ProviderRuntimeSettingsTarget,
    QuestionReadAttemptRepository, QuestionReadContinuationAttachRequest,
    QuestionReadContinuationRepositoryFactory, QuestionReadMaterializeOutcome,
    QuestionReadMaterializeRequest, QuestionReadOperationAcceptRequest,
    QuestionReadOperationFinishOutcome, QuestionReadOperationIssueOutcome,
    QuestionReadOperationIssueRequest, QuestionReadOperationState,
    QuestionSessionArtifactRepositoryFactory, QuestionSessionMaterializeRequest, QuestionSnapshot,
    QuestionSnapshotRepository, StorageError, TaskQueryRepository,
};
use chrono::{Duration, Utc};

use crate::{
    AssessmentGuardError, FormalAssessmentPolicy, TaskAction, authorize_task_action,
    protocol_observation::{
        ProviderProtocolObservationRecordError, record_provider_protocol_observation,
    },
};

const MAX_CORRELATION_ID_BYTES: usize = 128;
const MAX_QUESTIONS_PER_READ: usize = 5_000;
const MAX_PRE_QUESTION_OPERATIONS: usize = 128;
const MAX_PRE_QUESTION_DELAY_SECONDS: u64 = 15 * 60;

#[derive(Clone, Debug)]
pub struct ReadTaskQuestionsCommand {
    pub owner_id: UserId,
    pub task_id: TaskId,
    pub correlation_id: String,
}

fn result_from_snapshot(snapshot: QuestionSnapshot) -> ProviderQuestionReadResult {
    ProviderQuestionReadResult::Questions {
        snapshot_id: snapshot.id,
        task_id: snapshot.task_id,
        provider_id: snapshot.provider_id,
        provider_version: snapshot.provider_version,
        captured_at: snapshot.captured_at,
        questions: snapshot.questions,
    }
}

fn validate_materialized_questions(
    task_id: TaskId,
    questions: &[Question],
) -> Result<(), ProviderQuestionReadError> {
    if questions.is_empty() || questions.len() > MAX_QUESTIONS_PER_READ {
        return Err(ProviderQuestionReadError::ProviderResponseInvalid);
    }
    let mut ids = BTreeSet::new();
    let mut remote_ids = BTreeSet::new();
    let mut positions = BTreeSet::new();
    if questions.iter().any(|question| {
        question.task_id != task_id
            || question.validate().is_err()
            || !ids.insert(question.id)
            || !positions.insert(question.position)
            || question
                .remote_question_id
                .as_ref()
                .is_some_and(|remote_id| !remote_ids.insert(remote_id.as_str()))
    }) {
        Err(ProviderQuestionReadError::ProviderResponseInvalid)
    } else {
        Ok(())
    }
}

fn valid_provider_label(provider_id: &ProviderId, value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 96
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_' | b'.')
        })
        && value
            .strip_prefix(provider_id.as_str())
            .is_some_and(|suffix| suffix.starts_with('.') && suffix.len() > 1)
}

fn question_read_access(owner_id: UserId, correlation_id: &str) -> SecretAccess {
    SecretAccess {
        actor: SecretActor::User(owner_id),
        correlation_id: correlation_id.to_owned(),
        reason: "durable Question read flow".to_owned(),
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum ProviderQuestionReadResult {
    Questions {
        snapshot_id: QuestionSnapshotId,
        task_id: TaskId,
        provider_id: ProviderId,
        provider_version: String,
        captured_at: Timestamp,
        questions: Vec<Question>,
    },
    Completed {
        task_id: TaskId,
        provider_id: ProviderId,
        provider_version: String,
    },
}

#[derive(Clone)]
pub struct ProviderQuestionReadService<Q, A, S> {
    registry: Arc<ProviderRegistry>,
    tasks: Q,
    accounts: A,
    snapshots: S,
    durable: Option<DurableQuestionReadDependencies>,
    protocol_observations: Option<Arc<dyn ProtocolObservationRepository>>,
}

#[derive(Clone)]
struct DurableQuestionReadDependencies {
    settings: Arc<dyn ProviderRuntimeSettingsRepository>,
    attempts: Arc<dyn QuestionReadAttemptRepository>,
    continuations: Arc<dyn QuestionReadContinuationRepositoryFactory>,
    artifacts: Arc<dyn QuestionSessionArtifactRepositoryFactory>,
}

impl<Q, A, S> std::fmt::Debug for ProviderQuestionReadService<Q, A, S> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProviderQuestionReadService")
            .field("registry", &self.registry)
            .field("tasks", &"configured")
            .field("accounts", &"configured")
            .field("snapshots", &"configured")
            .field("durable", &self.durable.is_some())
            .field(
                "protocol_observations",
                &self.protocol_observations.is_some(),
            )
            .finish()
    }
}

impl<Q, A, S> ProviderQuestionReadService<Q, A, S> {
    pub const fn new(registry: Arc<ProviderRegistry>, tasks: Q, accounts: A, snapshots: S) -> Self {
        Self {
            registry,
            tasks,
            accounts,
            snapshots,
            durable: None,
            protocol_observations: None,
        }
    }

    #[must_use]
    pub fn with_durable_flow(
        mut self,
        settings: Arc<dyn ProviderRuntimeSettingsRepository>,
        attempts: Arc<dyn QuestionReadAttemptRepository>,
        continuations: Arc<dyn QuestionReadContinuationRepositoryFactory>,
        artifacts: Arc<dyn QuestionSessionArtifactRepositoryFactory>,
    ) -> Self {
        self.durable = Some(DurableQuestionReadDependencies {
            settings,
            attempts,
            continuations,
            artifacts,
        });
        self
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

impl<Q, A, S> ProviderQuestionReadService<Q, A, S>
where
    Q: TaskQueryRepository,
    A: ProviderAccountRuntimeRepository,
    S: QuestionSnapshotRepository,
{
    /// Discovers and parses one complete, fresh Question set. Provider output
    /// is returned only after every reference and normalized Question passes
    /// identity, ordering, size and sanitization checks.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderQuestionReadError`] for ownership, task/provider
    /// capability, account state, Provider I/O, or all-or-nothing validation
    /// failures.
    #[allow(clippy::too_many_lines)]
    pub async fn read(
        &self,
        command: ReadTaskQuestionsCommand,
    ) -> Result<ProviderQuestionReadResult, ProviderQuestionReadError> {
        if !valid_correlation_id(&command.correlation_id) {
            return Err(ProviderQuestionReadError::InvalidCorrelationId);
        }
        let task = self
            .tasks
            .find_owned_task(command.owner_id, command.task_id)
            .await?
            .ok_or(ProviderQuestionReadError::TaskNotFound)?;
        if !task
            .capabilities
            .contains(&TaskCapability::QuestionInventory)
        {
            return Err(ProviderQuestionReadError::TaskCapabilityUnavailable);
        }
        authorize_task_action(&task, TaskAction::Parse, FormalAssessmentPolicy::default())?;
        let account = self
            .accounts
            .find_runtime_provider_account(task.provider_account_id)
            .await?
            .filter(|account| account.owner_id == command.owner_id)
            .ok_or(ProviderQuestionReadError::TaskNotFound)?;
        if !matches!(account.auth_state, AuthState::Authenticated) {
            return Err(ProviderQuestionReadError::AccountNotAuthenticated);
        }
        let entry = self.registry.get(&account.provider_id).ok_or_else(|| {
            ProviderQuestionReadError::ProviderNotRegistered(account.provider_id.clone())
        })?;
        let inventory = entry.question_inventory.as_ref().ok_or_else(|| {
            ProviderQuestionReadError::CapabilityUnavailable(account.provider_id.clone())
        })?;
        let runtime_settings = self
            .resolve_runtime_settings(&account, &task, entry)
            .await?;
        let context = ProviderContext {
            provider_id: account.provider_id.clone(),
            account_id: account.id,
            credential_refs: account.credential_refs,
            correlation_id: command.correlation_id.clone(),
        };

        if let Some(durable) = &self.durable
            && let Some(existing) = durable
                .attempts
                .find_latest_owned_question_read_attempt(command.owner_id, task.id)
                .await?
                .filter(|attempt| {
                    attempt.provider_id == account.provider_id
                        && attempt.provider_account_id == account.id
                        && attempt.provider_version == entry.metadata.implementation_version
                })
        {
            match existing.state {
                QuestionReadAttemptState::Active => {
                    return self
                        .run_durable_flow(
                            durable,
                            inventory.as_ref(),
                            &context,
                            &task,
                            &account.provider_id,
                            &entry.metadata.implementation_version,
                            &runtime_settings,
                            existing,
                        )
                        .await;
                }
                QuestionReadAttemptState::Ambiguous => {
                    return Err(ProviderQuestionReadError::AmbiguousAttempt(existing.id));
                }
                QuestionReadAttemptState::Materialized => {
                    let snapshot_id = existing
                        .question_snapshot_id
                        .ok_or(ProviderQuestionReadError::ProviderResponseInvalid)?;
                    let snapshot = self
                        .snapshots
                        .find_owned_question_snapshot(command.owner_id, snapshot_id)
                        .await?
                        .filter(|snapshot| {
                            snapshot.task_id == task.id
                                && snapshot.provider_id == account.provider_id
                                && snapshot.provider_version
                                    == entry.metadata.implementation_version
                        })
                        .ok_or(ProviderQuestionReadError::ProviderResponseInvalid)?;
                    return Ok(result_from_snapshot(snapshot));
                }
                QuestionReadAttemptState::Completed => {
                    return Ok(ProviderQuestionReadResult::Completed {
                        task_id: task.id,
                        provider_id: account.provider_id,
                        provider_version: entry.metadata.implementation_version.clone(),
                    });
                }
                QuestionReadAttemptState::Rejected
                | QuestionReadAttemptState::Cancelled
                | QuestionReadAttemptState::Expired => {}
            }
        }

        let initial = match inventory
            .prepare_question_read_attempt(&context, task.id, &task.remote_id, &runtime_settings)
            .await
        {
            Ok(initial) => initial,
            Err(error) => {
                self.record_protocol_observation(
                    &account.provider_id,
                    task.id,
                    &context.correlation_id,
                    "prepare-attempt",
                    &error,
                )
                .await?;
                return Err(error.into());
            }
        };
        if let Some(initial) = initial {
            let durable = self
                .durable
                .as_ref()
                .ok_or(ProviderQuestionReadError::DurableStateUnavailable)?;
            let now = Utc::now();
            let (continuation_type, expected_digest, phase, value, ttl_seconds) =
                initial.into_parts();
            let maximum_ttl = u64::try_from(MAX_QUESTION_READ_ATTEMPT_TTL_SECONDS)
                .map_err(|_| ProviderQuestionReadError::ProviderResponseInvalid)?;
            if ttl_seconds > maximum_ttl {
                return Err(ProviderQuestionReadError::ProviderResponseInvalid);
            }
            let expires_at = now
                + Duration::seconds(
                    i64::try_from(ttl_seconds)
                        .map_err(|_| ProviderQuestionReadError::ProviderResponseInvalid)?,
                );
            let attempt = QuestionReadAttempt::active(
                command.owner_id,
                account.id,
                task.id,
                account.provider_id.clone(),
                entry.metadata.implementation_version.clone(),
                now,
                expires_at,
            )
            .map_err(|_| ProviderQuestionReadError::ProviderResponseInvalid)?;
            durable
                .attempts
                .create_question_read_attempt(
                    &attempt,
                    AuditActor::User(command.owner_id),
                    &command.correlation_id,
                )
                .await?;
            let access = question_read_access(command.owner_id, &command.correlation_id);
            let scoped = durable
                .continuations
                .for_provider(account.provider_id.clone());
            let attached = scoped
                .attach_question_read_continuation(QuestionReadContinuationAttachRequest {
                    attempt_id: attempt.id,
                    continuation_type: &continuation_type,
                    phase: &phase,
                    value,
                    attached_at: now,
                    access: &access,
                })
                .await?;
            if attached.continuation_digest != expected_digest
                || attached.continuation_type != continuation_type
                || attached.phase != phase
            {
                return Err(ProviderQuestionReadError::ProviderResponseInvalid);
            }
            return self
                .run_durable_flow(
                    durable,
                    inventory.as_ref(),
                    &context,
                    &task,
                    &account.provider_id,
                    &entry.metadata.implementation_version,
                    &runtime_settings,
                    attempt,
                )
                .await;
        }

        if !task.capabilities.contains(&TaskCapability::QuestionParse) {
            return Err(ProviderQuestionReadError::TaskCapabilityUnavailable);
        }
        let parser = entry.question_parse.as_ref().ok_or_else(|| {
            ProviderQuestionReadError::CapabilityUnavailable(account.provider_id.clone())
        })?;
        let mut references = match inventory
            .list_question_refs(&context, &task.remote_id)
            .await
        {
            Ok(references) => references,
            Err(error) => {
                self.record_protocol_observation(
                    &account.provider_id,
                    task.id,
                    &context.correlation_id,
                    "inventory",
                    &error,
                )
                .await?;
                return Err(error.into());
            }
        };
        validate_references(&references)?;
        references.sort_by_key(|reference| reference.position);
        let parsed = match parser
            .parse_question_set(&context, task.id, &task.remote_id, &references)
            .await
        {
            Ok(parsed) => parsed,
            Err(error) => {
                self.record_protocol_observation(
                    &account.provider_id,
                    task.id,
                    &context.correlation_id,
                    "parse",
                    &error,
                )
                .await?;
                return Err(error.into());
            }
        };
        let (questions, artifact) = parsed.into_parts();
        if questions.len() != references.len() {
            return Err(ProviderQuestionReadError::ProviderResponseInvalid);
        }
        for (reference, question) in references.iter().zip(&questions) {
            validate_question_binding(&task, reference, question)?;
        }
        validate_materialized_questions(task.id, &questions)?;
        let captured_at = Utc::now();
        let snapshot = QuestionSnapshot {
            id: QuestionSnapshotId::new(),
            task_id: task.id,
            provider_id: account.provider_id.clone(),
            provider_version: entry.metadata.implementation_version.clone(),
            captured_at,
            questions: questions.clone(),
        };
        if let Some(artifact) = artifact {
            let durable = self
                .durable
                .as_ref()
                .ok_or(ProviderQuestionReadError::DurableStateUnavailable)?;
            let (artifact_type, expected_digest, phase, value, ttl_seconds) = artifact.into_parts();
            let maximum_ttl = u64::try_from(MAX_QUESTION_SESSION_TTL_SECONDS)
                .map_err(|_| ProviderQuestionReadError::ProviderResponseInvalid)?;
            if ttl_seconds == 0 || ttl_seconds > maximum_ttl {
                return Err(ProviderQuestionReadError::ProviderResponseInvalid);
            }
            let session = QuestionSession::active(
                command.owner_id,
                account.id,
                task.id,
                account.provider_id.clone(),
                entry.metadata.implementation_version.clone(),
                snapshot.id,
                artifact_type.clone(),
                expected_digest,
                captured_at,
                captured_at
                    + Duration::seconds(
                        i64::try_from(ttl_seconds)
                            .map_err(|_| ProviderQuestionReadError::ProviderResponseInvalid)?,
                    ),
            )
            .map_err(|_| ProviderQuestionReadError::ProviderResponseInvalid)?;
            let access = question_read_access(command.owner_id, &command.correlation_id);
            let persisted = durable
                .artifacts
                .for_provider(account.provider_id.clone())
                .materialize_question_session(QuestionSessionMaterializeRequest {
                    snapshot: &snapshot,
                    session: &session,
                    artifact_phase: &phase,
                    artifact: value,
                    materialized_at: captured_at,
                    access: &access,
                })
                .await?;
            if persisted.session_id != session.id
                || persisted.execution_id.is_some()
                || persisted.continuation_type != artifact_type
                || persisted.continuation_digest != expected_digest
                || persisted.phase != phase
                || persisted.revision != 1
            {
                return Err(ProviderQuestionReadError::ProviderResponseInvalid);
            }
        } else {
            self.snapshots.save_question_snapshot(&snapshot).await?;
        }
        Ok(result_from_snapshot(snapshot))
    }

    async fn record_protocol_observation(
        &self,
        provider_id: &ProviderId,
        task_id: TaskId,
        correlation_id: &str,
        stage: &str,
        error: &ProviderError,
    ) -> Result<(), ProviderQuestionReadError> {
        let occurrence_scope = format!("question-read:{task_id}:{correlation_id}:{stage}");
        record_provider_protocol_observation(
            self.protocol_observations.as_deref(),
            provider_id,
            None,
            &occurrence_scope,
            error,
            Utc::now(),
        )
        .await
        .map_err(|error| match error {
            ProviderProtocolObservationRecordError::Invalid => {
                ProviderQuestionReadError::InvalidProtocolObservation
            }
            ProviderProtocolObservationRecordError::Storage(error) => {
                ProviderQuestionReadError::Storage(error)
            }
        })
    }

    async fn resolve_runtime_settings(
        &self,
        account: &ProviderAccount,
        task: &Task,
        entry: &ProviderEntry,
    ) -> Result<ResolvedProviderRuntimeSettings, ProviderQuestionReadError> {
        let (provider_patch, account_patch, task_patch) = if let Some(durable) = &self.durable {
            (
                durable
                    .settings
                    .find_provider_runtime_settings(&ProviderRuntimeSettingsTarget::Provider {
                        provider_id: account.provider_id.clone(),
                    })
                    .await?,
                durable
                    .settings
                    .find_provider_runtime_settings(
                        &ProviderRuntimeSettingsTarget::ProviderAccount {
                            provider_id: account.provider_id.clone(),
                            provider_account_id: account.id,
                        },
                    )
                    .await?,
                durable
                    .settings
                    .find_provider_runtime_settings(&ProviderRuntimeSettingsTarget::Task {
                        provider_id: account.provider_id.clone(),
                        provider_account_id: account.id,
                        task_id: task.id,
                    })
                    .await?,
            )
        } else {
            (None, None, None)
        };
        entry
            .runtime_settings
            .resolve_with_sources(
                provider_patch.as_ref().map(|record| &record.patch),
                account_patch.as_ref().map(|record| &record.patch),
                task_patch.as_ref().map(|record| &record.patch),
            )
            .map(|(resolved, _)| resolved)
            .map_err(|_| ProviderQuestionReadError::RuntimeSettingsInvalid)
    }

    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    async fn run_durable_flow(
        &self,
        durable: &DurableQuestionReadDependencies,
        inventory: &dyn QuestionInventoryCapability,
        context: &ProviderContext,
        task: &Task,
        provider_id: &ProviderId,
        provider_version: &str,
        runtime_settings: &ResolvedProviderRuntimeSettings,
        attempt: QuestionReadAttempt,
    ) -> Result<ProviderQuestionReadResult, ProviderQuestionReadError> {
        let scoped = durable.continuations.for_provider(provider_id.clone());
        let access = question_read_access(attempt.owner_user_id, &context.correlation_id);
        for _ in 0..MAX_PRE_QUESTION_OPERATIONS {
            let resolved = scoped
                .resolve_question_read_continuation(attempt.id, &access)
                .await?
                .ok_or(ProviderQuestionReadError::DurableStateUnavailable)?;
            if let Some(latest) = &resolved.latest_operation {
                match latest.state {
                    QuestionReadOperationState::Issued => {
                        if attempt.is_expired_at(Utc::now()) {
                            let outcome = scoped
                                .finish_question_read_operation(
                                    latest,
                                    QuestionReadOperationState::Ambiguous,
                                    None,
                                    Utc::now(),
                                    &access,
                                )
                                .await?;
                            if !matches!(
                                outcome,
                                QuestionReadOperationFinishOutcome::Finished { .. }
                                    | QuestionReadOperationFinishOutcome::Duplicate(_)
                            ) {
                                return Err(ProviderQuestionReadError::StateConflict);
                            }
                            return Err(ProviderQuestionReadError::AmbiguousAttempt(attempt.id));
                        }
                        return Err(ProviderQuestionReadError::ConcurrentAttempt(attempt.id));
                    }
                    QuestionReadOperationState::Ambiguous => {
                        return Err(ProviderQuestionReadError::AmbiguousAttempt(attempt.id));
                    }
                    QuestionReadOperationState::Rejected => {
                        return Err(ProviderQuestionReadError::StateConflict);
                    }
                    QuestionReadOperationState::Accepted => {}
                }
            }
            let continuation = ResolvedProviderQuestionReadContinuation {
                continuation_type: &resolved.metadata.continuation_type,
                continuation_digest: resolved.metadata.continuation_digest,
                phase: &resolved.metadata.phase,
                revision: resolved.metadata.revision,
                value: &resolved.value,
            };
            let prepared = match inventory
                .prepare_question_read_operation(
                    context,
                    task.id,
                    &task.remote_id,
                    continuation,
                    runtime_settings,
                )
                .await
            {
                Ok(prepared) => prepared,
                Err(error) => {
                    self.record_protocol_observation(
                        provider_id,
                        task.id,
                        &context.correlation_id,
                        "prepare-operation",
                        &error,
                    )
                    .await?;
                    return Err(error.into());
                }
            };
            let operation_type = prepared.operation_type().to_owned();
            let request_digest = prepared.request_digest();
            let delay_seconds = prepared.delay_before_execute_seconds();
            if !valid_provider_label(provider_id, &operation_type)
                || request_digest == [0; 32]
                || delay_seconds > MAX_PRE_QUESTION_DELAY_SECONDS
            {
                return Err(ProviderQuestionReadError::ProviderResponseInvalid);
            }
            let issued_at = Utc::now();
            let operation = match scoped
                .issue_question_read_operation(QuestionReadOperationIssueRequest {
                    attempt_id: attempt.id,
                    expected_continuation_revision: resolved.metadata.revision,
                    operation_type,
                    request_digest,
                    issued_at,
                    access: &access,
                })
                .await?
            {
                QuestionReadOperationIssueOutcome::Issued(operation) => operation,
                QuestionReadOperationIssueOutcome::Duplicate(operation) => {
                    return Err(ProviderQuestionReadError::ConcurrentAttempt(
                        operation.attempt_id,
                    ));
                }
                QuestionReadOperationIssueOutcome::Conflict
                | QuestionReadOperationIssueOutcome::Unavailable => {
                    return Err(ProviderQuestionReadError::StateConflict);
                }
            };
            if delay_seconds > 0 {
                tokio::time::sleep(std::time::Duration::from_secs(delay_seconds)).await;
            }
            let outcome = match prepared.execute(context).await {
                Ok(outcome) => outcome,
                Err(error) => {
                    let finished = scoped
                        .finish_question_read_operation(
                            &operation,
                            QuestionReadOperationState::Ambiguous,
                            None,
                            Utc::now(),
                            &access,
                        )
                        .await?;
                    if !matches!(
                        finished,
                        QuestionReadOperationFinishOutcome::Finished { .. }
                            | QuestionReadOperationFinishOutcome::Duplicate(_)
                    ) {
                        return Err(ProviderQuestionReadError::StateConflict);
                    }
                    self.record_protocol_observation(
                        provider_id,
                        task.id,
                        &context.correlation_id,
                        "execute-operation",
                        &error,
                    )
                    .await?;
                    return Err(ProviderQuestionReadError::Provider(error));
                }
            };
            match outcome {
                ProviderQuestionReadStepOutcome::Continue {
                    continuation,
                    response_digest,
                    received_at,
                } => {
                    let (next_type, expected_digest, next_phase, replacement, _) =
                        continuation.into_parts();
                    let accepted = scoped
                        .accept_question_read_operation(QuestionReadOperationAcceptRequest {
                            operation: &operation,
                            next_continuation_type: &next_type,
                            next_phase: &next_phase,
                            replacement,
                            result_digest: response_digest,
                            accepted_at: received_at,
                            access: &access,
                        })
                        .await?;
                    match accepted {
                        QuestionReadOperationFinishOutcome::Accepted { continuation, .. }
                            if continuation.continuation_digest == expected_digest => {}
                        QuestionReadOperationFinishOutcome::Duplicate(_) => {}
                        _ => return Err(ProviderQuestionReadError::StateConflict),
                    }
                }
                ProviderQuestionReadStepOutcome::Materialize(materialization) => {
                    let (questions, artifact, response_digest, received_at) =
                        materialization.into_parts();
                    validate_materialized_questions(task.id, &questions)?;
                    let (artifact_type, artifact_digest, phase, value, ttl_seconds) =
                        artifact.into_parts();
                    let snapshot = QuestionSnapshot {
                        id: QuestionSnapshotId::new(),
                        task_id: task.id,
                        provider_id: provider_id.clone(),
                        provider_version: provider_version.to_owned(),
                        captured_at: received_at,
                        questions,
                    };
                    let session =
                        QuestionSession::active(
                            attempt.owner_user_id,
                            attempt.provider_account_id,
                            task.id,
                            provider_id.clone(),
                            provider_version.to_owned(),
                            snapshot.id,
                            artifact_type,
                            artifact_digest,
                            received_at,
                            received_at
                                + Duration::seconds(i64::try_from(ttl_seconds).map_err(|_| {
                                    ProviderQuestionReadError::ProviderResponseInvalid
                                })?),
                        )
                        .map_err(|_| ProviderQuestionReadError::ProviderResponseInvalid)?;
                    let materialized = scoped
                        .materialize_question_read_operation(QuestionReadMaterializeRequest {
                            operation: &operation,
                            snapshot: &snapshot,
                            session: &session,
                            artifact_phase: &phase,
                            artifact: value,
                            result_digest: response_digest,
                            materialized_at: received_at,
                            access: &access,
                        })
                        .await?;
                    if !matches!(
                        materialized,
                        QuestionReadMaterializeOutcome::Materialized { .. }
                            | QuestionReadMaterializeOutcome::Duplicate { .. }
                    ) {
                        return Err(ProviderQuestionReadError::StateConflict);
                    }
                    return Ok(result_from_snapshot(snapshot));
                }
                ProviderQuestionReadStepOutcome::Completed {
                    receipt,
                    response_digest,
                } => {
                    let finished = scoped
                        .finish_question_read_operation(
                            &operation,
                            QuestionReadOperationState::Accepted,
                            Some(response_digest),
                            receipt.received_at,
                            &access,
                        )
                        .await?;
                    if !matches!(
                        finished,
                        QuestionReadOperationFinishOutcome::Finished { .. }
                            | QuestionReadOperationFinishOutcome::Duplicate(_)
                    ) {
                        return Err(ProviderQuestionReadError::StateConflict);
                    }
                    return Ok(ProviderQuestionReadResult::Completed {
                        task_id: task.id,
                        provider_id: provider_id.clone(),
                        provider_version: provider_version.to_owned(),
                    });
                }
            }
        }
        Err(ProviderQuestionReadError::OperationLimitExceeded)
    }
}

fn validate_references(references: &[RemoteQuestionRef]) -> Result<(), ProviderQuestionReadError> {
    if references.len() > MAX_QUESTIONS_PER_READ {
        return Err(ProviderQuestionReadError::ProviderResponseInvalid);
    }
    let mut remote_ids = BTreeSet::new();
    let mut positions = BTreeSet::new();
    for reference in references {
        if reference.validate().is_err()
            || !remote_ids.insert(reference.remote_id.as_str())
            || !positions.insert(reference.position)
        {
            return Err(ProviderQuestionReadError::ProviderResponseInvalid);
        }
    }
    Ok(())
}

fn validate_question_binding(
    task: &asterism_domain::Task,
    reference: &RemoteQuestionRef,
    question: &Question,
) -> Result<(), ProviderQuestionReadError> {
    if question.task_id != task.id
        || question.remote_question_id.as_deref() != Some(reference.remote_id.as_str())
        || question.position != reference.position
        || (reference.kind_hint != QuestionKind::Unknown && question.kind != reference.kind_hint)
        || question.validate().is_err()
    {
        Err(ProviderQuestionReadError::ProviderResponseInvalid)
    } else {
        Ok(())
    }
}

fn valid_correlation_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_CORRELATION_ID_BYTES
        && value.trim() == value
        && !value.chars().any(char::is_control)
}

#[derive(Debug, thiserror::Error)]
pub enum ProviderQuestionReadError {
    #[error("task was not found")]
    TaskNotFound,
    #[error("task does not advertise the required Question capability")]
    TaskCapabilityUnavailable,
    #[error("Provider account is not authenticated")]
    AccountNotAuthenticated,
    #[error("provider `{0}` is not registered")]
    ProviderNotRegistered(ProviderId),
    #[error("provider `{0}` exposes no complete Question read pipeline")]
    CapabilityUnavailable(ProviderId),
    #[error("question read correlation id is invalid")]
    InvalidCorrelationId,
    #[error("Provider supplied an invalid protocol observation")]
    InvalidProtocolObservation,
    #[error("Provider returned invalid, duplicate, or unsanitized Questions")]
    ProviderResponseInvalid,
    #[error("durable Question read state is unavailable")]
    DurableStateUnavailable,
    #[error("Provider runtime settings could not be resolved")]
    RuntimeSettingsInvalid,
    #[error("Question read attempt `{0}` has an ambiguous remote outcome")]
    AmbiguousAttempt(QuestionReadAttemptId),
    #[error("Question read attempt `{0}` is already being executed")]
    ConcurrentAttempt(QuestionReadAttemptId),
    #[error("Question read attempt state changed concurrently")]
    StateConflict,
    #[error("Question read attempt exceeded the bounded operation count")]
    OperationLimitExceeded,
    #[error(transparent)]
    Assessment(#[from] AssessmentGuardError),
    #[error(transparent)]
    Provider(#[from] ProviderError),
    #[error(transparent)]
    Storage(#[from] StorageError),
    #[error(transparent)]
    Secret(#[from] SecretStoreError),
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeMap,
        sync::{
            Mutex,
            atomic::{AtomicBool, Ordering},
        },
    };

    use asterism_domain::{
        AssessmentClass, OrchestrationState, ProtocolObservationKind, ProtocolSurface,
        ProviderAccount, ProviderAccountId, RemoteState, SourceType, Task,
    };
    use asterism_provider_api::{
        PreparedProviderQuestionReadOperation, ProviderCapability, ProviderEntry,
        ProviderErrorKind, ProviderIdentity, ProviderMetadata, ProviderQuestionMaterialization,
        ProviderQuestionReadContinuation, ProviderQuestionReadStepOutcome, ProviderResult,
        ProviderRouteContext, ProviderRuntimeSettingsSchema, QuestionInventoryCapability,
        QuestionParseCapability, ResolvedProviderQuestionReadContinuation,
        ResolvedProviderRuntimeSettings, VerificationLevel,
    };
    use asterism_secrets::{SecretKey, SecretValue};
    use asterism_storage::{
        Database, QuestionReadAttemptRepository, QuestionSnapshot, QuestionSnapshotRepository,
        SecretKeyring, SqliteProtocolObservationRepository, SqliteProviderAccountRepository,
        SqliteProviderRuntimeSettingsRepository, SqliteQuestionReadAttemptRepository,
        SqliteQuestionSnapshotRepository, SqliteSecretStore, SqliteTaskQueryRepository, TaskPage,
    };
    use async_trait::async_trait;
    use chrono::Utc;

    use super::*;

    #[derive(Clone, Debug)]
    struct FakeTaskRepository {
        owner_id: UserId,
        task: Task,
    }

    #[async_trait]
    impl TaskQueryRepository for FakeTaskRepository {
        async fn list_owned_tasks(
            &self,
            owner_id: UserId,
            _provider_account_id: Option<ProviderAccountId>,
            _limit: u32,
            _offset: u64,
        ) -> Result<TaskPage, StorageError> {
            let items = if owner_id == self.owner_id {
                vec![self.task.clone()]
            } else {
                Vec::new()
            };
            Ok(TaskPage {
                total: items.len() as u64,
                items,
            })
        }

        async fn find_owned_task(
            &self,
            owner_id: UserId,
            task_id: TaskId,
        ) -> Result<Option<Task>, StorageError> {
            Ok((owner_id == self.owner_id && task_id == self.task.id).then(|| self.task.clone()))
        }
    }

    #[derive(Clone, Debug)]
    struct FakeAccountRepository(ProviderAccount);

    #[async_trait]
    impl ProviderAccountRuntimeRepository for FakeAccountRepository {
        async fn find_runtime_provider_account(
            &self,
            account_id: ProviderAccountId,
        ) -> Result<Option<ProviderAccount>, StorageError> {
            Ok((account_id == self.0.id).then(|| self.0.clone()))
        }
    }

    #[derive(Debug)]
    struct FakeQuestions {
        metadata: ProviderMetadata,
        references: Mutex<Vec<RemoteQuestionRef>>,
        parsed_task_id: Mutex<Option<TaskId>>,
        protocol_drift: AtomicBool,
    }

    #[derive(Clone, Debug, Default)]
    struct RecordingSnapshots {
        snapshots: Arc<Mutex<Vec<QuestionSnapshot>>>,
        reject: Arc<AtomicBool>,
    }

    #[async_trait]
    impl QuestionSnapshotRepository for RecordingSnapshots {
        async fn save_question_snapshot(
            &self,
            snapshot: &QuestionSnapshot,
        ) -> Result<(), StorageError> {
            if self.reject.load(Ordering::Relaxed) {
                return Err(StorageError::InvalidData(
                    "fixture rejected Question snapshot".to_owned(),
                ));
            }
            self.snapshots.lock().unwrap().push(snapshot.clone());
            Ok(())
        }

        async fn find_owned_question_snapshot(
            &self,
            _owner_id: UserId,
            question_snapshot_id: QuestionSnapshotId,
        ) -> Result<Option<QuestionSnapshot>, StorageError> {
            Ok(self
                .snapshots
                .lock()
                .unwrap()
                .iter()
                .find(|snapshot| snapshot.id == question_snapshot_id)
                .cloned())
        }

        async fn find_latest_owned_question_snapshot(
            &self,
            _owner_id: UserId,
            _task_id: TaskId,
        ) -> Result<Option<QuestionSnapshot>, StorageError> {
            Ok(self.snapshots.lock().unwrap().last().cloned())
        }
    }

    impl ProviderIdentity for FakeQuestions {
        fn metadata(&self) -> &ProviderMetadata {
            &self.metadata
        }
    }

    #[async_trait]
    impl QuestionInventoryCapability for FakeQuestions {
        async fn list_question_refs(
            &self,
            _context: &ProviderContext,
            _remote_task_id: &str,
        ) -> ProviderResult<Vec<RemoteQuestionRef>> {
            Ok(self.references.lock().unwrap().clone())
        }
    }

    #[async_trait]
    impl QuestionParseCapability for FakeQuestions {
        async fn parse_question(
            &self,
            _context: &ProviderContext,
            task_id: TaskId,
            _remote_task_id: &str,
            reference: &RemoteQuestionRef,
        ) -> ProviderResult<Question> {
            *self.parsed_task_id.lock().unwrap() = Some(task_id);
            if self.protocol_drift.load(Ordering::Relaxed) {
                return Err(ProviderError::new(
                    ProviderErrorKind::ProtocolDrift,
                    "question type changed",
                )
                .try_with_protocol_observation(
                    ProtocolSurface::QuestionParse,
                    ProtocolObservationKind::UnknownQuestionKind,
                    serde_json::json!({"reply_type_digest": "sha256:test", "length": 17}),
                )
                .unwrap());
            }
            Ok(Question {
                id: asterism_domain::QuestionId::new(),
                task_id,
                remote_question_id: Some(reference.remote_id.clone()),
                kind: reference.kind_hint,
                stem: format!("Question {}", reference.position),
                options: Vec::new(),
                attachments: Vec::new(),
                metadata_sanitized: serde_json::json!({"safe": true}),
                position: reference.position,
            })
        }
    }

    #[tokio::test]
    async fn complete_question_set_is_owner_scoped_sorted_and_validated() {
        let fixture = fixture(true);
        let result = fixture
            .service
            .read(ReadTaskQuestionsCommand {
                owner_id: fixture.owner_id,
                task_id: fixture.task_id,
                correlation_id: "question-read-1".to_owned(),
            })
            .await
            .unwrap();

        let ProviderQuestionReadResult::Questions {
            snapshot_id,
            questions,
            ..
        } = result
        else {
            panic!("expected a Question snapshot");
        };
        assert_eq!(questions.len(), 2);
        assert_eq!(questions[0].position, 1);
        assert_eq!(questions[1].position, 2);
        let snapshots = fixture.snapshots.snapshots.lock().unwrap();
        assert_eq!(snapshots.len(), 1);
        assert_eq!(snapshots[0].id, snapshot_id);
        assert_eq!(snapshots[0].questions, questions);
        assert_eq!(
            *fixture.capability.parsed_task_id.lock().unwrap(),
            Some(fixture.task_id)
        );
    }

    #[tokio::test]
    async fn duplicate_references_fail_before_any_question_is_parsed() {
        let fixture = fixture(true);
        let duplicate = reference("question-1", 2);
        fixture
            .capability
            .references
            .lock()
            .unwrap()
            .push(duplicate);
        assert!(matches!(
            fixture
                .service
                .read(ReadTaskQuestionsCommand {
                    owner_id: fixture.owner_id,
                    task_id: fixture.task_id,
                    correlation_id: "question-read-2".to_owned(),
                })
                .await,
            Err(ProviderQuestionReadError::ProviderResponseInvalid)
        ));
        assert!(fixture.capability.parsed_task_id.lock().unwrap().is_none());
        assert!(fixture.snapshots.snapshots.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn undeclared_question_capability_never_calls_provider() {
        let fixture = fixture(false);
        assert!(matches!(
            fixture
                .service
                .read(ReadTaskQuestionsCommand {
                    owner_id: fixture.owner_id,
                    task_id: fixture.task_id,
                    correlation_id: "question-read-3".to_owned(),
                })
                .await,
            Err(ProviderQuestionReadError::TaskCapabilityUnavailable)
        ));
        assert!(fixture.capability.parsed_task_id.lock().unwrap().is_none());
        assert!(fixture.snapshots.snapshots.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn storage_failure_returns_no_question_result() {
        let fixture = fixture(true);
        fixture.snapshots.reject.store(true, Ordering::Relaxed);
        assert!(matches!(
            fixture
                .service
                .read(ReadTaskQuestionsCommand {
                    owner_id: fixture.owner_id,
                    task_id: fixture.task_id,
                    correlation_id: "question-read-storage-failure".to_owned(),
                })
                .await,
            Err(ProviderQuestionReadError::Storage(_))
        ));
        assert!(fixture.snapshots.snapshots.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn parser_drift_is_observed_before_question_read_fails() {
        let database = Database::connect("sqlite::memory:").await.unwrap();
        database.migrate().await.unwrap();
        let mut fixture = fixture(true);
        fixture
            .capability
            .protocol_drift
            .store(true, Ordering::Relaxed);
        fixture.service = fixture.service.with_protocol_observations(Arc::new(
            SqliteProtocolObservationRepository::new(database.clone()),
        ));

        assert!(matches!(
            fixture
                .service
                .read(ReadTaskQuestionsCommand {
                    owner_id: fixture.owner_id,
                    task_id: fixture.task_id,
                    correlation_id: "question-read-drift".to_owned(),
                })
                .await,
            Err(ProviderQuestionReadError::Provider(error))
                if error.kind == ProviderErrorKind::ProtocolDrift
        ));

        let observation: (String, String, i64, Option<String>) = sqlx::query_as(
            "SELECT surface, kind, occurrence_count, last_execution_id \
             FROM protocol_observations",
        )
        .fetch_one(database.pool())
        .await
        .unwrap();
        assert_eq!(
            observation,
            (
                "question_parse".to_owned(),
                "unknown_question_kind".to_owned(),
                1,
                None,
            )
        );
        assert!(fixture.snapshots.snapshots.lock().unwrap().is_empty());
    }

    #[derive(Debug)]
    struct FakeDurableQuestions {
        metadata: ProviderMetadata,
    }

    impl ProviderIdentity for FakeDurableQuestions {
        fn metadata(&self) -> &ProviderMetadata {
            &self.metadata
        }
    }

    #[async_trait]
    impl QuestionInventoryCapability for FakeDurableQuestions {
        async fn list_question_refs(
            &self,
            _context: &ProviderContext,
            _remote_task_id: &str,
        ) -> ProviderResult<Vec<RemoteQuestionRef>> {
            panic!("durable flow must not use read-only inventory")
        }

        async fn prepare_question_read_attempt(
            &self,
            context: &ProviderContext,
            _task_id: TaskId,
            _remote_task_id: &str,
            _runtime_settings: &ResolvedProviderRuntimeSettings,
        ) -> ProviderResult<Option<ProviderQuestionReadContinuation>> {
            Ok(Some(ProviderQuestionReadContinuation::try_new(
                &context.provider_id,
                "provider-alpha.pre-question.v1",
                "provider-alpha.ready-to-start",
                SecretValue::new(b"frozen-start-state".to_vec()),
                300,
            )?))
        }

        async fn prepare_question_read_operation(
            &self,
            context: &ProviderContext,
            task_id: TaskId,
            _remote_task_id: &str,
            continuation: ResolvedProviderQuestionReadContinuation<'_>,
            _runtime_settings: &ResolvedProviderRuntimeSettings,
        ) -> ProviderResult<Box<dyn PreparedProviderQuestionReadOperation>> {
            if continuation.continuation_type != "provider-alpha.pre-question.v1"
                || continuation.phase != "provider-alpha.ready-to-start"
                || continuation.revision != 1
                || continuation.value.expose_secret() != b"frozen-start-state"
            {
                return Err(ProviderError::new(
                    asterism_provider_api::ProviderErrorKind::ProtocolDrift,
                    "fixture continuation changed",
                ));
            }
            Ok(Box::new(FakeDurableCommand {
                provider_id: context.provider_id.clone(),
                task_id,
            }))
        }
    }

    #[derive(Debug)]
    struct FakeDurableCommand {
        provider_id: ProviderId,
        task_id: TaskId,
    }

    #[async_trait]
    impl PreparedProviderQuestionReadOperation for FakeDurableCommand {
        fn operation_type(&self) -> &'static str {
            "provider-alpha.start-question.v1"
        }

        fn request_digest(&self) -> [u8; 32] {
            [41; 32]
        }

        fn delay_before_execute_seconds(&self) -> u64 {
            0
        }

        async fn execute(
            self: Box<Self>,
            _context: &ProviderContext,
        ) -> ProviderResult<ProviderQuestionReadStepOutcome> {
            let received_at = Utc::now();
            let question = Question {
                id: asterism_domain::QuestionId::new(),
                task_id: self.task_id,
                remote_question_id: Some("remote-question-1".to_owned()),
                kind: QuestionKind::Unknown,
                stem: "Durably materialized".to_owned(),
                options: Vec::new(),
                attachments: Vec::new(),
                metadata_sanitized: serde_json::json!({}),
                position: 1,
            };
            let artifact = ProviderQuestionReadContinuation::try_new(
                &self.provider_id,
                "provider-alpha.question-attempt.v1",
                "provider-alpha.current-question",
                SecretValue::new(b"one-time-question-state".to_vec()),
                600,
            )?;
            Ok(ProviderQuestionReadStepOutcome::Materialize(
                ProviderQuestionMaterialization::try_new(
                    vec![question],
                    artifact,
                    [42; 32],
                    received_at,
                )?,
            ))
        }
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn durable_flow_issues_before_execute_and_materializes_one_atomic_snapshot() {
        let database = Database::connect("sqlite::memory:").await.unwrap();
        database.migrate().await.unwrap();
        let owner_id = UserId::new();
        let account_id = ProviderAccountId::new();
        let task_id = TaskId::new();
        let provider_id = ProviderId::new("provider-alpha").unwrap();
        let now = Utc::now();
        let timestamp = now.to_rfc3339_opts(chrono::SecondsFormat::Nanos, true);
        sqlx::query(
            "INSERT INTO users \
             (id, username, password_hash, status, roles_json, permissions_json, created_at, updated_at) \
             VALUES (?, 'durable-read-owner', '$argon2id$test', 'active', '[\"user\"]', '[]', ?, ?)",
        )
        .bind(owner_id.to_string())
        .bind(&timestamp)
        .bind(&timestamp)
        .execute(database.pool())
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO provider_accounts \
             (id, owner_user_id, provider_id, display_name, auth_state_json, created_at, updated_at) \
             VALUES (?, ?, ?, 'Durable Read', '{\"state\":\"authenticated\"}', ?, ?)",
        )
        .bind(account_id.to_string())
        .bind(owner_id.to_string())
        .bind(provider_id.as_str())
        .bind(&timestamp)
        .bind(&timestamp)
        .execute(database.pool())
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO tasks \
             (id, provider_account_id, remote_id, remote_fingerprint, source_type, \
              assessment_class, title, remote_state, orchestration_state, discovered_at, \
              updated_at, capabilities_json) \
             VALUES (?, ?, 'durable-read', 'v1:durable-read', 'practice', 'routine', \
                     'Durable Read', 'pending', 'ready', ?, ?, '[\"question_inventory\"]')",
        )
        .bind(task_id.to_string())
        .bind(account_id.to_string())
        .bind(&timestamp)
        .bind(&timestamp)
        .execute(database.pool())
        .await
        .unwrap();

        let metadata = ProviderMetadata {
            id: provider_id.clone(),
            display_name: "provider-alpha".to_owned(),
            implementation_version: "durable-v1".to_owned(),
            verification: VerificationLevel::Development,
            scan_min_interval_seconds: None,
            capture_recipe_version: None,
            capabilities: BTreeSet::from([ProviderCapability::QuestionInventory]),
            auth_methods: BTreeSet::new(),
            session_kinds: BTreeSet::new(),
        };
        let capability = Arc::new(FakeDurableQuestions {
            metadata: metadata.clone(),
        });
        let mut registry = ProviderRegistry::default();
        registry
            .register(ProviderEntry {
                metadata,
                runtime_settings: ProviderRuntimeSettingsSchema::default(),
                authentication: None,
                course_inventory: None,
                task_inventory: None,
                task_detail: None,
                task_progress: None,
                duration_read: None,
                question_inventory: Some(capability),
                question_parse: None,
                answer_resolve: None,
                submission_build: None,
                submission_execute: None,
                submission_verify: None,
                answer_history_harvest: None,
                task_execution: None,
                browser_bridge: None,
            })
            .unwrap();
        let mut keys = BTreeMap::new();
        keys.insert("question-read-key".to_owned(), SecretKey::new([51; 32]));
        let secret_store = SqliteSecretStore::new(
            database.clone(),
            Arc::new(SecretKeyring::new("question-read-key", keys).unwrap()),
        );
        let attempts = SqliteQuestionReadAttemptRepository::new(database.clone());
        let service = ProviderQuestionReadService::new(
            Arc::new(registry),
            SqliteTaskQueryRepository::new(database.clone()),
            SqliteProviderAccountRepository::new(database.clone()),
            SqliteQuestionSnapshotRepository::new(database.clone()),
        )
        .with_durable_flow(
            Arc::new(SqliteProviderRuntimeSettingsRepository::new(
                database.clone(),
            )),
            Arc::new(attempts.clone()),
            Arc::new(secret_store.clone()),
            Arc::new(secret_store),
        );
        let result = service
            .read(ReadTaskQuestionsCommand {
                owner_id,
                task_id,
                correlation_id: "durable-question-read".to_owned(),
            })
            .await
            .unwrap();
        assert!(matches!(
            result,
            ProviderQuestionReadResult::Questions { ref questions, .. }
                if questions.len() == 1 && questions[0].task_id == task_id
        ));
        let attempt = attempts
            .find_latest_owned_question_read_attempt(owner_id, task_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(attempt.state, QuestionReadAttemptState::Materialized);
        let operation_state: String = sqlx::query_scalar(
            "SELECT state FROM question_read_attempt_operations WHERE attempt_id = ?",
        )
        .bind(attempt.id.to_string())
        .fetch_one(database.pool())
        .await
        .unwrap();
        let session_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM question_sessions WHERE id = ? AND question_snapshot_id = ?",
        )
        .bind(attempt.question_session_id.unwrap().to_string())
        .bind(attempt.question_snapshot_id.unwrap().to_string())
        .fetch_one(database.pool())
        .await
        .unwrap();
        assert_eq!(operation_state, "accepted");
        assert_eq!(session_count, 1);
    }

    struct Fixture {
        service: ProviderQuestionReadService<
            FakeTaskRepository,
            FakeAccountRepository,
            RecordingSnapshots,
        >,
        owner_id: UserId,
        task_id: TaskId,
        capability: Arc<FakeQuestions>,
        snapshots: RecordingSnapshots,
    }

    fn fixture(advertises_questions: bool) -> Fixture {
        let owner_id = UserId::new();
        let account_id = ProviderAccountId::new();
        let provider_id = ProviderId::new("provider-alpha").unwrap();
        let now = Utc::now();
        let task = Task {
            id: TaskId::new(),
            provider_account_id: account_id,
            course_id: None,
            remote_id: "work:100:200:work-1".to_owned(),
            source_type: SourceType::Work,
            assessment_class: AssessmentClass::Formal,
            title: "work".to_owned(),
            remote_state: RemoteState::Pending,
            orchestration_state: OrchestrationState::Ready,
            opens_at: None,
            due_at: None,
            closes_at: None,
            discovered_at: now,
            updated_at: now,
            latest_snapshot_id: None,
            capabilities: if advertises_questions {
                vec![
                    TaskCapability::QuestionInventory,
                    TaskCapability::QuestionParse,
                ]
            } else {
                Vec::new()
            },
        };
        let account = ProviderAccount {
            id: account_id,
            owner_id,
            provider_id: provider_id.clone(),
            display_name: "primary".to_owned(),
            tenant: None,
            auth_state: AuthState::Authenticated,
            network_profile_id: None,
            credential_refs: vec![asterism_domain::SecretId::new()],
            created_at: now,
            updated_at: now,
        };
        let metadata = ProviderMetadata {
            id: provider_id,
            display_name: "provider-alpha".to_owned(),
            implementation_version: "0.1.0".to_owned(),
            verification: VerificationLevel::Development,
            scan_min_interval_seconds: None,
            capture_recipe_version: None,
            capabilities: BTreeSet::from([
                ProviderCapability::QuestionInventory,
                ProviderCapability::QuestionParse,
            ]),
            auth_methods: BTreeSet::new(),
            session_kinds: BTreeSet::new(),
        };
        let capability = Arc::new(FakeQuestions {
            metadata,
            references: Mutex::new(vec![reference("question-2", 2), reference("question-1", 1)]),
            parsed_task_id: Mutex::new(None),
            protocol_drift: AtomicBool::new(false),
        });
        let mut registry = ProviderRegistry::default();
        registry
            .register(ProviderEntry {
                metadata: capability.metadata.clone(),
                runtime_settings: ProviderRuntimeSettingsSchema::default(),
                authentication: None,
                course_inventory: None,
                task_inventory: None,
                task_detail: None,
                task_progress: None,
                duration_read: None,
                question_inventory: Some(capability.clone()),
                question_parse: Some(capability.clone()),
                answer_resolve: None,
                submission_build: None,
                submission_execute: None,
                submission_verify: None,
                answer_history_harvest: None,
                task_execution: None,
                browser_bridge: None,
            })
            .unwrap();
        let snapshots = RecordingSnapshots::default();
        let service = ProviderQuestionReadService::new(
            Arc::new(registry),
            FakeTaskRepository {
                owner_id,
                task: task.clone(),
            },
            FakeAccountRepository(account),
            snapshots.clone(),
        );
        Fixture {
            service,
            owner_id,
            task_id: task.id,
            capability,
            snapshots,
        }
    }

    fn reference(remote_id: &str, position: u32) -> RemoteQuestionRef {
        RemoteQuestionRef {
            remote_id: remote_id.to_owned(),
            position,
            kind_hint: QuestionKind::Unknown,
            metadata_sanitized: serde_json::json!({"safe": true}),
            route_context: ProviderRouteContext::default(),
        }
    }
}
