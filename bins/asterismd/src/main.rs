use std::{net::SocketAddr, path::PathBuf, sync::Arc};

use anyhow::Context;
use asterism_api::{ApiState, build_router};
use asterism_config::{
    Config, ConfigFile, ConfigOverrides, DEFAULT_CONFIG_FILE, DatabaseOverrides, Environment,
    SchedulerOverrides, ServerOverrides,
};
use asterism_engine::{
    ProviderScanService, ScanSchedulerConfig, ScanSchedulerTickReport, ScanSchedulerWorker,
};
use asterism_provider_api::ProviderRegistry;
use asterism_scheduler::RetryPolicy;
use asterism_storage::{
    Database, SqliteProviderAccountRepository, SqliteProviderScanRepository,
    SqliteSchedulerRepository,
};
use clap::Parser;
use tokio::{sync::watch, time::MissedTickBehavior};
use tracing_subscriber::EnvFilter;

type DaemonScanWorker = ScanSchedulerWorker<
    SqliteSchedulerRepository,
    SqliteProviderAccountRepository,
    ProviderScanService<SqliteProviderScanRepository>,
>;

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

    /// Enable the unified scan scheduler loop.
    #[arg(long, num_args = 0..=1, default_missing_value = "true")]
    scheduler_enabled: Option<bool>,

    /// Seconds between scheduler ticks.
    #[arg(long)]
    scheduler_tick_interval_seconds: Option<u64>,

    /// Maximum due scan periods materialized per tick.
    #[arg(long)]
    scheduler_materialize_limit: Option<u32>,

    /// Maximum scan jobs claimed per tick.
    #[arg(long)]
    scheduler_claim_limit: Option<u32>,

    /// Scheduler claim lifetime in seconds.
    #[arg(long)]
    scheduler_claim_ttl_seconds: Option<u64>,

    /// Total scan attempts before dead-lettering.
    #[arg(long)]
    scheduler_retry_max_attempts: Option<u32>,

    /// Initial retry delay in seconds.
    #[arg(long)]
    scheduler_retry_initial_delay_seconds: Option<u64>,

    /// Exponential retry multiplier.
    #[arg(long)]
    scheduler_retry_multiplier: Option<u32>,

    /// Maximum retry delay in seconds.
    #[arg(long)]
    scheduler_retry_max_delay_seconds: Option<u64>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("asterism=info")),
        )
        .init();

    let config = load_config(Arguments::parse())?;

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

    let providers = Arc::new(ProviderRegistry::default());
    let app = build_router(ApiState::new(
        database.clone(),
        providers.clone(),
        config.server.session_ttl_seconds,
        config.server.secure_cookies,
    ));
    let listener = tokio::net::TcpListener::bind(config.server.bind)
        .await
        .context("failed to bind the Asterism HTTP listener")?;
    tracing::info!(address = %config.server.bind, "asterismd started");

    let (shutdown_sender, shutdown_receiver) = watch::channel(false);
    let scheduler_handle = start_scan_scheduler(&database, providers, &config, shutdown_receiver)?;

    let graceful_shutdown_sender = shutdown_sender.clone();
    let server_result = axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async move {
        shutdown_signal().await;
        let _ = graceful_shutdown_sender.send(true);
    })
    .await;
    let _ = shutdown_sender.send(true);
    if let Some(handle) = scheduler_handle {
        handle.await.context("scan scheduler task panicked")?;
    }
    server_result.context("Asterism HTTP server failed")?;
    database.close().await;
    tracing::info!("asterismd stopped");
    Ok(())
}

fn load_config(arguments: Arguments) -> anyhow::Result<Config> {
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
        scheduler: SchedulerOverrides {
            enabled: arguments.scheduler_enabled,
            tick_interval_seconds: arguments.scheduler_tick_interval_seconds,
            materialize_limit: arguments.scheduler_materialize_limit,
            claim_limit: arguments.scheduler_claim_limit,
            claim_ttl_seconds: arguments.scheduler_claim_ttl_seconds,
            retry_max_attempts: arguments.scheduler_retry_max_attempts,
            retry_initial_delay_seconds: arguments.scheduler_retry_initial_delay_seconds,
            retry_multiplier: arguments.scheduler_retry_multiplier,
            retry_max_delay_seconds: arguments.scheduler_retry_max_delay_seconds,
        },
    };
    Config::load(&config_file, &environment, &overrides)
        .context("failed to load Asterism configuration")
}

fn start_scan_scheduler(
    database: &Database,
    providers: Arc<ProviderRegistry>,
    config: &Config,
    shutdown: watch::Receiver<bool>,
) -> anyhow::Result<Option<tokio::task::JoinHandle<()>>> {
    let handle = if config.scheduler.enabled {
        let worker = ScanSchedulerWorker::new(
            SqliteSchedulerRepository::new(database.clone()),
            SqliteProviderAccountRepository::new(database.clone()),
            ProviderScanService::new(
                providers,
                SqliteProviderScanRepository::new(database.clone()),
            ),
            ScanSchedulerConfig {
                worker_id: format!("asterismd-scan-{}", std::process::id()),
                materialize_limit: config.scheduler.materialize_limit,
                claim_limit: config.scheduler.claim_limit,
                claim_ttl_seconds: config.scheduler.claim_ttl_seconds,
                retry_policy: RetryPolicy {
                    max_attempts: config.scheduler.retry_max_attempts,
                    initial_delay_seconds: config.scheduler.retry_initial_delay_seconds,
                    multiplier: config.scheduler.retry_multiplier,
                    max_delay_seconds: config.scheduler.retry_max_delay_seconds,
                },
            },
        )
        .context("failed to configure the scan scheduler")?;
        let tick_interval = std::time::Duration::from_secs(config.scheduler.tick_interval_seconds);
        Some(tokio::spawn(run_scan_scheduler(
            worker,
            tick_interval,
            shutdown,
        )))
    } else {
        tracing::info!("scan scheduler disabled by configuration");
        None
    };
    Ok(handle)
}

async fn run_scan_scheduler(
    worker: DaemonScanWorker,
    tick_interval: std::time::Duration,
    mut shutdown: watch::Receiver<bool>,
) {
    let mut interval = tokio::time::interval(tick_interval);
    interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            biased;
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    break;
                }
            }
            _ = interval.tick() => {
                match worker.tick_once(chrono::Utc::now()).await {
                    Ok(report) if report != ScanSchedulerTickReport::default() => {
                        tracing::info!(
                            materialized = report.materialized,
                            claimed = report.claimed,
                            completed = report.completed,
                            retry_scheduled = report.retry_scheduled,
                            dead_lettered = report.dead_lettered,
                            "scan scheduler tick completed"
                        );
                    }
                    Ok(_) => {}
                    Err(error) => {
                        tracing::error!(%error, "scan scheduler tick failed");
                    }
                }
            }
        }
    }
    tracing::info!("scan scheduler stopped");
}

async fn shutdown_signal() {
    if let Err(error) = tokio::signal::ctrl_c().await {
        tracing::error!(%error, "failed to install the shutdown signal handler");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn scan_scheduler_stops_before_database_shutdown() {
        let database = Database::connect("sqlite::memory:").await.unwrap();
        database.migrate().await.unwrap();
        let (shutdown_sender, shutdown_receiver) = watch::channel(false);
        let handle = start_scan_scheduler(
            &database,
            Arc::new(ProviderRegistry::default()),
            &Config::default(),
            shutdown_receiver,
        )
        .unwrap()
        .unwrap();

        shutdown_sender.send(true).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(1), handle)
            .await
            .unwrap()
            .unwrap();
        database.close().await;
    }

    #[test]
    fn scheduler_cli_flags_are_optional_overrides() {
        let arguments = Arguments::try_parse_from([
            "asterismd",
            "--scheduler-enabled=false",
            "--scheduler-tick-interval-seconds=9",
            "--scheduler-claim-limit=2",
        ])
        .unwrap();
        assert_eq!(arguments.scheduler_enabled, Some(false));
        assert_eq!(arguments.scheduler_tick_interval_seconds, Some(9));
        assert_eq!(arguments.scheduler_claim_limit, Some(2));
        assert_eq!(arguments.scheduler_materialize_limit, None);
    }
}
