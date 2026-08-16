use std::{fmt, sync::Arc};

use asterism_domain::{SubmissionReceipt, SubmissionScore, Timestamp};
use asterism_provider_api::{
    ProviderContext, ProviderError, ProviderErrorKind, ProviderResult, TaskDetailCapability,
};
use async_trait::async_trait;
use chrono::Utc;
use reqwest::Url;
use serde_json::Value;
use sha2::{Digest, Sha256};
use zeroize::{Zeroize, Zeroizing};

use crate::{
    encrypted::ZeroizingJsonValue,
    submission_execute::valid_submission_version,
    submission_verify::{
        UaiSubmissionPolicyEvidence, bound_verification_state, verified_submission_policy,
        verified_submission_score,
    },
};

const MAX_UPLOAD_GRANT_RESPONSE_BYTES: usize = 64 * 1_024;
const MAX_UPLOAD_RESPONSE_BYTES: usize = 64 * 1_024;
const MAX_UPLOAD_TOKEN_BYTES: usize = 8 * 1_024;
const MAX_UPLOAD_KEY_BYTES: usize = 1_024;
const MAX_UPLOAD_HASH_BYTES: usize = 1_024;
const MAX_UPLOAD_ARTIFACT_BYTES: usize = 64 * 1_024 * 1_024;
const MAX_UPLOAD_SUBMISSION_BYTES: usize = 4 * 1_024 * 1_024;
const MAX_UPLOAD_VERIFICATION_BYTES: usize = 4 * 1_024 * 1_024;
const MAX_UPLOAD_NESTED_ANSWER_BYTES: usize = 1_024 * 1_024;
const MINIMAL_MP3_BYTES: usize = 4_096;
const QINIU_UPLOAD_ROUTE: &str = "https://upload-z1.qiniup.com/";
const UAI_UPLOAD_GRANT_ROUTE: &str = "https://ucontent.unipus.cn/media/user_resource/cms/token";
const UAI_UPLOAD_SUBMISSION_ROUTE: &str =
    "https://ucontent.unipus.cn/course/api/v3/newExploration/submit";
const UAI_UPLOAD_REFERER: &str = "https://ucontent.unipus.cn/";
const UAI_UPLOAD_SUBMISSION_CONTENT_TYPE: &str = "application/json; charset=utf-8";

/// Provider-private boundary for the audited grant, object-store and final
/// single-upload mutation stages. Shared Core still owns the durable
/// artifact/attempt state machine.
#[async_trait]
pub trait UaiUploadTransport: Send + Sync {
    async fn request_upload_grant(
        &self,
        context: &ProviderContext,
        intent: &UaiUploadIntent,
        artifact: &UaiUploadArtifact,
    ) -> ProviderResult<UaiUploadGrant>;

    async fn upload_artifact(
        &self,
        context: &ProviderContext,
        grant: &UaiUploadGrant,
        artifact: &UaiUploadArtifact,
    ) -> ProviderResult<UaiUploadedArtifact>;

    async fn submit_uploaded_artifact(
        &self,
        context: &ProviderContext,
        submission: &UaiUploadSubmission,
    ) -> ProviderResult<SubmissionReceipt>;

    async fn verify_uploaded_artifact(
        &self,
        context: &ProviderContext,
        submission: &UaiUploadSubmission,
        receipt: &SubmissionReceipt,
    ) -> ProviderResult<UaiUploadVerification>;
}

/// Short-lived CMS/object-store authorization returned by UAI.
pub struct UaiUploadGrant {
    token: Zeroizing<String>,
    file_key: String,
    intent_fingerprint: String,
    artifact_digest: String,
    remote_task_id: String,
    task_fingerprint: String,
    course_resource_id: String,
    unit_id: String,
    group_id: String,
    upload_position: u32,
    grant_request_digest: [u8; 32],
    grant_response_digest: [u8; 32],
}

impl UaiUploadGrant {
    pub fn expose_token(&self) -> &str {
        self.token.as_str()
    }

    pub fn file_key(&self) -> &str {
        &self.file_key
    }

    pub(crate) fn intent_fingerprint(&self) -> &str {
        &self.intent_fingerprint
    }

    pub(crate) fn artifact_digest(&self) -> &str {
        &self.artifact_digest
    }

    pub(crate) fn remote_task_id(&self) -> &str {
        &self.remote_task_id
    }

    pub(crate) fn task_fingerprint(&self) -> &str {
        &self.task_fingerprint
    }

    pub(crate) fn course_resource_id(&self) -> &str {
        &self.course_resource_id
    }

    pub(crate) fn unit_id(&self) -> &str {
        &self.unit_id
    }

    pub(crate) fn group_id(&self) -> &str {
        &self.group_id
    }

    pub(crate) const fn upload_position(&self) -> u32 {
        self.upload_position
    }

    pub const fn grant_request_digest(&self) -> [u8; 32] {
        self.grant_request_digest
    }

    pub const fn grant_response_digest(&self) -> [u8; 32] {
        self.grant_response_digest
    }

    fn validate_for_artifact(&self, artifact: &UaiUploadArtifact) -> ProviderResult<()> {
        if self.artifact_digest == artifact.digest() {
            Ok(())
        } else {
            Err(ProviderError::new(
                ProviderErrorKind::RemoteChanged,
                "UAI upload grant is foreign to the selected artifact",
            ))
        }
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "the encrypted grant state restores every independent upload binding"
    )]
    pub(crate) fn restore_grant_state(
        token: Zeroizing<String>,
        mut file_key: String,
        mut intent_fingerprint: String,
        mut artifact_digest: String,
        mut remote_task_id: String,
        mut task_fingerprint: String,
        mut course_resource_id: String,
        mut unit_id: String,
        mut group_id: String,
        upload_position: u32,
        grant_request_digest: [u8; 32],
        grant_response_digest: [u8; 32],
    ) -> ProviderResult<Self> {
        let expected_remote_task_id = format!("group:{course_resource_id}:{unit_id}:{group_id}");
        if token.is_empty()
            || token.len() > MAX_UPLOAD_TOKEN_BYTES
            || token.chars().any(char::is_control)
            || file_key.is_empty()
            || file_key.len() > MAX_UPLOAD_KEY_BYTES
            || file_key.chars().any(char::is_control)
            || !valid_private_binding(&intent_fingerprint, "uai-upload-v1:", 256)
            || !valid_private_binding(&artifact_digest, "sha256:", 256)
            || !is_remote_component(&course_resource_id)
            || !is_remote_component(&unit_id)
            || !is_remote_component(&group_id)
            || remote_task_id != expected_remote_task_id
            || remote_task_id.len() > 512
            || !valid_private_binding(&task_fingerprint, "v1:", 512)
            || !(1..=2).contains(&upload_position)
            || grant_request_digest == [0; 32]
            || grant_response_digest == [0; 32]
        {
            file_key.zeroize();
            intent_fingerprint.zeroize();
            artifact_digest.zeroize();
            remote_task_id.zeroize();
            task_fingerprint.zeroize();
            course_resource_id.zeroize();
            unit_id.zeroize();
            group_id.zeroize();
            return Err(ProviderError::new(
                ProviderErrorKind::RemoteChanged,
                "UAI recovered upload grant state is invalid",
            ));
        }
        Ok(Self {
            token,
            file_key,
            intent_fingerprint,
            artifact_digest,
            remote_task_id,
            task_fingerprint,
            course_resource_id,
            unit_id,
            group_id,
            upload_position,
            grant_request_digest,
            grant_response_digest,
        })
    }
}

impl fmt::Debug for UaiUploadGrant {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UaiUploadGrant")
            .field("token", &"[REDACTED]")
            .field("file_key", &"[ROUTE]")
            .field("intent_fingerprint", &self.intent_fingerprint)
            .field("artifact_digest", &self.artifact_digest)
            .field("remote_task_id", &self.remote_task_id)
            .field("task_fingerprint", &self.task_fingerprint)
            .field("course_resource_id", &self.course_resource_id)
            .field("unit_id", &self.unit_id)
            .field("group_id", &self.group_id)
            .field("upload_position", &self.upload_position)
            .field("grant_request_digest", &"[HASHED]")
            .field("grant_response_digest", &"[HASHED]")
            .finish()
    }
}

impl Drop for UaiUploadGrant {
    fn drop(&mut self) {
        self.file_key.zeroize();
        self.intent_fingerprint.zeroize();
        self.artifact_digest.zeroize();
        self.remote_task_id.zeroize();
        self.task_fingerprint.zeroize();
        self.course_resource_id.zeroize();
        self.unit_id.zeroize();
        self.group_id.zeroize();
        self.grant_request_digest.zeroize();
        self.grant_response_digest.zeroize();
    }
}

/// Definite accepted Qiniu response bound to the exact granted object key.
pub struct UaiUploadObjectResult {
    file_key: String,
    object_hash: Option<Zeroizing<String>>,
    response_digest: [u8; 32],
}

impl UaiUploadObjectResult {
    pub fn file_key(&self) -> &str {
        &self.file_key
    }

    pub fn object_hash(&self) -> Option<&str> {
        self.object_hash.as_deref().map(String::as_str)
    }

    pub const fn response_digest(&self) -> [u8; 32] {
        self.response_digest
    }

    fn into_file_key(mut self) -> String {
        std::mem::take(&mut self.file_key)
    }
}

impl fmt::Debug for UaiUploadObjectResult {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UaiUploadObjectResult")
            .field("file_key", &"[ROUTE]")
            .field(
                "object_hash",
                &self.object_hash.as_ref().map(|_| "[REDACTED]"),
            )
            .field("response_digest", &"[HASHED]")
            .finish()
    }
}

impl Drop for UaiUploadObjectResult {
    fn drop(&mut self) {
        self.file_key.zeroize();
        self.response_digest.zeroize();
    }
}

/// Exact object-store result retaining the immutable Task/module/artifact
/// binding required by the final Provider plan and future shared Draft.
pub struct UaiUploadedArtifact {
    remote_task_id: String,
    task_fingerprint: String,
    course_resource_id: String,
    unit_id: String,
    group_id: String,
    upload_position: u32,
    file_key: String,
    artifact_digest: String,
    intent_fingerprint: String,
    object_request_digest: [u8; 32],
    object_response_digest: [u8; 32],
    object_hash: Option<Zeroizing<String>>,
}

impl UaiUploadedArtifact {
    pub fn remote_task_id(&self) -> &str {
        &self.remote_task_id
    }

    pub fn group_id(&self) -> &str {
        &self.group_id
    }

    pub fn unit_id(&self) -> &str {
        &self.unit_id
    }

    pub const fn upload_position(&self) -> u32 {
        self.upload_position
    }

    pub fn file_key(&self) -> &str {
        &self.file_key
    }

    pub fn artifact_digest(&self) -> &str {
        &self.artifact_digest
    }

    pub fn intent_fingerprint(&self) -> &str {
        &self.intent_fingerprint
    }

    pub fn course_resource_id(&self) -> &str {
        &self.course_resource_id
    }

    pub(crate) fn task_fingerprint(&self) -> &str {
        &self.task_fingerprint
    }

    pub const fn object_request_digest(&self) -> [u8; 32] {
        self.object_request_digest
    }

    pub const fn object_response_digest(&self) -> [u8; 32] {
        self.object_response_digest
    }

    pub fn object_hash(&self) -> Option<&str> {
        self.object_hash.as_deref().map(String::as_str)
    }

    #[cfg(test)]
    pub(crate) fn from_grant(
        grant: &UaiUploadGrant,
        returned_file_key: String,
    ) -> ProviderResult<Self> {
        if returned_file_key != grant.file_key {
            return Err(ProviderError::new(
                ProviderErrorKind::RemoteChanged,
                "UAI uploaded artifact changed the granted object key",
            ));
        }
        Ok(Self {
            remote_task_id: grant.remote_task_id.clone(),
            task_fingerprint: grant.task_fingerprint.clone(),
            course_resource_id: grant.course_resource_id.clone(),
            unit_id: grant.unit_id.clone(),
            group_id: grant.group_id.clone(),
            upload_position: grant.upload_position,
            file_key: returned_file_key,
            artifact_digest: grant.artifact_digest.clone(),
            intent_fingerprint: grant.intent_fingerprint.clone(),
            object_request_digest: [0; 32],
            object_response_digest: [0; 32],
            object_hash: None,
        })
    }

    pub(crate) fn from_object_result(
        grant: &UaiUploadGrant,
        multipart: &UaiMultipartUpload,
        result: &UaiUploadObjectResult,
    ) -> ProviderResult<Self> {
        if result.file_key() != grant.file_key()
            || multipart.request_digest() == [0; 32]
            || result.response_digest() == [0; 32]
        {
            return Err(ProviderError::new(
                ProviderErrorKind::RemoteChanged,
                "UAI object result is foreign to the upload grant",
            ));
        }
        Ok(Self {
            remote_task_id: grant.remote_task_id.clone(),
            task_fingerprint: grant.task_fingerprint.clone(),
            course_resource_id: grant.course_resource_id.clone(),
            unit_id: grant.unit_id.clone(),
            group_id: grant.group_id.clone(),
            upload_position: grant.upload_position,
            file_key: result.file_key.clone(),
            artifact_digest: grant.artifact_digest.clone(),
            intent_fingerprint: grant.intent_fingerprint.clone(),
            object_request_digest: multipart.request_digest(),
            object_response_digest: result.response_digest(),
            object_hash: result.object_hash.clone(),
        })
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "the encrypted object state restores every independent upload binding"
    )]
    pub(crate) fn restore_object_state(
        mut remote_task_id: String,
        mut task_fingerprint: String,
        mut course_resource_id: String,
        mut unit_id: String,
        mut group_id: String,
        upload_position: u32,
        mut file_key: String,
        mut artifact_digest: String,
        mut intent_fingerprint: String,
        object_request_digest: [u8; 32],
        object_response_digest: [u8; 32],
        object_hash: Option<Zeroizing<String>>,
    ) -> ProviderResult<Self> {
        let expected_remote_task_id = format!("group:{course_resource_id}:{unit_id}:{group_id}");
        let valid_hash = object_hash.as_deref().is_none_or(|value| {
            !value.trim().is_empty()
                && value.len() <= MAX_UPLOAD_HASH_BYTES
                && !value.chars().any(char::is_control)
        });
        if !is_remote_component(&course_resource_id)
            || !is_remote_component(&unit_id)
            || !is_remote_component(&group_id)
            || remote_task_id != expected_remote_task_id
            || remote_task_id.len() > 512
            || !valid_private_binding(&task_fingerprint, "v1:", 512)
            || !(1..=2).contains(&upload_position)
            || file_key.is_empty()
            || file_key.len() > MAX_UPLOAD_KEY_BYTES
            || file_key.chars().any(char::is_control)
            || !valid_private_binding(&artifact_digest, "sha256:", 256)
            || !valid_private_binding(&intent_fingerprint, "uai-upload-v1:", 256)
            || object_request_digest == [0; 32]
            || object_response_digest == [0; 32]
            || !valid_hash
        {
            remote_task_id.zeroize();
            task_fingerprint.zeroize();
            course_resource_id.zeroize();
            unit_id.zeroize();
            group_id.zeroize();
            file_key.zeroize();
            artifact_digest.zeroize();
            intent_fingerprint.zeroize();
            return Err(ProviderError::new(
                ProviderErrorKind::RemoteChanged,
                "UAI recovered object-upload state is invalid",
            ));
        }
        Ok(Self {
            remote_task_id,
            task_fingerprint,
            course_resource_id,
            unit_id,
            group_id,
            upload_position,
            file_key,
            artifact_digest,
            intent_fingerprint,
            object_request_digest,
            object_response_digest,
            object_hash,
        })
    }

    #[cfg(test)]
    pub(crate) fn fixture(upload_position: u32) -> Self {
        Self {
            remote_task_id: "group:2001:unit-1:group-upload".to_owned(),
            task_fingerprint: "v1:compound-upload".to_owned(),
            course_resource_id: "2001".to_owned(),
            unit_id: "unit-1".to_owned(),
            group_id: "group-upload".to_owned(),
            upload_position,
            file_key: "course/42/nothing.mp3".to_owned(),
            artifact_digest: "sha256:synthetic-artifact".to_owned(),
            intent_fingerprint: "uai-upload-v1:synthetic-intent".to_owned(),
            object_request_digest: [3; 32],
            object_response_digest: [4; 32],
            object_hash: Some(Zeroizing::new("synthetic-qiniu-etag".to_owned())),
        }
    }
}

impl fmt::Debug for UaiUploadedArtifact {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UaiUploadedArtifact")
            .field("remote_task_id", &self.remote_task_id)
            .field("task_fingerprint", &self.task_fingerprint)
            .field("course_resource_id", &self.course_resource_id)
            .field("unit_id", &self.unit_id)
            .field("group_id", &self.group_id)
            .field("upload_position", &self.upload_position)
            .field("file_key", &"[ROUTE]")
            .field("artifact_digest", &self.artifact_digest)
            .field("intent_fingerprint", &self.intent_fingerprint)
            .field("object_request_digest", &"[HASHED]")
            .field("object_response_digest", &"[HASHED]")
            .field(
                "object_hash",
                &self.object_hash.as_ref().map(|_| "[REDACTED]"),
            )
            .finish()
    }
}

impl Drop for UaiUploadedArtifact {
    fn drop(&mut self) {
        self.remote_task_id.zeroize();
        self.task_fingerprint.zeroize();
        self.course_resource_id.zeroize();
        self.unit_id.zeroize();
        self.group_id.zeroize();
        self.file_key.zeroize();
        self.artifact_digest.zeroize();
        self.intent_fingerprint.zeroize();
        self.object_request_digest.zeroize();
        self.object_response_digest.zeroize();
    }
}

/// Fresh Task-bound intent for exactly one audited upload module.
#[derive(Clone, Eq, PartialEq)]
pub struct UaiUploadIntent {
    remote_task_id: String,
    task_fingerprint: String,
    course_resource_id: String,
    unit_id: String,
    group_id: String,
    upload_position: u32,
    artifact_digest: String,
    fingerprint: String,
}

impl UaiUploadIntent {
    pub fn remote_task_id(&self) -> &str {
        &self.remote_task_id
    }

    pub fn course_resource_id(&self) -> &str {
        &self.course_resource_id
    }

    pub fn group_id(&self) -> &str {
        &self.group_id
    }

    pub fn unit_id(&self) -> &str {
        &self.unit_id
    }

    pub const fn upload_position(&self) -> u32 {
        self.upload_position
    }

    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    pub(crate) fn matches_artifact(&self, artifact: &UaiUploadArtifact) -> bool {
        self.artifact_digest == artifact.digest()
    }
}

impl fmt::Debug for UaiUploadIntent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UaiUploadIntent")
            .field("remote_task_id", &self.remote_task_id)
            .field("task_fingerprint", &self.task_fingerprint)
            .field("course_resource_id", &self.course_resource_id)
            .field("unit_id", &self.unit_id)
            .field("group_id", &self.group_id)
            .field("upload_position", &self.upload_position)
            .field("artifact_digest", &self.artifact_digest)
            .field("fingerprint", &self.fingerprint)
            .finish()
    }
}

/// Provider-private fresh Task gate for upload grant acquisition.
#[derive(Clone)]
pub struct UaiUploadPreparation {
    details: Arc<dyn TaskDetailCapability>,
}

impl UaiUploadPreparation {
    pub fn new(details: Arc<dyn TaskDetailCapability>) -> Self {
        Self { details }
    }

    /// Re-discovers the exact Group and freezes its unique upload module plus
    /// the selected artifact digest before either remote upload stage.
    ///
    /// # Errors
    ///
    /// Returns a typed error when the Task changes, lacks a unique audited
    /// upload position, or no longer matches its stable identity.
    pub async fn prepare_intent(
        &self,
        context: &ProviderContext,
        remote_task_id: &str,
        artifact: &UaiUploadArtifact,
    ) -> ProviderResult<UaiUploadIntent> {
        let detail = self.details.task_detail(context, remote_task_id).await?;
        build_upload_intent(&detail, remote_task_id, artifact)
    }

    /// Re-discovers the exact single upload Group after object storage has
    /// accepted the artifact, then freezes the one native submission plan.
    /// Compound upload Groups deliberately require the shared Artifact plus
    /// Submission Draft contract so their other answers remain atomic.
    ///
    /// # Errors
    ///
    /// Returns a typed error when the Task, fingerprint, upload position,
    /// Course publish version or artifact binding changed.
    pub async fn prepare_submission(
        &self,
        context: &ProviderContext,
        uploaded: &UaiUploadedArtifact,
    ) -> ProviderResult<UaiUploadSubmission> {
        let detail = self
            .details
            .task_detail(context, uploaded.remote_task_id())
            .await?;
        build_upload_submission(&detail, uploaded)
    }
}

impl fmt::Debug for UaiUploadPreparation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UaiUploadPreparation")
            .field("details", &"configured")
            .finish()
    }
}

/// Provider-private immutable final mutation plan for one single
/// `multiFileUpload` Group. It carries no account credential or Course-instance
/// route; the native transport resolves both immediately before mutation.
pub struct UaiUploadSubmission {
    remote_task_id: String,
    course_resource_id: String,
    unit_id: String,
    group_id: String,
    file_key: String,
    artifact_digest: String,
    upload_intent_fingerprint: String,
    course_publish_version: u64,
    fingerprint: String,
}

impl UaiUploadSubmission {
    pub fn remote_task_id(&self) -> &str {
        &self.remote_task_id
    }

    pub fn course_resource_id(&self) -> &str {
        &self.course_resource_id
    }

    pub fn unit_id(&self) -> &str {
        &self.unit_id
    }

    pub fn group_id(&self) -> &str {
        &self.group_id
    }

    pub fn artifact_digest(&self) -> &str {
        &self.artifact_digest
    }

    pub fn upload_intent_fingerprint(&self) -> &str {
        &self.upload_intent_fingerprint
    }

    pub const fn course_publish_version(&self) -> u64 {
        self.course_publish_version
    }

    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    pub(crate) fn final_sequence_binding_digest(&self) -> [u8; 32] {
        let mut digest = Sha256::new();
        for field in [
            b"asterism:uai:upload-final-sequence-binding:v1".as_slice(),
            self.remote_task_id.as_bytes(),
            self.course_resource_id.as_bytes(),
            self.unit_id.as_bytes(),
            self.group_id.as_bytes(),
            self.file_key.as_bytes(),
            self.artifact_digest.as_bytes(),
            self.upload_intent_fingerprint.as_bytes(),
            self.fingerprint.as_bytes(),
        ] {
            digest.update(field);
            digest.update(b"\0");
        }
        digest.update(self.course_publish_version.to_be_bytes());
        digest.finalize().into()
    }

    pub(crate) fn expose_file_key(&self) -> &str {
        &self.file_key
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "the encrypted final-plan state restores every independent immutable binding"
    )]
    pub(crate) fn restore_final_plan(
        mut remote_task_id: String,
        mut course_resource_id: String,
        mut unit_id: String,
        mut group_id: String,
        mut file_key: String,
        mut artifact_digest: String,
        mut upload_intent_fingerprint: String,
        course_publish_version: u64,
        mut fingerprint: String,
    ) -> ProviderResult<Self> {
        let expected_remote_task_id = format!("group:{course_resource_id}:{unit_id}:{group_id}");
        if !is_remote_component(&course_resource_id)
            || !is_remote_component(&unit_id)
            || !is_remote_component(&group_id)
            || remote_task_id != expected_remote_task_id
            || remote_task_id.len() > 512
            || file_key.is_empty()
            || file_key.len() > MAX_UPLOAD_KEY_BYTES
            || file_key.chars().any(char::is_control)
            || !valid_private_binding(&artifact_digest, "sha256:", 256)
            || !valid_private_binding(&upload_intent_fingerprint, "uai-upload-v1:", 256)
            || !valid_private_binding(&fingerprint, "uai-upload-submit-v1:", 256)
            || course_publish_version == 0
            || i64::try_from(course_publish_version).is_err()
        {
            remote_task_id.zeroize();
            course_resource_id.zeroize();
            unit_id.zeroize();
            group_id.zeroize();
            file_key.zeroize();
            artifact_digest.zeroize();
            upload_intent_fingerprint.zeroize();
            fingerprint.zeroize();
            return Err(ProviderError::new(
                ProviderErrorKind::RemoteChanged,
                "UAI recovered single-upload final plan is invalid",
            ));
        }
        Ok(Self {
            remote_task_id,
            course_resource_id,
            unit_id,
            group_id,
            file_key,
            artifact_digest,
            upload_intent_fingerprint,
            course_publish_version,
            fingerprint,
        })
    }

    #[cfg(test)]
    pub(crate) fn fixture(file_key: &str, fingerprint_suffix: &str) -> Self {
        Self {
            remote_task_id: "group:2001:unit-1:group-upload".to_owned(),
            course_resource_id: "2001".to_owned(),
            unit_id: "unit-1".to_owned(),
            group_id: "group-upload".to_owned(),
            file_key: file_key.to_owned(),
            artifact_digest: "sha256:synthetic-artifact".to_owned(),
            upload_intent_fingerprint: "uai-upload-v1:synthetic-intent".to_owned(),
            course_publish_version: 123_290,
            fingerprint: format!("uai-upload-submit-v1:{fingerprint_suffix}"),
        }
    }
}

impl fmt::Debug for UaiUploadSubmission {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UaiUploadSubmission")
            .field("remote_task_id", &self.remote_task_id)
            .field("course_resource_id", &self.course_resource_id)
            .field("unit_id", &self.unit_id)
            .field("group_id", &self.group_id)
            .field("file_key", &"[ROUTE]")
            .field("artifact_digest", &self.artifact_digest)
            .field("upload_intent_fingerprint", &self.upload_intent_fingerprint)
            .field("course_publish_version", &self.course_publish_version)
            .field("fingerprint", &self.fingerprint)
            .finish()
    }
}

impl Drop for UaiUploadSubmission {
    fn drop(&mut self) {
        self.remote_task_id.zeroize();
        self.course_resource_id.zeroize();
        self.unit_id.zeroize();
        self.group_id.zeroize();
        self.file_key.zeroize();
        self.artifact_digest.zeroize();
        self.upload_intent_fingerprint.zeroize();
        self.fingerprint.zeroize();
    }
}

/// Exact receipt-versioned confirmation that the remote user-module retains
/// the immutable uploaded object key. It deliberately does not claim Task
/// completion; fresh progress remains a separate authority.
pub struct UaiUploadVerification {
    remote_task_id: String,
    artifact_digest: String,
    submission_version: String,
    result_digest: [u8; 32],
    score: Option<SubmissionScore>,
    policy: Option<UaiSubmissionPolicyEvidence>,
    verified_at: Timestamp,
}

impl UaiUploadVerification {
    pub fn remote_task_id(&self) -> &str {
        &self.remote_task_id
    }

    pub fn artifact_digest(&self) -> &str {
        &self.artifact_digest
    }

    pub fn submission_version(&self) -> &str {
        &self.submission_version
    }

    /// Exact digest of the accepted receipt-versioned user-module readback.
    pub const fn result_digest(&self) -> [u8; 32] {
        self.result_digest
    }

    pub const fn score(&self) -> Option<SubmissionScore> {
        self.score
    }

    pub const fn policy(&self) -> Option<&UaiSubmissionPolicyEvidence> {
        self.policy.as_ref()
    }

    pub const fn verified_at(&self) -> Timestamp {
        self.verified_at
    }

    pub const fn requires_fresh_progress_read(&self) -> bool {
        true
    }
}

impl fmt::Debug for UaiUploadVerification {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UaiUploadVerification")
            .field("remote_task_id", &self.remote_task_id)
            .field("artifact_digest", &self.artifact_digest)
            .field("submission_version", &self.submission_version)
            .field("result_digest", &"[HASHED]")
            .field("score", &self.score)
            .field("policy", &self.policy)
            .field("verified_at", &self.verified_at)
            .finish()
    }
}

impl Drop for UaiUploadVerification {
    fn drop(&mut self) {
        self.remote_task_id.zeroize();
        self.artifact_digest.zeroize();
        self.submission_version.zeroize();
        self.result_digest.zeroize();
    }
}

/// Bounded Provider-owned artifact bytes. Core must eventually supply these
/// through an owner/account/Task/Draft-bound artifact handle.
pub struct UaiUploadArtifact {
    filename: String,
    media_type: String,
    bytes: Zeroizing<Vec<u8>>,
}

impl UaiUploadArtifact {
    /// Freezes one bounded artifact accepted by the audited upload route.
    ///
    /// # Errors
    ///
    /// Rejects empty/oversized bytes and unsafe file metadata.
    pub fn try_new(filename: &str, media_type: &str, bytes: Vec<u8>) -> ProviderResult<Self> {
        if bytes.is_empty() || bytes.len() > MAX_UPLOAD_ARTIFACT_BYTES {
            return Err(invalid_input(
                "UAI upload artifact has an invalid bounded size",
            ));
        }
        if filename.is_empty()
            || filename.len() > 128
            || !filename
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        {
            return Err(invalid_input("UAI upload artifact filename is invalid"));
        }
        if media_type != "audio/mpeg" {
            return Err(invalid_input(
                "UAI audited upload artifact must use audio/mpeg",
            ));
        }
        Ok(Self {
            filename: filename.to_owned(),
            media_type: media_type.to_owned(),
            bytes: Zeroizing::new(bytes),
        })
    }

    /// Builds the exact donor placeholder MP3 used by the optional empty-file
    /// upload capability.
    pub fn donor_minimal_mp3() -> Self {
        let mut bytes = vec![0_u8; MINIMAL_MP3_BYTES];
        bytes[..4].copy_from_slice(&[0xff, 0xfb, 0x90, 0x00]);
        Self {
            filename: "nothing.mp3".to_owned(),
            media_type: "audio/mpeg".to_owned(),
            bytes: Zeroizing::new(bytes),
        }
    }

    pub fn filename(&self) -> &str {
        &self.filename
    }

    pub fn media_type(&self) -> &str {
        &self.media_type
    }

    pub fn expose_bytes(&self) -> &[u8] {
        self.bytes.as_slice()
    }

    pub fn digest(&self) -> String {
        let mut digest = Sha256::new();
        digest.update(b"asterism:uai:upload-artifact:v1\0");
        digest.update(self.filename.as_bytes());
        digest.update(b"\0");
        digest.update(self.media_type.as_bytes());
        digest.update(b"\0");
        digest.update(self.bytes.as_slice());
        format!("sha256:{:x}", digest.finalize())
    }
}

impl fmt::Debug for UaiUploadArtifact {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UaiUploadArtifact")
            .field("filename", &self.filename)
            .field("media_type", &self.media_type)
            .field("byte_length", &self.bytes.len())
            .finish()
    }
}

impl Drop for UaiUploadArtifact {
    fn drop(&mut self) {
        self.filename.zeroize();
        self.media_type.zeroize();
    }
}

/// Exact fresh CMS grant request for one bound upload intent and account.
/// Its URL remains redacted while the digest can be registered before dispatch.
pub struct UaiUploadGrantRequest {
    url: Zeroizing<String>,
    referer: &'static str,
    intent_fingerprint: String,
    request_digest: [u8; 32],
}

impl UaiUploadGrantRequest {
    /// Stable identity over method, exact query URL and required Referer.
    pub const fn request_digest(&self) -> [u8; 32] {
        self.request_digest
    }

    pub(crate) fn expose_url(&self) -> &str {
        self.url.as_str()
    }

    pub(crate) const fn referer(&self) -> &'static str {
        self.referer
    }

    pub(crate) fn intent_fingerprint(&self) -> &str {
        &self.intent_fingerprint
    }
}

impl Drop for UaiUploadGrantRequest {
    fn drop(&mut self) {
        self.intent_fingerprint.zeroize();
        self.request_digest.zeroize();
    }
}

impl fmt::Debug for UaiUploadGrantRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UaiUploadGrantRequest")
            .field("url", &"[ACCOUNT-BOUND ROUTE]")
            .field("request_digest", &"[HASHED]")
            .finish()
    }
}

/// Builds the donor-audited CMS grant request only after fresh Course-instance
/// and app-user discovery.
///
/// # Errors
///
/// Rejects a foreign artifact, malformed fresh identity or invalid fixed URL.
pub fn build_upload_grant_request(
    intent: &UaiUploadIntent,
    artifact: &UaiUploadArtifact,
    course_instance_id: &str,
    app_user_id: &str,
) -> ProviderResult<UaiUploadGrantRequest> {
    if !intent.matches_artifact(artifact)
        || !is_remote_component(course_instance_id)
        || !is_account_identity(app_user_id)
    {
        return Err(invalid_input(
            "UAI upload grant request identity or artifact is invalid",
        ));
    }
    let mut url = Url::parse(UAI_UPLOAD_GRANT_ROUTE)
        .map_err(|_| invalid_response("UAI upload grant route is invalid"))?;
    url.query_pairs_mut()
        .append_pair("name", artifact.filename())
        .append_pair("filetype", "audio")
        .append_pair("isconvert", "0")
        .append_pair("courseid", course_instance_id)
        .append_pair("userid", app_user_id);
    let mut digest = Sha256::new();
    digest.update(b"asterism:uai:upload-grant-request:v1\0");
    digest.update(b"GET\0");
    digest.update(url.as_str().as_bytes());
    digest.update(b"\0referer\0");
    digest.update(UAI_UPLOAD_REFERER.as_bytes());
    Ok(UaiUploadGrantRequest {
        url: Zeroizing::new(url.into()),
        referer: UAI_UPLOAD_REFERER,
        intent_fingerprint: intent.fingerprint.clone(),
        request_digest: digest.finalize().into(),
    })
}

/// Complete zeroizing multipart body for the fixed Qiniu upload route.
pub struct UaiMultipartUpload {
    content_type: String,
    body: Zeroizing<Vec<u8>>,
}

impl UaiMultipartUpload {
    pub fn content_type(&self) -> &str {
        &self.content_type
    }

    pub fn expose_body(&self) -> &[u8] {
        self.body.as_slice()
    }

    /// Exact pre-dispatch identity for the fixed Qiniu object-upload request.
    /// It includes method, route, deterministic content type and the complete
    /// multipart body (token, key and artifact bytes) without exposing them.
    pub fn request_digest(&self) -> [u8; 32] {
        let mut digest = Sha256::new();
        digest.update(b"asterism:uai:qiniu-upload-request:v1\0");
        digest.update(b"POST\0");
        digest.update(QINIU_UPLOAD_ROUTE.as_bytes());
        digest.update(b"\0content-type\0");
        digest.update(self.content_type.as_bytes());
        digest.update(b"\0body\0");
        digest.update(self.body.as_slice());
        digest.finalize().into()
    }
}

impl fmt::Debug for UaiMultipartUpload {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UaiMultipartUpload")
            .field("content_type", &self.content_type)
            .field("body", &"[REDACTED]")
            .field("byte_length", &self.body.len())
            .finish()
    }
}

impl Drop for UaiMultipartUpload {
    fn drop(&mut self) {
        self.content_type.zeroize();
    }
}

/// Complete zeroizing final UAI upload-submission request.
pub struct UaiUploadSubmissionRequest {
    url: Zeroizing<String>,
    content_type: &'static str,
    body: Zeroizing<String>,
    sequence_binding_digest: [u8; 32],
    request_digest: [u8; 32],
}

impl UaiUploadSubmissionRequest {
    pub fn expose_body(&self) -> &str {
        self.body.as_str()
    }

    /// Exact pre-dispatch identity over method, route, content type and body.
    pub const fn request_digest(&self) -> [u8; 32] {
        self.request_digest
    }

    pub const fn sequence_binding_digest(&self) -> [u8; 32] {
        self.sequence_binding_digest
    }

    pub(crate) fn expose_url(&self) -> &str {
        self.url.as_str()
    }

    pub(crate) const fn content_type(&self) -> &'static str {
        self.content_type
    }
}

impl Drop for UaiUploadSubmissionRequest {
    fn drop(&mut self) {
        self.sequence_binding_digest.zeroize();
        self.request_digest.zeroize();
    }
}

impl fmt::Debug for UaiUploadSubmissionRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UaiUploadSubmissionRequest")
            .field("url", &"[ROUTE]")
            .field("content_type", &self.content_type)
            .field("body", &"[REDACTED]")
            .field("sequence_binding_digest", &"[HASHED]")
            .field("request_digest", &"[HASHED]")
            .finish()
    }
}

/// Parses one CMS upload grant and retains the token only in zeroizing memory.
///
/// # Errors
///
/// Returns a typed invalid-response or protocol-drift error for rejected,
/// malformed or unbounded responses.
pub fn parse_upload_grant(
    document: &str,
    intent: &UaiUploadIntent,
) -> ProviderResult<UaiUploadGrant> {
    if document.is_empty() || document.len() > MAX_UPLOAD_GRANT_RESPONSE_BYTES {
        return Err(invalid_response(
            "UAI upload grant response is empty or oversized",
        ));
    }
    let root = ZeroizingJsonValue::new(
        serde_json::from_str::<Value>(document)
            .map_err(|_| invalid_response("UAI upload grant response is not valid JSON"))?,
    );
    let root = root
        .as_value()
        .as_object()
        .ok_or_else(|| protocol_drift("UAI upload grant response is not an object"))?;
    if root.get("code").and_then(Value::as_i64) != Some(200) {
        return Err(invalid_response("UAI rejected the upload grant request"));
    }
    let token = required_string(root.get("upToken"), MAX_UPLOAD_TOKEN_BYTES, "upload token")?;
    let file_key = required_string(root.get("fileKey"), MAX_UPLOAD_KEY_BYTES, "upload file key")?;
    if token.chars().any(char::is_control) || file_key.chars().any(char::is_control) {
        return Err(protocol_drift("UAI upload token or file key is unsafe"));
    }
    Ok(UaiUploadGrant {
        token: Zeroizing::new(token),
        file_key,
        intent_fingerprint: intent.fingerprint.clone(),
        artifact_digest: intent.artifact_digest.clone(),
        remote_task_id: intent.remote_task_id.clone(),
        task_fingerprint: intent.task_fingerprint.clone(),
        course_resource_id: intent.course_resource_id.clone(),
        unit_id: intent.unit_id.clone(),
        group_id: intent.group_id.clone(),
        upload_position: intent.upload_position,
        grant_request_digest: [0; 32],
        grant_response_digest: [0; 32],
    })
}

/// Parses one CMS grant and binds it to the exact already-materialized request
/// plus complete response bytes.
///
/// # Errors
///
/// Rejects a request from another upload intent or any malformed grant before
/// retaining its request/response lineage.
pub fn parse_upload_grant_bound(
    document: &str,
    intent: &UaiUploadIntent,
    request: &UaiUploadGrantRequest,
) -> ProviderResult<UaiUploadGrant> {
    if request.intent_fingerprint() != intent.fingerprint() || request.request_digest() == [0; 32] {
        return Err(ProviderError::new(
            ProviderErrorKind::RemoteChanged,
            "UAI upload grant request is foreign to the upload intent",
        ));
    }
    let mut grant = parse_upload_grant(document, intent)?;
    grant.grant_request_digest = request.request_digest();
    grant.grant_response_digest = Sha256::digest(document.as_bytes()).into();
    Ok(grant)
}

/// Builds a bounded multipart body containing only token, exact key and the
/// selected artifact.
///
/// # Errors
///
/// Returns an invalid-input error if the composed multipart body would exceed
/// the artifact bound plus fixed framing overhead.
pub fn build_upload_multipart(
    grant: &UaiUploadGrant,
    artifact: &UaiUploadArtifact,
) -> ProviderResult<UaiMultipartUpload> {
    grant.validate_for_artifact(artifact)?;
    let boundary = upload_boundary(grant.file_key(), artifact.expose_bytes());
    let mut body = Zeroizing::new(Vec::with_capacity(
        artifact.expose_bytes().len().saturating_add(16 * 1_024),
    ));
    append_text_part(&mut body, &boundary, "token", grant.expose_token());
    append_text_part(&mut body, &boundary, "key", grant.file_key());
    body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    body.extend_from_slice(
        format!(
            "Content-Disposition: form-data; name=\"file\"; filename=\"{}\"\r\n",
            artifact.filename()
        )
        .as_bytes(),
    );
    body.extend_from_slice(format!("Content-Type: {}\r\n\r\n", artifact.media_type()).as_bytes());
    body.extend_from_slice(artifact.expose_bytes());
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    if body.len() > MAX_UPLOAD_ARTIFACT_BYTES.saturating_add(16 * 1_024) {
        return Err(invalid_input("UAI multipart upload exceeds the size limit"));
    }
    Ok(UaiMultipartUpload {
        content_type: format!("multipart/form-data; boundary={boundary}"),
        body,
    })
}

/// Parses the object-store response and requires the exact granted file key.
///
/// # Errors
///
/// Returns a typed invalid-response or remote-changed error for malformed,
/// oversized or mismatched responses.
pub fn parse_upload_object_result(
    document: &str,
    expected_file_key: &str,
) -> ProviderResult<UaiUploadObjectResult> {
    if document.is_empty() || document.len() > MAX_UPLOAD_RESPONSE_BYTES {
        return Err(invalid_response(
            "UAI object upload response is empty or oversized",
        ));
    }
    let root = ZeroizingJsonValue::new(
        serde_json::from_str::<Value>(document)
            .map_err(|_| invalid_response("UAI object upload response is not valid JSON"))?,
    );
    let root = root
        .as_value()
        .as_object()
        .ok_or_else(|| protocol_drift("UAI object upload response is not an object"))?;
    let key = root
        .get("key")
        .and_then(Value::as_str)
        .filter(|key| !key.is_empty() && key.len() <= MAX_UPLOAD_KEY_BYTES)
        .ok_or_else(|| protocol_drift("UAI object upload response has no file key"))?;
    if key != expected_file_key {
        return Err(ProviderError::new(
            ProviderErrorKind::RemoteChanged,
            "UAI object upload response changed the granted file key",
        ));
    }
    let object_hash = match root.get("hash") {
        None | Some(Value::Null) => None,
        Some(Value::String(value))
            if !value.trim().is_empty()
                && value.len() <= MAX_UPLOAD_HASH_BYTES
                && !value.chars().any(char::is_control) =>
        {
            Some(Zeroizing::new(value.to_owned()))
        }
        Some(_) => {
            return Err(protocol_drift("UAI object upload response hash is invalid"));
        }
    };
    Ok(UaiUploadObjectResult {
        file_key: key.to_owned(),
        object_hash,
        response_digest: Sha256::digest(document.as_bytes()).into(),
    })
}

/// Compatibility projection retaining only the donor-required object key.
///
/// # Errors
///
/// Applies the complete typed object-result validation before discarding the
/// optional bounded hash and response digest.
pub fn parse_upload_result(document: &str, expected_file_key: &str) -> ProviderResult<String> {
    parse_upload_object_result(document, expected_file_key)
        .map(UaiUploadObjectResult::into_file_key)
}

fn build_upload_intent(
    detail: &asterism_provider_api::RemoteTaskDetail,
    remote_task_id: &str,
    artifact: &UaiUploadArtifact,
) -> ProviderResult<UaiUploadIntent> {
    if detail.task.remote_id != remote_task_id {
        return Err(ProviderError::new(
            ProviderErrorKind::RemoteChanged,
            "UAI fresh upload Task identity changed",
        ));
    }
    let task = detail
        .normalized_detail
        .get("task")
        .and_then(Value::as_object)
        .ok_or_else(|| protocol_drift("UAI fresh upload Task has no normalized detail"))?;
    if task.get("schema").and_then(Value::as_str) != Some("uai.group-task.v1") {
        return Err(protocol_drift(
            "UAI fresh upload Task has an unknown normalized schema",
        ));
    }
    let course_resource_id = safe_remote_component(task.get("course_resource_id"), "Course")?;
    let unit_id = task
        .get("unit")
        .and_then(Value::as_object)
        .and_then(|unit| safe_remote_component(unit.get("id"), "Unit").ok())
        .ok_or_else(|| protocol_drift("UAI fresh upload Task has no valid Unit identity"))?;
    let group_id = safe_remote_component(task.get("group_id"), "Group")?;
    let expected_remote_task_id = format!("group:{course_resource_id}:{unit_id}:{group_id}");
    if expected_remote_task_id != remote_task_id {
        return Err(ProviderError::new(
            ProviderErrorKind::RemoteChanged,
            "UAI fresh upload hierarchy does not match the requested Task",
        ));
    }
    let task_types = task
        .get("task_types")
        .and_then(Value::as_array)
        .ok_or_else(|| protocol_drift("UAI fresh upload Task has no type array"))?;
    let question_count = task
        .get("question_count")
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .filter(|value| *value == task_types.len() && *value > 0)
        .ok_or_else(|| protocol_drift("UAI fresh upload Task type/count shape changed"))?;
    let positions = task_types
        .iter()
        .enumerate()
        .filter_map(|(index, value)| (value.as_str() == Some("multiFileUpload")).then_some(index))
        .collect::<Vec<_>>();
    if positions.len() != 1
        || task_types
            .iter()
            .any(|value| value.as_str().is_none_or(str::is_empty))
    {
        return Err(ProviderError::new(
            ProviderErrorKind::UnsupportedTask,
            "UAI upload requires exactly one audited upload module",
        ));
    }
    let upload_position = u32::try_from(positions[0] + 1)
        .map_err(|_| protocol_drift("UAI upload module position exceeds its bound"))?;
    let artifact_digest = artifact.digest();
    let mut digest = Sha256::new();
    digest.update(b"asterism:uai:upload-intent:v1\0");
    digest.update(remote_task_id.as_bytes());
    digest.update(b"\0");
    digest.update(detail.task.fingerprint.as_bytes());
    digest.update(b"\0");
    digest.update(upload_position.to_be_bytes());
    digest.update(b"\0");
    digest.update(artifact_digest.as_bytes());
    let fingerprint = format!("uai-upload-v1:{:x}", digest.finalize());
    let _ = question_count;
    Ok(UaiUploadIntent {
        remote_task_id: remote_task_id.to_owned(),
        task_fingerprint: detail.task.fingerprint.clone(),
        course_resource_id,
        unit_id,
        group_id,
        upload_position,
        artifact_digest,
        fingerprint,
    })
}

fn build_upload_submission(
    detail: &asterism_provider_api::RemoteTaskDetail,
    uploaded: &UaiUploadedArtifact,
) -> ProviderResult<UaiUploadSubmission> {
    if detail.task.remote_id != uploaded.remote_task_id
        || detail.task.fingerprint != uploaded.task_fingerprint
    {
        return Err(ProviderError::new(
            ProviderErrorKind::RemoteChanged,
            "UAI upload Task changed after object storage",
        ));
    }
    let task = detail
        .normalized_detail
        .get("task")
        .and_then(Value::as_object)
        .ok_or_else(|| protocol_drift("UAI fresh upload submission has no normalized Task"))?;
    if task.get("schema").and_then(Value::as_str) != Some("uai.group-task.v1")
        || task.get("course_resource_id").and_then(Value::as_str)
            != Some(uploaded.course_resource_id.as_str())
        || task
            .get("unit")
            .and_then(Value::as_object)
            .and_then(|unit| unit.get("id"))
            .and_then(Value::as_str)
            != Some(uploaded.unit_id.as_str())
        || task.get("group_id").and_then(Value::as_str) != Some(uploaded.group_id.as_str())
        || uploaded.upload_position != 1
    {
        return Err(ProviderError::new(
            ProviderErrorKind::RemoteChanged,
            "UAI upload submission identity changed after object storage",
        ));
    }
    let task_types = task
        .get("task_types")
        .and_then(Value::as_array)
        .ok_or_else(|| protocol_drift("UAI upload submission has no Task type array"))?;
    if task_types.as_slice() != [Value::String("multiFileUpload".to_owned())]
        || task.get("question_count").and_then(Value::as_u64) != Some(1)
    {
        return Err(ProviderError::new(
            ProviderErrorKind::UnsupportedTask,
            "UAI final upload submission requires one single multiFileUpload module",
        ));
    }
    let course_publish_version = task
        .get("course_publish_version")
        .and_then(Value::as_u64)
        .filter(|value| *value > 0 && i64::try_from(*value).is_ok())
        .ok_or_else(|| {
            protocol_drift("UAI final upload submission has no current Course publish version")
        })?;
    let mut digest = Sha256::new();
    digest.update(b"asterism:uai:upload-submission:v1\0");
    digest.update(uploaded.remote_task_id.as_bytes());
    digest.update(b"\0");
    digest.update(uploaded.task_fingerprint.as_bytes());
    digest.update(b"\0");
    digest.update(uploaded.intent_fingerprint.as_bytes());
    digest.update(b"\0");
    digest.update(uploaded.artifact_digest.as_bytes());
    digest.update(b"\0");
    digest.update(uploaded.file_key.as_bytes());
    digest.update(b"\0");
    digest.update(course_publish_version.to_be_bytes());
    Ok(UaiUploadSubmission {
        remote_task_id: uploaded.remote_task_id.clone(),
        course_resource_id: uploaded.course_resource_id.clone(),
        unit_id: uploaded.unit_id.clone(),
        group_id: uploaded.group_id.clone(),
        file_key: uploaded.file_key.clone(),
        artifact_digest: uploaded.artifact_digest.clone(),
        upload_intent_fingerprint: uploaded.intent_fingerprint.clone(),
        course_publish_version,
        fingerprint: format!("uai-upload-submit-v1:{:x}", digest.finalize()),
    })
}

/// Builds the donor-audited upload request inside the native mutation boundary.
/// The Apache donor provides the `instanceId=0`/file-key child semantics while
/// the current Rust donor provides the minimal question/judge envelope and
/// fresh Course publish-version binding.
///
/// # Errors
///
/// Rejects malformed fresh route/account identities or an oversized body.
pub fn build_upload_submission_request(
    submission: &UaiUploadSubmission,
    course_instance_id: &str,
    open_id: &str,
) -> ProviderResult<UaiUploadSubmissionRequest> {
    if !is_remote_component(course_instance_id)
        || open_id.is_empty()
        || open_id.len() > 8 * 1_024
        || open_id.chars().any(char::is_control)
    {
        return Err(invalid_input(
            "UAI upload submission route identity is invalid",
        ));
    }
    let inner_answer = Zeroizing::new(
        serde_json::to_string(&serde_json::json!({
            "value": [],
            "children": [{
                "value": [submission.expose_file_key()],
                "isDone": true,
                "isRight": true,
                "replyCategory": "objective",
            }],
            "progress": {},
            "record": {"url": ""},
        }))
        .map_err(|_| invalid_response("UAI upload answer cannot be serialized"))?,
    );
    let judges = Zeroizing::new(
        serde_json::to_string(&[serde_json::json!({
            "value": submission.expose_file_key(),
            "question_type": "multiFileUpload",
            "reply_type": "multiFileUpload",
            "versions": {
                "course": submission.course_publish_version,
                "group": 1,
                "template": 1,
                "answer": 3,
                "content": 0,
            },
            "payloads": [],
        })])
        .map_err(|_| invalid_response("UAI upload judge cannot be serialized"))?,
    );
    let body = ZeroizingJsonValue::new(serde_json::json!({
        "quesDatas": [{
            "instanceId": "0",
            "answer": inner_answer.as_str(),
            "context": "{\"state\":\"submitted\"}",
            "contextVersion": 1,
            "answerVersion": 1,
        }],
        "groupId": submission.group_id,
        "isCompleted": [true],
        "thirdPartyJudges": judges.as_str(),
        "submitType": 1,
        "hideLoading": false,
        "associationGroupId": "",
        "courseId": course_instance_id,
        "openId": open_id,
        "version": "default",
    }));
    let encoded = Zeroizing::new(
        serde_json::to_string(body.as_value())
            .map_err(|_| invalid_response("UAI upload submission cannot be serialized"))?,
    );
    if encoded.is_empty() || encoded.len() > MAX_UPLOAD_SUBMISSION_BYTES {
        return Err(invalid_response(
            "UAI upload submission body exceeds the size limit",
        ));
    }
    let url = Url::parse(UAI_UPLOAD_SUBMISSION_ROUTE)
        .map_err(|_| invalid_response("UAI upload submission route is invalid"))?;
    let mut digest = Sha256::new();
    digest.update(b"asterism:uai:upload-submission-request:v1\0");
    digest.update(b"POST\0");
    digest.update(url.as_str().as_bytes());
    digest.update(b"\0content-type\0");
    digest.update(UAI_UPLOAD_SUBMISSION_CONTENT_TYPE.as_bytes());
    digest.update(b"\0body\0");
    digest.update(encoded.as_bytes());
    Ok(UaiUploadSubmissionRequest {
        url: Zeroizing::new(url.into()),
        content_type: UAI_UPLOAD_SUBMISSION_CONTENT_TYPE,
        body: encoded,
        sequence_binding_digest: submission.final_sequence_binding_digest(),
        request_digest: digest.finalize().into(),
    })
}

/// Verifies one uploaded key through the exact receipt-versioned user-module
/// readback. This proves answer persistence only, not Group completion.
///
/// # Errors
///
/// Rejects an invalid receipt, malformed/foreign route state, any non-zero or
/// duplicate module identity, incomplete child or changed object key.
pub fn parse_upload_verification(
    document: &str,
    submission: &UaiUploadSubmission,
    receipt: &SubmissionReceipt,
) -> ProviderResult<UaiUploadVerification> {
    receipt
        .validate()
        .map_err(|_| invalid_response("UAI upload verification receipt is invalid"))?;
    let version = receipt
        .provider_trace_id
        .as_deref()
        .filter(|value| receipt.remote_status == "accepted" && valid_submission_version(value))
        .ok_or_else(|| {
            invalid_response("UAI upload verification requires an accepted version receipt")
        })?;
    if document.is_empty() || document.len() > MAX_UPLOAD_VERIFICATION_BYTES {
        return Err(invalid_response(
            "UAI upload verification response is empty or oversized",
        ));
    }
    let response = ZeroizingJsonValue::new(
        serde_json::from_str(document)
            .map_err(|_| invalid_response("UAI upload verification response is not valid JSON"))?,
    );
    let result_digest = Sha256::digest(document.as_bytes()).into();
    let state = bound_verification_state(response.as_value(), submission.group_id(), version)?;
    let score = verified_submission_score(state)?;
    validate_upload_question_data(state, submission.expose_file_key())?;
    let policy = verified_submission_policy(state, submission.group_id(), version, result_digest)?;
    Ok(UaiUploadVerification {
        remote_task_id: submission.remote_task_id.clone(),
        artifact_digest: submission.artifact_digest.clone(),
        submission_version: version.to_owned(),
        result_digest,
        score,
        policy,
        verified_at: Utc::now(),
    })
}

fn validate_upload_question_data(
    state: &serde_json::Map<String, Value>,
    expected_file_key: &str,
) -> ProviderResult<()> {
    let question_data = state
        .get("quesData")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty() && value.len() <= MAX_UPLOAD_VERIFICATION_BYTES)
        .ok_or_else(|| protocol_drift("UAI upload verification has no bounded Question data"))?;
    let questions = ZeroizingJsonValue::new(
        serde_json::from_str(question_data)
            .map_err(|_| invalid_response("UAI upload Question readback is not valid JSON"))?,
    );
    let entries = questions
        .as_value()
        .as_array()
        .filter(|entries| entries.len() == 1)
        .ok_or_else(|| {
            ProviderError::new(
                ProviderErrorKind::RemoteChanged,
                "UAI upload readback does not contain one exact module",
            )
        })?;
    let entry = entries[0]
        .as_object()
        .ok_or_else(|| protocol_drift("UAI upload readback module is not an object"))?;
    let instance_is_zero = match entry.get("instanceId") {
        Some(Value::String(value)) => value == "0",
        Some(Value::Number(value)) => value.as_u64() == Some(0),
        _ => false,
    };
    if !instance_is_zero {
        return Err(ProviderError::new(
            ProviderErrorKind::RemoteChanged,
            "UAI upload readback module identity changed",
        ));
    }
    validate_upload_readback_entry(entry, expected_file_key)
}

fn validate_upload_readback_entry(
    entry: &serde_json::Map<String, Value>,
    expected_file_key: &str,
) -> ProviderResult<()> {
    let context = entry
        .get("context")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty() && value.len() <= 64 * 1_024)
        .ok_or_else(|| protocol_drift("UAI upload readback has no bounded context"))?;
    let context = ZeroizingJsonValue::new(
        serde_json::from_str(context)
            .map_err(|_| invalid_response("UAI upload readback context is not valid JSON"))?,
    );
    if context.as_value().get("state").and_then(Value::as_str) != Some("submitted") {
        return Err(ProviderError::new(
            ProviderErrorKind::RemoteChanged,
            "UAI upload readback is not submitted",
        ));
    }
    let answer = entry
        .get("answer")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty() && value.len() <= MAX_UPLOAD_NESTED_ANSWER_BYTES)
        .ok_or_else(|| protocol_drift("UAI upload readback has no bounded answer"))?;
    let answer = ZeroizingJsonValue::new(
        serde_json::from_str(answer)
            .map_err(|_| invalid_response("UAI upload readback answer is not valid JSON"))?,
    );
    let children = answer
        .as_value()
        .get("children")
        .and_then(Value::as_array)
        .filter(|children| children.len() == 1)
        .ok_or_else(|| {
            ProviderError::new(
                ProviderErrorKind::RemoteChanged,
                "UAI upload readback does not contain one exact answer child",
            )
        })?;
    let child = children[0]
        .as_object()
        .ok_or_else(|| protocol_drift("UAI upload readback child is not an object"))?;
    let values = child
        .get("value")
        .and_then(Value::as_array)
        .filter(|values| values.len() == 1)
        .ok_or_else(|| protocol_drift("UAI upload readback child has no exact value"))?;
    if child.get("isDone").and_then(Value::as_bool) != Some(true)
        || values[0].as_str() != Some(expected_file_key)
    {
        return Err(ProviderError::new(
            ProviderErrorKind::RemoteChanged,
            "UAI upload readback differs from the immutable uploaded artifact",
        ));
    }
    Ok(())
}

pub(crate) fn validate_upload_readback_question(
    entry: &Value,
    expected_file_key: &str,
) -> ProviderResult<()> {
    let entry = entry
        .as_object()
        .ok_or_else(|| protocol_drift("UAI upload readback module is not an object"))?;
    let instance_is_zero = match entry.get("instanceId") {
        Some(Value::String(value)) => value == "0",
        Some(Value::Number(value)) => value.as_u64() == Some(0),
        _ => false,
    };
    if !instance_is_zero {
        return Err(ProviderError::new(
            ProviderErrorKind::RemoteChanged,
            "UAI upload readback module identity changed",
        ));
    }
    validate_upload_readback_entry(entry, expected_file_key)
}

fn safe_remote_component(value: Option<&Value>, label: &'static str) -> ProviderResult<String> {
    value
        .and_then(Value::as_str)
        .filter(|value| is_remote_component(value))
        .map(str::to_owned)
        .ok_or_else(|| {
            protocol_drift(format!(
                "UAI fresh upload Task has no valid {label} identity"
            ))
        })
}

fn is_remote_component(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn valid_private_binding(value: &str, prefix: &str, maximum: usize) -> bool {
    value.starts_with(prefix)
        && value.len() > prefix.len()
        && value.len() <= maximum
        && value.trim() == value
        && !value.chars().any(char::is_control)
}

fn is_account_identity(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 512
        && value.trim() == value
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
}

fn upload_boundary(file_key: &str, bytes: &[u8]) -> String {
    let mut digest = Sha256::new();
    digest.update(b"asterism:uai:upload-boundary:v1\0");
    digest.update(file_key.as_bytes());
    digest.update(bytes.len().to_le_bytes());
    format!("----asterism-uai-{:x}", digest.finalize())
}

fn append_text_part(body: &mut Vec<u8>, boundary: &str, name: &str, value: &str) {
    body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    body.extend_from_slice(
        format!("Content-Disposition: form-data; name=\"{name}\"\r\n\r\n").as_bytes(),
    );
    body.extend_from_slice(value.as_bytes());
    body.extend_from_slice(b"\r\n");
}

fn required_string(
    value: Option<&Value>,
    maximum_bytes: usize,
    label: &'static str,
) -> ProviderResult<String> {
    value
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty() && value.len() <= maximum_bytes)
        .map(str::to_owned)
        .ok_or_else(|| protocol_drift(format!("UAI upload grant has no valid {label}")))
}

fn invalid_input(message: impl Into<String>) -> ProviderError {
    ProviderError::new(ProviderErrorKind::InvalidResponse, message)
}

fn invalid_response(message: impl Into<String>) -> ProviderError {
    ProviderError::new(ProviderErrorKind::InvalidResponse, message)
}

fn protocol_drift(message: impl Into<String>) -> ProviderError {
    ProviderError::new(ProviderErrorKind::ProtocolDrift, message)
}

#[cfg(test)]
mod tests {
    use asterism_domain::{AssessmentClass, RemoteState, SourceType, TaskCapability};
    use asterism_provider_api::{RemoteTask, RemoteTaskDetail};

    use super::*;

    #[test]
    fn upload_intent_is_fresh_task_position_and_artifact_bound() {
        let artifact = UaiUploadArtifact::donor_minimal_mp3();
        let detail = upload_detail(&["multichoice", "multiFileUpload"]);
        let intent =
            build_upload_intent(&detail, "group:2001:unit-1:group-upload", &artifact).unwrap();
        assert_eq!(intent.upload_position(), 2);
        assert_eq!(intent.course_resource_id(), "2001");
        assert_eq!(intent.group_id(), "group-upload");
        assert!(intent.matches_artifact(&artifact));

        let duplicate = upload_detail(&["multiFileUpload", "multiFileUpload"]);
        assert!(
            build_upload_intent(&duplicate, "group:2001:unit-1:group-upload", &artifact,).is_err()
        );
        assert!(
            build_upload_intent(&detail, "group:other:unit-1:group-upload", &artifact).is_err()
        );
    }

    #[test]
    fn upload_grant_is_bounded_and_redacted() {
        let artifact = UaiUploadArtifact::donor_minimal_mp3();
        let intent = fixture_intent(&artifact);
        let request =
            build_upload_grant_request(&intent, &artifact, "course-instance-1", "app-user-1")
                .unwrap();
        let request_digest = request.request_digest();
        assert_ne!(request_digest, [0; 32]);
        assert_eq!(request.request_digest(), request_digest);
        assert_eq!(
            request.expose_url(),
            "https://ucontent.unipus.cn/media/user_resource/cms/token?name=nothing.mp3&filetype=audio&isconvert=0&courseid=course-instance-1&userid=app-user-1"
        );
        assert!(!format!("{request:?}").contains("app-user-1"));
        assert_ne!(
            build_upload_grant_request(&intent, &artifact, "course-instance-1", "app-user-2",)
                .unwrap()
                .request_digest(),
            request_digest
        );
        assert!(
            build_upload_grant_request(
                &intent,
                &artifact,
                "course-instance-1",
                "tenant:app-user.3",
            )
            .is_ok()
        );
        assert!(
            build_upload_grant_request(&intent, &artifact, "unsafe/course", "app-user-1").is_err()
        );
        assert!(
            build_upload_grant_request(&intent, &artifact, "course-instance-1", "unsafe/user")
                .is_err()
        );
        let foreign_artifact = UaiUploadArtifact::try_new(
            "different.mp3",
            "audio/mpeg",
            vec![7_u8; MINIMAL_MP3_BYTES],
        )
        .unwrap();
        assert!(
            build_upload_grant_request(
                &intent,
                &foreign_artifact,
                "course-instance-1",
                "app-user-1",
            )
            .is_err()
        );
        let document =
            r#"{"code":200,"upToken":"secret-upload-token","fileKey":"course/42/nothing.mp3"}"#;
        let grant = parse_upload_grant_bound(document, &intent, &request).unwrap();
        assert_eq!(grant.expose_token(), "secret-upload-token");
        assert_eq!(grant.file_key(), "course/42/nothing.mp3");
        assert_eq!(grant.grant_request_digest(), request_digest);
        assert_eq!(
            grant.grant_response_digest(),
            <[u8; 32]>::from(Sha256::digest(document.as_bytes()))
        );
        assert!(!format!("{grant:?}").contains("secret-upload-token"));
        let foreign_intent = build_upload_intent(
            &upload_detail(&["multiFileUpload"]),
            "group:2001:unit-1:group-upload",
            &foreign_artifact,
        )
        .unwrap();
        let foreign_request = build_upload_grant_request(
            &foreign_intent,
            &foreign_artifact,
            "course-instance-1",
            "app-user-1",
        )
        .unwrap();
        assert!(parse_upload_grant_bound(document, &intent, &foreign_request).is_err());
        assert!(parse_upload_grant(r#"{"code":500}"#, &intent).is_err());
    }

    #[test]
    fn donor_minimal_mp3_builds_one_secret_zeroizing_multipart_body() {
        let artifact = UaiUploadArtifact::donor_minimal_mp3();
        let intent = fixture_intent(&artifact);
        let grant = parse_upload_grant(
            r#"{"code":200,"upToken":"secret-upload-token","fileKey":"course/42/nothing.mp3"}"#,
            &intent,
        )
        .unwrap();
        assert_eq!(artifact.expose_bytes().len(), MINIMAL_MP3_BYTES);
        assert_eq!(&artifact.expose_bytes()[..4], &[0xff, 0xfb, 0x90, 0x00]);
        let multipart = build_upload_multipart(&grant, &artifact).unwrap();
        let request_digest = multipart.request_digest();
        assert_ne!(request_digest, [0; 32]);
        assert_eq!(multipart.request_digest(), request_digest);
        assert!(
            multipart
                .content_type()
                .starts_with("multipart/form-data; boundary=")
        );
        let body = multipart.expose_body();
        assert!(
            body.windows(b"secret-upload-token".len())
                .any(|window| window == b"secret-upload-token")
        );
        assert!(
            body.windows(grant.file_key().len())
                .any(|window| window == grant.file_key().as_bytes())
        );
        assert!(!format!("{multipart:?}").contains("secret-upload-token"));
        let rotated_grant = parse_upload_grant(
            r#"{"code":200,"upToken":"rotated-upload-token","fileKey":"course/42/nothing.mp3"}"#,
            &intent,
        )
        .unwrap();
        assert_ne!(
            build_upload_multipart(&rotated_grant, &artifact)
                .unwrap()
                .request_digest(),
            request_digest
        );

        let foreign_artifact = UaiUploadArtifact::try_new(
            "different.mp3",
            "audio/mpeg",
            vec![7_u8; MINIMAL_MP3_BYTES],
        )
        .unwrap();
        assert!(build_upload_multipart(&grant, &foreign_artifact).is_err());
    }

    #[test]
    fn object_upload_result_must_repeat_the_exact_granted_key() {
        let document = r#"{"hash":"synthetic-qiniu-etag","key":"course/42/nothing.mp3"}"#;
        let result = parse_upload_object_result(document, "course/42/nothing.mp3").unwrap();
        assert_eq!(result.file_key(), "course/42/nothing.mp3");
        assert_eq!(result.object_hash(), Some("synthetic-qiniu-etag"));
        assert_eq!(
            result.response_digest(),
            <[u8; 32]>::from(Sha256::digest(document.as_bytes()))
        );
        assert!(!format!("{result:?}").contains("synthetic-qiniu-etag"));
        assert_eq!(
            parse_upload_result(
                r#"{"key":"course/42/nothing.mp3"}"#,
                "course/42/nothing.mp3"
            )
            .unwrap(),
            "course/42/nothing.mp3"
        );
        assert!(
            parse_upload_result(r#"{"key":"other/file.mp3"}"#, "course/42/nothing.mp3").is_err()
        );
        assert!(
            parse_upload_object_result(
                r#"{"hash":"","key":"course/42/nothing.mp3"}"#,
                "course/42/nothing.mp3",
            )
            .is_err()
        );
        assert!(
            parse_upload_object_result(
                r#"{"hash":7,"key":"course/42/nothing.mp3"}"#,
                "course/42/nothing.mp3",
            )
            .is_err()
        );
    }

    #[test]
    fn uploaded_artifact_retains_task_module_and_artifact_binding() {
        let artifact = UaiUploadArtifact::donor_minimal_mp3();
        let intent = fixture_intent(&artifact);
        let grant = parse_upload_grant(
            r#"{"code":200,"upToken":"secret-upload-token","fileKey":"course/42/nothing.mp3"}"#,
            &intent,
        )
        .unwrap();
        let multipart = build_upload_multipart(&grant, &artifact).unwrap();
        let object_result = parse_upload_object_result(
            r#"{"hash":"synthetic-qiniu-etag","key":"course/42/nothing.mp3"}"#,
            grant.file_key(),
        )
        .unwrap();
        let uploaded =
            UaiUploadedArtifact::from_object_result(&grant, &multipart, &object_result).unwrap();
        assert_eq!(uploaded.remote_task_id(), intent.remote_task_id());
        assert_eq!(uploaded.group_id(), intent.group_id());
        assert_eq!(uploaded.upload_position(), intent.upload_position());
        assert_eq!(uploaded.file_key(), grant.file_key());
        assert_eq!(uploaded.artifact_digest(), artifact.digest());
        assert_eq!(uploaded.intent_fingerprint(), intent.fingerprint());
        assert_eq!(uploaded.object_request_digest(), multipart.request_digest());
        assert_eq!(
            uploaded.object_response_digest(),
            object_result.response_digest()
        );
        assert_eq!(uploaded.object_hash(), Some("synthetic-qiniu-etag"));
        assert!(!format!("{uploaded:?}").contains("course/42/nothing.mp3"));
        assert!(UaiUploadedArtifact::from_grant(&grant, "other/key.mp3".to_owned()).is_err());
    }

    #[test]
    fn single_upload_builds_one_fresh_publish_bound_submission() {
        let artifact = UaiUploadArtifact::donor_minimal_mp3();
        let detail = upload_detail(&["multiFileUpload"]);
        let intent =
            build_upload_intent(&detail, "group:2001:unit-1:group-upload", &artifact).unwrap();
        let grant = parse_upload_grant(
            r#"{"code":200,"upToken":"secret-upload-token","fileKey":"course/42/nothing.mp3"}"#,
            &intent,
        )
        .unwrap();
        let uploaded =
            UaiUploadedArtifact::from_grant(&grant, "course/42/nothing.mp3".to_owned()).unwrap();
        let submission = build_upload_submission(&detail, &uploaded).unwrap();
        assert_eq!(submission.unit_id(), "unit-1");
        assert_eq!(submission.course_publish_version(), 123_290);
        assert_eq!(submission.artifact_digest(), artifact.digest());
        assert_eq!(submission.upload_intent_fingerprint(), intent.fingerprint());
        assert!(
            submission
                .fingerprint()
                .starts_with("uai-upload-submit-v1:")
        );
        assert!(!format!("{submission:?}").contains("course/42/nothing.mp3"));

        let request =
            build_upload_submission_request(&submission, "course-instance-1", "openid-1").unwrap();
        let request_digest = request.request_digest();
        assert_ne!(request_digest, [0; 32]);
        assert_eq!(request.request_digest(), request_digest);
        assert_eq!(
            request.expose_url(),
            "https://ucontent.unipus.cn/course/api/v3/newExploration/submit"
        );
        assert_eq!(request.content_type(), "application/json; charset=utf-8");
        assert!(!format!("{request:?}").contains("openid-1"));
        assert_ne!(
            build_upload_submission_request(&submission, "course-instance-1", "openid-2")
                .unwrap()
                .request_digest(),
            request_digest
        );
        let parsed: Value = serde_json::from_str(request.expose_body()).unwrap();
        assert_eq!(parsed["groupId"], "group-upload");
        assert_eq!(parsed["quesDatas"][0]["instanceId"], "0");
        assert_eq!(parsed["isCompleted"], serde_json::json!([true]));
        let answer: Value =
            serde_json::from_str(parsed["quesDatas"][0]["answer"].as_str().unwrap()).unwrap();
        assert_eq!(
            answer["children"][0]["value"],
            serde_json::json!(["course/42/nothing.mp3"])
        );
        let judges: Value =
            serde_json::from_str(parsed["thirdPartyJudges"].as_str().unwrap()).unwrap();
        assert_eq!(judges[0]["question_type"], "multiFileUpload");
        assert_eq!(judges[0]["versions"]["course"], 123_290);

        let compound_detail = upload_detail(&["multichoice", "multiFileUpload"]);
        let compound_intent = build_upload_intent(
            &compound_detail,
            "group:2001:unit-1:group-upload",
            &artifact,
        )
        .unwrap();
        let compound_grant = parse_upload_grant(
            r#"{"code":200,"upToken":"secret-upload-token","fileKey":"course/42/nothing.mp3"}"#,
            &compound_intent,
        )
        .unwrap();
        let compound_uploaded =
            UaiUploadedArtifact::from_grant(&compound_grant, compound_grant.file_key().to_owned())
                .unwrap();
        assert!(build_upload_submission(&compound_detail, &compound_uploaded).is_err());
    }

    #[test]
    fn receipt_versioned_upload_readback_requires_exact_completed_key() {
        let submission = fixture_submission();
        let receipt = SubmissionReceipt {
            remote_status: "accepted".to_owned(),
            message_sanitized: Some("synthetic accepted upload".to_owned()),
            provider_trace_id: Some("upload-v1".to_owned()),
            received_at: Utc::now(),
        };
        let answer = serde_json::json!({
            "value": [],
            "children": [{"value": [submission.expose_file_key()], "isDone": true}],
            "progress": {},
            "record": {"url": ""},
        })
        .to_string();
        let questions = serde_json::json!([{
            "instanceId": "0",
            "answer": answer,
            "context": "{\"state\":\"submitted\"}",
        }])
        .to_string();
        let document = serde_json::json!({
            "success": true,
            "code": 0,
            "data": {
                "course": "course-instance-1",
                "module": "group-upload-upload-v1",
                "state": {
                    "version": "upload-v1",
                    "quesData": questions,
                    "__EXTEND_DATA__": {"__SUBMIT_INFO__": {
                        "course_id": "course-instance-1",
                        "group_id": "group-upload",
                        "version": "upload-v1"
                    }}
                }
            }
        })
        .to_string();
        let verified = parse_upload_verification(&document, &submission, &receipt).unwrap();
        assert_eq!(verified.remote_task_id(), submission.remote_task_id());
        assert_eq!(verified.artifact_digest(), submission.artifact_digest());
        assert_eq!(verified.submission_version(), "upload-v1");
        assert_eq!(
            verified.result_digest(),
            <[u8; 32]>::from(Sha256::digest(document.as_bytes()))
        );
        assert_eq!(verified.score(), None);
        assert!(verified.policy().is_none());
        assert!(verified.requires_fresh_progress_read());
        assert!(!format!("{verified:?}").contains(submission.expose_file_key()));

        assert_upload_policy_evidence(&document, &submission, &receipt);

        assert!(
            parse_upload_verification(
                &document.replace("course/42/nothing.mp3", "other/key.mp3"),
                &submission,
                &receipt,
            )
            .is_err()
        );
        assert!(
            parse_upload_verification(
                &document.replace(
                    "\\\"instanceId\\\":\\\"0\\\"",
                    "\\\"instanceId\\\":\\\"1\\\"",
                ),
                &submission,
                &receipt,
            )
            .is_err()
        );
        let mut receipt_without_version = receipt;
        receipt_without_version.provider_trace_id = None;
        assert!(
            parse_upload_verification(&document, &submission, &receipt_without_version).is_err()
        );
    }

    fn assert_upload_policy_evidence(
        document: &str,
        submission: &UaiUploadSubmission,
        receipt: &SubmissionReceipt,
    ) {
        let mut policy_document: Value = serde_json::from_str(document).unwrap();
        let submit_info = policy_document["data"]["state"]["__EXTEND_DATA__"]["__SUBMIT_INFO__"]
            .as_object_mut()
            .unwrap();
        submit_info.insert("strategyId".to_owned(), serde_json::json!(3001));
        submit_info.insert(
            "strategy".to_owned(),
            serde_json::json!({
                "endTime": 1_790_812_800,
                "record_every_submit": false,
                "record_max_submit": true,
                "required": true,
                "startTime": 1_785_542_400,
                "task_mini_score_pct": "60.000",
            }),
        );
        submit_info.insert(
            "state".to_owned(),
            serde_json::json!({
                "expired": false,
                "lastSubmit": 1_786_752_000,
                "not_start": false,
            }),
        );
        let policy_document = serde_json::to_string(&policy_document).unwrap();
        let verified = parse_upload_verification(&policy_document, submission, receipt).unwrap();
        assert_eq!(
            verified.result_digest(),
            <[u8; 32]>::from(Sha256::digest(policy_document.as_bytes()))
        );
        let policy = verified.policy().unwrap();
        assert_eq!(policy.group_id(), "group-upload");
        assert_eq!(policy.submission_version(), "upload-v1");
        assert_eq!(policy.strategy_id(), 3001);
        assert!(policy.required());
        assert!(!policy.record_every_submit());
        assert!(policy.record_max_submit());
        assert_eq!(policy.task_minimum_score_milli_percent(), 60_000);
        assert_eq!(policy.opens_at().unwrap().timestamp(), 1_785_542_400);
        assert_eq!(policy.closes_at().unwrap().timestamp(), 1_790_812_800);
        assert_eq!(policy.last_submit_at().unwrap().timestamp(), 1_786_752_000);
        assert!(!policy.submit_expired());
        assert!(!policy.submit_not_started());
        assert_eq!(
            policy.result_digest(),
            <[u8; 32]>::from(Sha256::digest(policy_document.as_bytes()))
        );
        let debug = format!("{policy:?}");
        assert!(!debug.contains("group-upload"));
        assert!(!debug.contains("upload-v1"));

        let mut partial_policy: Value = serde_json::from_str(&policy_document).unwrap();
        partial_policy["data"]["state"]["__EXTEND_DATA__"]["__SUBMIT_INFO__"]
            .as_object_mut()
            .unwrap()
            .remove("strategy");
        assert!(
            parse_upload_verification(
                &serde_json::to_string(&partial_policy).unwrap(),
                submission,
                receipt,
            )
            .is_err()
        );
    }

    fn fixture_intent(artifact: &UaiUploadArtifact) -> UaiUploadIntent {
        let artifact_digest = artifact.digest();
        UaiUploadIntent {
            remote_task_id: "group:2001:unit-1:group-upload".to_owned(),
            task_fingerprint: "v1:upload".to_owned(),
            course_resource_id: "2001".to_owned(),
            unit_id: "unit-1".to_owned(),
            group_id: "group-upload".to_owned(),
            upload_position: 1,
            artifact_digest,
            fingerprint: "uai-upload-v1:fixture".to_owned(),
        }
    }

    fn fixture_submission() -> UaiUploadSubmission {
        let artifact = UaiUploadArtifact::donor_minimal_mp3();
        let detail = upload_detail(&["multiFileUpload"]);
        let intent =
            build_upload_intent(&detail, "group:2001:unit-1:group-upload", &artifact).unwrap();
        let grant = parse_upload_grant(
            r#"{"code":200,"upToken":"secret-upload-token","fileKey":"course/42/nothing.mp3"}"#,
            &intent,
        )
        .unwrap();
        let uploaded =
            UaiUploadedArtifact::from_grant(&grant, "course/42/nothing.mp3".to_owned()).unwrap();
        build_upload_submission(&detail, &uploaded).unwrap()
    }

    fn upload_detail(task_types: &[&str]) -> RemoteTaskDetail {
        let normalized = serde_json::json!({
            "schema": "uai.group-task.v1",
            "course_resource_id": "2001",
            "unit": {"id": "unit-1", "title": "Unit 1"},
            "section": {"id": "section-1", "title": "Section 1"},
            "micro": {"id": "micro-1", "title": "Speaking"},
            "group_id": "group-upload",
            "course_publish_version": 123_290,
            "task_types": task_types,
            "question_count": task_types.len(),
        });
        RemoteTaskDetail {
            task: RemoteTask {
                remote_id: "group:2001:unit-1:group-upload".to_owned(),
                course_remote_id: Some("course-resource:2001".to_owned()),
                title: "Upload recording".to_owned(),
                source_type: SourceType::Resource,
                assessment_class: AssessmentClass::Routine,
                remote_state: RemoteState::Unknown,
                opens_at: None,
                due_at: None,
                closes_at: None,
                capabilities: vec![TaskCapability::BrowserBridge],
                fingerprint: "v1:upload".to_owned(),
                normalized: normalized.clone(),
                raw_sanitized: serde_json::json!({"schema":"uai.group-task.raw.v1"}),
            },
            normalized_detail: serde_json::json!({
                "schema": "uai.group-task-detail.v1",
                "task": normalized,
            }),
        }
    }
}
