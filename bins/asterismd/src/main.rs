use std::{net::SocketAddr, path::PathBuf, sync::Arc};

use anyhow::Context;
use asterism_api::{ApiState, build_router};
use asterism_config::{
    Config, ConfigFile, ConfigOverrides, DEFAULT_CONFIG_FILE, DatabaseOverrides, Environment,
    ServerOverrides,
};
use asterism_provider_api::ProviderRegistry;
use asterism_storage::Database;
use clap::Parser;
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(version, about)]
struct Arguments {
    /// TOML configuration file. A missing default file is allowed.
    #[arg(long)]
    config: Option<PathBuf>,

    /// Address on which the HTTP API listens.
    #[arg(long)]
    bind: Option<SocketAddr>,

    /// SQLx-compatible `SQLite` URL.
    #[arg(long)]
    database_url: Option<String>,

    /// Web session lifetime in seconds.
    #[arg(long)]
    session_ttl_seconds: Option<u64>,

    /// Mark session cookies Secure.
    #[arg(long, num_args = 0..=1, default_missing_value = "true")]
    secure_cookies: Option<bool>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("asterism=info")),
        )
        .init();

    let arguments = Arguments::parse();
    let environment = Environment::from_process().context("failed to read Asterism environment")?;
    let config_file = arguments
        .config
        .map(ConfigFile::required)
        .or_else(|| {
            environment
                .config_file()
                .map(|path| ConfigFile::required(path.to_owned()))
        })
        .unwrap_or_else(|| ConfigFile::optional(DEFAULT_CONFIG_FILE));
    let overrides = ConfigOverrides {
        server: ServerOverrides {
            bind: arguments.bind,
            session_ttl_seconds: arguments.session_ttl_seconds,
            secure_cookies: arguments.secure_cookies,
        },
        database: DatabaseOverrides {
            url: arguments.database_url,
        },
    };
    let config = Config::load(&config_file, &environment, &overrides)
        .context("failed to load Asterism configuration")?;

    let database = Database::connect(&config.database.url)
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
        config.server.session_ttl_seconds,
        config.server.secure_cookies,
    ));
    let listener = tokio::net::TcpListener::bind(config.server.bind)
        .await
        .context("failed to bind the Asterism HTTP listener")?;
    tracing::info!(address = %config.server.bind, "asterismd started");

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
