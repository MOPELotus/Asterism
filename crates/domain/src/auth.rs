use serde::{Deserialize, Serialize};

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
