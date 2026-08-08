use anyhow::{Context, bail};
use clap::{Parser, Subcommand};
use reqwest::StatusCode;
use serde::de::DeserializeOwned;

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
    /// Inspect service state.
    System {
        #[command(subcommand)]
        command: SystemCommand,
    },
    /// Inspect registered providers and their capabilities.
    Provider {
        #[command(subcommand)]
        command: ProviderCommand,
    },
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

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let arguments = Arguments::parse();
    let client = ApiClient::new(&arguments.url);
    let value = match arguments.command {
        Command::System {
            command: SystemCommand::Health,
        } => {
            client
                .get::<serde_json::Value>("/api/v1/system/health")
                .await?
        }
        Command::Provider {
            command: ProviderCommand::List,
        } => client.get::<serde_json::Value>("/api/v1/providers").await?,
    };
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}

#[derive(Debug)]
struct ApiClient {
    base_url: String,
    http: reqwest::Client,
}

impl ApiClient {
    fn new(base_url: &str) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_owned(),
            http: reqwest::Client::new(),
        }
    }

    async fn get<T: DeserializeOwned>(&self, path: &str) -> anyhow::Result<T> {
        let url = format!("{}{path}", self.base_url);
        let response = self
            .http
            .get(&url)
            .send()
            .await
            .with_context(|| format!("failed to request {url}"))?;
        let status = response.status();
        let bytes = response.bytes().await.context("failed to read response")?;
        if status != StatusCode::OK {
            bail!(
                "Asterism API returned {status}: {}",
                String::from_utf8_lossy(&bytes)
            );
        }
        serde_json::from_slice(&bytes).context("Asterism API returned invalid JSON")
    }
}
