use std::{net::SocketAddr, sync::Arc};

use anyhow::Context;
use asterism_api::{ApiState, build_router};
use asterism_provider_api::ProviderRegistry;
use asterism_storage::Database;
use clap::Parser;
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(version, about)]
struct Arguments {
    /// Address on which the HTTP API listens.
    #[arg(long, env = "ASTERISM_BIND", default_value = "127.0.0.1:8068")]
    bind: SocketAddr,

    /// SQLx-compatible `SQLite` URL.
    #[arg(
        long,
        env = "ASTERISM_DATABASE_URL",
        default_value = "sqlite://asterism.db"
    )]
    database_url: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("asterism=info")),
        )
        .init();

    let arguments = Arguments::parse();
    let database = Database::connect(&arguments.database_url)
        .await
        .context("failed to connect to the Asterism database")?;
    database
        .migrate()
        .await
        .context("failed to migrate the Asterism database")?;

    let app = build_router(ApiState {
        database: database.clone(),
        providers: Arc::new(ProviderRegistry::default()),
    });
    let listener = tokio::net::TcpListener::bind(arguments.bind)
        .await
        .context("failed to bind the Asterism HTTP listener")?;
    tracing::info!(address = %arguments.bind, "asterismd started");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("Asterism HTTP server failed")?;
    database.close().await;
    tracing::info!("asterismd stopped");
    Ok(())
}

async fn shutdown_signal() {
    if let Err(error) = tokio::signal::ctrl_c().await {
        tracing::error!(%error, "failed to install the shutdown signal handler");
    }
}
