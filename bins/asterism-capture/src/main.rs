mod automatic;
mod input;
mod manual;

use std::io::{self, Write};

use anyhow::{Context, bail};
use asterism_capture::CaptureClient;
use clap::{Parser, Subcommand};
use serde::Serialize;

use crate::automatic::{AutomaticCommand, AutomaticCredentialSummary, run_automatic};
use crate::manual::{ManualCommand, ManualCredentialSummary, run_manual};

#[derive(Debug, Parser)]
#[command(version, about)]
struct Arguments {
    /// Public base URL of the Asterism server.
    #[arg(long, env = "ASTERISM_URL")]
    url: String,

    /// Permit cleartext HTTP only when the server host is loopback.
    #[arg(long)]
    allow_insecure_loopback: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Verify outbound connectivity and inspect the non-secret server health summary.
    Doctor,

    /// Claim a pairing and submit manually entered credentials without secret argv values.
    Manual(ManualCommand),

    /// Execute the Provider's frozen browser recipe in an isolated Chromium profile.
    Automatic(AutomaticCommand),
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let arguments = Arguments::parse();
    let client = CaptureClient::new(&arguments.url, arguments.allow_insecure_loopback)?;
    match arguments.command {
        Command::Doctor => {
            let health = client.health().await?;
            write_json(&health)?;
        }
        Command::Manual(command) => {
            write_json(&run_manual_until_cancel(&client, command).await?)?;
        }
        Command::Automatic(command) => {
            write_json(&run_automatic_until_cancel(&client, command).await?)?;
        }
    }
    Ok(())
}

async fn run_automatic_until_cancel(
    client: &CaptureClient,
    command: AutomaticCommand,
) -> anyhow::Result<AutomaticCredentialSummary> {
    tokio::select! {
        result = run_automatic(client, command) => result,
        signal = tokio::signal::ctrl_c() => {
            signal.context("failed to listen for local cancellation")?;
            bail!("automatic Capture workflow cancelled locally; browser secrets were discarded and the server session remains owner-controlled")
        }
    }
}

async fn run_manual_until_cancel(
    client: &CaptureClient,
    command: ManualCommand,
) -> anyhow::Result<ManualCredentialSummary> {
    tokio::select! {
        result = run_manual(client, command) => result,
        signal = tokio::signal::ctrl_c() => {
            signal.context("failed to listen for local cancellation")?;
            bail!("manual Capture workflow cancelled locally; the server session remains owner-controlled")
        }
    }
}

fn write_json(value: &impl Serialize) -> anyhow::Result<()> {
    let stdout = io::stdout();
    let mut output = stdout.lock();
    serde_json::to_writer_pretty(&mut output, value).context("failed to write command output")?;
    writeln!(output).context("failed to finish command output")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::*;

    #[test]
    fn manual_cli_never_accepts_secret_values_as_arguments() {
        let mut command = Arguments::command();
        let manual = command.find_subcommand_mut("manual").unwrap();
        let help = manual.render_long_help().to_string();

        assert!(!help.contains("--pairing-token"));
        assert!(!help.contains("--credential-value"));
        assert!(!help.contains("--tenant-value"));
        let automatic = command.find_subcommand_mut("automatic").unwrap();
        let help = automatic.render_long_help().to_string();
        assert!(!help.contains("--pairing-token"));
        assert!(!help.contains("--credential-value"));
    }
}
