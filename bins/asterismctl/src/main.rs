mod client;
mod input;

use std::collections::BTreeSet;

use anyhow::Context;
use asterism_domain::ServiceScope;
use clap::{Args, Parser, Subcommand, ValueEnum};
use reqwest::StatusCode;
use serde::Serialize;
use serde_json::json;

use crate::{
    client::{ApiClient, CreateServiceTokenRequest, write_json},
    input::{PasswordMode, read_password, service_token_from_process},
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
    }
}

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
}
