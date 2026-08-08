use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::{ServiceTokenId, Timestamp, UserId, WebSessionId};

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
