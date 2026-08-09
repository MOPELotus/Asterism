mod client;
mod input;

use std::collections::BTreeSet;

use anyhow::Context;
use asterism_domain::{AuthMethod, NormalizedAnswer, ServiceScope, SessionKind};
use asterism_secrets::{CredentialAcquisition, SecretPurpose};
use clap::{Args, Parser, Subcommand, ValueEnum};
use reqwest::StatusCode;
use serde::Serialize;
use serde_json::json;

use crate::{
    client::{ApiClient, CreateServiceTokenRequest, write_json},
    input::{PasswordMode, read_credential_values, read_password, service_token_from_process},
};

const DEFAULT_TOKEN_TTL_SECONDS: u64 = 30 * 24 * 60 * 60;

#[derive(Debug, Parser)]
#[command(version, about)]
struct Arguments {
    /// Base URL of a running asterismd instance.
    #[arg(long, env = "ASTERISM_URL", default_value = "http://127.0.0.1:8068")]
    url: String,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Create the one-time initial Master and issue the first CLI token.
    Init(PasswordTokenCommand),
    /// Inspect or establish an authenticated identity.
    Auth {
        #[command(subcommand)]
        command: AuthCommand,
    },
    /// Inspect registered providers and their capabilities.
    Provider {
        #[command(subcommand)]
        command: ProviderCommand,
    },
    /// Manage owner-scoped Provider accounts.
    ProviderAccount {
        #[command(subcommand)]
        command: ProviderAccountCommand,
    },
    /// Create or revoke scoped service tokens.
    ServiceToken {
        #[command(subcommand)]
        command: ServiceTokenCommand,
    },
    /// Inspect service state.
    System {
        #[command(subcommand)]
        command: SystemCommand,
    },
    /// Inspect owner-scoped tasks discovered by Provider scans.
    Task {
        #[command(subcommand)]
        command: TaskCommand,
    },
    /// Inspect owner-scoped execution state and progress.
    Execution {
        #[command(subcommand)]
        command: ExecutionCommand,
    },
}

#[derive(Debug, Subcommand)]
enum AuthCommand {
    /// Authenticate with a password and issue a new CLI token.
    Login(PasswordTokenCommand),
    /// Show the identity associated with `ASTERISM_TOKEN`.
    Whoami,
}

#[derive(Debug, Subcommand)]
enum SystemCommand {
    /// Check daemon and database health.
    Health,
}

#[derive(Debug, Subcommand)]
enum ProviderCommand {
    /// List registered provider metadata.
    List,
}

#[derive(Debug, Subcommand)]
enum ProviderAccountCommand {
    /// List accounts owned by the authenticated user.
    List,
    /// Get one owned account.
    Get { account_id: String },
    /// Create an account using a canonical Provider ID.
    Create {
        #[arg(long)]
        provider: String,
        #[arg(long)]
        name: String,
        #[arg(long)]
        tenant: Option<String>,
    },
    /// Replace the mutable account metadata.
    Update {
        account_id: String,
        #[arg(long)]
        name: String,
        #[arg(long)]
        tenant: Option<String>,
    },
    /// Delete an owned account and its dependent local data.
    Delete { account_id: String },
    /// Collect and commit the account's current Provider inventory.
    Scan { account_id: String },
    /// Inspect or configure the account's periodic scan schedule.
    Schedule {
        #[command(subcommand)]
        command: ScanScheduleCommand,
    },
    /// Validate and replace account credentials.
    Credential {
        #[command(subcommand)]
        command: CredentialCommand,
    },
    /// Start, inspect, or cancel Provider authentication sessions.
    Auth {
        #[command(subcommand)]
        command: ProviderAuthCommand,
    },
}

#[derive(Debug, Subcommand)]
enum CredentialCommand {
    /// Read credential values from hidden prompts or stdin and complete an authentication session.
    Import(CredentialImportCommand),
}

#[derive(Debug, Subcommand)]
enum ProviderAuthCommand {
    /// Start one server-TTL authentication attempt.
    Start {
        account_id: String,
        #[arg(long, value_enum)]
        method: CliAuthMethod,
    },
    /// Read the latest attempt, or one exact session when `--session` is set.
    Status {
        account_id: String,
        #[arg(long)]
        session: Option<String>,
    },
    /// Cancel one active authentication attempt.
    Cancel {
        account_id: String,
        session_id: String,
    },
}

#[derive(Debug, Args)]
struct CredentialImportCommand {
    account_id: String,
    /// Authentication session returned by `provider-account auth start`.
    #[arg(long)]
    session: String,
    /// Credential field to submit; repeat in the same order as the prompted or piped values.
    #[arg(long = "purpose", value_enum, required = true)]
    purposes: Vec<CliCredentialPurpose>,
    #[arg(long, value_enum)]
    auth_method: CliAuthMethod,
    #[arg(long, value_enum)]
    session_kind: CliSessionKind,
    #[arg(long, value_enum, default_value = "manual-import")]
    acquired_via: CliCredentialAcquisition,
    #[arg(long)]
    expires_at: Option<String>,
}

#[derive(Debug, Subcommand)]
enum ScanScheduleCommand {
    /// Get the effective schedule for an owned account.
    Get { account_id: String },
    /// Set a schedule; omit the interval to snapshot the current Provider/account default.
    Set {
        account_id: String,
        #[arg(long)]
        interval_seconds: Option<u64>,
        /// Persist the schedule in a disabled state.
        #[arg(long)]
        disabled: bool,
    },
}

#[derive(Debug, Subcommand)]
enum ServiceTokenCommand {
    /// Issue a scoped token using `ASTERISM_TOKEN`.
    Create(TokenOptions),
    /// Revoke a token by ID using `ASTERISM_TOKEN`.
    Revoke {
        /// Service token ID returned when the token was created.
        token_id: String,
    },
}

#[derive(Debug, Subcommand)]
enum TaskCommand {
    /// List tasks with optional account filtering and pagination.
    List {
        #[arg(long)]
        account: Option<String>,
        #[arg(long, default_value_t = 50)]
        limit: u32,
        #[arg(long, default_value_t = 0)]
        offset: u64,
    },
    /// Get one task by ID.
    Get { task_id: String },
    /// Rediscover and get one task's current sanitized Provider detail.
    Detail { task_id: String },
    /// Read one task's current normalized Provider progress.
    Progress { task_id: String },
    /// Discover and parse one task's complete current Provider Question set.
    Questions { task_id: String },
    /// Resolve Provider-native candidates for one immutable Question snapshot.
    ResolveAnswers {
        task_id: String,
        snapshot_id: String,
    },
    /// Read persisted multi-source candidates for one Question snapshot.
    AnswerCandidates {
        task_id: String,
        snapshot_id: String,
    },
    /// Persist one Core-attributed Manual candidate for a Question.
    AddManualAnswer {
        task_id: String,
        snapshot_id: String,
        question_id: String,
        /// A typed `NormalizedAnswer` JSON value, for example {"type":"boolean","value":true}.
        #[arg(long)]
        answer: String,
        /// Optional confidence from 0 to 10000 basis points.
        #[arg(long)]
        confidence_basis_points: Option<u16>,
        #[arg(long)]
        explanation: Option<String>,
    },
    /// Derive a conservative, non-persisted selection plan from stored candidates.
    AnswerResolution {
        task_id: String,
        snapshot_id: String,
    },
    /// Build one reviewable draft from exactly one persisted Candidate per Question.
    BuildSubmission {
        task_id: String,
        snapshot_id: String,
        #[arg(required = true, num_args = 1..)]
        candidate_ids: Vec<String>,
    },
    /// Read one persisted `SubmissionDraft` without calling the Provider.
    SubmissionDraft {
        task_id: String,
        snapshot_id: String,
        draft_id: String,
    },
    /// Read one persisted verified `SubmissionResult` without calling the Provider.
    SubmissionResult {
        task_id: String,
        snapshot_id: String,
        draft_id: String,
        result_id: String,
    },
    /// Schedule one task through the shared idempotent Core Action.
    Execute {
        task_id: String,
        /// Stable caller-provided key. Reuse it when retrying the same request.
        #[arg(long)]
        idempotency_key: String,
    },
}

#[derive(Debug, Subcommand)]
enum ExecutionCommand {
    /// List owner-scoped Executions, optionally filtered by Task.
    List {
        #[arg(long)]
        task: Option<String>,
        #[arg(long, default_value_t = 50)]
        limit: u32,
        #[arg(long, default_value_t = 0)]
        offset: u64,
    },
    /// Get one Execution with current progress and Attempt history.
    Get { execution_id: String },
    /// List one Execution's sanitized logs in chronological order.
    Logs {
        execution_id: String,
        #[arg(long, default_value_t = 50)]
        limit: u32,
        #[arg(long, default_value_t = 0)]
        offset: u64,
    },
}

#[derive(Debug, Args)]
struct PasswordTokenCommand {
    /// Master username.
    #[arg(long)]
    username: String,

    /// Read one password line from stdin instead of prompting on the terminal.
    #[arg(long)]
    password_stdin: bool,

    #[command(flatten)]
    token: TokenOptions,
}

#[derive(Debug, Args)]
struct TokenOptions {
    /// Human-readable token name.
    #[arg(long, default_value = "asterismctl")]
    name: String,

    /// Token scope. Repeat the option or use comma-separated values.
    #[arg(long = "scope", value_enum, value_delimiter = ',')]
    scopes: Vec<CliScope>,

    /// Token lifetime. The safe default is 30 days.
    #[arg(long, default_value_t = DEFAULT_TOKEN_TTL_SECONDS)]
    expires_in_seconds: u64,
}

impl TokenOptions {
    fn into_request(self) -> CreateServiceTokenRequest {
        let scopes = if self.scopes.is_empty() {
            default_cli_scopes()
        } else {
            self.scopes.into_iter().map(ServiceScope::from).collect()
        };
        CreateServiceTokenRequest {
            name: self.name,
            scopes,
            expires_in_seconds: Some(self.expires_in_seconds),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum CliScope {
    SystemRead,
    ProviderRead,
    ProviderManage,
    TaskRead,
    TaskExecute,
    CreditRead,
    CreditManage,
    AuditRead,
    ServiceTokenManage,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, ValueEnum)]
enum CliCredentialPurpose {
    #[value(name = "provider-username")]
    Username,
    #[value(name = "provider-password")]
    Password,
    #[value(name = "provider-cookie")]
    Cookie,
    #[value(name = "provider-access-token")]
    AccessToken,
    #[value(name = "provider-refresh-token")]
    RefreshToken,
    #[value(name = "provider-composite-session")]
    CompositeSession,
}

impl From<CliCredentialPurpose> for SecretPurpose {
    fn from(purpose: CliCredentialPurpose) -> Self {
        match purpose {
            CliCredentialPurpose::Username => Self::ProviderUsername,
            CliCredentialPurpose::Password => Self::ProviderPassword,
            CliCredentialPurpose::Cookie => Self::ProviderCookie,
            CliCredentialPurpose::AccessToken => Self::ProviderAccessToken,
            CliCredentialPurpose::RefreshToken => Self::ProviderRefreshToken,
            CliCredentialPurpose::CompositeSession => Self::ProviderCompositeSession,
        }
    }
}

impl CliCredentialPurpose {
    const fn prompt(self) -> &'static str {
        match self {
            Self::Username => "Provider username",
            Self::Password => "Provider password",
            Self::Cookie => "Provider cookie",
            Self::AccessToken => "Provider access token",
            Self::RefreshToken => "Provider refresh token",
            Self::CompositeSession => "Provider composite session",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum CliAuthMethod {
    Password,
    QrCode,
    ExternalBrowserOauth,
    AssistedSession,
    ImportedCookie,
    ImportedToken,
}

impl From<CliAuthMethod> for AuthMethod {
    fn from(method: CliAuthMethod) -> Self {
        match method {
            CliAuthMethod::Password => Self::Password,
            CliAuthMethod::QrCode => Self::QrCode,
            CliAuthMethod::ExternalBrowserOauth => Self::ExternalBrowserOauth,
            CliAuthMethod::AssistedSession => Self::AssistedSession,
            CliAuthMethod::ImportedCookie => Self::ImportedCookie,
            CliAuthMethod::ImportedToken => Self::ImportedToken,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum CliSessionKind {
    Cookie,
    BearerToken,
    Jwt,
    Composite,
    ProviderSpecific,
}

impl From<CliSessionKind> for SessionKind {
    fn from(kind: CliSessionKind) -> Self {
        match kind {
            CliSessionKind::Cookie => Self::Cookie,
            CliSessionKind::BearerToken => Self::BearerToken,
            CliSessionKind::Jwt => Self::Jwt,
            CliSessionKind::Composite => Self::Composite,
            CliSessionKind::ProviderSpecific => Self::ProviderSpecific,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum CliCredentialAcquisition {
    NativeProviderLogin,
    CaptureTool,
    BrowserExtension,
    AndroidHelper,
    ManualImport,
}

impl From<CliCredentialAcquisition> for CredentialAcquisition {
    fn from(acquisition: CliCredentialAcquisition) -> Self {
        match acquisition {
            CliCredentialAcquisition::NativeProviderLogin => Self::NativeProviderLogin,
            CliCredentialAcquisition::CaptureTool => Self::CaptureTool,
            CliCredentialAcquisition::BrowserExtension => Self::BrowserExtension,
            CliCredentialAcquisition::AndroidHelper => Self::AndroidHelper,
            CliCredentialAcquisition::ManualImport => Self::ManualImport,
        }
    }
}

impl From<CliScope> for ServiceScope {
    fn from(scope: CliScope) -> Self {
        match scope {
            CliScope::SystemRead => Self::SystemRead,
            CliScope::ProviderRead => Self::ProviderRead,
            CliScope::ProviderManage => Self::ProviderManage,
            CliScope::TaskRead => Self::TaskRead,
            CliScope::TaskExecute => Self::TaskExecute,
            CliScope::CreditRead => Self::CreditRead,
            CliScope::CreditManage => Self::CreditManage,
            CliScope::AuditRead => Self::AuditRead,
            CliScope::ServiceTokenManage => Self::ServiceTokenManage,
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let arguments = Arguments::parse();
    let client = ApiClient::new(&arguments.url)?;
    match arguments.command {
        Command::Init(command) => {
            issue_from_password(&client, "/api/v1/auth/bootstrap", command, true).await
        }
        Command::Auth {
            command: AuthCommand::Login(command),
        } => issue_from_password(&client, "/api/v1/auth/login", command, false).await,
        Command::Auth {
            command: AuthCommand::Whoami,
        } => {
            let token = service_token_from_process()?;
            let value = client
                .get_authorized("/api/v1/auth/session", &token)
                .await?;
            write_json(&value)
        }
        Command::Provider {
            command: ProviderCommand::List,
        } => {
            let token = service_token_from_process()?;
            let value = client.get_authorized("/api/v1/providers", &token).await?;
            write_json(&value)
        }
        Command::ProviderAccount { command } => handle_provider_account(&client, command).await,
        Command::ServiceToken {
            command: ServiceTokenCommand::Create(options),
        } => {
            let token = service_token_from_process()?;
            let issued = client
                .create_service_token_with_bearer(&token, &options.into_request())
                .await?;
            eprintln!(
                "The service token is shown once; store it in a secret manager and remove it from terminal history."
            );
            write_json(&issued)
        }
        Command::ServiceToken {
            command: ServiceTokenCommand::Revoke { token_id },
        } => {
            let token = service_token_from_process()?;
            client.revoke_service_token(&token, &token_id).await?;
            write_json(&json!({ "revoked": token_id }))
        }
        Command::System {
            command: SystemCommand::Health,
        } => {
            let value = client.get_public("/api/v1/system/health").await?;
            write_json(&value)
        }
        Command::Task { command } => handle_task(&client, command).await,
        Command::Execution { command } => handle_execution(&client, command).await,
    }
}

async fn handle_provider_account(
    client: &ApiClient,
    command: ProviderAccountCommand,
) -> anyhow::Result<()> {
    let token = service_token_from_process()?;
    match command {
        ProviderAccountCommand::List => {
            let value = client
                .get_authorized("/api/v1/provider-accounts", &token)
                .await?;
            write_json(&value)
        }
        ProviderAccountCommand::Get { account_id } => {
            let path = format!("/api/v1/provider-accounts/{account_id}");
            let value = client.get_authorized(&path, &token).await?;
            write_json(&value)
        }
        ProviderAccountCommand::Create {
            provider,
            name,
            tenant,
        } => {
            let value = client
                .post_authorized(
                    "/api/v1/provider-accounts",
                    &token,
                    &json!({
                        "provider_id": provider,
                        "display_name": name,
                        "tenant": tenant,
                    }),
                    StatusCode::CREATED,
                )
                .await?;
            write_json(&value)
        }
        ProviderAccountCommand::Update {
            account_id,
            name,
            tenant,
        } => {
            let path = format!("/api/v1/provider-accounts/{account_id}");
            let value = client
                .put_authorized(
                    &path,
                    &token,
                    &json!({
                        "display_name": name,
                        "tenant": tenant,
                    }),
                )
                .await?;
            write_json(&value)
        }
        ProviderAccountCommand::Delete { account_id } => {
            let path = format!("/api/v1/provider-accounts/{account_id}");
            client.delete_authorized(&path, &token).await?;
            write_json(&json!({ "deleted": account_id }))
        }
        ProviderAccountCommand::Scan { account_id } => {
            let path = format!("/api/v1/provider-accounts/{account_id}/scan");
            let value = client.post_authorized_empty(&path, &token).await?;
            write_json(&value)
        }
        ProviderAccountCommand::Schedule { command } => match command {
            ScanScheduleCommand::Get { account_id } => {
                let path = format!("/api/v1/provider-accounts/{account_id}/scan-schedule");
                let value = client.get_authorized(&path, &token).await?;
                write_json(&value)
            }
            ScanScheduleCommand::Set {
                account_id,
                interval_seconds,
                disabled,
            } => {
                let path = format!("/api/v1/provider-accounts/{account_id}/scan-schedule");
                let mut payload = json!({"enabled": !disabled});
                if let Some(interval_seconds) = interval_seconds {
                    payload["desired_interval_seconds"] = json!(interval_seconds);
                }
                let value = client.put_authorized(&path, &token, &payload).await?;
                write_json(&value)
            }
        },
        ProviderAccountCommand::Credential {
            command: CredentialCommand::Import(command),
        } => handle_credential_import(client, &token, command).await,
        ProviderAccountCommand::Auth { command } => {
            handle_provider_auth(client, &token, command).await
        }
    }
}

async fn handle_provider_auth(
    client: &ApiClient,
    token: &asterism_secrets::SecretString,
    command: ProviderAuthCommand,
) -> anyhow::Result<()> {
    let value = match command {
        ProviderAuthCommand::Start { account_id, method } => {
            let path = format!("/api/v1/provider-accounts/{account_id}/auth-sessions");
            client
                .post_authorized(
                    &path,
                    token,
                    &json!({ "method": AuthMethod::from(method) }),
                    StatusCode::CREATED,
                )
                .await?
        }
        ProviderAuthCommand::Status {
            account_id,
            session,
        } => {
            let path = match session {
                Some(session_id) => {
                    format!("/api/v1/provider-accounts/{account_id}/auth-sessions/{session_id}")
                }
                None => format!("/api/v1/provider-accounts/{account_id}/auth-sessions/latest"),
            };
            client.get_authorized(&path, token).await?
        }
        ProviderAuthCommand::Cancel {
            account_id,
            session_id,
        } => {
            let path = format!("/api/v1/provider-accounts/{account_id}/auth-sessions/{session_id}");
            client.delete_authorized_json(&path, token).await?
        }
    };
    write_json(&value)
}

async fn handle_credential_import(
    client: &ApiClient,
    token: &asterism_secrets::SecretString,
    command: CredentialImportCommand,
) -> anyhow::Result<()> {
    ensure_unique_credential_purposes(&command.purposes)?;
    let prompts = command
        .purposes
        .iter()
        .copied()
        .map(CliCredentialPurpose::prompt)
        .collect::<Vec<_>>();
    let values = read_credential_values(&prompts)?;
    let path = format!(
        "/api/v1/provider-accounts/{}/auth-sessions/{}/credentials",
        command.account_id, command.session
    );
    let response = {
        let request = PutProviderCredentialsRequest {
            auth_method: command.auth_method.into(),
            acquired_via: command.acquired_via.into(),
            session_kind: command.session_kind.into(),
            expires_at: command.expires_at,
            fields: command
                .purposes
                .iter()
                .copied()
                .zip(&values)
                .map(|(purpose, value)| PutProviderCredentialField {
                    purpose: purpose.into(),
                    value: value.expose_secret(),
                })
                .collect(),
        };
        client.put_authorized(&path, token, &request).await
    };
    drop(values);
    write_json(&response?)
}

fn ensure_unique_credential_purposes(purposes: &[CliCredentialPurpose]) -> anyhow::Result<()> {
    let mut unique = BTreeSet::new();
    for purpose in purposes {
        if !unique.insert(*purpose) {
            anyhow::bail!(
                "credential purpose {} was supplied more than once",
                purpose.prompt()
            );
        }
    }
    Ok(())
}

#[derive(Serialize)]
struct PutProviderCredentialsRequest<'a> {
    auth_method: AuthMethod,
    acquired_via: CredentialAcquisition,
    session_kind: SessionKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    expires_at: Option<String>,
    fields: Vec<PutProviderCredentialField<'a>>,
}

#[derive(Serialize)]
struct PutProviderCredentialField<'a> {
    purpose: SecretPurpose,
    value: &'a str,
}

#[allow(
    clippy::too_many_lines,
    reason = "the CLI keeps every Task subcommand mapped to its explicit versioned HTTP path in one auditable dispatch"
)]
async fn handle_task(client: &ApiClient, command: TaskCommand) -> anyhow::Result<()> {
    let token = service_token_from_process()?;
    let value = match command {
        TaskCommand::List {
            account,
            limit,
            offset,
        } => {
            client
                .get_authorized_with_query(
                    "/api/v1/tasks",
                    &token,
                    &TaskListParameters {
                        provider_account_id: account,
                        limit,
                        offset,
                    },
                )
                .await?
        }
        TaskCommand::Get { task_id } => {
            let path = format!("/api/v1/tasks/{task_id}");
            client.get_authorized(&path, &token).await?
        }
        TaskCommand::Detail { task_id } => {
            let path = format!("/api/v1/tasks/{task_id}/detail");
            client.get_authorized(&path, &token).await?
        }
        TaskCommand::Progress { task_id } => {
            let path = format!("/api/v1/tasks/{task_id}/progress");
            client.get_authorized(&path, &token).await?
        }
        TaskCommand::Questions { task_id } => {
            let path = format!("/api/v1/tasks/{task_id}/questions");
            client.get_authorized(&path, &token).await?
        }
        TaskCommand::ResolveAnswers {
            task_id,
            snapshot_id,
        } => {
            let path = format!(
                "/api/v1/tasks/{task_id}/question-snapshots/{snapshot_id}/provider-answer-candidates"
            );
            client.post_authorized_empty(&path, &token).await?
        }
        TaskCommand::AnswerCandidates {
            task_id,
            snapshot_id,
        } => {
            let path = format!(
                "/api/v1/tasks/{task_id}/question-snapshots/{snapshot_id}/answer-candidates"
            );
            client.get_authorized(&path, &token).await?
        }
        TaskCommand::AddManualAnswer {
            task_id,
            snapshot_id,
            question_id,
            answer,
            confidence_basis_points,
            explanation,
        } => {
            let answer = serde_json::from_str::<NormalizedAnswer>(&answer)
                .context("manual answer must be a valid NormalizedAnswer JSON value")?;
            let path = format!(
                "/api/v1/tasks/{task_id}/question-snapshots/{snapshot_id}/answer-candidates"
            );
            client
                .post_authorized(
                    &path,
                    &token,
                    &json!({
                        "question_id": question_id,
                        "answer": answer,
                        "confidence_basis_points": confidence_basis_points,
                        "explanation": explanation
                    }),
                    StatusCode::CREATED,
                )
                .await?
        }
        TaskCommand::AnswerResolution {
            task_id,
            snapshot_id,
        } => {
            let path = format!(
                "/api/v1/tasks/{task_id}/question-snapshots/{snapshot_id}/answer-resolution"
            );
            client.get_authorized(&path, &token).await?
        }
        TaskCommand::BuildSubmission {
            task_id,
            snapshot_id,
            candidate_ids,
        } => {
            let path = format!(
                "/api/v1/tasks/{task_id}/question-snapshots/{snapshot_id}/submission-drafts"
            );
            client
                .post_authorized(
                    &path,
                    &token,
                    &json!({"answer_candidate_ids": candidate_ids}),
                    StatusCode::CREATED,
                )
                .await?
        }
        TaskCommand::SubmissionDraft {
            task_id,
            snapshot_id,
            draft_id,
        } => {
            let path = format!(
                "/api/v1/tasks/{task_id}/question-snapshots/{snapshot_id}/submission-drafts/{draft_id}"
            );
            client.get_authorized(&path, &token).await?
        }
        TaskCommand::SubmissionResult {
            task_id,
            snapshot_id,
            draft_id,
            result_id,
        } => {
            let path = format!(
                "/api/v1/tasks/{task_id}/question-snapshots/{snapshot_id}/submission-drafts/{draft_id}/results/{result_id}"
            );
            client.get_authorized(&path, &token).await?
        }
        TaskCommand::Execute {
            task_id,
            idempotency_key,
        } => {
            let path = format!("/api/v1/tasks/{task_id}/execute");
            client
                .post_authorized_idempotent(&path, &token, &idempotency_key)
                .await?
        }
    };
    write_json(&value)
}

async fn handle_execution(client: &ApiClient, command: ExecutionCommand) -> anyhow::Result<()> {
    let token = service_token_from_process()?;
    let value = match command {
        ExecutionCommand::List {
            task,
            limit,
            offset,
        } => {
            client
                .get_authorized_with_query(
                    "/api/v1/executions",
                    &token,
                    &ExecutionListParameters {
                        task_id: task,
                        limit,
                        offset,
                    },
                )
                .await?
        }
        ExecutionCommand::Get { execution_id } => {
            let path = format!("/api/v1/executions/{execution_id}");
            client.get_authorized(&path, &token).await?
        }
        ExecutionCommand::Logs {
            execution_id,
            limit,
            offset,
        } => {
            let path = format!("/api/v1/executions/{execution_id}/logs");
            client
                .get_authorized_with_query(&path, &token, &LogListParameters { limit, offset })
                .await?
        }
    };
    write_json(&value)
}

#[derive(Debug, Serialize)]
struct TaskListParameters {
    #[serde(skip_serializing_if = "Option::is_none")]
    provider_account_id: Option<String>,
    limit: u32,
    offset: u64,
}

#[derive(Debug, Serialize)]
struct ExecutionListParameters {
    #[serde(skip_serializing_if = "Option::is_none")]
    task_id: Option<String>,
    limit: u32,
    offset: u64,
}

#[derive(Debug, Serialize)]
struct LogListParameters {
    limit: u32,
    offset: u64,
}

async fn issue_from_password(
    client: &ApiClient,
    authentication_path: &str,
    command: PasswordTokenCommand,
    confirm_password: bool,
) -> anyhow::Result<()> {
    let mode = if command.password_stdin {
        PasswordMode::Stdin
    } else {
        PasswordMode::Terminal {
            confirm: confirm_password,
        }
    };
    let password = read_password(mode)?;
    let session = client
        .establish_session(authentication_path, &command.username, &password)
        .await?;
    let issued = match client
        .create_service_token_with_session(&session, &command.token.into_request())
        .await
    {
        Ok(issued) => issued,
        Err(error) => {
            if let Err(logout_error) = client.logout(&session).await {
                eprintln!("warning: failed to revoke temporary Web Session: {logout_error:#}");
            }
            return Err(error).context("failed to issue CLI service token");
        }
    };

    let logout_result = client.logout(&session).await;
    eprintln!(
        "The service token is shown once; store it in a secret manager and set ASTERISM_TOKEN only when needed."
    );
    write_json(&issued)?;
    logout_result
        .context("the token above was issued, but the temporary Web Session could not be revoked")
}

fn default_cli_scopes() -> BTreeSet<ServiceScope> {
    [
        ServiceScope::SystemRead,
        ServiceScope::ProviderRead,
        ServiceScope::ProviderManage,
        ServiceScope::TaskRead,
        ServiceScope::TaskExecute,
        ServiceScope::CreditRead,
        ServiceScope::CreditManage,
        ServiceScope::AuditRead,
        ServiceScope::ServiceTokenManage,
    ]
    .into_iter()
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_cli_token_has_management_without_integration_scopes() {
        let scopes = default_cli_scopes();
        assert!(scopes.contains(&ServiceScope::SystemRead));
        assert!(scopes.contains(&ServiceScope::ServiceTokenManage));
        assert_eq!(scopes.len(), 9);
    }

    #[test]
    fn explicit_scopes_replace_defaults() {
        let request = TokenOptions {
            name: "read-only".to_owned(),
            scopes: vec![CliScope::ProviderRead],
            expires_in_seconds: 60,
        }
        .into_request();
        assert_eq!(request.scopes, [ServiceScope::ProviderRead].into());
    }

    #[test]
    fn provider_account_scan_command_is_account_scoped() {
        let arguments =
            Arguments::try_parse_from(["asterismctl", "provider-account", "scan", "account-id"])
                .unwrap();
        assert!(matches!(
            arguments.command,
            Command::ProviderAccount {
                command: ProviderAccountCommand::Scan { account_id }
            } if account_id == "account-id"
        ));
    }

    #[test]
    fn task_execute_requires_an_explicit_reusable_idempotency_key() {
        let arguments = Arguments::try_parse_from([
            "asterismctl",
            "task",
            "execute",
            "task-id",
            "--idempotency-key",
            "manual-run-1",
        ])
        .unwrap();
        assert!(matches!(
            arguments.command,
            Command::Task {
                command: TaskCommand::Execute {
                    task_id,
                    idempotency_key,
                }
            } if task_id == "task-id" && idempotency_key == "manual-run-1"
        ));
        assert!(Arguments::try_parse_from(["asterismctl", "task", "execute", "task-id"]).is_err());
    }

    #[test]
    fn task_detail_is_a_distinct_fresh_provider_read() {
        let arguments =
            Arguments::try_parse_from(["asterismctl", "task", "detail", "task-id"]).unwrap();
        assert!(matches!(
            arguments.command,
            Command::Task {
                command: TaskCommand::Detail { task_id }
            } if task_id == "task-id"
        ));
    }

    #[test]
    fn task_progress_is_a_distinct_fresh_provider_read() {
        let arguments =
            Arguments::try_parse_from(["asterismctl", "task", "progress", "task-id"]).unwrap();
        assert!(matches!(
            arguments.command,
            Command::Task {
                command: TaskCommand::Progress { task_id }
            } if task_id == "task-id"
        ));
    }

    #[test]
    fn task_questions_is_one_complete_fresh_provider_read() {
        let arguments =
            Arguments::try_parse_from(["asterismctl", "task", "questions", "task-id"]).unwrap();
        assert!(matches!(
            arguments.command,
            Command::Task {
                command: TaskCommand::Questions { task_id }
            } if task_id == "task-id"
        ));
    }

    #[test]
    fn task_answer_resolution_requires_an_explicit_snapshot() {
        let arguments = Arguments::try_parse_from([
            "asterismctl",
            "task",
            "resolve-answers",
            "task-id",
            "snapshot-id",
        ])
        .unwrap();
        assert!(matches!(
            arguments.command,
            Command::Task {
                command: TaskCommand::ResolveAnswers {
                    task_id,
                    snapshot_id,
                }
            } if task_id == "task-id" && snapshot_id == "snapshot-id"
        ));
    }

    #[test]
    fn task_answer_candidates_read_requires_an_explicit_snapshot() {
        let arguments = Arguments::try_parse_from([
            "asterismctl",
            "task",
            "answer-candidates",
            "task-id",
            "snapshot-id",
        ])
        .unwrap();
        assert!(matches!(
            arguments.command,
            Command::Task {
                command: TaskCommand::AnswerCandidates {
                    task_id,
                    snapshot_id,
                }
            } if task_id == "task-id" && snapshot_id == "snapshot-id"
        ));
    }

    #[test]
    fn task_manual_answer_keeps_binding_and_typed_payload_explicit() {
        let arguments = Arguments::try_parse_from([
            "asterismctl",
            "task",
            "add-manual-answer",
            "task-id",
            "snapshot-id",
            "question-id",
            "--answer",
            r#"{"type":"boolean","value":true}"#,
            "--confidence-basis-points",
            "8500",
        ])
        .unwrap();
        assert!(matches!(
            arguments.command,
            Command::Task {
                command: TaskCommand::AddManualAnswer {
                    task_id,
                    snapshot_id,
                    question_id,
                    answer,
                    confidence_basis_points: Some(8500),
                    explanation: None,
                }
            } if task_id == "task-id"
                && snapshot_id == "snapshot-id"
                && question_id == "question-id"
                && answer == r#"{"type":"boolean","value":true}"#
        ));
    }

    #[test]
    fn task_answer_resolution_plan_requires_an_explicit_snapshot() {
        let arguments = Arguments::try_parse_from([
            "asterismctl",
            "task",
            "answer-resolution",
            "task-id",
            "snapshot-id",
        ])
        .unwrap();
        assert!(matches!(
            arguments.command,
            Command::Task {
                command: TaskCommand::AnswerResolution {
                    task_id,
                    snapshot_id,
                }
            } if task_id == "task-id" && snapshot_id == "snapshot-id"
        ));
    }

    #[test]
    fn task_submission_build_requires_snapshot_and_candidate_identities() {
        let arguments = Arguments::try_parse_from([
            "asterismctl",
            "task",
            "build-submission",
            "task-id",
            "snapshot-id",
            "candidate-a",
            "candidate-b",
        ])
        .unwrap();
        assert!(matches!(
            arguments.command,
            Command::Task {
                command: TaskCommand::BuildSubmission {
                    task_id,
                    snapshot_id,
                    candidate_ids,
                }
            } if task_id == "task-id"
                && snapshot_id == "snapshot-id"
                && candidate_ids == ["candidate-a", "candidate-b"]
        ));
    }

    #[test]
    fn task_submission_draft_read_requires_all_three_identities() {
        let arguments = Arguments::try_parse_from([
            "asterismctl",
            "task",
            "submission-draft",
            "task-id",
            "snapshot-id",
            "draft-id",
        ])
        .unwrap();
        assert!(matches!(
            arguments.command,
            Command::Task {
                command: TaskCommand::SubmissionDraft {
                    task_id,
                    snapshot_id,
                    draft_id,
                }
            } if task_id == "task-id"
                && snapshot_id == "snapshot-id"
                && draft_id == "draft-id"
        ));
    }

    #[test]
    fn task_submission_result_read_requires_the_complete_binding() {
        let arguments = Arguments::try_parse_from([
            "asterismctl",
            "task",
            "submission-result",
            "task-id",
            "snapshot-id",
            "draft-id",
            "result-id",
        ])
        .unwrap();
        assert!(matches!(
            arguments.command,
            Command::Task {
                command: TaskCommand::SubmissionResult {
                    task_id,
                    snapshot_id,
                    draft_id,
                    result_id,
                }
            } if task_id == "task-id"
                && snapshot_id == "snapshot-id"
                && draft_id == "draft-id"
                && result_id == "result-id"
        ));
    }

    #[test]
    fn execution_get_is_a_distinct_owner_scoped_surface() {
        let arguments =
            Arguments::try_parse_from(["asterismctl", "execution", "get", "execution-id"]).unwrap();
        assert!(matches!(
            arguments.command,
            Command::Execution {
                command: ExecutionCommand::Get { execution_id }
            } if execution_id == "execution-id"
        ));
    }

    #[test]
    fn execution_list_keeps_task_filter_and_pagination_explicit() {
        let arguments = Arguments::try_parse_from([
            "asterismctl",
            "execution",
            "list",
            "--task",
            "task-id",
            "--limit",
            "25",
            "--offset",
            "50",
        ])
        .unwrap();
        assert!(matches!(
            arguments.command,
            Command::Execution {
                command: ExecutionCommand::List {
                    task: Some(task),
                    limit: 25,
                    offset: 50,
                }
            } if task == "task-id"
        ));
    }

    #[test]
    fn execution_logs_keep_pagination_explicit() {
        let arguments = Arguments::try_parse_from([
            "asterismctl",
            "execution",
            "logs",
            "execution-id",
            "--limit",
            "25",
            "--offset",
            "50",
        ])
        .unwrap();
        assert!(matches!(
            arguments.command,
            Command::Execution {
                command: ExecutionCommand::Logs {
                    execution_id,
                    limit: 25,
                    offset: 50,
                }
            } if execution_id == "execution-id"
        ));
    }

    #[test]
    fn provider_account_schedule_command_keeps_desired_interval_explicit() {
        let arguments = Arguments::try_parse_from([
            "asterismctl",
            "provider-account",
            "schedule",
            "set",
            "account-id",
            "--interval-seconds",
            "60",
            "--disabled",
        ])
        .unwrap();
        assert!(matches!(
            arguments.command,
            Command::ProviderAccount {
                command: ProviderAccountCommand::Schedule {
                    command: ScanScheduleCommand::Set {
                        account_id,
                        interval_seconds: Some(60),
                        disabled: true,
                    }
                }
            } if account_id == "account-id"
        ));

        let provider_default = Arguments::try_parse_from([
            "asterismctl",
            "provider-account",
            "schedule",
            "set",
            "account-id",
        ])
        .unwrap();
        assert!(matches!(
            provider_default.command,
            Command::ProviderAccount {
                command: ProviderAccountCommand::Schedule {
                    command: ScanScheduleCommand::Set {
                        interval_seconds: None,
                        disabled: false,
                        ..
                    }
                }
            }
        ));
    }

    #[test]
    fn credential_import_has_no_plaintext_command_line_option() {
        let arguments = Arguments::try_parse_from([
            "asterismctl",
            "provider-account",
            "credential",
            "import",
            "account-id",
            "--session",
            "session-id",
            "--purpose",
            "provider-cookie",
            "--auth-method",
            "imported-cookie",
            "--session-kind",
            "cookie",
        ])
        .unwrap();
        assert!(matches!(
            arguments.command,
            Command::ProviderAccount {
                command: ProviderAccountCommand::Credential {
                    command: CredentialCommand::Import(CredentialImportCommand {
                        account_id,
                        session,
                        acquired_via: CliCredentialAcquisition::ManualImport,
                        ..
                    })
                }
            } if account_id == "account-id" && session == "session-id"
        ));
        assert!(
            Arguments::try_parse_from([
                "asterismctl",
                "provider-account",
                "credential",
                "import",
                "account-id",
                "--session",
                "session-id",
                "--purpose",
                "provider-cookie",
                "--auth-method",
                "imported-cookie",
                "--session-kind",
                "cookie",
                "--value",
                "must-not-be-accepted",
            ])
            .is_err()
        );
    }

    #[test]
    fn credential_import_accepts_an_ordered_multi_field_bundle() {
        let arguments = Arguments::try_parse_from([
            "asterismctl",
            "provider-account",
            "credential",
            "import",
            "account-id",
            "--session",
            "session-id",
            "--purpose",
            "provider-username",
            "--purpose",
            "provider-password",
            "--auth-method",
            "password",
            "--session-kind",
            "provider-specific",
            "--acquired-via",
            "native-provider-login",
        ])
        .unwrap();

        assert!(matches!(
            arguments.command,
            Command::ProviderAccount {
                command: ProviderAccountCommand::Credential {
                    command: CredentialCommand::Import(CredentialImportCommand {
                        purposes,
                        acquired_via: CliCredentialAcquisition::NativeProviderLogin,
                        ..
                    })
                }
            } if purposes == [CliCredentialPurpose::Username, CliCredentialPurpose::Password]
        ));
    }

    #[test]
    fn credential_import_rejects_duplicate_purposes_before_input() {
        assert!(
            ensure_unique_credential_purposes(&[
                CliCredentialPurpose::Cookie,
                CliCredentialPurpose::Cookie,
            ])
            .is_err()
        );
    }

    #[test]
    fn provider_auth_status_defaults_to_the_latest_account_attempt() {
        let arguments = Arguments::try_parse_from([
            "asterismctl",
            "provider-account",
            "auth",
            "status",
            "account-id",
        ])
        .unwrap();
        assert!(matches!(
            arguments.command,
            Command::ProviderAccount {
                command: ProviderAccountCommand::Auth {
                    command: ProviderAuthCommand::Status {
                        account_id,
                        session: None,
                    }
                }
            } if account_id == "account-id"
        ));
    }
}
