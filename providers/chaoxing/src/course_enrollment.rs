use std::{collections::BTreeSet, fmt, sync::Arc};

use asterism_domain::HumanRequiredReason;
use asterism_provider_api::{
    CourseEnrollmentCapability, CourseInventoryCapability, ExecutionMutationIssue,
    ExecutionMutationRecoveryRecord, ExecutionMutationSink, ProviderContext,
    ProviderCourseEnrollmentDispatchOutcome, ProviderCourseEnrollmentDraft,
    ProviderCourseEnrollmentVerification, ProviderError, ProviderErrorKind, ProviderIdentity,
    ProviderMetadata, ProviderResult, RemoteCourse,
};
use asterism_secrets::SecretString;
use async_trait::async_trait;
use chrono::Utc;
use sha2::{Digest, Sha256};

use crate::{
    ChaoxingCourseEnrollmentTransport, ChaoxingCourseInviteApiDocument,
    ChaoxingCourseInviteApiPreparation, ChaoxingCourseInvitePreviewDocument,
    ChaoxingCourseInvitePreviewPreparation, ChaoxingCourseInvitePreviewRedirect,
    ChaoxingCourseJoinPreparation, ChaoxingIssuedCourseEnrollment,
    course_invite::{
        COURSE_ENROLLMENT_ARTIFACT_TYPE, COURSE_ENROLLMENT_OPERATION_TYPE,
        validate_shared_course_enrollment_draft,
    },
    metadata::development_metadata,
    parse_course_invite_api_redirect, parse_course_invite_preview, parse_course_join_receipt,
};

/// Shared `CourseEnrollmentCapability` adapter over the strict Chaoxing invite
/// parser, single-send transport and fresh Course inventory boundary.
pub struct ChaoxingCourseEnrollment<T> {
    metadata: ProviderMetadata,
    courses: Arc<dyn CourseInventoryCapability>,
    transport: Arc<T>,
}

impl<T> ChaoxingCourseEnrollment<T> {
    /// Builds the unregistered capability adapter.
    ///
    /// # Errors
    ///
    /// Returns an internal error if compile-time Provider metadata is invalid.
    pub fn try_new(
        courses: Arc<dyn CourseInventoryCapability>,
        transport: Arc<T>,
    ) -> ProviderResult<Self> {
        Ok(Self {
            metadata: development_metadata()?,
            courses,
            transport,
        })
    }
}

impl<T> fmt::Debug for ChaoxingCourseEnrollment<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ChaoxingCourseEnrollment")
            .field("metadata", &self.metadata)
            .field("courses", &"configured")
            .field("transport", &"configured")
            .finish()
    }
}

impl<T> ProviderIdentity for ChaoxingCourseEnrollment<T>
where
    T: Send + Sync,
{
    fn metadata(&self) -> &ProviderMetadata {
        &self.metadata
    }
}

#[async_trait]
impl<T> CourseEnrollmentCapability for ChaoxingCourseEnrollment<T>
where
    T: ChaoxingCourseEnrollmentTransport,
{
    async fn prepare_course_enrollment(
        &self,
        context: &ProviderContext,
        invitation: SecretString,
    ) -> ProviderResult<ProviderCourseEnrollmentDraft> {
        self.validate_context(context)?;
        let direct = ChaoxingCourseInvitePreviewPreparation::try_prepare(
            invitation.expose_secret().to_owned(),
        )?;
        let preview = match self.transport.fetch_direct_preview(context, &direct).await {
            Ok(document) => parse_course_invite_preview(&document)?,
            Err(error) if error.kind == ProviderErrorKind::RemoteChanged => {
                self.prepare_through_invite_api(context, invitation).await?
            }
            Err(error) => return Err(error),
        };
        let join = ChaoxingCourseJoinPreparation::try_prepare(&preview)?;
        ProviderCourseEnrollmentDraft::try_new(
            context.provider_id.clone(),
            COURSE_ENROLLMENT_ARTIFACT_TYPE,
            preview.course_id(),
            preview.class_id(),
            serde_json::json!({
                "course_id": preview.course_id(),
                "class_id": preview.class_id(),
                "display_action": "join_course_class",
            }),
            join.frozen_request(),
        )
    }

    async fn execute_course_enrollment(
        &self,
        context: &ProviderContext,
        draft: &ProviderCourseEnrollmentDraft,
        mutations: &dyn ExecutionMutationSink,
    ) -> ProviderResult<ProviderCourseEnrollmentDispatchOutcome> {
        self.validate_context(context)?;
        validate_shared_course_enrollment_draft(draft)?;
        let prepared = self
            .transport
            .prepare_frozen_join_transport(context, draft)
            .await?;
        let issue = ExecutionMutationIssue::new(
            1,
            COURSE_ENROLLMENT_OPERATION_TYPE,
            draft.request_digest(),
        )?;
        // Validate every deterministic binding before the durable point of no
        // return. The marker is passed to transport only after issue succeeds.
        let issued = ChaoxingIssuedCourseEnrollment::try_bind(draft, &issue)?;
        mutations.issue(&issue).await?;
        let document = self
            .transport
            .send_issued_frozen_join(prepared, issued)
            .await
            .map_err(post_issue_error)?;
        let receipt = parse_course_join_receipt(&document).map_err(post_issue_error)?;
        let shared_receipt = receipt.bind_shared_draft(draft).map_err(post_issue_error)?;
        mutations
            .record_receipt(shared_receipt)
            .await
            .map_err(post_issue_error)?;
        Ok(if shared_receipt.accepted() {
            ProviderCourseEnrollmentDispatchOutcome::Accepted
        } else {
            ProviderCourseEnrollmentDispatchOutcome::Rejected
        })
    }

    async fn verify_course_enrollment(
        &self,
        context: &ProviderContext,
        draft: &ProviderCourseEnrollmentDraft,
        mutation: &ExecutionMutationRecoveryRecord,
    ) -> ProviderResult<ProviderCourseEnrollmentVerification> {
        self.validate_context(context)?;
        validate_shared_course_enrollment_draft(draft)?;
        validate_recovery_issue(draft, mutation)?;
        let courses = self.courses.list_courses(context).await?;
        let (observation_digest, membership_present) =
            fresh_membership_observation(context, draft, &courses)?;
        ProviderCourseEnrollmentVerification::try_new(observation_digest, membership_present)
    }
}

impl<T> ChaoxingCourseEnrollment<T>
where
    T: ChaoxingCourseEnrollmentTransport,
{
    async fn prepare_through_invite_api(
        &self,
        context: &ProviderContext,
        invitation: SecretString,
    ) -> ProviderResult<crate::ChaoxingCourseInvitePreview> {
        let timestamp = u64::try_from(Utc::now().timestamp_millis()).map_err(|_| {
            ProviderError::new(
                ProviderErrorKind::Internal,
                "Chaoxing Course invite timestamp is invalid",
            )
        })?;
        let preparation = ChaoxingCourseInviteApiPreparation::try_prepare(
            invitation.expose_secret().to_owned(),
            timestamp,
        )?;
        let response: ChaoxingCourseInviteApiDocument = self
            .transport
            .fetch_invite_api(context, &preparation)
            .await?;
        let redirect: ChaoxingCourseInvitePreviewRedirect =
            parse_course_invite_api_redirect(&response)?;
        let document: ChaoxingCourseInvitePreviewDocument = self
            .transport
            .fetch_redirect_preview(context, &redirect)
            .await?;
        parse_course_invite_preview(&document)
    }

    fn validate_context(&self, context: &ProviderContext) -> ProviderResult<()> {
        if context.provider_id != self.metadata.id {
            return Err(ProviderError::new(
                ProviderErrorKind::InvalidResponse,
                "Chaoxing Course enrollment received a foreign Provider context",
            ));
        }
        if context.credential_refs.is_empty() {
            return Err(ProviderError::new(
                ProviderErrorKind::Authentication,
                "Chaoxing Course enrollment requires an authenticated account",
            ));
        }
        Ok(())
    }
}

fn validate_recovery_issue(
    draft: &ProviderCourseEnrollmentDraft,
    mutation: &ExecutionMutationRecoveryRecord,
) -> ProviderResult<()> {
    let issue = mutation.issue();
    if issue.ordinal() != 1
        || issue.operation_type() != COURSE_ENROLLMENT_OPERATION_TYPE
        || issue.request_digest() != draft.request_digest()
    {
        return Err(ProviderError::new(
            ProviderErrorKind::InvalidResponse,
            "Chaoxing Course enrollment recovery issue is foreign",
        ));
    }
    Ok(())
}

fn fresh_membership_observation(
    context: &ProviderContext,
    draft: &ProviderCourseEnrollmentDraft,
    courses: &[RemoteCourse],
) -> ProviderResult<([u8; 32], bool)> {
    let expected = format!(
        "course:{}:{}",
        draft.remote_course_id(),
        draft.remote_class_id()
    );
    let mut identities = BTreeSet::new();
    for course in courses {
        if course.remote_id.is_empty()
            || course.remote_id.len() > 512
            || course.remote_id.trim() != course.remote_id
            || course.remote_id.chars().any(char::is_control)
            || !identities.insert(course.remote_id.clone())
        {
            return Err(ProviderError::new(
                ProviderErrorKind::ProtocolDrift,
                "Chaoxing Course enrollment fresh inventory is ambiguous",
            ));
        }
    }
    let membership_present = identities.contains(&expected);
    let mut digest = Sha256::new();
    digest.update(b"asterism.chaoxing.shared-course-enrollment-observation.v1\0");
    digest.update(context.provider_id.as_str().as_bytes());
    digest.update(context.account_id.to_string().as_bytes());
    digest.update(draft.preview_digest());
    digest.update(draft.request_digest());
    digest.update([u8::from(membership_present)]);
    digest.update(
        u32::try_from(identities.len())
            .expect("Chaoxing Course inventory is bounded")
            .to_be_bytes(),
    );
    for identity in identities {
        digest.update(
            u32::try_from(identity.len())
                .expect("Chaoxing Course identity is bounded")
                .to_be_bytes(),
        );
        digest.update(identity.as_bytes());
    }
    Ok((digest.finalize().into(), membership_present))
}

fn post_issue_error(_error: ProviderError) -> ProviderError {
    ProviderError::human_required(
        "Chaoxing Course enrollment was issued and requires fresh inventory recovery",
        HumanRequiredReason::ManualIntervention,
    )
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    };

    use asterism_domain::{ProviderAccountId, ProviderId, SecretId};
    use asterism_provider_api::{ExecutionMutationReceipt, ProviderRouteContext, RemoteCourse};
    use serde_json::json;

    use super::*;

    const PREVIEW: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fixtures/providers/chaoxing/course-invite/middle-view.html"
    ));
    const API_SUCCESS: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fixtures/providers/chaoxing/course-invite/invite-api-success.json"
    ));
    const ACCEPTED: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fixtures/providers/chaoxing/course-invite/participate-accepted.json"
    ));

    struct FixtureTransport {
        events: Arc<Mutex<Vec<&'static str>>>,
        issued: Arc<AtomicBool>,
        direct_remote_changed: AtomicBool,
        fail_send: AtomicBool,
    }

    impl FixtureTransport {
        fn new(events: Arc<Mutex<Vec<&'static str>>>, issued: Arc<AtomicBool>) -> Self {
            Self {
                events,
                issued,
                direct_remote_changed: AtomicBool::new(false),
                fail_send: AtomicBool::new(false),
            }
        }
    }

    #[async_trait]
    impl ChaoxingCourseEnrollmentTransport for FixtureTransport {
        type PreparedJoin = [u8; 32];

        async fn fetch_direct_preview(
            &self,
            _context: &ProviderContext,
            preparation: &ChaoxingCourseInvitePreviewPreparation,
        ) -> ProviderResult<ChaoxingCourseInvitePreviewDocument> {
            self.events.lock().unwrap().push("preview");
            if self.direct_remote_changed.load(Ordering::Relaxed) {
                return Err(ProviderError::new(
                    ProviderErrorKind::RemoteChanged,
                    "fixture direct preview changed",
                ));
            }
            ChaoxingCourseInvitePreviewDocument::for_preparation(preparation, PREVIEW.to_owned())
        }

        async fn fetch_invite_api(
            &self,
            _context: &ProviderContext,
            preparation: &ChaoxingCourseInviteApiPreparation,
        ) -> ProviderResult<ChaoxingCourseInviteApiDocument> {
            self.events.lock().unwrap().push("invite_api");
            ChaoxingCourseInviteApiDocument::for_preparation(preparation, API_SUCCESS.to_owned())
        }

        async fn fetch_redirect_preview(
            &self,
            _context: &ProviderContext,
            redirect: &ChaoxingCourseInvitePreviewRedirect,
        ) -> ProviderResult<ChaoxingCourseInvitePreviewDocument> {
            self.events.lock().unwrap().push("redirect_preview");
            ChaoxingCourseInvitePreviewDocument::for_redirect(redirect, PREVIEW.to_owned())
        }

        async fn prepare_join_transport(
            &self,
            _context: &ProviderContext,
            preparation: &ChaoxingCourseJoinPreparation,
        ) -> ProviderResult<Self::PreparedJoin> {
            Ok(preparation.request_digest_bytes())
        }

        async fn send_issued_join(
            &self,
            prepared: Self::PreparedJoin,
            issued: crate::ChaoxingIssuedCourseJoin<'_>,
        ) -> ProviderResult<crate::ChaoxingCourseJoinReceiptDocument> {
            assert_eq!(prepared, issued.preparation().request_digest_bytes());
            crate::ChaoxingCourseJoinReceiptDocument::for_preparation(
                issued.preparation(),
                ACCEPTED.to_owned(),
            )
        }

        async fn prepare_frozen_join_transport(
            &self,
            _context: &ProviderContext,
            draft: &ProviderCourseEnrollmentDraft,
        ) -> ProviderResult<Self::PreparedJoin> {
            self.events.lock().unwrap().push("prepare_join");
            validate_shared_course_enrollment_draft(draft)?;
            Ok(draft.request_digest())
        }

        async fn send_issued_frozen_join(
            &self,
            prepared: Self::PreparedJoin,
            issued: ChaoxingIssuedCourseEnrollment<'_>,
        ) -> ProviderResult<crate::ChaoxingCourseJoinReceiptDocument> {
            assert!(self.issued.load(Ordering::Acquire));
            assert_eq!(prepared, issued.draft().request_digest());
            self.events.lock().unwrap().push("send_join");
            if self.fail_send.load(Ordering::Relaxed) {
                return Err(ProviderError::new(
                    ProviderErrorKind::Network,
                    "fixture mutation outcome is ambiguous",
                ));
            }
            crate::ChaoxingCourseJoinReceiptDocument::for_shared_draft(
                issued.draft(),
                ACCEPTED.to_owned(),
            )
        }
    }

    struct FixtureCourses {
        metadata: ProviderMetadata,
        courses: Mutex<Vec<RemoteCourse>>,
        reads: AtomicUsize,
    }

    impl FixtureCourses {
        fn new(courses: Vec<RemoteCourse>) -> Self {
            Self {
                metadata: development_metadata().unwrap(),
                courses: Mutex::new(courses),
                reads: AtomicUsize::new(0),
            }
        }
    }

    impl ProviderIdentity for FixtureCourses {
        fn metadata(&self) -> &ProviderMetadata {
            &self.metadata
        }
    }

    #[async_trait]
    impl CourseInventoryCapability for FixtureCourses {
        async fn list_courses(
            &self,
            _context: &ProviderContext,
        ) -> ProviderResult<Vec<RemoteCourse>> {
            self.reads.fetch_add(1, Ordering::Relaxed);
            Ok(self.courses.lock().unwrap().clone())
        }
    }

    struct RecordingMutations {
        events: Arc<Mutex<Vec<&'static str>>>,
        issued_flag: Arc<AtomicBool>,
        issue: Mutex<Option<ExecutionMutationIssue>>,
        receipt: Mutex<Option<ExecutionMutationReceipt>>,
    }

    impl RecordingMutations {
        fn new(events: Arc<Mutex<Vec<&'static str>>>, issued_flag: Arc<AtomicBool>) -> Self {
            Self {
                events,
                issued_flag,
                issue: Mutex::new(None),
                receipt: Mutex::new(None),
            }
        }
    }

    #[async_trait]
    impl ExecutionMutationSink for RecordingMutations {
        async fn issue(&self, issue: &ExecutionMutationIssue) -> ProviderResult<()> {
            let mut persisted = self.issue.lock().unwrap();
            if persisted.is_some() {
                return Err(ProviderError::human_required(
                    "fixture issue cannot be replayed",
                    HumanRequiredReason::ManualIntervention,
                ));
            }
            *persisted = Some(issue.clone());
            self.issued_flag.store(true, Ordering::Release);
            self.events.lock().unwrap().push("issue");
            Ok(())
        }

        async fn record_receipt(&self, receipt: ExecutionMutationReceipt) -> ProviderResult<()> {
            *self.receipt.lock().unwrap() = Some(receipt);
            self.events.lock().unwrap().push("receipt");
            Ok(())
        }
    }

    #[tokio::test]
    async fn shared_draft_keeps_only_safe_preview_and_secret_exact_request() {
        let fixture = fixture(vec![]);
        let draft = fixture
            .capability
            .prepare_course_enrollment(&context(), SecretString::new("12345678".to_owned()))
            .await
            .unwrap();
        assert_eq!(draft.remote_course_id(), "1001");
        assert_eq!(draft.remote_class_id(), "2001");
        assert_eq!(
            draft.preview_sanitized(),
            &json!({
                "course_id": "1001",
                "class_id": "2001",
                "display_action": "join_course_class",
            })
        );
        for key in draft.preview_sanitized().as_object().unwrap().keys() {
            let key = key.to_ascii_lowercase();
            assert!(
                !["invite", "enc", "check", "token", "secret"]
                    .iter()
                    .any(|needle| key.contains(needle))
            );
        }
        let request = draft.request().expose_secret();
        assert!(request.starts_with(b"GET\0https://mooc1.chaoxing.com/"));
        assert!(
            request
                .windows(b"inviteCode=12345678".len())
                .any(|value| { value == b"inviteCode=12345678" })
        );
        assert!(
            request
                .windows(b"PRIVATE_ADD_CLASS_ENC".len())
                .any(|value| { value == b"PRIVATE_ADD_CLASS_ENC" })
        );
        validate_shared_course_enrollment_draft(&draft).unwrap();
        let debug = format!("{draft:?}");
        assert!(!debug.contains("12345678"));
        assert!(!debug.contains("PRIVATE_ADD_CLASS_ENC"));
    }

    #[tokio::test]
    async fn shared_execute_persists_issue_before_single_send_and_definite_receipt() {
        let fixture = fixture(vec![]);
        let context = context();
        let draft = fixture
            .capability
            .prepare_course_enrollment(&context, SecretString::new("12345678".to_owned()))
            .await
            .unwrap();
        fixture.events.lock().unwrap().clear();
        let outcome = fixture
            .capability
            .execute_course_enrollment(&context, &draft, fixture.mutations.as_ref())
            .await
            .unwrap();
        assert_eq!(outcome, ProviderCourseEnrollmentDispatchOutcome::Accepted);
        assert_eq!(
            *fixture.events.lock().unwrap(),
            vec!["prepare_join", "issue", "send_join", "receipt"]
        );
        let issue = fixture.mutations.issue.lock().unwrap().clone().unwrap();
        assert_eq!(issue.ordinal(), 1);
        assert_eq!(issue.operation_type(), COURSE_ENROLLMENT_OPERATION_TYPE);
        assert_eq!(issue.request_digest(), draft.request_digest());
        assert!(
            fixture
                .mutations
                .receipt
                .lock()
                .unwrap()
                .unwrap()
                .accepted()
        );

        let sends_before = fixture
            .events
            .lock()
            .unwrap()
            .iter()
            .filter(|event| **event == "send_join")
            .count();
        assert!(
            fixture
                .capability
                .execute_course_enrollment(&context, &draft, fixture.mutations.as_ref())
                .await
                .is_err()
        );
        let sends_after = fixture
            .events
            .lock()
            .unwrap()
            .iter()
            .filter(|event| **event == "send_join")
            .count();
        assert_eq!(sends_after, sends_before);
    }

    #[tokio::test]
    async fn shared_verify_uses_only_fresh_inventory_for_receipt_or_ambiguity() {
        let fixture = fixture(vec![course("course:1001:2001")]);
        let context = context();
        let draft = fixture
            .capability
            .prepare_course_enrollment(&context, SecretString::new("12345678".to_owned()))
            .await
            .unwrap();
        let issue = ExecutionMutationIssue::new(
            1,
            COURSE_ENROLLMENT_OPERATION_TYPE,
            draft.request_digest(),
        )
        .unwrap();
        let ambiguous =
            ExecutionMutationRecoveryRecord::try_new(issue.clone(), None, None).unwrap();
        let verified = fixture
            .capability
            .verify_course_enrollment(&context, &draft, &ambiguous)
            .await
            .unwrap();
        assert!(verified.membership_present());
        assert_ne!(verified.observation_digest(), [0; 32]);

        fixture.courses.courses.lock().unwrap().clear();
        let absent = fixture
            .capability
            .verify_course_enrollment(&context, &draft, &ambiguous)
            .await
            .unwrap();
        assert!(!absent.membership_present());
        assert_eq!(fixture.courses.reads.load(Ordering::Relaxed), 2);

        let foreign = ExecutionMutationRecoveryRecord::try_new(
            ExecutionMutationIssue::new(1, "chaoxing.foreign", draft.request_digest()).unwrap(),
            None,
            None,
        )
        .unwrap();
        assert!(
            fixture
                .capability
                .verify_course_enrollment(&context, &draft, &foreign)
                .await
                .is_err()
        );
        assert_eq!(fixture.courses.reads.load(Ordering::Relaxed), 2);
    }

    #[tokio::test]
    async fn ambiguous_send_has_no_receipt_and_can_only_enter_fresh_verification() {
        let fixture = fixture(vec![course("course:1001:2001")]);
        let context = context();
        let draft = fixture
            .capability
            .prepare_course_enrollment(&context, SecretString::new("12345678".to_owned()))
            .await
            .unwrap();
        fixture.events.lock().unwrap().clear();
        fixture.transport.fail_send.store(true, Ordering::Relaxed);
        assert!(
            fixture
                .capability
                .execute_course_enrollment(&context, &draft, fixture.mutations.as_ref())
                .await
                .is_err()
        );
        assert_eq!(
            *fixture.events.lock().unwrap(),
            vec!["prepare_join", "issue", "send_join"]
        );
        assert!(fixture.mutations.receipt.lock().unwrap().is_none());
        let issue = fixture.mutations.issue.lock().unwrap().clone().unwrap();
        let ambiguous = ExecutionMutationRecoveryRecord::try_new(issue, None, None).unwrap();
        let verification = fixture
            .capability
            .verify_course_enrollment(&context, &draft, &ambiguous)
            .await
            .unwrap();
        assert!(verification.membership_present());

        let sends_before = fixture
            .events
            .lock()
            .unwrap()
            .iter()
            .filter(|event| **event == "send_join")
            .count();
        assert!(
            fixture
                .capability
                .execute_course_enrollment(&context, &draft, fixture.mutations.as_ref())
                .await
                .is_err()
        );
        let sends_after = fixture
            .events
            .lock()
            .unwrap()
            .iter()
            .filter(|event| **event == "send_join")
            .count();
        assert_eq!(sends_after, sends_before);
    }

    #[tokio::test]
    async fn direct_remote_change_uses_strict_read_only_invite_api_fallback() {
        let fixture = fixture(vec![]);
        fixture
            .transport
            .direct_remote_changed
            .store(true, Ordering::Relaxed);
        let draft = fixture
            .capability
            .prepare_course_enrollment(&context(), SecretString::new("12345678".to_owned()))
            .await
            .unwrap();
        assert_eq!(draft.remote_course_id(), "1001");
        assert_eq!(
            *fixture.events.lock().unwrap(),
            vec!["preview", "invite_api", "redirect_preview"]
        );
    }

    struct Fixture {
        capability: ChaoxingCourseEnrollment<FixtureTransport>,
        transport: Arc<FixtureTransport>,
        courses: Arc<FixtureCourses>,
        mutations: Arc<RecordingMutations>,
        events: Arc<Mutex<Vec<&'static str>>>,
    }

    fn fixture(courses: Vec<RemoteCourse>) -> Fixture {
        let events = Arc::new(Mutex::new(Vec::new()));
        let issued = Arc::new(AtomicBool::new(false));
        let transport = Arc::new(FixtureTransport::new(events.clone(), issued.clone()));
        let courses = Arc::new(FixtureCourses::new(courses));
        let mutations = Arc::new(RecordingMutations::new(events.clone(), issued));
        let capability =
            ChaoxingCourseEnrollment::try_new(courses.clone(), transport.clone()).unwrap();
        Fixture {
            capability,
            transport,
            courses,
            mutations,
            events,
        }
    }

    fn context() -> ProviderContext {
        ProviderContext {
            provider_id: ProviderId::new("chaoxing").unwrap(),
            account_id: ProviderAccountId::new(),
            credential_refs: vec![SecretId::new()],
            correlation_id: "shared-course-enrollment-fixture".to_owned(),
        }
    }

    fn course(remote_id: &str) -> RemoteCourse {
        RemoteCourse {
            remote_id: remote_id.to_owned(),
            title: "Synthetic Course".to_owned(),
            term: None,
            teacher: None,
            remote_status: None,
            metadata_sanitized: json!({"fixture": true}),
            route_context: ProviderRouteContext::default(),
        }
    }
}
