use std::io::{self, Write};

use anyhow::Context;
use asterism_capture::CaptureClient;
use clap::{Parser, Subcommand};

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
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let arguments = Arguments::parse();
    let client = CaptureClient::new(&arguments.url, arguments.allow_insecure_loopback)?;
    match arguments.command {
        Command::Doctor => {
            let health = client.health().await?;
            let stdout = io::stdout();
            let mut output = stdout.lock();
            serde_json::to_writer_pretty(&mut output, &health)
                .context("failed to write the health summary")?;
            writeln!(output).context("failed to finish the health summary")?;
        }
    }
    Ok(())
}
