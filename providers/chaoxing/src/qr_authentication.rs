use std::{collections::BTreeMap, fmt};

use asterism_domain::{AuthSessionId, ProviderAccountId, Timestamp};
use asterism_networking::{ResolvedNetworkProfile, build_http_client};
use asterism_provider_api::{
    ProviderAuthContext, ProviderError, ProviderErrorKind, ProviderInteractiveAuthContinuation,
    ProviderResult, ResolvedProviderInteractiveAuthContinuation,
};
use asterism_secrets::{SecretString, SecretValue};
use async_trait::async_trait;
use chrono::{Duration, Utc};
use reqwest::{
    Client, Response, StatusCode, Url,
    header::{
        ACCEPT, CONTENT_TYPE, COOKIE, HeaderMap, HeaderValue, RETRY_AFTER, SET_COOKIE, USER_AGENT,
    },
};
use scraper::{Html, Selector};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zeroize::{Zeroize, Zeroizing};

use crate::{
    ChaoxingCookieSession,
    metadata::PROVIDER_ID,
    native_http::{classify_reqwest_error, fetch_course_list_html},
};

const QR_LOGIN_PAGE: &str = "https://passport2.chaoxing.com/login";
const QR_ACTIVATE_ENDPOINT: &str = "https://passport2.chaoxing.com/createqr";
const QR_POLL_ENDPOINT: &str = "https://passport2.chaoxing.com/getauthstatus";
const QR_CHALLENGE_ENDPOINT: &str = "https://passport2.chaoxing.com/toauthlogin";
const COURSE_LIST_ENDPOINT: &str = "https://mooc2-ans.chaoxing.com/mooc2-ans/visit/courselistdata";
const QR_WEB_USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) \
AppleWebKit/537.36 (KHTML, like Gecko) Chrome/107.0.0.0 Safari/537.36 Edg/107.0.1418.35";
const MAX_QR_HTML_BYTES: usize = 512 * 1_024;
const MAX_QR_ACTIVATION_BYTES: usize = 1024 * 1_024;
const MAX_QR_JSON_BYTES: usize = 64 * 1_024;
const MAX_QR_SECRET_BYTES: usize = 2_048;
const MAX_QR_ACTION_BYTES: usize = 4_096;
const MAX_QR_POLLS: u32 = 900;
const QR_TTL_SECONDS: u64 = 15 * 60;
const QR_TTL_DURATION_SECONDS: i64 = 15 * 60;
pub(crate) const QR_CONTINUATION_TYPE: &str = "chaoxing.qr.v1";
pub(crate) const QR_PHASE_AWAITING_SCAN: &str = "chaoxing.qr.awaiting-scan";
pub(crate) const QR_PHASE_AWAITING_CONFIRMATION: &str = "chaoxing.qr.awaiting-confirmation";
pub(crate) const QR_PHASE_IDENTITY_VALIDATION: &str = "chaoxing.qr.identity-validation";
pub(crate) const QR_PHASE_AUTHENTICATED: &str = "chaoxing.qr.authenticated";
const MAX_COOKIE_HEADERS: usize = 128;
const MAX_COOKIE_ENTRIES: usize = 256;
const MAX_COOKIE_HEADER_BYTES: usize = 64 * 1_024;
const MAX_SET_COOKIE_BYTES: usize = 16 * 1_024;
const MAX_COOKIE_NAME_BYTES: usize = 256;
const MAX_COOKIE_VALUE_BYTES: usize = 8 * 1_024;
const MAX_COOKIE_PATH_BYTES: usize = 1024;
const QR_CONTINUATION_SCHEMA: &str = "chaoxing.qr-continuation.v1";

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum QrContinuationPhase {
    AwaitingScan,
    AwaitingConfirmation,
    IdentityValidation,
    Authenticated,
}

impl QrContinuationPhase {
    const fn label(self) -> &'static str {
        match self {
            Self::AwaitingScan => QR_PHASE_AWAITING_SCAN,
            Self::AwaitingConfirmation => QR_PHASE_AWAITING_CONFIRMATION,
            Self::IdentityValidation => QR_PHASE_IDENTITY_VALIDATION,
            Self::Authenticated => QR_PHASE_AUTHENTICATED,
        }
    }

    const fn is_waiting(self) -> bool {
        matches!(
            self,
            Self::AwaitingScan | Self::AwaitingConfirmation | Self::IdentityValidation
        )
    }
}

/// One Core-bound QR challenge. All Provider challenge and Cookie material is
/// secret-owned, redacted in diagnostics and zeroized on drop.
pub struct ChaoxingQrChallenge {
    account_id: ProviderAccountId,
    auth_session_id: AuthSessionId,
    correlation_digest: [u8; 32],
    uuid: SecretString,
    enc: SecretString,
    challenge_url: SecretString,
    cookies: SensitiveCookieJar,
    continuation_revision: u32,
    poll_count: u32,
    claimed_poll_sequence: Option<u32>,
    phase: QrContinuationPhase,
    expires_at: Timestamp,
    begin_evidence_digest: [u8; 32],
    identity_validation_digest: Option<[u8; 32]>,
    authenticated_at: Option<Timestamp>,
}

impl ChaoxingQrChallenge {
    /// Returns the QR payload through an explicit secret boundary. The caller
    /// may render it to the authorized user but must not persist or log it.
    pub const fn challenge_url(&self) -> &SecretString {
        &self.challenge_url
    }

    pub const fn expires_at(&self) -> Timestamp {
        self.expires_at
    }

    pub(crate) fn to_provider_continuation(
        &self,
    ) -> ProviderResult<ProviderInteractiveAuthContinuation> {
        let mut encoded = self.encode()?;
        ProviderInteractiveAuthContinuation::try_new(
            &crate::metadata::development_metadata()?.id,
            QR_CONTINUATION_TYPE,
            self.phase.label(),
            SecretValue::new(std::mem::take(&mut *encoded)),
            QR_TTL_SECONDS,
            MAX_QR_POLLS,
        )
    }

    pub(crate) fn decode_bound(
        context: &ProviderAuthContext,
        continuation: &ResolvedProviderInteractiveAuthContinuation<'_>,
    ) -> ProviderResult<Self> {
        validate_auth_context(context)?;
        let value_digest: [u8; 32] = Sha256::digest(continuation.value.expose_secret()).into();
        if continuation.continuation_type != QR_CONTINUATION_TYPE
            || continuation.continuation_digest != value_digest
        {
            return Err(qr_binding_error());
        }
        let mut wire =
            serde_json::from_slice::<QrContinuationWire>(continuation.value.expose_secret())
                .map_err(|_| qr_binding_error())?;
        let mut challenge = wire.take_challenge()?;
        let poll_sequence_matches = if challenge.phase.is_waiting() {
            continuation.poll_sequence > challenge.poll_count
                && continuation.poll_sequence <= MAX_QR_POLLS
        } else {
            continuation.poll_sequence == challenge.poll_count
        };
        if continuation.phase != challenge.phase.label()
            || continuation.revision != challenge.continuation_revision
            || !poll_sequence_matches
            || challenge.account_id != context.account_id
            || Some(challenge.auth_session_id) != context.auth_session_id
            || challenge.correlation_digest != correlation_digest(&context.correlation_id)?
            || (challenge.phase.is_waiting() && challenge.expires_at <= Utc::now())
        {
            return Err(qr_binding_error());
        }
        if challenge.phase.is_waiting() {
            challenge.claimed_poll_sequence = Some(continuation.poll_sequence);
        }
        Ok(challenge)
    }

    fn validate_binding(&self, context: &ProviderAuthContext) -> ProviderResult<()> {
        let auth_session_id = context.auth_session_id.ok_or_else(qr_binding_error)?;
        if context.provider_id.as_str() != PROVIDER_ID
            || context.account_id != self.account_id
            || auth_session_id != self.auth_session_id
            || correlation_digest(&context.correlation_id)? != self.correlation_digest
            || !self.phase.is_waiting()
            || self.expires_at <= Utc::now()
            || self.poll_count >= MAX_QR_POLLS
            || self
                .claimed_poll_sequence
                .is_none_or(|sequence| sequence <= self.poll_count || sequence > MAX_QR_POLLS)
        {
            return Err(qr_binding_error());
        }
        Ok(())
    }

    fn begin_poll(&mut self, context: &ProviderAuthContext) -> ProviderResult<()> {
        self.validate_binding(context)?;
        self.poll_count = self
            .claimed_poll_sequence
            .take()
            .ok_or_else(qr_binding_error)?;
        self.continuation_revision = self
            .continuation_revision
            .checked_add(1)
            .ok_or_else(qr_binding_error)?;
        Ok(())
    }

    fn encode(&self) -> ProviderResult<Zeroizing<Vec<u8>>> {
        let wire = QrContinuationWireRef::from_challenge(self)?;
        serde_json::to_vec(&wire)
            .map(Zeroizing::new)
            .map_err(|_| invalid_qr_response("Chaoxing QR continuation could not be encoded"))
    }

    fn continuation_digest(&self) -> ProviderResult<[u8; 32]> {
        Ok(Sha256::digest(self.encode()?.as_slice()).into())
    }

    fn poll_result_digest(
        &self,
        consumed_continuation_digest: [u8; 32],
        state: QrPollState,
        response_digest: [u8; 32],
    ) -> ProviderResult<[u8; 32]> {
        let cookie_digest = self.cookies.digest()?;
        let identity_digest = self.identity_validation_digest.unwrap_or([0; 32]);
        Ok(hash_fields(
            b"chaoxing.qr.poll-result.v1",
            &[
                consumed_continuation_digest.as_slice(),
                self.continuation_revision.to_be_bytes().as_slice(),
                self.poll_count.to_be_bytes().as_slice(),
                state.label().as_bytes(),
                response_digest.as_slice(),
                cookie_digest.as_slice(),
                identity_digest.as_slice(),
            ],
        ))
    }

    pub(crate) fn authenticated_session(&self) -> ProviderResult<ChaoxingCookieSession> {
        if self.phase != QrContinuationPhase::Authenticated
            || self.identity_validation_digest.is_none()
            || self.authenticated_at.is_none()
        {
            return Err(qr_binding_error());
        }
        self.scoped_course_session()
    }

    fn scoped_course_session(&self) -> ProviderResult<ChaoxingCookieSession> {
        let cookie = self
            .cookies
            .header_for(&static_url(COURSE_LIST_ENDPOINT)?)?
            .ok_or_else(qr_binding_error)?;
        ChaoxingCookieSession::try_new(cookie.as_str()).map_err(|_| qr_binding_error())
    }

    pub(crate) fn authenticated_at(&self) -> ProviderResult<Timestamp> {
        self.authenticated_at.ok_or_else(qr_binding_error)
    }
}

#[derive(Serialize)]
struct QrContinuationWireRef<'a> {
    schema: &'static str,
    account_id: ProviderAccountId,
    auth_session_id: AuthSessionId,
    correlation_digest: [u8; 32],
    uuid: &'a str,
    enc: &'a str,
    cookies: Vec<QrCookieWireRef<'a>>,
    continuation_revision: u32,
    poll_count: u32,
    phase: QrContinuationPhase,
    expires_at: Timestamp,
    begin_evidence_digest: [u8; 32],
    identity_validation_digest: Option<[u8; 32]>,
    authenticated_at: Option<Timestamp>,
}

impl<'a> QrContinuationWireRef<'a> {
    fn from_challenge(challenge: &'a ChaoxingQrChallenge) -> ProviderResult<Self> {
        Ok(Self {
            schema: QR_CONTINUATION_SCHEMA,
            account_id: challenge.account_id,
            auth_session_id: challenge.auth_session_id,
            correlation_digest: challenge.correlation_digest,
            uuid: challenge.uuid.expose_secret(),
            enc: challenge.enc.expose_secret(),
            cookies: challenge.cookies.wire_entries()?,
            continuation_revision: challenge.continuation_revision,
            poll_count: challenge.poll_count,
            phase: challenge.phase,
            expires_at: challenge.expires_at,
            begin_evidence_digest: challenge.begin_evidence_digest,
            identity_validation_digest: challenge.identity_validation_digest,
            authenticated_at: challenge.authenticated_at,
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct QrContinuationWire {
    schema: String,
    account_id: ProviderAccountId,
    auth_session_id: AuthSessionId,
    correlation_digest: [u8; 32],
    uuid: String,
    enc: String,
    cookies: Vec<QrCookieWire>,
    continuation_revision: u32,
    poll_count: u32,
    phase: QrContinuationPhase,
    expires_at: Timestamp,
    begin_evidence_digest: [u8; 32],
    identity_validation_digest: Option<[u8; 32]>,
    authenticated_at: Option<Timestamp>,
}

impl QrContinuationWire {
    fn take_challenge(&mut self) -> ProviderResult<ChaoxingQrChallenge> {
        if self.schema != QR_CONTINUATION_SCHEMA
            || !valid_qr_secret(&self.uuid)
            || !valid_qr_secret(&self.enc)
            || self.uuid == self.enc
            || self.continuation_revision == 0
            || self.continuation_revision > self.poll_count.saturating_add(1)
            || self.poll_count > MAX_QR_POLLS
            || self.begin_evidence_digest == [0; 32]
            || match self.phase {
                QrContinuationPhase::AwaitingScan
                | QrContinuationPhase::AwaitingConfirmation
                | QrContinuationPhase::IdentityValidation => {
                    self.poll_count >= MAX_QR_POLLS
                        || self.identity_validation_digest.is_some()
                        || self.authenticated_at.is_some()
                }
                QrContinuationPhase::Authenticated => {
                    self.poll_count == 0
                        || self
                            .identity_validation_digest
                            .is_none_or(|digest| digest == [0; 32])
                        || self.authenticated_at.is_none_or(|at| at > self.expires_at)
                }
            }
        {
            return Err(qr_binding_error());
        }
        let uuid = SecretString::new(std::mem::take(&mut self.uuid));
        let enc = SecretString::new(std::mem::take(&mut self.enc));
        let challenge_url = build_challenge_url(&uuid, &enc)?;
        Ok(ChaoxingQrChallenge {
            account_id: self.account_id,
            auth_session_id: self.auth_session_id,
            correlation_digest: self.correlation_digest,
            uuid,
            enc,
            challenge_url,
            cookies: SensitiveCookieJar::take_wire_entries(&mut self.cookies)?,
            continuation_revision: self.continuation_revision,
            poll_count: self.poll_count,
            claimed_poll_sequence: None,
            phase: self.phase,
            expires_at: self.expires_at,
            begin_evidence_digest: self.begin_evidence_digest,
            identity_validation_digest: self.identity_validation_digest,
            authenticated_at: self.authenticated_at,
        })
    }
}

impl Drop for QrContinuationWire {
    fn drop(&mut self) {
        self.schema.zeroize();
        self.uuid.zeroize();
        self.enc.zeroize();
        self.correlation_digest.zeroize();
        self.begin_evidence_digest.zeroize();
        self.identity_validation_digest.zeroize();
    }
}

impl fmt::Debug for ChaoxingQrChallenge {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ChaoxingQrChallenge")
            .field("account_id", &self.account_id)
            .field("auth_session_id", &self.auth_session_id)
            .field("challenge", &"[REDACTED]")
            .field("cookie_count", &self.cookies.len())
            .field("continuation_revision", &self.continuation_revision)
            .field("poll_count", &self.poll_count)
            .field("phase", &self.phase)
            .field("expires_at", &self.expires_at)
            .field(
                "identity_validation_digest",
                &self.identity_validation_digest.map(|_| "[HASHED]"),
            )
            .finish_non_exhaustive()
    }
}

/// One typed result from exactly one QR status poll.
pub enum ChaoxingQrPollOutcome {
    AwaitingScan {
        result_digest: [u8; 32],
    },
    AwaitingConfirmation {
        result_digest: [u8; 32],
    },
    IdentityValidation {
        result_digest: [u8; 32],
    },
    Authenticated {
        session: ChaoxingCookieSession,
        result_digest: [u8; 32],
    },
    Rejected {
        result_digest: [u8; 32],
    },
    Expired {
        result_digest: [u8; 32],
    },
}

impl ChaoxingQrPollOutcome {
    pub const fn result_digest(&self) -> [u8; 32] {
        match self {
            Self::AwaitingScan { result_digest }
            | Self::AwaitingConfirmation { result_digest }
            | Self::IdentityValidation { result_digest }
            | Self::Authenticated { result_digest, .. }
            | Self::Rejected { result_digest }
            | Self::Expired { result_digest } => *result_digest,
        }
    }
}

impl fmt::Debug for ChaoxingQrPollOutcome {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AwaitingScan { .. } => formatter.write_str("AwaitingScan([HASHED])"),
            Self::AwaitingConfirmation { .. } => {
                formatter.write_str("AwaitingConfirmation([HASHED])")
            }
            Self::IdentityValidation { .. } => formatter.write_str("IdentityValidation([HASHED])"),
            Self::Authenticated { .. } => formatter.write_str("Authenticated([REDACTED])"),
            Self::Rejected { .. } => formatter.write_str("Rejected([HASHED])"),
            Self::Expired { .. } => formatter.write_str("Expired([HASHED])"),
        }
    }
}

/// Provider-owned QR protocol boundary. The caller owns user presentation,
/// polling cadence, cancellation and the eventual atomic credential commit.
#[async_trait]
pub trait ChaoxingQrAuthenticationTransport: Send + Sync {
    async fn begin_qr(&self, context: &ProviderAuthContext) -> ProviderResult<ChaoxingQrChallenge>;

    async fn poll_qr(
        &self,
        context: &ProviderAuthContext,
        challenge: &mut ChaoxingQrChallenge,
    ) -> ProviderResult<ChaoxingQrPollOutcome>;
}

/// Native HTTPS implementation of the audited `CxKitty` QR sequence. It never
/// loops or persists state internally: each `poll_qr` call performs one POST.
pub struct NativeChaoxingQrAuthenticationTransport {
    client: Client,
}

impl NativeChaoxingQrAuthenticationTransport {
    /// Builds the QR transport from the shared non-redirecting network profile.
    ///
    /// # Errors
    ///
    /// Returns a sanitized internal error when the client cannot be built.
    pub fn try_new(network: &ResolvedNetworkProfile) -> ProviderResult<Self> {
        let client = build_http_client(network).map_err(|_| {
            ProviderError::new(
                ProviderErrorKind::Internal,
                "Chaoxing QR HTTP client initialization failed",
            )
        })?;
        Ok(Self { client })
    }
}

impl fmt::Debug for NativeChaoxingQrAuthenticationTransport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NativeChaoxingQrAuthenticationTransport")
            .field("client", &"configured")
            .finish()
    }
}

#[async_trait]
impl ChaoxingQrAuthenticationTransport for NativeChaoxingQrAuthenticationTransport {
    async fn begin_qr(&self, context: &ProviderAuthContext) -> ProviderResult<ChaoxingQrChallenge> {
        validate_auth_context(context)?;
        let login_url = static_url(QR_LOGIN_PAGE)?;
        let response = self
            .client
            .get(login_url)
            .header(USER_AGENT, QR_WEB_USER_AGENT)
            .header(ACCEPT, "text/html,application/xhtml+xml")
            .send()
            .await
            .map_err(|error| classify_reqwest_error(&error))?;
        validate_qr_response_status(&response)?;
        validate_content_type(&response, &["text/html", "application/xhtml+xml"])?;
        let mut cookies = SensitiveCookieJar::default();
        cookies.absorb(response.headers(), response.url())?;
        let login_document = read_utf8_response(response, MAX_QR_HTML_BYTES).await?;
        let login_response_digest = Sha256::digest(login_document.as_bytes()).into();
        let (uuid, enc) = parse_qr_login_document(login_document.as_str())?;

        let mut activate_url = static_url(QR_ACTIVATE_ENDPOINT)?;
        activate_url
            .query_pairs_mut()
            .append_pair("uuid", uuid.expose_secret())
            .append_pair("fid", "-1");
        let request = self
            .client
            .get(activate_url)
            .header(ACCEPT, "image/*,*/*;q=0.8");
        let response = apply_cookie(request, &cookies, QR_ACTIVATE_ENDPOINT)?
            .send()
            .await
            .map_err(|error| classify_reqwest_error(&error))?;
        validate_qr_response_status(&response)?;
        cookies.absorb(response.headers(), response.url())?;
        let activation = read_response_bytes(response, MAX_QR_ACTIVATION_BYTES).await?;
        let activation_response_digest = Sha256::digest(activation.as_slice()).into();

        let challenge_url = build_challenge_url(&uuid, &enc)?;
        let begin_evidence_digest = qr_begin_evidence_digest(
            login_response_digest,
            activation_response_digest,
            cookies.digest()?,
            Sha256::digest(challenge_url.expose_secret().as_bytes()).into(),
        );
        Ok(ChaoxingQrChallenge {
            account_id: context.account_id,
            auth_session_id: context.auth_session_id.ok_or_else(qr_binding_error)?,
            correlation_digest: correlation_digest(&context.correlation_id)?,
            uuid,
            enc,
            challenge_url,
            cookies,
            continuation_revision: 1,
            poll_count: 0,
            claimed_poll_sequence: None,
            phase: QrContinuationPhase::AwaitingScan,
            expires_at: Utc::now() + Duration::seconds(QR_TTL_DURATION_SECONDS),
            begin_evidence_digest,
            identity_validation_digest: None,
            authenticated_at: None,
        })
    }

    async fn poll_qr(
        &self,
        context: &ProviderAuthContext,
        challenge: &mut ChaoxingQrChallenge,
    ) -> ProviderResult<ChaoxingQrPollOutcome> {
        let consumed_continuation_digest = challenge.continuation_digest()?;
        challenge.begin_poll(context)?;
        if challenge.phase == QrContinuationPhase::IdentityValidation {
            let session = challenge.scoped_course_session()?;
            let identity_document = fetch_course_list_html(&self.client, &session, "0").await?;
            let identity_digest: [u8; 32] =
                Sha256::digest(identity_document.as_str().as_bytes()).into();
            challenge.identity_validation_digest = Some(identity_digest);
            challenge.authenticated_at = Some(Utc::now());
            challenge.phase = QrContinuationPhase::Authenticated;
            let result_digest = challenge.poll_result_digest(
                consumed_continuation_digest,
                QrPollState::Authenticated,
                identity_digest,
            )?;
            return Ok(ChaoxingQrPollOutcome::Authenticated {
                session,
                result_digest,
            });
        }
        let request = self
            .client
            .post(static_url(QR_POLL_ENDPOINT)?)
            .header(ACCEPT, "application/json")
            .form(&[
                ("enc", challenge.enc.expose_secret()),
                ("uuid", challenge.uuid.expose_secret()),
            ]);
        let response = apply_cookie(request, &challenge.cookies, QR_POLL_ENDPOINT)?
            .send()
            .await
            .map_err(|error| classify_reqwest_error(&error))?;
        validate_qr_response_status(&response)?;
        validate_content_type(&response, &["application/json", "text/json"])?;
        challenge
            .cookies
            .absorb(response.headers(), response.url())?;
        let bytes = read_response_bytes(response, MAX_QR_JSON_BYTES).await?;
        let response_digest = Sha256::digest(bytes.as_slice()).into();
        let state = parse_qr_poll_state(bytes.as_slice())?;
        match state {
            QrPollState::AwaitingScan => {
                challenge.phase = QrContinuationPhase::AwaitingScan;
                let result_digest = challenge.poll_result_digest(
                    consumed_continuation_digest,
                    state,
                    response_digest,
                )?;
                Ok(ChaoxingQrPollOutcome::AwaitingScan { result_digest })
            }
            QrPollState::AwaitingConfirmation => {
                challenge.phase = QrContinuationPhase::AwaitingConfirmation;
                let result_digest = challenge.poll_result_digest(
                    consumed_continuation_digest,
                    state,
                    response_digest,
                )?;
                Ok(ChaoxingQrPollOutcome::AwaitingConfirmation { result_digest })
            }
            QrPollState::IdentityValidation => Err(ProviderError::new(
                ProviderErrorKind::Internal,
                "Chaoxing QR parser produced a Provider-only continuation phase",
            )),
            QrPollState::Rejected => {
                let result_digest = challenge.poll_result_digest(
                    consumed_continuation_digest,
                    state,
                    response_digest,
                )?;
                Ok(ChaoxingQrPollOutcome::Rejected { result_digest })
            }
            QrPollState::Expired => {
                let result_digest = challenge.poll_result_digest(
                    consumed_continuation_digest,
                    state,
                    response_digest,
                )?;
                Ok(ChaoxingQrPollOutcome::Expired { result_digest })
            }
            QrPollState::Authenticated => {
                challenge.scoped_course_session().map_err(|_| {
                    ProviderError::new(
                        ProviderErrorKind::Authentication,
                        "Chaoxing QR success lacked an authenticated identity Cookie",
                    )
                })?;
                challenge.phase = QrContinuationPhase::IdentityValidation;
                let result_digest = challenge.poll_result_digest(
                    consumed_continuation_digest,
                    QrPollState::IdentityValidation,
                    response_digest,
                )?;
                Ok(ChaoxingQrPollOutcome::IdentityValidation { result_digest })
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum QrPollState {
    AwaitingScan,
    AwaitingConfirmation,
    IdentityValidation,
    Authenticated,
    Rejected,
    Expired,
}

impl QrPollState {
    const fn label(self) -> &'static str {
        match self {
            Self::AwaitingScan => "awaiting_scan",
            Self::AwaitingConfirmation => "awaiting_confirmation",
            Self::IdentityValidation => "identity_validation",
            Self::Authenticated => "authenticated",
            Self::Rejected => "rejected",
            Self::Expired => "expired",
        }
    }
}

#[derive(Deserialize)]
struct QrPollEnvelope {
    status: Option<bool>,
    #[serde(default, rename = "type")]
    kind: Option<String>,
}

impl Drop for QrPollEnvelope {
    fn drop(&mut self) {
        self.kind.zeroize();
    }
}

fn parse_qr_poll_state(document: &[u8]) -> ProviderResult<QrPollState> {
    let envelope = serde_json::from_slice::<QrPollEnvelope>(document)
        .map_err(|_| invalid_qr_response("Chaoxing QR status is not valid JSON"))?;
    match (envelope.status, envelope.kind.as_deref()) {
        (Some(true), _) => Ok(QrPollState::Authenticated),
        (Some(false), None | Some("")) => Ok(QrPollState::AwaitingScan),
        (Some(false), Some("4")) => Ok(QrPollState::AwaitingConfirmation),
        (Some(false), Some("1")) => Ok(QrPollState::Rejected),
        (Some(false), Some("2")) => Ok(QrPollState::Expired),
        _ => Err(ProviderError::new(
            ProviderErrorKind::ProtocolDrift,
            "Chaoxing QR status contains an unknown state",
        )),
    }
}

fn parse_qr_login_document(document: &str) -> ProviderResult<(SecretString, SecretString)> {
    if document.is_empty() || document.len() > MAX_QR_HTML_BYTES {
        return Err(invalid_qr_response(
            "Chaoxing QR login page is empty or oversized",
        ));
    }
    let html = Html::parse_document(document);
    let uuid = unique_qr_input(&html, "uuid")?;
    let enc = unique_qr_input(&html, "enc")?;
    if uuid == enc {
        return Err(invalid_qr_response(
            "Chaoxing QR login page reused its challenge values",
        ));
    }
    Ok((SecretString::new(uuid), SecretString::new(enc)))
}

fn unique_qr_input(html: &Html, id: &str) -> ProviderResult<String> {
    let selector = Selector::parse(&format!("input#{id}"))
        .expect("static Chaoxing QR input selector must be valid");
    let mut inputs = html.select(&selector);
    let input = inputs
        .next()
        .ok_or_else(|| invalid_qr_response("Chaoxing QR login page omitted challenge material"))?;
    if inputs.next().is_some() {
        return Err(invalid_qr_response(
            "Chaoxing QR login page duplicated challenge material",
        ));
    }
    input
        .value()
        .attr("value")
        .filter(|value| valid_qr_secret(value))
        .map(ToOwned::to_owned)
        .ok_or_else(|| invalid_qr_response("Chaoxing QR challenge material is invalid"))
}

fn valid_qr_secret(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_QR_SECRET_BYTES
        && value.trim() == value
        && value.is_ascii()
        && !value.chars().any(char::is_control)
}

fn build_challenge_url(uuid: &SecretString, enc: &SecretString) -> ProviderResult<SecretString> {
    let mut url = static_url(QR_CHALLENGE_ENDPOINT)?;
    url.query_pairs_mut()
        .append_pair("uuid", uuid.expose_secret())
        .append_pair("enc", enc.expose_secret())
        .append_pair("xxtrefer", "")
        .append_pair("clientid", "")
        .append_pair("mobiletip", "");
    if url.as_str().len() > MAX_QR_ACTION_BYTES {
        return Err(invalid_qr_response(
            "Chaoxing QR challenge action exceeds the size limit",
        ));
    }
    Ok(SecretString::new(url))
}

fn qr_begin_evidence_digest(
    login_response_digest: [u8; 32],
    activation_response_digest: [u8; 32],
    cookie_digest: [u8; 32],
    challenge_url_digest: [u8; 32],
) -> [u8; 32] {
    hash_fields(
        b"chaoxing.qr.begin-result.v1",
        &[
            login_response_digest.as_slice(),
            activation_response_digest.as_slice(),
            cookie_digest.as_slice(),
            challenge_url_digest.as_slice(),
        ],
    )
}

fn hash_fields(domain: &[u8], fields: &[&[u8]]) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update((domain.len() as u64).to_be_bytes());
    digest.update(domain);
    for field in fields {
        digest.update((field.len() as u64).to_be_bytes());
        digest.update(field);
    }
    digest.finalize().into()
}

fn validate_auth_context(context: &ProviderAuthContext) -> ProviderResult<()> {
    if context.provider_id.as_str() != PROVIDER_ID
        || context.auth_session_id.is_none()
        || correlation_digest(&context.correlation_id).is_err()
    {
        return Err(qr_binding_error());
    }
    Ok(())
}

fn correlation_digest(value: &str) -> ProviderResult<[u8; 32]> {
    if value.is_empty()
        || value.len() > 512
        || value.trim() != value
        || value.chars().any(char::is_control)
    {
        return Err(qr_binding_error());
    }
    Ok(Sha256::digest(value.as_bytes()).into())
}

fn apply_cookie(
    request: reqwest::RequestBuilder,
    cookies: &SensitiveCookieJar,
    request_url: &str,
) -> ProviderResult<reqwest::RequestBuilder> {
    let Some(cookie) = cookies.header_for(&static_url(request_url)?)? else {
        return Ok(request);
    };
    let mut value = HeaderValue::from_str(cookie.as_str()).map_err(|_| cookie_error())?;
    value.set_sensitive(true);
    Ok(request.header(COOKIE, value))
}

async fn read_utf8_response(
    response: Response,
    maximum: usize,
) -> ProviderResult<Zeroizing<String>> {
    let mut bytes = read_response_bytes(response, maximum).await?;
    match String::from_utf8(std::mem::take(&mut *bytes)) {
        Ok(document) => Ok(Zeroizing::new(document)),
        Err(error) => {
            let mut bytes = error.into_bytes();
            bytes.zeroize();
            Err(invalid_qr_response(
                "Chaoxing QR endpoint returned invalid UTF-8",
            ))
        }
    }
}

async fn read_response_bytes(
    mut response: Response,
    maximum: usize,
) -> ProviderResult<Zeroizing<Vec<u8>>> {
    if response
        .content_length()
        .is_some_and(|length| length > maximum as u64)
    {
        return Err(invalid_qr_response(
            "Chaoxing QR response exceeds the size limit",
        ));
    }
    let mut bytes = Zeroizing::new(Vec::new());
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| classify_reqwest_error(&error))?
    {
        if bytes.len().saturating_add(chunk.len()) > maximum {
            return Err(invalid_qr_response(
                "Chaoxing QR response exceeds the size limit",
            ));
        }
        bytes.extend_from_slice(&chunk);
    }
    if bytes.is_empty() {
        return Err(invalid_qr_response(
            "Chaoxing QR endpoint returned an empty response",
        ));
    }
    Ok(bytes)
}

fn validate_qr_response_status(response: &Response) -> ProviderResult<()> {
    let status = response.status();
    if status == StatusCode::TOO_MANY_REQUESTS {
        let mut error = ProviderError::new(
            ProviderErrorKind::RateLimited,
            "Chaoxing rate limited the QR request",
        );
        error.retry_after_seconds = response
            .headers()
            .get(RETRY_AFTER)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|seconds| *seconds <= 3_600);
        return Err(error);
    }
    if status.is_server_error() {
        return Err(ProviderError::new(
            ProviderErrorKind::ProviderUnavailable,
            "Chaoxing QR endpoint is temporarily unavailable",
        ));
    }
    if status.is_redirection() {
        return Err(ProviderError::new(
            ProviderErrorKind::ProtocolDrift,
            "Chaoxing QR endpoint returned an unexpected redirect",
        ));
    }
    if !status.is_success() {
        return Err(invalid_qr_response(
            "Chaoxing QR endpoint returned an unexpected status",
        ));
    }
    Ok(())
}

fn validate_content_type(response: &Response, allowed: &[&str]) -> ProviderResult<()> {
    let Some(value) = response.headers().get(CONTENT_TYPE) else {
        return Ok(());
    };
    let value = value.to_str().map_err(|_| {
        invalid_qr_response("Chaoxing QR endpoint returned an invalid Content-Type")
    })?;
    let media_type = value.split(';').next().unwrap_or_default().trim();
    if allowed
        .iter()
        .any(|allowed| media_type.eq_ignore_ascii_case(allowed))
    {
        Ok(())
    } else {
        Err(invalid_qr_response(
            "Chaoxing QR endpoint returned an unexpected Content-Type",
        ))
    }
}

fn static_url(value: &str) -> ProviderResult<Url> {
    Url::parse(value).map_err(|_| {
        ProviderError::new(
            ProviderErrorKind::Internal,
            "Chaoxing compile-time QR route is invalid",
        )
    })
}

#[derive(Default)]
struct SensitiveCookieJar(BTreeMap<CookieKey, SecretValue>);

impl SensitiveCookieJar {
    fn len(&self) -> usize {
        self.0.len()
    }

    fn absorb(&mut self, headers: &HeaderMap, response_url: &Url) -> ProviderResult<()> {
        let response_host = response_url
            .host_str()
            .map(str::to_ascii_lowercase)
            .filter(|host| response_url.scheme() == "https" && valid_chaoxing_host(host))
            .ok_or_else(cookie_error)?;
        let values = headers.get_all(SET_COOKIE);
        if values.iter().count() > MAX_COOKIE_HEADERS {
            return Err(cookie_error());
        }
        for value in values {
            let value = value.to_str().map_err(|_| cookie_error())?;
            let (key, cookie_value, delete) = parse_set_cookie(value, &response_host)?;
            if delete {
                self.0.remove(&key);
            } else {
                if self.0.len() >= MAX_COOKIE_ENTRIES && !self.0.contains_key(&key) {
                    return Err(cookie_error());
                }
                self.0
                    .insert(key, SecretValue::new(cookie_value.into_bytes()));
            }
        }
        Ok(())
    }

    fn header_for(&self, request_url: &Url) -> ProviderResult<Option<Zeroizing<String>>> {
        let host = request_url
            .host_str()
            .map(str::to_ascii_lowercase)
            .filter(|host| request_url.scheme() == "https" && valid_chaoxing_host(host))
            .ok_or_else(cookie_error)?;
        let mut selected = BTreeMap::<&str, (&CookieKey, &SecretValue)>::new();
        for (key, value) in &self.0 {
            if domain_matches(&host, key) && path_matches(request_url.path(), &key.path) {
                let replace = selected
                    .get(key.name.as_str())
                    .is_none_or(|(existing, _)| existing.path.len() < key.path.len());
                if replace {
                    selected.insert(key.name.as_str(), (key, value));
                }
            }
        }
        if selected.is_empty() {
            return Ok(None);
        }
        let mut fields = Vec::with_capacity(selected.len());
        for (name, (_, value)) in selected {
            let value = std::str::from_utf8(value.expose_secret()).map_err(|_| cookie_error())?;
            fields.push(Zeroizing::new(format!("{name}={value}")));
        }
        let header = Zeroizing::new(
            fields
                .iter()
                .map(|field| field.as_str())
                .collect::<Vec<_>>()
                .join("; "),
        );
        if header.len() > MAX_COOKIE_HEADER_BYTES || HeaderValue::from_str(&header).is_err() {
            return Err(cookie_error());
        }
        Ok(Some(header))
    }

    fn wire_entries(&self) -> ProviderResult<Vec<QrCookieWireRef<'_>>> {
        self.0
            .iter()
            .map(|(key, value)| {
                let value =
                    std::str::from_utf8(value.expose_secret()).map_err(|_| cookie_error())?;
                if !valid_cookie_value(value) {
                    return Err(cookie_error());
                }
                Ok(QrCookieWireRef {
                    name: &key.name,
                    domain: &key.domain,
                    path: &key.path,
                    host_only: key.host_only,
                    value,
                })
            })
            .collect()
    }

    fn take_wire_entries(entries: &mut Vec<QrCookieWire>) -> ProviderResult<Self> {
        if entries.len() > MAX_COOKIE_ENTRIES {
            return Err(cookie_error());
        }
        let mut jar = BTreeMap::new();
        let mut previous = None;
        for entry in entries {
            let key = CookieKey {
                name: std::mem::take(&mut entry.name),
                domain: std::mem::take(&mut entry.domain),
                path: std::mem::take(&mut entry.path),
                host_only: entry.host_only,
            };
            if !valid_cookie_name(&key.name)
                || !valid_chaoxing_host(&key.domain)
                || !valid_cookie_path(&key.path)
                || previous.as_ref().is_some_and(|previous| previous >= &key)
                || !valid_cookie_value(&entry.value)
            {
                return Err(cookie_error());
            }
            let value = std::mem::take(&mut entry.value);
            previous = Some(CookieKey {
                name: key.name.clone(),
                domain: key.domain.clone(),
                path: key.path.clone(),
                host_only: key.host_only,
            });
            jar.insert(key, SecretValue::new(value.into_bytes()));
        }
        Ok(Self(jar))
    }

    fn digest(&self) -> ProviderResult<[u8; 32]> {
        let mut encoded =
            Zeroizing::new(serde_json::to_vec(&self.wire_entries()?).map_err(|_| cookie_error())?);
        let digest = Sha256::digest(encoded.as_slice()).into();
        encoded.zeroize();
        Ok(digest)
    }
}

#[derive(Serialize)]
struct QrCookieWireRef<'a> {
    name: &'a str,
    domain: &'a str,
    path: &'a str,
    host_only: bool,
    value: &'a str,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct QrCookieWire {
    name: String,
    domain: String,
    path: String,
    host_only: bool,
    value: String,
}

impl Drop for QrCookieWire {
    fn drop(&mut self) {
        self.name.zeroize();
        self.domain.zeroize();
        self.path.zeroize();
        self.value.zeroize();
    }
}

#[derive(Eq, Ord, PartialEq, PartialOrd)]
struct CookieKey {
    name: String,
    domain: String,
    path: String,
    host_only: bool,
}

fn parse_set_cookie(value: &str, response_host: &str) -> ProviderResult<(CookieKey, String, bool)> {
    if value.is_empty() || value.len() > MAX_SET_COOKIE_BYTES || value.chars().any(char::is_control)
    {
        return Err(cookie_error());
    }
    let mut fields = value.split(';');
    let (name, cookie_value) = fields
        .next()
        .and_then(|field| field.trim().split_once('='))
        .ok_or_else(cookie_error)?;
    if !valid_cookie_name(name) || !valid_cookie_value(cookie_value) {
        return Err(cookie_error());
    }
    let mut domain = response_host.to_owned();
    let mut host_only = true;
    let mut path = "/".to_owned();
    let mut delete = cookie_value.is_empty();
    for field in fields {
        let field = field.trim();
        let (attribute, attribute_value) = field
            .split_once('=')
            .map_or((field, None), |(name, value)| (name, Some(value.trim())));
        if attribute.eq_ignore_ascii_case("domain") {
            let candidate = attribute_value
                .map(|value| value.trim_start_matches('.').to_ascii_lowercase())
                .filter(|value| valid_chaoxing_host(value))
                .ok_or_else(cookie_error)?;
            if response_host != candidate && !response_host.ends_with(&format!(".{candidate}")) {
                return Err(cookie_error());
            }
            domain = candidate;
            host_only = false;
        } else if attribute.eq_ignore_ascii_case("path") {
            attribute_value
                .filter(|value| valid_cookie_path(value))
                .ok_or_else(cookie_error)?
                .clone_into(&mut path);
        } else if attribute.eq_ignore_ascii_case("max-age") {
            let seconds = attribute_value
                .and_then(|value| value.parse::<i64>().ok())
                .ok_or_else(cookie_error)?;
            delete |= seconds <= 0;
        }
    }
    Ok((
        CookieKey {
            name: name.to_owned(),
            domain,
            path,
            host_only,
        },
        cookie_value.to_owned(),
        delete,
    ))
}

fn domain_matches(host: &str, key: &CookieKey) -> bool {
    if key.host_only {
        host == key.domain
    } else {
        host == key.domain || host.ends_with(&format!(".{}", key.domain))
    }
}

fn path_matches(request_path: &str, cookie_path: &str) -> bool {
    request_path == cookie_path
        || cookie_path == "/"
        || request_path
            .strip_prefix(cookie_path)
            .is_some_and(|suffix| cookie_path.ends_with('/') || suffix.starts_with('/'))
}

fn valid_chaoxing_host(value: &str) -> bool {
    value == "chaoxing.com" || value.ends_with(".chaoxing.com")
}

fn valid_cookie_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_COOKIE_NAME_BYTES
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(
                    byte,
                    b'!' | b'#'
                        | b'$'
                        | b'%'
                        | b'&'
                        | b'\''
                        | b'*'
                        | b'+'
                        | b'-'
                        | b'.'
                        | b'^'
                        | b'_'
                        | b'`'
                        | b'|'
                        | b'~'
                )
        })
}

fn valid_cookie_value(value: &str) -> bool {
    value.len() <= MAX_COOKIE_VALUE_BYTES
        && value
            .bytes()
            .all(|byte| byte.is_ascii_graphic() && !matches!(byte, b';' | b',' | b'"' | b'\\'))
}

fn valid_cookie_path(value: &str) -> bool {
    value.starts_with('/')
        && value.len() <= MAX_COOKIE_PATH_BYTES
        && !value.chars().any(char::is_control)
        && !value.contains(';')
}

fn invalid_qr_response(message: &'static str) -> ProviderError {
    ProviderError::new(ProviderErrorKind::InvalidResponse, message)
}

fn qr_binding_error() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::Authentication,
        "Chaoxing QR challenge is expired, terminal or foreign to this authentication session",
    )
}

fn cookie_error() -> ProviderError {
    invalid_qr_response("Chaoxing QR Cookie state is malformed or out of scope")
}

#[cfg(test)]
impl ChaoxingQrChallenge {
    pub(crate) fn fixture(context: &ProviderAuthContext) -> Self {
        let (uuid, enc) = parse_qr_login_document(include_str!(
            "../../../fixtures/providers/chaoxing/auth/qr-login.html"
        ))
        .unwrap();
        let mut cookies = SensitiveCookieJar::default();
        let mut headers = HeaderMap::new();
        headers.append(
            SET_COOKIE,
            HeaderValue::from_static("JSESSIONID=SAFE_QR_SESSION; Path=/; HttpOnly; Secure"),
        );
        headers.append(
            SET_COOKIE,
            HeaderValue::from_static("_uid=SAFE_QR_UID; Domain=.chaoxing.com; Path=/; Secure"),
        );
        cookies
            .absorb(&headers, &static_url(QR_LOGIN_PAGE).unwrap())
            .unwrap();
        Self {
            account_id: context.account_id,
            auth_session_id: context.auth_session_id.unwrap(),
            correlation_digest: correlation_digest(&context.correlation_id).unwrap(),
            challenge_url: build_challenge_url(&uuid, &enc).unwrap(),
            uuid,
            enc,
            cookies,
            continuation_revision: 1,
            poll_count: 0,
            claimed_poll_sequence: None,
            phase: QrContinuationPhase::AwaitingScan,
            expires_at: Utc::now() + Duration::minutes(15),
            begin_evidence_digest: [7; 32],
            identity_validation_digest: None,
            authenticated_at: None,
        }
    }

    pub(crate) fn fixture_awaiting_confirmation(
        &mut self,
        context: &ProviderAuthContext,
    ) -> ChaoxingQrPollOutcome {
        let consumed = self.continuation_digest().unwrap();
        self.begin_poll(context).unwrap();
        self.phase = QrContinuationPhase::AwaitingConfirmation;
        let result_digest = self
            .poll_result_digest(consumed, QrPollState::AwaitingConfirmation, [8; 32])
            .unwrap();
        ChaoxingQrPollOutcome::AwaitingConfirmation { result_digest }
    }

    pub(crate) fn fixture_authenticated(
        &mut self,
        context: &ProviderAuthContext,
    ) -> ChaoxingQrPollOutcome {
        assert_eq!(self.phase, QrContinuationPhase::IdentityValidation);
        let consumed = self.continuation_digest().unwrap();
        self.begin_poll(context).unwrap();
        self.phase = QrContinuationPhase::Authenticated;
        self.identity_validation_digest = Some([9; 32]);
        self.authenticated_at = Some(Utc::now());
        let session = self.authenticated_session().unwrap();
        let result_digest = self
            .poll_result_digest(consumed, QrPollState::Authenticated, [10; 32])
            .unwrap();
        ChaoxingQrPollOutcome::Authenticated {
            session,
            result_digest,
        }
    }

    pub(crate) fn fixture_identity_validation(
        &mut self,
        context: &ProviderAuthContext,
    ) -> ChaoxingQrPollOutcome {
        let consumed = self.continuation_digest().unwrap();
        self.begin_poll(context).unwrap();
        self.phase = QrContinuationPhase::IdentityValidation;
        let result_digest = self
            .poll_result_digest(consumed, QrPollState::IdentityValidation, [13; 32])
            .unwrap();
        ChaoxingQrPollOutcome::IdentityValidation { result_digest }
    }

    pub(crate) fn fixture_rejected(
        &mut self,
        context: &ProviderAuthContext,
    ) -> ChaoxingQrPollOutcome {
        let consumed = self.continuation_digest().unwrap();
        self.begin_poll(context).unwrap();
        let result_digest = self
            .poll_result_digest(consumed, QrPollState::Rejected, [11; 32])
            .unwrap();
        ChaoxingQrPollOutcome::Rejected { result_digest }
    }

    pub(crate) fn fixture_expired(
        &mut self,
        context: &ProviderAuthContext,
    ) -> ChaoxingQrPollOutcome {
        let consumed = self.continuation_digest().unwrap();
        self.begin_poll(context).unwrap();
        let result_digest = self
            .poll_result_digest(consumed, QrPollState::Expired, [12; 32])
            .unwrap();
        ChaoxingQrPollOutcome::Expired { result_digest }
    }

    fn fixture_claim_poll(&mut self, poll_sequence: u32) {
        self.claimed_poll_sequence = Some(poll_sequence);
    }
}

#[cfg(test)]
mod tests {
    use asterism_domain::{AuthSessionId, ProviderAccountId, ProviderId};
    use reqwest::header::{HeaderMap, HeaderValue, SET_COOKIE};

    use super::*;

    const QR_LOGIN: &str = include_str!("../../../fixtures/providers/chaoxing/auth/qr-login.html");
    const QR_AWAITING: &[u8] =
        include_bytes!("../../../fixtures/providers/chaoxing/auth/qr-awaiting.json");
    const QR_SCANNED: &[u8] =
        include_bytes!("../../../fixtures/providers/chaoxing/auth/qr-scanned.json");
    const QR_AUTHENTICATED: &[u8] =
        include_bytes!("../../../fixtures/providers/chaoxing/auth/qr-authenticated.json");
    const QR_REJECTED: &[u8] =
        include_bytes!("../../../fixtures/providers/chaoxing/auth/qr-rejected.json");
    const QR_EXPIRED: &[u8] =
        include_bytes!("../../../fixtures/providers/chaoxing/auth/qr-expired.json");

    #[test]
    fn login_page_builds_exact_redacted_challenge_url() {
        let (uuid, enc) = parse_qr_login_document(QR_LOGIN).unwrap();
        let challenge = build_challenge_url(&uuid, &enc).unwrap();
        let url = Url::parse(challenge.expose_secret()).unwrap();
        assert_eq!(url.scheme(), "https");
        assert_eq!(url.host_str(), Some("passport2.chaoxing.com"));
        assert_eq!(url.path(), "/toauthlogin");
        assert_eq!(
            url.query_pairs()
                .find(|(name, _)| name == "uuid")
                .map(|(_, value)| value.into_owned()),
            Some("SAFE_QR_UUID_0123456789".to_owned())
        );
        assert!(!format!("{challenge:?}").contains("SAFE_QR_UUID"));

        assert!(parse_qr_login_document(&QR_LOGIN.replace("id=\"enc\"", "id=\"uuid\"")).is_err());
        assert!(
            parse_qr_login_document(&QR_LOGIN.replace(
                "SAFE_QR_ENC_abcdefghijklmnopqrstuvwxyz",
                "SAFE_QR_UUID_0123456789"
            ))
            .is_err()
        );
    }

    #[test]
    fn poll_states_are_typed_without_retaining_identity_text() {
        for (document, expected) in [
            (QR_AWAITING, QrPollState::AwaitingScan),
            (QR_SCANNED, QrPollState::AwaitingConfirmation),
            (QR_AUTHENTICATED, QrPollState::Authenticated),
            (QR_REJECTED, QrPollState::Rejected),
            (QR_EXPIRED, QrPollState::Expired),
        ] {
            assert_eq!(parse_qr_poll_state(document).unwrap(), expected);
        }
        assert_eq!(
            parse_qr_poll_state(br#"{"status":false,"type":"99","uid":"PRIVATE_UID"}"#)
                .unwrap_err()
                .kind,
            ProviderErrorKind::ProtocolDrift
        );
    }

    #[test]
    fn cookie_jar_scopes_domains_paths_replacement_and_deletion() {
        let passport = static_url(QR_LOGIN_PAGE).unwrap();
        let course = static_url(COURSE_LIST_ENDPOINT).unwrap();
        let mut jar = SensitiveCookieJar::default();
        let mut headers = HeaderMap::new();
        headers.append(
            SET_COOKIE,
            HeaderValue::from_static("JSESSIONID=PRIVATE_SESSION; Path=/; HttpOnly; Secure"),
        );
        headers.append(
            SET_COOKIE,
            HeaderValue::from_static("_uid=SAFE_UID; Domain=.chaoxing.com; Path=/; Secure"),
        );
        jar.absorb(&headers, &passport).unwrap();
        let passport_header = jar.header_for(&passport).unwrap().unwrap();
        assert!(passport_header.contains("JSESSIONID=PRIVATE_SESSION"));
        assert!(passport_header.contains("_uid=SAFE_UID"));
        let course_header = jar.header_for(&course).unwrap().unwrap();
        assert!(!course_header.contains("PRIVATE_SESSION"));
        assert_eq!(course_header.as_str(), "_uid=SAFE_UID");
        let outcome = ChaoxingQrPollOutcome::Authenticated {
            session: ChaoxingCookieSession::try_new(course_header.as_str()).unwrap(),
            result_digest: [1; 32],
        };
        assert!(!format!("{outcome:?}").contains("SAFE_UID"));

        let mut replace = HeaderMap::new();
        replace.append(
            SET_COOKIE,
            HeaderValue::from_static("_uid=NEW_UID; Domain=.chaoxing.com; Path=/; Secure"),
        );
        jar.absorb(&replace, &passport).unwrap();
        assert_eq!(
            jar.header_for(&course).unwrap().unwrap().as_str(),
            "_uid=NEW_UID"
        );
        let mut delete = HeaderMap::new();
        delete.append(
            SET_COOKIE,
            HeaderValue::from_static("_uid=; Domain=.chaoxing.com; Path=/; Max-Age=0"),
        );
        jar.absorb(&delete, &passport).unwrap();
        assert!(jar.header_for(&course).unwrap().is_none());
    }

    #[test]
    fn challenge_binding_rejects_foreign_or_terminal_polling() {
        let context = auth_context();
        let mut challenge = ChaoxingQrChallenge::fixture(&context);
        challenge.fixture_claim_poll(1);
        challenge.begin_poll(&context).unwrap();
        assert_eq!(challenge.poll_count, 1);
        assert!(!format!("{challenge:?}").contains("SAFE_QR_UUID"));

        let mut foreign = context.clone();
        foreign.account_id = ProviderAccountId::new();
        assert_eq!(
            challenge.validate_binding(&foreign).unwrap_err().kind,
            ProviderErrorKind::Authentication
        );
        challenge.phase = QrContinuationPhase::Authenticated;
        assert!(challenge.validate_binding(&context).is_err());
    }

    #[test]
    fn continuation_roundtrip_binds_context_revision_phase_and_exact_digest() {
        let context = auth_context();
        let challenge = ChaoxingQrChallenge::fixture(&context);
        let continuation = challenge.to_provider_continuation().unwrap();
        let digest = continuation.continuation_digest();
        let (continuation_type, phase, value, _, _) = continuation.into_parts();
        let decoded = ChaoxingQrChallenge::decode_bound(
            &context,
            &ResolvedProviderInteractiveAuthContinuation {
                continuation_type: &continuation_type,
                continuation_digest: digest,
                phase: &phase,
                revision: 1,
                poll_sequence: 1,
                value: &value,
            },
        )
        .unwrap();
        assert_eq!(decoded.poll_count, 0);
        assert_eq!(decoded.phase, QrContinuationPhase::AwaitingScan);
        assert!(!format!("{decoded:?}").contains("SAFE_QR_SESSION"));

        let mut foreign = context.clone();
        foreign.account_id = ProviderAccountId::new();
        assert!(
            ChaoxingQrChallenge::decode_bound(
                &foreign,
                &ResolvedProviderInteractiveAuthContinuation {
                    continuation_type: &continuation_type,
                    continuation_digest: digest,
                    phase: &phase,
                    revision: 1,
                    poll_sequence: 1,
                    value: &value,
                },
            )
            .is_err()
        );
        assert!(
            ChaoxingQrChallenge::decode_bound(
                &context,
                &ResolvedProviderInteractiveAuthContinuation {
                    continuation_type: &continuation_type,
                    continuation_digest: digest,
                    phase: QR_PHASE_AWAITING_CONFIRMATION,
                    revision: 1,
                    poll_sequence: 1,
                    value: &value,
                },
            )
            .is_err()
        );
        assert!(
            ChaoxingQrChallenge::decode_bound(
                &context,
                &ResolvedProviderInteractiveAuthContinuation {
                    continuation_type: &continuation_type,
                    continuation_digest: digest,
                    phase: &phase,
                    revision: 1,
                    poll_sequence: 0,
                    value: &value,
                },
            )
            .is_err()
        );
        assert!(
            ChaoxingQrChallenge::decode_bound(
                &context,
                &ResolvedProviderInteractiveAuthContinuation {
                    continuation_type: &continuation_type,
                    continuation_digest: [99; 32],
                    phase: &phase,
                    revision: 1,
                    poll_sequence: 1,
                    value: &value,
                },
            )
            .is_err()
        );
    }

    #[test]
    fn retryable_claim_gap_keeps_revision_and_accepts_the_next_core_sequence() {
        let context = auth_context();
        let challenge = ChaoxingQrChallenge::fixture(&context);
        let continuation = challenge.to_provider_continuation().unwrap();
        let digest = continuation.continuation_digest();
        let (continuation_type, phase, value, _, _) = continuation.into_parts();

        // Sequence 1 was consumed by a retryable Core claim and released. The
        // persisted Provider value is still revision 1 with sequence 0.
        let mut retried = ChaoxingQrChallenge::decode_bound(
            &context,
            &ResolvedProviderInteractiveAuthContinuation {
                continuation_type: &continuation_type,
                continuation_digest: digest,
                phase: &phase,
                revision: 1,
                poll_sequence: 2,
                value: &value,
            },
        )
        .unwrap();
        let _ = retried.fixture_awaiting_confirmation(&context);
        assert_eq!(retried.continuation_revision, 2);
        assert_eq!(retried.poll_count, 2);

        let continuation = retried.to_provider_continuation().unwrap();
        let digest = continuation.continuation_digest();
        let (continuation_type, phase, value, _, _) = continuation.into_parts();
        assert!(
            ChaoxingQrChallenge::decode_bound(
                &context,
                &ResolvedProviderInteractiveAuthContinuation {
                    continuation_type: &continuation_type,
                    continuation_digest: digest,
                    phase: &phase,
                    revision: 2,
                    poll_sequence: 2,
                    value: &value,
                },
            )
            .is_err()
        );
        assert!(
            ChaoxingQrChallenge::decode_bound(
                &context,
                &ResolvedProviderInteractiveAuthContinuation {
                    continuation_type: &continuation_type,
                    continuation_digest: digest,
                    phase: &phase,
                    revision: 2,
                    poll_sequence: 3,
                    value: &value,
                },
            )
            .is_ok()
        );
    }

    #[test]
    fn expired_waiting_state_closes_but_authenticated_state_can_finalize() {
        let context = auth_context();
        let mut waiting = ChaoxingQrChallenge::fixture(&context);
        waiting.expires_at = Utc::now() - Duration::minutes(1);
        let continuation = waiting.to_provider_continuation().unwrap();
        let digest = continuation.continuation_digest();
        let (continuation_type, phase, value, _, _) = continuation.into_parts();
        assert!(
            ChaoxingQrChallenge::decode_bound(
                &context,
                &ResolvedProviderInteractiveAuthContinuation {
                    continuation_type: &continuation_type,
                    continuation_digest: digest,
                    phase: &phase,
                    revision: 1,
                    poll_sequence: 1,
                    value: &value,
                },
            )
            .is_err()
        );

        let mut authenticated = ChaoxingQrChallenge::fixture(&context);
        authenticated.fixture_claim_poll(1);
        let _ = authenticated.fixture_identity_validation(&context);
        authenticated.fixture_claim_poll(2);
        let _ = authenticated.fixture_authenticated(&context);
        authenticated.authenticated_at = Some(Utc::now() - Duration::minutes(2));
        authenticated.expires_at = Utc::now() - Duration::minutes(1);
        let continuation = authenticated.to_provider_continuation().unwrap();
        let digest = continuation.continuation_digest();
        let (continuation_type, phase, value, _, _) = continuation.into_parts();
        let decoded = ChaoxingQrChallenge::decode_bound(
            &context,
            &ResolvedProviderInteractiveAuthContinuation {
                continuation_type: &continuation_type,
                continuation_digest: digest,
                phase: &phase,
                revision: 3,
                poll_sequence: 2,
                value: &value,
            },
        )
        .unwrap();
        assert!(decoded.authenticated_session().is_ok());
    }

    fn auth_context() -> ProviderAuthContext {
        ProviderAuthContext {
            provider_id: ProviderId::new(PROVIDER_ID).unwrap(),
            account_id: ProviderAccountId::new(),
            auth_session_id: Some(AuthSessionId::new()),
            correlation_id: "chaoxing-qr-test".to_owned(),
        }
    }
}
