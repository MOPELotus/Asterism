use std::sync::Arc;

use asterism_domain::{
    AuthState, CourseEnrollmentAttempt, CourseEnrollmentAttemptId, CourseEnrollmentAttemptState,
    CourseEnrollmentDraftId, CourseEnrollmentMutationReceipt, CourseEnrollmentVerification,
    ProviderAccountId, ProviderId, Timestamp, UserId,
};
use asterism_provider_api::{
    ExecutionMutationIssue, ExecutionMutationReceipt, ExecutionMutationRecoveryRecord,
    ExecutionMutationSink, ProviderContext, ProviderCourseEnrollmentDispatchOutcome, ProviderError,
    ProviderErrorKind, ProviderRegistry, ProviderResult,
};
use asterism_secrets::{SecretAccess, SecretActor, SecretStoreError, SecretString};
use asterism_storage::{
    CourseEnrollmentAttemptCreateOutcome, CourseEnrollmentAttemptCreateRequest,
    CourseEnrollmentAttemptMutationIssueRequest, CourseEnrollmentAttemptReceiptRequest,
    CourseEnrollmentAttemptVerificationBeginRequest,
    CourseEnrollmentAttemptVerificationRecordRequest, CourseEnrollmentDraftCreateOutcome,
    CourseEnrollmentDraftCreateRequest, CourseEnrollmentDraftRecord,
    CourseEnrollmentDraftResolveRequest, CourseEnrollmentRepository,
    CourseEnrollmentRepositoryFactory, ProviderAccountRuntimeRepository, StorageError,
};

const MAX_CORRELATION_ID_BYTES: usize = 128;

#[derive(Debug)]
pub struct PrepareCourseEnrollmentCommand {
    pub draft_id: CourseEnrollmentDraftId,
    pub owner_user_id: UserId,
    pub provider_account_id: ProviderAccountId,
    pub invitation: SecretString,
    pub correlation_id: String,
    pub at: Timestamp,
}

#[derive(Clone, Debug)]
pub struct ExecuteCourseEnrollmentCommand {
    pub attempt_id: CourseEnrollmentAttemptId,
    pub draft_id: CourseEnrollmentDraftId,
    pub owner_user_id: UserId,
    pub provider_account_id: ProviderAccountId,
    pub correlation_id: String,
    pub at: Timestamp,
}

#[derive(Clone, Debug)]
pub struct RecoverCourseEnrollmentCommand {
    pub attempt_id: CourseEnrollmentAttemptId,
    pub draft_id: CourseEnrollmentDraftId,
    pub owner_user_id: UserId,
    pub provider_account_id: ProviderAccountId,
    pub correlation_id: String,
    pub at: Timestamp,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CourseEnrollmentRunResult {
    pub attempt: CourseEnrollmentAttempt,
}

#[derive(Clone)]
pub struct CourseEnrollmentService<A> {
    registry: Arc<ProviderRegistry>,
    accounts: A,
    repositories: Arc<dyn CourseEnrollmentRepositoryFactory>,
}

impl<A> CourseEnrollmentService<A> {
    pub const fn new(
        registry: Arc<ProviderRegistry>,
        accounts: A,
        repositories: Arc<dyn CourseEnrollmentRepositoryFactory>,
    ) -> Self {
        Self {
            registry,
            accounts,
            repositories,
        }
    }
}

impl<A> std::fmt::Debug for CourseEnrollmentService<A> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CourseEnrollmentService")
            .field("registry", &self.registry)
            .field("accounts", &"configured")
            .field("repositories", &"configured")
            .finish()
    }
}

impl<A> CourseEnrollmentService<A>
where
    A: ProviderAccountRuntimeRepository,
{
    /// Previews one invitation and atomically encrypts the exact immutable
    /// Provider request before returning its sanitized confirmation record.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid ownership/authentication/capability
    /// bindings, unsafe Provider output, or encrypted persistence failure.
    pub async fn prepare(
        &self,
        command: PrepareCourseEnrollmentCommand,
    ) -> Result<CourseEnrollmentDraftRecord, CourseEnrollmentServiceError> {
        validate_correlation_id(&command.correlation_id)?;
        let (provider_id, credential_refs) = self
            .resolve_account(command.owner_user_id, command.provider_account_id)
            .await?;
        let entry = self.registry.get(&provider_id).ok_or_else(|| {
            CourseEnrollmentServiceError::ProviderNotRegistered(provider_id.clone())
        })?;
        let capability = entry.course_enrollment.as_ref().ok_or_else(|| {
            CourseEnrollmentServiceError::CapabilityUnavailable(provider_id.clone())
        })?;
        let provider_draft = capability
            .prepare_course_enrollment(
                &ProviderContext {
                    provider_id: provider_id.clone(),
                    account_id: command.provider_account_id,
                    credential_refs,
                    correlation_id: command.correlation_id.clone(),
                },
                command.invitation,
            )
            .await?;
        if provider_draft.provider_id() != &provider_id {
            return Err(CourseEnrollmentServiceError::ProviderResponseInvalid);
        }
        let access = enrollment_access(&command.correlation_id, "freeze enrollment draft");
        let repository = self.repositories.for_provider(provider_id);
        match repository
            .create_course_enrollment_draft(CourseEnrollmentDraftCreateRequest {
                draft_id: command.draft_id,
                owner_user_id: command.owner_user_id,
                provider_account_id: command.provider_account_id,
                provider_draft: &provider_draft,
                created_at: command.at,
                correlation_id: &command.correlation_id,
                access: &access,
            })
            .await?
        {
            CourseEnrollmentDraftCreateOutcome::Created(record)
            | CourseEnrollmentDraftCreateOutcome::AlreadyExists(record) => Ok(record),
        }
    }

    /// Executes at most one remote enrollment issue and then switches to
    /// independent inventory verification. Post-issue Provider errors never
    /// authorize replay.
    ///
    /// # Errors
    ///
    /// Returns an error before issue for invalid bindings or Provider/storage
    /// failure. Once issued, retryable readback failure is represented by the
    /// durable `VerificationPending` Attempt returned to the caller.
    pub async fn execute(
        &self,
        command: ExecuteCourseEnrollmentCommand,
    ) -> Result<CourseEnrollmentRunResult, CourseEnrollmentServiceError> {
        validate_correlation_id(&command.correlation_id)?;
        let runtime = self.resolve_runtime(&command).await?;
        let attempt = match runtime
            .repository
            .create_course_enrollment_attempt(CourseEnrollmentAttemptCreateRequest {
                attempt_id: command.attempt_id,
                draft_id: command.draft_id,
                owner_user_id: command.owner_user_id,
                provider_account_id: command.provider_account_id,
                at: command.at,
            })
            .await?
        {
            CourseEnrollmentAttemptCreateOutcome::Created(attempt)
            | CourseEnrollmentAttemptCreateOutcome::AlreadyExists(attempt) => attempt,
        };
        if attempt.state != CourseEnrollmentAttemptState::Prepared {
            return self.recover_runtime(runtime, attempt, command.at).await;
        }
        let sink = CourseEnrollmentMutationSink {
            repository: runtime.repository.clone(),
            attempt_id: command.attempt_id,
            owner_user_id: command.owner_user_id,
            provider_account_id: command.provider_account_id,
            at: command.at,
        };
        let dispatch = runtime
            .capability
            .execute_course_enrollment(&runtime.context, &runtime.draft.provider_draft, &sink)
            .await;
        let attempt = runtime
            .repository
            .find_owned_course_enrollment_attempt(
                command.owner_user_id,
                command.provider_account_id,
                command.attempt_id,
            )
            .await?
            .ok_or(CourseEnrollmentServiceError::AttemptStateConflict)?;
        match dispatch {
            Ok(ProviderCourseEnrollmentDispatchOutcome::Accepted)
                if attempt.state == CourseEnrollmentAttemptState::ReceiptRecorded => {}
            Ok(ProviderCourseEnrollmentDispatchOutcome::Rejected)
                if attempt.state == CourseEnrollmentAttemptState::Rejected =>
            {
                return Ok(CourseEnrollmentRunResult { attempt });
            }
            Err(error) if attempt.state == CourseEnrollmentAttemptState::Prepared => {
                return Err(error.into());
            }
            Err(_)
                if matches!(
                    attempt.state,
                    CourseEnrollmentAttemptState::MutationIssued
                        | CourseEnrollmentAttemptState::ReceiptRecorded
                ) => {}
            Ok(_) | Err(_) => return Err(CourseEnrollmentServiceError::AttemptStateConflict),
        }
        self.recover_runtime(runtime, attempt, command.at.max(chrono::Utc::now()))
            .await
    }

    /// Performs read-only recovery for a previously issued Attempt.
    ///
    /// # Errors
    ///
    /// Returns an error for cross-owner/account/draft bindings or an invalid
    /// persisted lifecycle. Provider readback failure leaves the Attempt
    /// durably pending rather than replaying the mutation.
    pub async fn recover(
        &self,
        command: RecoverCourseEnrollmentCommand,
    ) -> Result<CourseEnrollmentRunResult, CourseEnrollmentServiceError> {
        validate_correlation_id(&command.correlation_id)?;
        let runtime = self
            .resolve_runtime(&ExecuteCourseEnrollmentCommand {
                attempt_id: command.attempt_id,
                draft_id: command.draft_id,
                owner_user_id: command.owner_user_id,
                provider_account_id: command.provider_account_id,
                correlation_id: command.correlation_id,
                at: command.at,
            })
            .await?;
        let attempt = runtime
            .repository
            .find_owned_course_enrollment_attempt(
                command.owner_user_id,
                command.provider_account_id,
                command.attempt_id,
            )
            .await?
            .ok_or(CourseEnrollmentServiceError::AttemptNotFound)?;
        self.recover_runtime(runtime, attempt, command.at).await
    }

    async fn resolve_account(
        &self,
        owner_user_id: UserId,
        provider_account_id: ProviderAccountId,
    ) -> Result<(ProviderId, Vec<asterism_domain::SecretId>), CourseEnrollmentServiceError> {
        let account = self
            .accounts
            .find_runtime_provider_account(provider_account_id)
            .await?
            .filter(|account| account.owner_id == owner_user_id)
            .ok_or(CourseEnrollmentServiceError::AccountNotFound)?;
        if account.auth_state != AuthState::Authenticated {
            return Err(CourseEnrollmentServiceError::AccountNotAuthenticated);
        }
        Ok((account.provider_id, account.credential_refs))
    }

    async fn resolve_runtime(
        &self,
        command: &ExecuteCourseEnrollmentCommand,
    ) -> Result<EnrollmentRuntime, CourseEnrollmentServiceError> {
        let (provider_id, credential_refs) = self
            .resolve_account(command.owner_user_id, command.provider_account_id)
            .await?;
        let entry = self.registry.get(&provider_id).ok_or_else(|| {
            CourseEnrollmentServiceError::ProviderNotRegistered(provider_id.clone())
        })?;
        let capability = entry.course_enrollment.clone().ok_or_else(|| {
            CourseEnrollmentServiceError::CapabilityUnavailable(provider_id.clone())
        })?;
        let repository = self.repositories.for_provider(provider_id.clone());
        let access = enrollment_access(&command.correlation_id, "resolve enrollment draft");
        let draft = repository
            .resolve_course_enrollment_draft(CourseEnrollmentDraftResolveRequest {
                draft_id: command.draft_id,
                owner_user_id: command.owner_user_id,
                provider_account_id: command.provider_account_id,
                correlation_id: &command.correlation_id,
                access: &access,
            })
            .await?
            .ok_or(CourseEnrollmentServiceError::DraftNotFound)?;
        Ok(EnrollmentRuntime {
            context: ProviderContext {
                provider_id,
                account_id: command.provider_account_id,
                credential_refs,
                correlation_id: command.correlation_id.clone(),
            },
            capability,
            repository,
            draft,
        })
    }

    async fn recover_runtime(
        &self,
        runtime: EnrollmentRuntime,
        attempt: CourseEnrollmentAttempt,
        at: Timestamp,
    ) -> Result<CourseEnrollmentRunResult, CourseEnrollmentServiceError> {
        if attempt.draft_id != runtime.draft.record.draft.id {
            return Err(CourseEnrollmentServiceError::AttemptStateConflict);
        }
        if matches!(
            attempt.state,
            CourseEnrollmentAttemptState::Succeeded | CourseEnrollmentAttemptState::Rejected
        ) {
            return Ok(CourseEnrollmentRunResult { attempt });
        }
        if !matches!(
            attempt.state,
            CourseEnrollmentAttemptState::MutationIssued
                | CourseEnrollmentAttemptState::ReceiptRecorded
                | CourseEnrollmentAttemptState::VerificationPending
        ) {
            return Err(CourseEnrollmentServiceError::AttemptStateConflict);
        }
        let pending = runtime
            .repository
            .begin_course_enrollment_verification(CourseEnrollmentAttemptVerificationBeginRequest {
                attempt_id: attempt.id,
                owner_user_id: runtime.draft.record.draft.owner_user_id,
                provider_account_id: runtime.draft.record.draft.provider_account_id,
                at,
            })
            .await?;
        let recovery = recovery_record(&pending)?;
        let Ok(verification) = runtime
            .capability
            .verify_course_enrollment(&runtime.context, &runtime.draft.provider_draft, &recovery)
            .await
        else {
            return Ok(CourseEnrollmentRunResult { attempt: pending });
        };
        let attempt = runtime
            .repository
            .record_course_enrollment_verification(
                CourseEnrollmentAttemptVerificationRecordRequest {
                    attempt_id: pending.id,
                    owner_user_id: runtime.draft.record.draft.owner_user_id,
                    provider_account_id: runtime.draft.record.draft.provider_account_id,
                    verification: CourseEnrollmentVerification {
                        observation_digest: verification.observation_digest(),
                        membership_present: verification.membership_present(),
                        observed_at: pending.updated_at.max(chrono::Utc::now()),
                    },
                },
            )
            .await?;
        Ok(CourseEnrollmentRunResult { attempt })
    }
}

struct EnrollmentRuntime {
    context: ProviderContext,
    capability: Arc<dyn asterism_provider_api::CourseEnrollmentCapability>,
    repository: Arc<dyn CourseEnrollmentRepository>,
    draft: asterism_storage::ResolvedCourseEnrollmentDraft,
}

struct CourseEnrollmentMutationSink {
    repository: Arc<dyn CourseEnrollmentRepository>,
    attempt_id: CourseEnrollmentAttemptId,
    owner_user_id: UserId,
    provider_account_id: ProviderAccountId,
    at: Timestamp,
}

#[async_trait::async_trait]
impl ExecutionMutationSink for CourseEnrollmentMutationSink {
    async fn issue(&self, issue: &ExecutionMutationIssue) -> ProviderResult<()> {
        if issue.ordinal() != 1 {
            return Err(invalid_provider_mutation());
        }
        self.repository
            .issue_course_enrollment_mutation(CourseEnrollmentAttemptMutationIssueRequest {
                attempt_id: self.attempt_id,
                owner_user_id: self.owner_user_id,
                provider_account_id: self.provider_account_id,
                operation_type: issue.operation_type(),
                request_digest: issue.request_digest(),
                at: self.at,
            })
            .await
            .map(|_| ())
            .map_err(|_| internal_provider_storage())
    }

    async fn record_receipt(&self, receipt: ExecutionMutationReceipt) -> ProviderResult<()> {
        if receipt.ordinal() != 1 || receipt.retry_after_seconds().is_some() {
            return Err(invalid_provider_mutation());
        }
        self.repository
            .record_course_enrollment_receipt(CourseEnrollmentAttemptReceiptRequest {
                attempt_id: self.attempt_id,
                owner_user_id: self.owner_user_id,
                provider_account_id: self.provider_account_id,
                receipt: CourseEnrollmentMutationReceipt {
                    response_digest: receipt.response_digest(),
                    accepted: receipt.accepted(),
                    observed_at: self.at,
                },
            })
            .await
            .map(|_| ())
            .map_err(|_| internal_provider_storage())
    }
}

fn recovery_record(
    attempt: &CourseEnrollmentAttempt,
) -> Result<ExecutionMutationRecoveryRecord, CourseEnrollmentServiceError> {
    let operation_type = attempt
        .issued_operation_type
        .clone()
        .ok_or(CourseEnrollmentServiceError::AttemptStateConflict)?;
    let request_digest = attempt
        .issued_request_digest
        .ok_or(CourseEnrollmentServiceError::AttemptStateConflict)?;
    let issue = ExecutionMutationIssue::new(1, operation_type, request_digest)
        .map_err(|_| CourseEnrollmentServiceError::AttemptStateConflict)?;
    let receipt = attempt
        .receipt
        .map(|receipt| ExecutionMutationReceipt::new(1, receipt.response_digest, receipt.accepted))
        .transpose()
        .map_err(|_| CourseEnrollmentServiceError::AttemptStateConflict)?;
    ExecutionMutationRecoveryRecord::try_new(issue, receipt, None)
        .map_err(|_| CourseEnrollmentServiceError::AttemptStateConflict)
}

fn enrollment_access(correlation_id: &str, reason: &str) -> SecretAccess {
    SecretAccess {
        actor: SecretActor::CoreService("course-enrollment-engine"),
        correlation_id: correlation_id.to_owned(),
        reason: reason.to_owned(),
    }
}

fn validate_correlation_id(value: &str) -> Result<(), CourseEnrollmentServiceError> {
    if !value.is_empty()
        && value.len() <= MAX_CORRELATION_ID_BYTES
        && value.trim() == value
        && !value.chars().any(char::is_control)
    {
        Ok(())
    } else {
        Err(CourseEnrollmentServiceError::InvalidCorrelationId)
    }
}

fn invalid_provider_mutation() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::InvalidResponse,
        "Course enrollment mutation identity is invalid",
    )
}

fn internal_provider_storage() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::Internal,
        "Core could not persist Course enrollment mutation state",
    )
}

#[derive(Debug, thiserror::Error)]
pub enum CourseEnrollmentServiceError {
    #[error("course enrollment correlation ID is invalid")]
    InvalidCorrelationId,
    #[error("Provider account was not found")]
    AccountNotFound,
    #[error("Provider account is not authenticated")]
    AccountNotAuthenticated,
    #[error("Provider `{0}` is not registered")]
    ProviderNotRegistered(ProviderId),
    #[error("Provider `{0}` does not implement course enrollment")]
    CapabilityUnavailable(ProviderId),
    #[error("Provider returned an invalid course enrollment artifact")]
    ProviderResponseInvalid,
    #[error("course enrollment draft was not found")]
    DraftNotFound,
    #[error("course enrollment Attempt was not found")]
    AttemptNotFound,
    #[error("course enrollment Attempt state is inconsistent")]
    AttemptStateConflict,
    #[error(transparent)]
    Storage(#[from] StorageError),
    #[error(transparent)]
    SecretStorage(#[from] SecretStoreError),
    #[error(transparent)]
    Provider(#[from] ProviderError),
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeSet,
        sync::{
            Mutex,
            atomic::{AtomicBool, AtomicUsize, Ordering},
        },
    };

    use asterism_domain::{CourseEnrollmentDraft, ProviderAccount, SecretId};
    use asterism_provider_api::{
        CourseEnrollmentCapability, ProviderCapability, ProviderCourseEnrollmentDraft,
        ProviderCourseEnrollmentVerification, ProviderEntry, ProviderIdentity, ProviderMetadata,
        VerificationLevel,
    };
    use asterism_secrets::SecretValue;
    use async_trait::async_trait;
    use serde_json::json;

    use super::*;

    #[derive(Clone)]
    struct FakeAccountRepository {
        account: ProviderAccount,
    }

    #[async_trait]
    impl ProviderAccountRuntimeRepository for FakeAccountRepository {
        async fn find_runtime_provider_account(
            &self,
            account_id: ProviderAccountId,
        ) -> Result<Option<ProviderAccount>, StorageError> {
            Ok((self.account.id == account_id).then(|| self.account.clone()))
        }
    }

    struct StoredFakeDraft {
        record: CourseEnrollmentDraftRecord,
        request: Vec<u8>,
    }

    #[derive(Default)]
    struct FakeEnrollmentRepository {
        draft: Mutex<Option<StoredFakeDraft>>,
        attempt: Mutex<Option<CourseEnrollmentAttempt>>,
    }

    #[async_trait]
    impl asterism_storage::CourseEnrollmentDraftRepository for FakeEnrollmentRepository {
        async fn create_course_enrollment_draft(
            &self,
            request: CourseEnrollmentDraftCreateRequest<'_>,
        ) -> Result<CourseEnrollmentDraftCreateOutcome, SecretStoreError> {
            let mut stored = self.draft.lock().map_err(|_| SecretStoreError::Storage)?;
            if let Some(existing) = stored.as_ref() {
                return Ok(CourseEnrollmentDraftCreateOutcome::AlreadyExists(
                    existing.record.clone(),
                ));
            }
            let record = CourseEnrollmentDraftRecord {
                draft: CourseEnrollmentDraft {
                    id: request.draft_id,
                    owner_user_id: request.owner_user_id,
                    provider_account_id: request.provider_account_id,
                    provider_id: request.provider_draft.provider_id().clone(),
                    remote_course_id: request.provider_draft.remote_course_id().to_owned(),
                    remote_class_id: request.provider_draft.remote_class_id().to_owned(),
                    preview_digest: request.provider_draft.preview_digest(),
                    request_digest: request.provider_draft.request_digest(),
                    artifact_secret_id: SecretId::new(),
                    created_at: request.created_at,
                },
                artifact_type: request.provider_draft.artifact_type().to_owned(),
                preview_sanitized: request.provider_draft.preview_sanitized().clone(),
            };
            *stored = Some(StoredFakeDraft {
                record: record.clone(),
                request: request.provider_draft.request().expose_secret().to_vec(),
            });
            Ok(CourseEnrollmentDraftCreateOutcome::Created(record))
        }

        async fn resolve_course_enrollment_draft(
            &self,
            request: CourseEnrollmentDraftResolveRequest<'_>,
        ) -> Result<Option<asterism_storage::ResolvedCourseEnrollmentDraft>, SecretStoreError>
        {
            let stored = self.draft.lock().map_err(|_| SecretStoreError::Storage)?;
            let Some(stored) = stored.as_ref().filter(|stored| {
                stored.record.draft.id == request.draft_id
                    && stored.record.draft.owner_user_id == request.owner_user_id
                    && stored.record.draft.provider_account_id == request.provider_account_id
            }) else {
                return Ok(None);
            };
            let provider_draft = ProviderCourseEnrollmentDraft::try_new(
                stored.record.draft.provider_id.clone(),
                stored.record.artifact_type.clone(),
                stored.record.draft.remote_course_id.clone(),
                stored.record.draft.remote_class_id.clone(),
                stored.record.preview_sanitized.clone(),
                SecretValue::new(stored.request.clone()),
            )
            .map_err(|_| SecretStoreError::AuthenticationFailed)?;
            Ok(Some(asterism_storage::ResolvedCourseEnrollmentDraft {
                record: stored.record.clone(),
                provider_draft,
            }))
        }
    }

    #[async_trait]
    impl asterism_storage::CourseEnrollmentAttemptRepository for FakeEnrollmentRepository {
        async fn create_course_enrollment_attempt(
            &self,
            request: CourseEnrollmentAttemptCreateRequest,
        ) -> Result<CourseEnrollmentAttemptCreateOutcome, StorageError> {
            let mut attempt = self
                .attempt
                .lock()
                .map_err(|_| StorageError::CourseEnrollmentStateConflict)?;
            if let Some(existing) = attempt.as_ref() {
                return Ok(CourseEnrollmentAttemptCreateOutcome::AlreadyExists(
                    existing.clone(),
                ));
            }
            let created =
                CourseEnrollmentAttempt::new(request.attempt_id, request.draft_id, request.at);
            *attempt = Some(created.clone());
            Ok(CourseEnrollmentAttemptCreateOutcome::Created(created))
        }

        async fn issue_course_enrollment_mutation(
            &self,
            request: CourseEnrollmentAttemptMutationIssueRequest<'_>,
        ) -> Result<CourseEnrollmentAttempt, StorageError> {
            let draft = self
                .draft
                .lock()
                .map_err(|_| StorageError::CourseEnrollmentStateConflict)?
                .as_ref()
                .map(|stored| stored.record.draft.clone())
                .ok_or(StorageError::CourseEnrollmentStateConflict)?;
            let mut attempt = self
                .attempt
                .lock()
                .map_err(|_| StorageError::CourseEnrollmentStateConflict)?;
            let attempt = attempt
                .as_mut()
                .ok_or(StorageError::CourseEnrollmentStateConflict)?;
            attempt
                .issue_mutation(
                    &draft,
                    request.operation_type,
                    request.request_digest,
                    request.at,
                )
                .map_err(|_| StorageError::CourseEnrollmentStateConflict)?;
            Ok(attempt.clone())
        }

        async fn record_course_enrollment_receipt(
            &self,
            request: CourseEnrollmentAttemptReceiptRequest,
        ) -> Result<CourseEnrollmentAttempt, StorageError> {
            self.mutate_attempt(|attempt| attempt.record_receipt(request.receipt))
        }

        async fn begin_course_enrollment_verification(
            &self,
            request: CourseEnrollmentAttemptVerificationBeginRequest,
        ) -> Result<CourseEnrollmentAttempt, StorageError> {
            self.mutate_attempt(|attempt| attempt.begin_verification(request.at))
        }

        async fn record_course_enrollment_verification(
            &self,
            request: CourseEnrollmentAttemptVerificationRecordRequest,
        ) -> Result<CourseEnrollmentAttempt, StorageError> {
            self.mutate_attempt(|attempt| attempt.record_verification(request.verification))
        }

        async fn find_owned_course_enrollment_attempt(
            &self,
            _owner_user_id: UserId,
            _provider_account_id: ProviderAccountId,
            attempt_id: CourseEnrollmentAttemptId,
        ) -> Result<Option<CourseEnrollmentAttempt>, StorageError> {
            Ok(self
                .attempt
                .lock()
                .map_err(|_| StorageError::CourseEnrollmentStateConflict)?
                .as_ref()
                .filter(|attempt| attempt.id == attempt_id)
                .cloned())
        }
    }

    impl FakeEnrollmentRepository {
        fn mutate_attempt(
            &self,
            mutate: impl FnOnce(
                &mut CourseEnrollmentAttempt,
            )
                -> Result<(), asterism_domain::CourseEnrollmentValidationError>,
        ) -> Result<CourseEnrollmentAttempt, StorageError> {
            let mut attempt = self
                .attempt
                .lock()
                .map_err(|_| StorageError::CourseEnrollmentStateConflict)?;
            let attempt = attempt
                .as_mut()
                .ok_or(StorageError::CourseEnrollmentStateConflict)?;
            mutate(attempt).map_err(|_| StorageError::CourseEnrollmentStateConflict)?;
            Ok(attempt.clone())
        }
    }

    struct FakeEnrollmentFactory {
        repository: Arc<FakeEnrollmentRepository>,
    }

    impl CourseEnrollmentRepositoryFactory for FakeEnrollmentFactory {
        fn for_provider(&self, _provider_id: ProviderId) -> Arc<dyn CourseEnrollmentRepository> {
            self.repository.clone()
        }
    }

    struct AmbiguousEnrollmentCapability {
        metadata: ProviderMetadata,
        dispatch_count: AtomicUsize,
        membership_present: AtomicBool,
    }

    impl ProviderIdentity for AmbiguousEnrollmentCapability {
        fn metadata(&self) -> &ProviderMetadata {
            &self.metadata
        }
    }

    #[async_trait]
    impl CourseEnrollmentCapability for AmbiguousEnrollmentCapability {
        async fn prepare_course_enrollment(
            &self,
            _context: &ProviderContext,
            _invitation: SecretString,
        ) -> ProviderResult<ProviderCourseEnrollmentDraft> {
            ProviderCourseEnrollmentDraft::try_new(
                self.metadata.id.clone(),
                "chaoxing.course-enrollment.v1",
                "course-7",
                "class-9",
                json!({"course_title": "Writing"}),
                SecretValue::new(b"GET\0https://example.invalid/participateCls?id=7".to_vec()),
            )
        }

        async fn execute_course_enrollment(
            &self,
            _context: &ProviderContext,
            draft: &ProviderCourseEnrollmentDraft,
            mutations: &dyn ExecutionMutationSink,
        ) -> ProviderResult<ProviderCourseEnrollmentDispatchOutcome> {
            self.dispatch_count.fetch_add(1, Ordering::SeqCst);
            mutations
                .issue(&ExecutionMutationIssue::new(
                    1,
                    "chaoxing.course-enrollment.join",
                    draft.request_digest(),
                )?)
                .await?;
            Err(ProviderError::new(
                ProviderErrorKind::Network,
                "ambiguous response after issue",
            ))
        }

        async fn verify_course_enrollment(
            &self,
            _context: &ProviderContext,
            draft: &ProviderCourseEnrollmentDraft,
            mutation: &ExecutionMutationRecoveryRecord,
        ) -> ProviderResult<ProviderCourseEnrollmentVerification> {
            if mutation.issue().request_digest() != draft.request_digest()
                || mutation.issue().ordinal() != 1
            {
                return Err(invalid_provider_mutation());
            }
            ProviderCourseEnrollmentVerification::try_new(
                [71; 32],
                self.membership_present.load(Ordering::SeqCst),
            )
        }
    }

    #[tokio::test]
    async fn ambiguous_issue_is_never_replayed_and_recovers_by_inventory_only() {
        let now = chrono::Utc::now();
        let owner_user_id = UserId::new();
        let provider_account_id = ProviderAccountId::new();
        let provider_id = ProviderId::new("chaoxing").unwrap();
        let metadata = ProviderMetadata {
            id: provider_id.clone(),
            display_name: "Chaoxing".to_owned(),
            implementation_version: "test".to_owned(),
            verification: VerificationLevel::Development,
            scan_min_interval_seconds: None,
            capture_recipe_version: None,
            capabilities: BTreeSet::from([ProviderCapability::CourseEnrollment]),
            auth_methods: BTreeSet::new(),
            session_kinds: BTreeSet::new(),
        };
        let capability = Arc::new(AmbiguousEnrollmentCapability {
            metadata: metadata.clone(),
            dispatch_count: AtomicUsize::new(0),
            membership_present: AtomicBool::new(false),
        });
        let mut entry = ProviderEntry::metadata_only(metadata);
        entry.course_enrollment = Some(capability.clone());
        let mut registry = ProviderRegistry::default();
        registry.register(entry).unwrap();
        let repository = Arc::new(FakeEnrollmentRepository::default());
        let service = CourseEnrollmentService::new(
            Arc::new(registry),
            FakeAccountRepository {
                account: ProviderAccount {
                    id: provider_account_id,
                    owner_id: owner_user_id,
                    provider_id,
                    display_name: "Account".to_owned(),
                    tenant: None,
                    auth_state: AuthState::Authenticated,
                    network_profile_id: None,
                    credential_refs: Vec::new(),
                    created_at: now,
                    updated_at: now,
                },
            },
            Arc::new(FakeEnrollmentFactory {
                repository: repository.clone(),
            }),
        );
        let draft_id = CourseEnrollmentDraftId::new();
        service
            .prepare(PrepareCourseEnrollmentCommand {
                draft_id,
                owner_user_id,
                provider_account_id,
                invitation: SecretString::new("invite-code"),
                correlation_id: "enrollment-test".to_owned(),
                at: now,
            })
            .await
            .unwrap();
        let attempt_id = CourseEnrollmentAttemptId::new();
        let pending = service
            .execute(ExecuteCourseEnrollmentCommand {
                attempt_id,
                draft_id,
                owner_user_id,
                provider_account_id,
                correlation_id: "enrollment-test".to_owned(),
                at: now,
            })
            .await
            .unwrap();
        assert_eq!(
            pending.attempt.state,
            CourseEnrollmentAttemptState::VerificationPending
        );
        assert_eq!(capability.dispatch_count.load(Ordering::SeqCst), 1);
        capability.membership_present.store(true, Ordering::SeqCst);
        let completed = service
            .recover(RecoverCourseEnrollmentCommand {
                attempt_id,
                draft_id,
                owner_user_id,
                provider_account_id,
                correlation_id: "enrollment-recovery".to_owned(),
                at: chrono::Utc::now(),
            })
            .await
            .unwrap();
        assert_eq!(
            completed.attempt.state,
            CourseEnrollmentAttemptState::Succeeded
        );
        assert_eq!(capability.dispatch_count.load(Ordering::SeqCst), 1);
    }
}
