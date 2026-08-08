use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::{AuthSessionId, ProviderAccountId, ServiceTokenId, Timestamp, UserId, WebSessionId};

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthMethod {
    Password,
    QrCode,
    ExternalBrowserOauth,
    AssistedSession,
    ImportedCookie,
    ImportedToken,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionKind {
    Cookie,
    BearerToken,
    Jwt,
    Composite,
    ProviderSpecific,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "state", content = "detail")]
pub enum AuthState {
    Idle,
    Starting,
    WaitingUser(WaitingUserState),
    ExchangingCredential,
    ValidatingCredential,
    Authenticated,
    Refreshing,
    Expired,
    AuthFailed,
    HumanRequired(HumanRequiredReason),
    ProviderUnavailable,
    ClientUpdateRequired,
    Cancelled,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AuthSession {
    pub id: AuthSessionId,
    pub owner_user_id: UserId,
    pub provider_account_id: ProviderAccountId,
    pub method: AuthMethod,
    pub state: AuthState,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    pub expires_at: Timestamp,
    pub revision: u32,
}

impl AuthSession {
    /// Creates a new observable authentication attempt in `Starting` state.
    ///
    /// # Errors
    ///
    /// Returns [`AuthSessionError::InvalidExpiry`] when the attempt does not
    /// expire after its creation time.
    pub fn starting(
        owner_user_id: UserId,
        provider_account_id: ProviderAccountId,
        method: AuthMethod,
        created_at: Timestamp,
        expires_at: Timestamp,
    ) -> Result<Self, AuthSessionError> {
        if expires_at <= created_at {
            return Err(AuthSessionError::InvalidExpiry);
        }
        Ok(Self {
            id: AuthSessionId::new(),
            owner_user_id,
            provider_account_id,
            method,
            state: AuthState::Starting,
            created_at,
            updated_at: created_at,
            expires_at,
            revision: 1,
        })
    }

    /// Applies one legal state transition and increments the optimistic
    /// concurrency revision.
    ///
    /// # Errors
    ///
    /// Rejects skipped or terminal transitions, timestamps before the prior
    /// update, and exhausted revisions.
    pub fn transition(&mut self, next: AuthState, at: Timestamp) -> Result<(), AuthSessionError> {
        if at < self.updated_at {
            return Err(AuthSessionError::TimestampRegression);
        }
        if at >= self.expires_at && !matches!(next, AuthState::Expired | AuthState::Cancelled) {
            return Err(AuthSessionError::SessionExpired);
        }
        if !valid_auth_transition(&self.state, &next) {
            return Err(AuthSessionError::InvalidTransition);
        }
        let revision = self
            .revision
            .checked_add(1)
            .ok_or(AuthSessionError::RevisionExhausted)?;
        self.state = next;
        self.updated_at = at;
        self.revision = revision;
        Ok(())
    }

    pub fn is_expired_at(&self, at: Timestamp) -> bool {
        at >= self.expires_at
    }
}

fn valid_auth_transition(current: &AuthState, next: &AuthState) -> bool {
    use AuthState::{
        AuthFailed, Authenticated, Cancelled, ClientUpdateRequired, ExchangingCredential, Expired,
        HumanRequired, ProviderUnavailable, Refreshing, Starting, ValidatingCredential,
        WaitingUser,
    };

    matches!(
        (current, next),
        (
            Starting | WaitingUser(_),
            WaitingUser(_)
                | ExchangingCredential
                | ValidatingCredential
                | AuthFailed
                | HumanRequired(_)
                | ProviderUnavailable
                | ClientUpdateRequired
                | Expired
                | Cancelled
        ) | (
            ExchangingCredential,
            WaitingUser(_)
                | ValidatingCredential
                | AuthFailed
                | HumanRequired(_)
                | ProviderUnavailable
                | ClientUpdateRequired
                | Expired
                | Cancelled
        ) | (
            ValidatingCredential | Refreshing,
            Authenticated
                | AuthFailed
                | HumanRequired(_)
                | ProviderUnavailable
                | ClientUpdateRequired
                | Expired
                | Cancelled
        ) | (Authenticated, Refreshing | Expired)
    )
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum AuthSessionError {
    #[error("authentication session expiry must follow creation")]
    InvalidExpiry,
    #[error("authentication session timestamp moved backwards")]
    TimestampRegression,
    #[error("authentication session state transition is invalid")]
    InvalidTransition,
    #[error("authentication session has expired")]
    SessionExpired,
    #[error("authentication session revision is exhausted")]
    RevisionExhausted,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WaitingUserState {
    QrScan,
    QrConfirm,
    BrowserCallback,
    SmsCode,
    SessionImport,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HumanRequiredReason {
    AuthRequired,
    SessionExpired,
    QrRequired,
    SmsVerification,
    ImageCaptcha,
    BrowserCallbackRequired,
    SessionImportRequired,
    UserConfirmation,
    BrowserRequired,
    UnsupportedTask,
    RemoteChanged,
    ManualIntervention,
}

#[cfg(test)]
mod auth_session_tests {
    use chrono::{Duration, Utc};

    use super::*;

    #[test]
    fn auth_session_follows_observable_happy_path() {
        let now = Utc::now();
        let mut session = AuthSession::starting(
            UserId::new(),
            ProviderAccountId::new(),
            AuthMethod::ImportedCookie,
            now,
            now + Duration::minutes(10),
        )
        .unwrap();
        session
            .transition(
                AuthState::WaitingUser(WaitingUserState::SessionImport),
                now + Duration::seconds(1),
            )
            .unwrap();
        session
            .transition(AuthState::ValidatingCredential, now + Duration::seconds(2))
            .unwrap();
        session
            .transition(AuthState::Authenticated, now + Duration::seconds(3))
            .unwrap();
        assert_eq!(session.revision, 4);
        assert!(!session.is_expired_at(now + Duration::minutes(9)));
    }

    #[test]
    fn auth_session_rejects_skips_regressions_and_terminal_revival() {
        let now = Utc::now();
        let mut session = AuthSession::starting(
            UserId::new(),
            ProviderAccountId::new(),
            AuthMethod::Password,
            now,
            now + Duration::minutes(10),
        )
        .unwrap();
        assert_eq!(
            session.transition(AuthState::Authenticated, now),
            Err(AuthSessionError::InvalidTransition)
        );
        assert_eq!(
            session.transition(AuthState::ValidatingCredential, now + Duration::minutes(10)),
            Err(AuthSessionError::SessionExpired)
        );
        session.transition(AuthState::Cancelled, now).unwrap();
        assert_eq!(
            session.transition(AuthState::Starting, now),
            Err(AuthSessionError::InvalidTransition)
        );
        assert_eq!(
            session.transition(AuthState::Expired, now - Duration::seconds(1)),
            Err(AuthSessionError::TimestampRegression)
        );
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "type", content = "id")]
pub enum AuditActor {
    User(UserId),
    ServiceToken(ServiceTokenId),
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct WebSession {
    pub id: WebSessionId,
    pub user_id: UserId,
    pub created_at: Timestamp,
    pub expires_at: Timestamp,
    pub revoked_at: Option<Timestamp>,
    pub last_used_at: Option<Timestamp>,
}

impl WebSession {
    pub fn is_active_at(&self, now: Timestamp) -> bool {
        self.revoked_at.is_none() && self.expires_at > now
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceScope {
    SystemRead,
    ProviderRead,
    ProviderManage,
    TaskRead,
    TaskExecute,
    CreditRead,
    CreditManage,
    AuditRead,
    ServiceTokenManage,
    QqIdentityAssert,
    TaskCommandProxy,
    NotificationDeliveryReport,
    BindingVerify,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ServiceToken {
    pub id: ServiceTokenId,
    pub owner_user_id: Option<UserId>,
    pub name: String,
    pub scopes: BTreeSet<ServiceScope>,
    pub created_at: Timestamp,
    pub expires_at: Option<Timestamp>,
    pub revoked_at: Option<Timestamp>,
    pub last_used_at: Option<Timestamp>,
}

impl ServiceToken {
    pub fn is_active_at(&self, now: Timestamp) -> bool {
        self.revoked_at.is_none() && self.expires_at.is_none_or(|expires_at| expires_at > now)
    }
}
