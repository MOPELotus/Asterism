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

    /// Web session lifetime in seconds.
    #[arg(long, env = "ASTERISM_SESSION_TTL_SECONDS", default_value_t = 43_200)]
    session_ttl_seconds: u64,

    /// Mark session cookies Secure (required for non-loopback listeners).
    #[arg(long, env = "ASTERISM_SECURE_COOKIES", default_value_t = false)]
    secure_cookies: bool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("asterism=info")),
        )
        .init();

    let arguments = Arguments::parse();
    if !arguments.bind.ip().is_loopback() {
        anyhow::bail!(
            "non-loopback listeners are unavailable during Phase 0; bind to loopback and place an HTTPS reverse proxy on the same host"
        );
    }
    if arguments.session_ttl_seconds == 0 {
        anyhow::bail!("--session-ttl-seconds must be greater than zero");
    }
    let database = Database::connect(&arguments.database_url)
        .await
        .context("failed to connect to the Asterism database")?;
    database
        .migrate()
        .await
        .context("failed to migrate the Asterism database")?;
    let recovery = database
        .recover_stale_work(chrono::Utc::now())
        .await
        .context("failed to recover stale Asterism work")?;
    tracing::info!(
        executions = recovery.executions_marked_recovering,
        execution_leases = recovery.expired_execution_leases_removed,
        scheduler_claims = recovery.scheduler_claims_requeued,
        "startup recovery completed"
    );

    let app = build_router(ApiState::new(
        database.clone(),
        Arc::new(ProviderRegistry::default()),
        arguments.session_ttl_seconds,
        arguments.secure_cookies,
    ));
    let listener = tokio::net::TcpListener::bind(arguments.bind)
        .await
        .context("failed to bind the Asterism HTTP listener")?;
    tracing::info!(address = %arguments.bind, "asterismd started");

    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
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
