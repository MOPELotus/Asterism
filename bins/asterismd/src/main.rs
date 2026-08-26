use std::{
    collections::BTreeMap,
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::Context;
use asterism_api::{ApiState, build_router};
use asterism_config::{
    Config, ConfigFile, ConfigOverrides, DEFAULT_CONFIG_FILE, DatabaseOverrides, Environment,
    ProviderOverrides, SchedulerOverrides, ServerOverrides,
};
use asterism_domain::{
    AssessmentClass, AuditActor, OrchestrationState, PlanScope, ProviderId, RemoteState,
    RequestSource, SourceType, TaskCapability,
};
use asterism_engine::{
    AnswerHistoryHarvestTickReport, AnswerHistoryHarvestWorker, AnswerHistoryHarvestWorkerConfig,
    BrowserBridgeCredentialProcessor, BrowserBridgeCredentialProcessorConfig,
    BrowserBridgeCredentialTickReport, BrowserBridgeWorkflowProcessor,
    BrowserBridgeWorkflowProcessorConfig, BrowserBridgeWorkflowTickReport, DispatchConfig,
    DispatchReport, ExecuteTaskCommand, ExecutionAiSelectionInput, ExecutionRequestService,
    ExecutionRunnerConfig, ExecutionSchedulerConfig, ExecutionSchedulerTickReport,
    ExecutionSchedulerWorker, FormalAssessmentPolicy, OutboxDispatcher, ProviderScanService,
    ScanSchedulerConfig, ScanSchedulerTickReport, ScanSchedulerWorker,
};
use asterism_events::EventBus;
use asterism_networking::{NetworkProfile, ResolvedNetworkProfile};
use asterism_provider_api::{ProviderCapability, ProviderRegistry};
use asterism_provider_chaoxing::build_development_provider_with_renewal;
use asterism_provider_cidaren::build_development_provider_with_stored_session as build_cidaren_with_stored_session;
use asterism_provider_uai::build_development_provider_with_renewal as build_uai_with_renewal;
use asterism_provider_upstream_worker::{UpstreamWorkerProvider, WorkerAuthProfile};
use asterism_provider_welearn::build_development_provider_with_renewal as build_welearn_with_renewal;
use asterism_scheduler::RetryPolicy;
use asterism_secrets::{SecretKey, SecretString, SecretValue};
use asterism_storage::{
    CourseAutomationPlanRepository, Database, RecoveryReport, SecretKeyring,
    SqliteAnswerBootstrapHarvestRepository, SqliteAnswerHistoryIngestionRepository,
    SqliteBrowserBridgeSessionRepository, SqliteCourseAutomationPlanRepository,
    SqliteExecutionLeaseRepository, SqliteExecutionRepository, SqliteOutboxRepository,
    SqliteProtocolObservationRepository, SqliteProviderAccountRepository,
    SqliteProviderCredentialResolver, SqliteProviderRuntimeSettingsRepository,
    SqliteProviderScanRepository, SqliteQuestionSnapshotRepository, SqliteSchedulerRepository,
    SqliteSecretStore, SqliteTaskQueryRepository,
};
use asterism_uai_worker_client::UaiWorkerClient;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use clap::Parser;
use tokio::{
    sync::{Mutex, watch},
    time::MissedTickBehavior,
};
use tower_http::services::{ServeDir, ServeFile};
use tracing_subscriber::EnvFilter;

type DaemonScanWorker = ScanSchedulerWorker<
    SqliteSchedulerRepository,
    SqliteProviderAccountRepository,
    ProviderScanService<SqliteProviderScanRepository>,
>;

type DaemonExecutionWorker = ExecutionSchedulerWorker<
    SqliteExecutionRepository,
    SqliteExecutionLeaseRepository,
    SqliteSchedulerRepository,
    SqliteProviderAccountRepository,
    SqliteTaskQueryRepository,
>;

type DaemonAnswerHistoryWorker = AnswerHistoryHarvestWorker<
    SqliteAnswerBootstrapHarvestRepository,
    SqliteProviderAccountRepository,
    SqliteTaskQueryRepository,
    SqliteAnswerHistoryIngestionRepository,
>;

type DaemonOutboxDispatcher = OutboxDispatcher<SqliteOutboxRepository, EventBus>;
type BackgroundTickLock = Arc<Mutex<()>>;

const SECRET_ACTIVE_KEY_ID_ENV: &str = "ASTERISM_SECRET_ACTIVE_KEY_ID";
const SECRET_KEYS_ENV: &str = "ASTERISM_SECRET_KEYS";
const LIVE_EVENT_CAPACITY: usize = 512;
const OUTBOX_BATCH_SIZE: u32 = 128;
const OUTBOX_CLAIM_TTL_SECONDS: u64 = 30;
const OUTBOX_MAX_ATTEMPTS: u32 = 8;
const OUTBOX_TICK_INTERVAL: std::time::Duration = std::time::Duration::from_millis(250);
const BROWSER_BRIDGE_CREDENTIAL_TICK_INTERVAL: std::time::Duration =
    std::time::Duration::from_secs(1);

#[derive(Debug, Parser)]
#[command(version, about)]
struct Arguments {
    /// TOML configuration file. A missing default file is allowed.
    #[arg(long)]
    config: Option<PathBuf>,

    /// Production `WebUI` directory. When `index.html` exists, it is served as an SPA.
    #[arg(long, env = "ASTERISM_WEB_DIST", default_value = "web/dist")]
    web_dist: Option<PathBuf>,

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

    /// Hard Core cap for concurrently running executions.
    #[arg(long)]
    scheduler_execution_concurrency_limit: Option<u32>,

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

    /// Expose the unverified Chaoxing Provider for local validation only.
    #[arg(long, num_args = 0..=1, default_missing_value = "true")]
    enable_development_chaoxing: Option<bool>,

    /// Expose the unverified `WELearn` Provider for local validation only.
    #[arg(long, num_args = 0..=1, default_missing_value = "true")]
    enable_development_welearn: Option<bool>,

    /// Expose the unverified UAI Provider for local validation only.
    #[arg(long, num_args = 0..=1, default_missing_value = "true")]
    enable_development_uai: Option<bool>,

    /// Expose the unverified Cidaren Provider for local validation only.
    #[arg(long, num_args = 0..=1, default_missing_value = "true")]
    enable_development_cidaren: Option<bool>,

    /// Pinned Chaoxing donor checkout used by the 0.0.1 worker.
    #[arg(
        long,
        env = "ASTERISM_CHAOXING_WORKER_UPSTREAM",
        default_value = "upstreams/chaoxing"
    )]
    chaoxing_worker_upstream: Option<PathBuf>,

    /// Pinned `CxKitty` checkout used for Chaoxing course Exam inventory/execution.
    #[arg(
        long,
        env = "ASTERISM_CHAOXING_AUXILIARY_UPSTREAM",
        default_value = "upstreams/chaoxing-exam"
    )]
    chaoxing_auxiliary_upstream: Option<PathBuf>,

    /// Chromium-compatible browser used for Chaoxing's audited DOM fallback.
    #[arg(long, env = "ASTERISM_CHAOXING_BROWSER_EXECUTABLE")]
    chaoxing_browser_executable: Option<PathBuf>,

    /// Pinned `WELearn` donor entrypoint used by the 0.0.1 worker.
    #[arg(
        long,
        env = "ASTERISM_WELEARN_WORKER_UPSTREAM",
        default_value = "upstreams/welearn/welearn_decompiled.py"
    )]
    welearn_worker_upstream: Option<PathBuf>,

    /// Pinned Cidaren donor checkout used by the 0.0.1 worker.
    #[arg(
        long,
        env = "ASTERISM_CIDAREN_WORKER_UPSTREAM",
        default_value = "upstreams/cidaren"
    )]
    cidaren_worker_upstream: Option<PathBuf>,

    /// Pinned upstream UAI Python entrypoint used by the 0.0.1 worker.
    #[arg(
        long,
        env = "ASTERISM_UAI_WORKER_UPSTREAM",
        default_value = "upstreams/uai/配置我运行我.py"
    )]
    uai_worker_upstream: Option<PathBuf>,

    /// Pinned `UnipusAIAutoPlayer` checkout used for UAI page-residence duration.
    #[arg(
        long,
        env = "ASTERISM_UAI_BROWSER_UPSTREAM",
        default_value = "upstreams/uai-browser"
    )]
    uai_browser_upstream: Option<PathBuf>,

    /// Chromium-compatible browser used by the upstream UAI userscript worker.
    #[arg(long, env = "ASTERISM_UAI_BROWSER_EXECUTABLE")]
    uai_browser_executable: Option<PathBuf>,

    /// Python executable used for the 0.0.1 UAI worker.
    #[arg(long, env = "ASTERISM_UAI_WORKER_PYTHON", default_value = "python")]
    uai_worker_python: PathBuf,

    /// Asterism's thin UAI Python adapter entrypoint.
    #[arg(
        long,
        env = "ASTERISM_UAI_WORKER_ADAPTER",
        default_value = "workers/uai/worker.py"
    )]
    uai_worker_adapter: PathBuf,

    /// Metadata containing the allowed UAI donor revision and entrypoint hash.
    #[arg(
        long,
        env = "ASTERISM_UAI_WORKER_SOURCE_METADATA",
        default_value = "workers/uai/SOURCE.json"
    )]
    uai_worker_source_metadata: PathBuf,

    /// Hard timeout for one UAI worker operation.
    #[arg(
        long,
        env = "ASTERISM_UAI_WORKER_TIMEOUT_SECONDS",
        default_value_t = 600,
        value_parser = clap::value_parser!(u64).range(1..=7200)
    )]
    uai_worker_timeout_seconds: u64,
}

#[tokio::main]
#[allow(
    clippy::too_many_lines,
    reason = "daemon startup keeps migration, recovery, worker lifetimes and ordered shutdown visible"
)]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("asterism=info")),
        )
        .init();

    let arguments = Arguments::parse();
    let provider_workers = build_provider_worker_clients(&arguments);
    let web_dist = arguments.web_dist.clone();
    let config = load_config(arguments)?;

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
        scheduler_jobs_cancelled = recovery.scheduler_jobs_cancelled,
        recovery_jobs = recovery.recovery_jobs_enqueued,
        "startup recovery completed"
    );

    let secret_keyring = load_secret_keyring_from_process()?;
    let secret_store =
        secret_keyring.map(|keyring| SqliteSecretStore::new(database.clone(), keyring));
    let secret_store_configured = secret_store.is_some();
    let execution_secret_store = secret_store.clone();
    let browser_bridge_secret_store = secret_store.clone();
    let browser_bridge_workflow_secret_store = secret_store.clone();
    let providers = Arc::new(build_provider_registry(
        &config,
        secret_store.as_ref(),
        &provider_workers,
    )?);
    let events = EventBus::new(LIVE_EVENT_CAPACITY);
    let (shutdown_sender, shutdown_receiver) = watch::channel(false);
    let background_tick_lock = Arc::new(Mutex::new(()));
    let mut api_state = ApiState::new(
        database.clone(),
        providers.clone(),
        config.server.session_ttl_seconds,
        config.server.secure_cookies,
    )
    .with_ai_config(config.ai.clone())
    .with_event_bus(events.clone())
    .with_stream_shutdown(shutdown_receiver.clone());
    api_state.hydrate_ai_config().await?;
    if let Some(secret_store) = secret_store.clone() {
        api_state = api_state.with_secret_store(secret_store);
    }
    for (provider, worker) in provider_workers {
        match worker.health().await {
            Ok(health) => tracing::info!(
                %provider,
                revision = %health.source.revision,
                python = %health.python,
                "upstream Provider worker configured"
            ),
            Err(error) => {
                tracing::warn!(%error, %provider, "configured upstream Provider worker is unavailable");
            }
        }
        api_state = api_state.with_provider_worker(provider, worker);
    }
    let challenge_escalation_handle =
        start_challenge_escalation_worker(api_state.clone(), shutdown_receiver.clone());
    let mut app = build_router(api_state.clone());
    if let Some(web_dist) = web_dist.as_ref() {
        let index = web_dist.join("index.html");
        if index.is_file() {
            app = app
                .nest_service("/assets", ServeDir::new(web_dist.join("assets")))
                .route_service("/", ServeFile::new(index.clone()))
                .route_service("/{*path}", ServeFile::new(index));
            tracing::info!(path = %web_dist.display(), "production WebUI configured");
        } else {
            tracing::warn!(path = %web_dist.display(), "production WebUI was not built; API-only mode");
        }
    }
    let listener = tokio::net::TcpListener::bind(config.server.bind)
        .await
        .context("failed to bind the Asterism HTTP listener")?;
    tracing::info!(address = %config.server.bind, secret_store_configured, "asterismd started");

    let outbox_dispatcher_handle = start_outbox_dispatcher(
        &database,
        events,
        background_tick_lock.clone(),
        shutdown_receiver.clone(),
    )?;
    let scan_scheduler_handle = start_scan_scheduler(
        &database,
        providers.clone(),
        api_state.clone(),
        &config,
        background_tick_lock.clone(),
        shutdown_receiver.clone(),
    )?;
    let execution_scheduler_handle = start_execution_scheduler(
        &database,
        providers.clone(),
        execution_secret_store,
        &config,
        background_tick_lock.clone(),
        shutdown_receiver.clone(),
    )?;
    let answer_history_handle = start_answer_history_worker(
        &database,
        providers.clone(),
        &config,
        background_tick_lock.clone(),
        shutdown_receiver.clone(),
    )?;
    let browser_bridge_credential_handle = start_browser_bridge_credential_processor(
        &database,
        providers.clone(),
        browser_bridge_secret_store,
        &config,
        background_tick_lock.clone(),
        shutdown_receiver.clone(),
    );
    let browser_bridge_workflow_handle = start_browser_bridge_workflow_processor(
        &database,
        providers,
        browser_bridge_workflow_secret_store,
        &config,
        background_tick_lock,
        shutdown_receiver,
    );

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
    if let Some(handle) = scan_scheduler_handle {
        handle.await.context("scan scheduler task panicked")?;
    }
    if let Some(handle) = execution_scheduler_handle {
        handle.await.context("execution scheduler task panicked")?;
    }
    if let Some(handle) = answer_history_handle {
        handle
            .await
            .context("answer history worker task panicked")?;
    }
    if let Some(handle) = browser_bridge_credential_handle {
        handle
            .await
            .context("BrowserBridge credential processor task panicked")?;
    }
    if let Some(handle) = browser_bridge_workflow_handle {
        handle
            .await
            .context("BrowserBridge workflow processor task panicked")?;
    }
    challenge_escalation_handle
        .await
        .context("Chaoxing challenge escalation processor panicked")?;
    outbox_dispatcher_handle
        .await
        .context("outbox dispatcher task panicked")?;
    server_result.context("Asterism HTTP server failed")?;
    database.close().await;
    tracing::info!("asterismd stopped");
    Ok(())
}

fn start_challenge_escalation_worker(
    state: ApiState,
    mut shutdown: watch::Receiver<bool>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(2));
        interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = interval.tick() => {
                    match state.process_chaoxing_challenge_escalation_tick().await {
                        Ok(report) if report.processed => {
                            tracing::info!(materialized = report.materialized, completed = report.completed, "Chaoxing challenge escalation tick completed");
                        }
                        Ok(_) => {}
                        Err(error) => tracing::warn!(%error, "Chaoxing challenge escalation tick deferred"),
                    }
                }
                result = shutdown.changed() => {
                    if result.is_err() || *shutdown.borrow() { break; }
                }
            }
        }
    })
}

fn start_outbox_dispatcher(
    database: &Database,
    events: EventBus,
    background_tick_lock: BackgroundTickLock,
    shutdown: watch::Receiver<bool>,
) -> anyhow::Result<tokio::task::JoinHandle<()>> {
    let dispatcher = OutboxDispatcher::new(
        SqliteOutboxRepository::new(database.clone()),
        events,
        DispatchConfig {
            worker_id: format!("asterismd-outbox-{}", std::process::id()),
            batch_size: OUTBOX_BATCH_SIZE,
            claim_ttl_seconds: OUTBOX_CLAIM_TTL_SECONDS,
            max_attempts: OUTBOX_MAX_ATTEMPTS,
        },
    )
    .context("failed to configure the outbox dispatcher")?;
    Ok(tokio::spawn(run_outbox_dispatcher(
        dispatcher,
        OUTBOX_TICK_INTERVAL,
        background_tick_lock,
        shutdown,
    )))
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
            execution_concurrency_limit: arguments.scheduler_execution_concurrency_limit,
            claim_ttl_seconds: arguments.scheduler_claim_ttl_seconds,
            retry_max_attempts: arguments.scheduler_retry_max_attempts,
            retry_initial_delay_seconds: arguments.scheduler_retry_initial_delay_seconds,
            retry_multiplier: arguments.scheduler_retry_multiplier,
            retry_max_delay_seconds: arguments.scheduler_retry_max_delay_seconds,
        },
        providers: ProviderOverrides {
            enable_development_chaoxing: arguments.enable_development_chaoxing,
            enable_development_welearn: arguments.enable_development_welearn,
            enable_development_uai: arguments.enable_development_uai,
            enable_development_cidaren: arguments.enable_development_cidaren,
        },
    };
    Config::load(&config_file, &environment, &overrides)
        .context("failed to load Asterism configuration")
}

fn worker_client(
    arguments: &Arguments,
    upstream: &Path,
    adapter: impl Into<PathBuf>,
    source: impl Into<PathBuf>,
    protocol: &str,
) -> UaiWorkerClient {
    UaiWorkerClient::new(
        arguments.uai_worker_python.clone(),
        adapter,
        upstream.to_path_buf(),
    )
    .with_source_metadata(source)
    .with_protocol(protocol)
    .with_timeout(std::time::Duration::from_secs(
        arguments.uai_worker_timeout_seconds,
    ))
}

fn build_provider_worker_clients(arguments: &Arguments) -> Vec<(String, UaiWorkerClient)> {
    let mut workers = Vec::new();
    if let Some(upstream) = &arguments.chaoxing_worker_upstream {
        let mut client = worker_client(
            arguments,
            upstream,
            "workers/chaoxing/worker.py",
            "workers/chaoxing/SOURCE.json",
            "asterism.chaoxing.worker.v1",
        );
        if let Some(auxiliary) = &arguments.chaoxing_auxiliary_upstream {
            client =
                client.with_path_environment("ASTERISM_CHAOXING_AUXILIARY_UPSTREAM", auxiliary);
        }
        if let Some(browser) = &arguments.chaoxing_browser_executable {
            client = client.with_path_environment("ASTERISM_CHAOXING_BROWSER_EXECUTABLE", browser);
        }
        workers.push(("chaoxing".to_owned(), client));
    }
    if let Some(upstream) = &arguments.welearn_worker_upstream {
        workers.push((
            "welearn".to_owned(),
            worker_client(
                arguments,
                upstream,
                "workers/welearn/worker.py",
                "workers/welearn/SOURCE.json",
                "asterism.welearn.worker.v1",
            ),
        ));
    }
    if let Some(upstream) = &arguments.uai_worker_upstream {
        let mut client = worker_client(
            arguments,
            upstream,
            arguments.uai_worker_adapter.clone(),
            arguments.uai_worker_source_metadata.clone(),
            "asterism.uai.worker.v1",
        );
        if let Some(browser_upstream) = &arguments.uai_browser_upstream {
            client =
                client.with_path_environment("ASTERISM_UAI_BROWSER_UPSTREAM", browser_upstream);
        }
        if let Some(browser) = &arguments.uai_browser_executable {
            client = client.with_path_environment("ASTERISM_UAI_BROWSER_EXECUTABLE", browser);
        }
        workers.push(("uai".to_owned(), client));
    }
    if let Some(upstream) = &arguments.cidaren_worker_upstream {
        workers.push((
            "cidaren".to_owned(),
            worker_client(
                arguments,
                upstream,
                "workers/cidaren/worker.py",
                "workers/cidaren/SOURCE.json",
                "asterism.cidaren.worker.v1",
            ),
        ));
    }
    workers
}

fn load_secret_keyring_from_process() -> anyhow::Result<Option<Arc<SecretKeyring>>> {
    let active_key_id = std::env::var(SECRET_ACTIVE_KEY_ID_ENV).ok();
    let encoded_keys = std::env::var(SECRET_KEYS_ENV).ok().map(SecretString::new);
    load_secret_keyring(active_key_id, encoded_keys)
}

fn load_secret_keyring(
    active_key_id: Option<String>,
    encoded_keys: Option<SecretString>,
) -> anyhow::Result<Option<Arc<SecretKeyring>>> {
    let (active_key_id, encoded_keys) = match (active_key_id, encoded_keys) {
        (None, None) => return Ok(None),
        (Some(active_key_id), Some(encoded_keys)) => (active_key_id, encoded_keys),
        _ => anyhow::bail!(
            "both {SECRET_ACTIVE_KEY_ID_ENV} and {SECRET_KEYS_ENV} must be configured together"
        ),
    };
    let mut keys = BTreeMap::new();
    for entry in encoded_keys.expose_secret().split(',') {
        let (key_id, encoded_key) = entry
            .split_once('=')
            .ok_or_else(|| anyhow::anyhow!("{SECRET_KEYS_ENV} has an invalid entry"))?;
        anyhow::ensure!(
            !keys.contains_key(key_id),
            "{SECRET_KEYS_ENV} contains a duplicate key ID"
        );
        let decoded = SecretValue::new(
            STANDARD
                .decode(encoded_key)
                .map_err(|_| anyhow::anyhow!("{SECRET_KEYS_ENV} contains invalid base64"))?,
        );
        let key_bytes: [u8; 32] = decoded
            .expose_secret()
            .try_into()
            .map_err(|_| anyhow::anyhow!("{SECRET_KEYS_ENV} keys must decode to 32 bytes"))?;
        keys.insert(key_id.to_owned(), SecretKey::new(key_bytes));
    }
    let keyring = SecretKeyring::new(active_key_id, keys)
        .context("failed to configure the SecretStore keyring")?;
    Ok(Some(Arc::new(keyring)))
}

#[allow(
    clippy::too_many_lines,
    reason = "keeps old and upstream-worker Provider selection explicit during the 0.0.1 transition"
)]
fn build_provider_registry(
    config: &Config,
    secret_store: Option<&SqliteSecretStore>,
    provider_workers: &[(String, UaiWorkerClient)],
) -> anyhow::Result<ProviderRegistry> {
    let mut registry = ProviderRegistry::default();
    if !config.providers.enable_development_chaoxing
        && !config.providers.enable_development_welearn
        && !config.providers.enable_development_uai
        && !config.providers.enable_development_cidaren
        && provider_workers.is_empty()
    {
        return Ok(registry);
    }
    let secret_store = secret_store
        .context("enabled development Providers require a configured SecretStore keyring")?;
    if let Some((_, worker)) = provider_workers
        .iter()
        .find(|(provider, _)| provider == "chaoxing")
    {
        let provider_id = ProviderId::new("chaoxing")
            .map_err(|_| anyhow::anyhow!("invalid Chaoxing Provider ID"))?;
        let runtime = Arc::new(SqliteProviderCredentialResolver::new(
            secret_store.clone(),
            provider_id.clone(),
        ));
        registry
            .register(UpstreamWorkerProvider::entry_with_renewal(
                provider_id,
                "Chaoxing",
                worker.clone(),
                runtime.clone(),
                runtime,
                WorkerAuthProfile::PasswordAndCookie,
            ))
            .context("failed to register the Chaoxing upstream worker Provider")?;
    } else if config.providers.enable_development_chaoxing {
        let provider_id = ProviderId::new("chaoxing")
            .map_err(|_| anyhow::anyhow!("the compile-time Chaoxing Provider ID is invalid"))?;
        let runtime = Arc::new(SqliteProviderCredentialResolver::new(
            secret_store.clone(),
            provider_id,
        ));
        let network = ResolvedNetworkProfile::resolve(&NetworkProfile::default(), None, None)
            .context("failed to resolve the development Chaoxing network profile")?;
        let entry = build_development_provider_with_renewal(&network, runtime.clone(), runtime)
            .context("failed to build the development Chaoxing Provider")?;
        registry
            .register(entry)
            .context("failed to register the development Chaoxing Provider")?;
        tracing::warn!(
            provider = "chaoxing",
            "unverified development Provider explicitly enabled"
        );
    }
    if let Some((_, worker)) = provider_workers
        .iter()
        .find(|(provider, _)| provider == "welearn")
    {
        let provider_id = ProviderId::new("welearn")
            .map_err(|_| anyhow::anyhow!("invalid WELearn Provider ID"))?;
        let runtime = Arc::new(SqliteProviderCredentialResolver::new(
            secret_store.clone(),
            provider_id.clone(),
        ));
        registry
            .register(UpstreamWorkerProvider::entry_with_renewal(
                provider_id,
                "WELearn",
                worker.clone(),
                runtime.clone(),
                runtime,
                WorkerAuthProfile::PasswordAndCookie,
            ))
            .context("failed to register the WELearn upstream worker Provider")?;
    } else if config.providers.enable_development_welearn {
        let provider_id = ProviderId::new("welearn")
            .map_err(|_| anyhow::anyhow!("the compile-time WELearn Provider ID is invalid"))?;
        let runtime = Arc::new(SqliteProviderCredentialResolver::new(
            secret_store.clone(),
            provider_id,
        ));
        let network = ResolvedNetworkProfile::resolve(&NetworkProfile::default(), None, None)
            .context("failed to resolve the development WELearn network profile")?;
        let entry = build_welearn_with_renewal(&network, runtime.clone(), runtime)
            .context("failed to build the development WELearn Provider")?;
        registry
            .register(entry)
            .context("failed to register the development WELearn Provider")?;
        tracing::warn!(
            provider = "welearn",
            "unverified development Provider explicitly enabled"
        );
    }
    if let Some((_, worker)) = provider_workers
        .iter()
        .find(|(provider, _)| provider == "uai")
    {
        let provider_id =
            ProviderId::new("uai").map_err(|_| anyhow::anyhow!("invalid UAI Provider ID"))?;
        let runtime = Arc::new(SqliteProviderCredentialResolver::new(
            secret_store.clone(),
            provider_id.clone(),
        ));
        registry
            .register(UpstreamWorkerProvider::entry_with_renewal(
                provider_id,
                "UAI",
                worker.clone(),
                runtime.clone(),
                runtime,
                WorkerAuthProfile::Password,
            ))
            .context("failed to register the UAI upstream worker Provider")?;
    } else if config.providers.enable_development_uai {
        let provider_id = ProviderId::new("uai")
            .map_err(|_| anyhow::anyhow!("the compile-time UAI Provider ID is invalid"))?;
        let runtime = Arc::new(SqliteProviderCredentialResolver::new(
            secret_store.clone(),
            provider_id,
        ));
        let network = ResolvedNetworkProfile::resolve(&NetworkProfile::default(), None, None)
            .context("failed to resolve the development UAI network profile")?;
        let entry = build_uai_with_renewal(&network, runtime.clone(), runtime)
            .context("failed to build the development UAI Provider")?;
        registry
            .register(entry)
            .context("failed to register the development UAI Provider")?;
        tracing::warn!(
            provider = "uai",
            "unverified development Provider explicitly enabled"
        );
    }
    if let Some((_, worker)) = provider_workers
        .iter()
        .find(|(provider, _)| provider == "cidaren")
    {
        let provider_id = ProviderId::new("cidaren")
            .map_err(|_| anyhow::anyhow!("invalid Cidaren Provider ID"))?;
        let runtime = Arc::new(SqliteProviderCredentialResolver::new(
            secret_store.clone(),
            provider_id.clone(),
        ));
        registry
            .register(UpstreamWorkerProvider::entry(
                provider_id,
                "Cidaren",
                worker.clone(),
                runtime,
                WorkerAuthProfile::ExternalOauthComposite,
            ))
            .context("failed to register the Cidaren upstream worker Provider")?;
    } else if config.providers.enable_development_cidaren {
        let provider_id = ProviderId::new("cidaren")
            .map_err(|_| anyhow::anyhow!("the compile-time Cidaren Provider ID is invalid"))?;
        let runtime = Arc::new(SqliteProviderCredentialResolver::new(
            secret_store.clone(),
            provider_id,
        ));
        let network = ResolvedNetworkProfile::resolve(&NetworkProfile::default(), None, None)
            .context("failed to resolve the development Cidaren network profile")?;
        let entry = build_cidaren_with_stored_session(&network, runtime)
            .context("failed to build the development Cidaren Provider")?;
        registry
            .register(entry)
            .context("failed to register the development Cidaren Provider")?;
        tracing::warn!(
            provider = "cidaren",
            "unverified development Provider explicitly enabled"
        );
    }
    Ok(registry)
}

fn start_scan_scheduler(
    database: &Database,
    providers: Arc<ProviderRegistry>,
    api_state: ApiState,
    config: &Config,
    background_tick_lock: BackgroundTickLock,
    shutdown: watch::Receiver<bool>,
) -> anyhow::Result<Option<tokio::task::JoinHandle<()>>> {
    let handle = if config.scheduler.enabled {
        let automation_database = database.clone();
        let automation_providers = providers.clone();
        let worker = ScanSchedulerWorker::new(
            SqliteSchedulerRepository::new(database.clone()),
            SqliteProviderAccountRepository::new(database.clone()),
            ProviderScanService::new(
                providers,
                SqliteProviderScanRepository::new(database.clone()),
            )
            .with_protocol_observations(Arc::new(
                SqliteProtocolObservationRepository::new(database.clone()),
            )),
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
            automation_database,
            automation_providers,
            api_state,
            tick_interval,
            background_tick_lock,
            shutdown,
        )))
    } else {
        tracing::info!("scan scheduler disabled by configuration");
        None
    };
    Ok(handle)
}

fn start_execution_scheduler(
    database: &Database,
    providers: Arc<ProviderRegistry>,
    secret_store: Option<SqliteSecretStore>,
    config: &Config,
    background_tick_lock: BackgroundTickLock,
    shutdown: watch::Receiver<bool>,
) -> anyhow::Result<Option<tokio::task::JoinHandle<()>>> {
    let handle = if config.scheduler.enabled {
        let execution_lease_ttl =
            std::time::Duration::from_secs(config.scheduler.claim_ttl_seconds);
        let worker = ExecutionSchedulerWorker::new(
            providers,
            SqliteExecutionRepository::new(database.clone()),
            SqliteExecutionLeaseRepository::new(database.clone()),
            SqliteSchedulerRepository::new(database.clone()),
            SqliteProviderAccountRepository::new(database.clone()),
            SqliteTaskQueryRepository::new(database.clone()),
            ExecutionSchedulerConfig {
                worker_id: format!("asterismd-execution-{}", std::process::id()),
                claim_limit: config.scheduler.execution_concurrency_limit,
                claim_ttl: execution_lease_ttl,
                runner: ExecutionRunnerConfig {
                    execution_lease_ttl,
                    heartbeat_interval: execution_lease_ttl / 3,
                    global_concurrency_limit: config.scheduler.execution_concurrency_limit,
                    retry_policy: RetryPolicy {
                        max_attempts: config.scheduler.retry_max_attempts,
                        initial_delay_seconds: config.scheduler.retry_initial_delay_seconds,
                        multiplier: config.scheduler.retry_multiplier,
                        max_delay_seconds: config.scheduler.retry_max_delay_seconds,
                    },
                    // Scheduling a Formal assessment is denied by default at the public
                    // request boundary. Once an explicitly confirmed Execution exists,
                    // the daemon must be able to run the frozen request.
                    formal_assessment_policy: FormalAssessmentPolicy {
                        allow_execution: true,
                        allow_submission: true,
                    },
                },
            },
        )
        .context("failed to configure the execution scheduler")?;
        let worker = worker.with_protocol_observations(Arc::new(
            SqliteProtocolObservationRepository::new(database.clone()),
        ));
        let worker = if let Some(secret_store) = secret_store {
            let secret_store = Arc::new(secret_store);
            worker
                .with_question_session_artifacts(secret_store.clone())
                .with_batch_execution_parent_snapshots(secret_store.clone())
                .with_execution_mutation_stage_outputs(secret_store.clone())
                .with_execution_invocation_drafts(secret_store)
        } else {
            worker
        };
        let tick_interval = std::time::Duration::from_secs(config.scheduler.tick_interval_seconds);
        Some(tokio::spawn(run_execution_scheduler(
            database.clone(),
            worker,
            tick_interval,
            background_tick_lock,
            shutdown,
        )))
    } else {
        None
    };
    Ok(handle)
}

fn start_answer_history_worker(
    database: &Database,
    providers: Arc<ProviderRegistry>,
    config: &Config,
    background_tick_lock: BackgroundTickLock,
    shutdown: watch::Receiver<bool>,
) -> anyhow::Result<Option<tokio::task::JoinHandle<()>>> {
    let has_history_provider = providers
        .metadata()
        .any(|metadata| metadata.advertises(ProviderCapability::AnswerHistoryHarvest));
    // Answer-history harvesting is a read-only, account-scoped background scan.
    // It must remain available when mutation/execution scheduling is disabled;
    // operators use that mode specifically while validating Provider accounts.
    if !has_history_provider {
        return Ok(None);
    }
    let worker = AnswerHistoryHarvestWorker::new(
        providers,
        SqliteAnswerBootstrapHarvestRepository::new(database.clone()),
        SqliteProviderAccountRepository::new(database.clone()),
        SqliteTaskQueryRepository::new(database.clone()),
        SqliteAnswerHistoryIngestionRepository::new(database.clone()),
        AnswerHistoryHarvestWorkerConfig {
            worker_id: format!("asterismd-answer-history-{}", std::process::id()),
            claim_limit: config.scheduler.claim_limit.min(100),
            claim_ttl_seconds: config.scheduler.claim_ttl_seconds,
            page_yield_delay_seconds: config.scheduler.tick_interval_seconds,
            retry_delay_seconds: config.scheduler.retry_initial_delay_seconds,
            max_provider_retry_delay_seconds: config.scheduler.retry_max_delay_seconds,
        },
    )
    .context("failed to configure the answer history worker")?
    .with_protocol_observations(Arc::new(SqliteProtocolObservationRepository::new(
        database.clone(),
    )));
    let tick_interval = std::time::Duration::from_secs(config.scheduler.tick_interval_seconds);
    Ok(Some(tokio::spawn(run_answer_history_worker(
        worker,
        tick_interval,
        background_tick_lock,
        shutdown,
    ))))
}

fn start_browser_bridge_credential_processor(
    database: &Database,
    providers: Arc<ProviderRegistry>,
    secret_store: Option<SqliteSecretStore>,
    config: &Config,
    background_tick_lock: BackgroundTickLock,
    shutdown: watch::Receiver<bool>,
) -> Option<tokio::task::JoinHandle<()>> {
    let secret_store = secret_store?;
    let has_credential_terminal = providers.metadata().any(|metadata| {
        providers
            .get(&metadata.id)
            .and_then(|entry| entry.browser_bridge.as_ref())
            .is_some_and(|capability| {
                !capability
                    .browser_bridge_credential_result_types()
                    .is_empty()
            })
    });
    if !has_credential_terminal {
        return None;
    }
    Some(tokio::spawn(run_browser_bridge_credential_processor(
        database.clone(),
        providers,
        secret_store,
        RetryPolicy {
            max_attempts: config.scheduler.retry_max_attempts,
            initial_delay_seconds: config.scheduler.retry_initial_delay_seconds,
            multiplier: config.scheduler.retry_multiplier,
            max_delay_seconds: config.scheduler.retry_max_delay_seconds,
        },
        std::time::Duration::from_secs(config.scheduler.claim_ttl_seconds),
        background_tick_lock,
        shutdown,
    )))
}

async fn run_browser_bridge_credential_processor(
    database: Database,
    providers: Arc<ProviderRegistry>,
    secret_store: SqliteSecretStore,
    retry_policy: RetryPolicy,
    claim_ttl: std::time::Duration,
    background_tick_lock: BackgroundTickLock,
    mut shutdown: watch::Receiver<bool>,
) {
    let mut interval = tokio::time::interval(BROWSER_BRIDGE_CREDENTIAL_TICK_INTERVAL);
    interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
    loop {
        let should_tick = tokio::select! {
            biased;
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    break;
                }
                false
            }
            _ = interval.tick() => true,
        };
        if !should_tick {
            continue;
        }
        let _tick_guard = background_tick_lock.lock().await;
        let provider_ids = providers
            .metadata()
            .filter(|metadata| {
                providers
                    .get(&metadata.id)
                    .and_then(|entry| entry.browser_bridge.as_ref())
                    .is_some_and(|capability| {
                        !capability
                            .browser_bridge_credential_result_types()
                            .is_empty()
                    })
            })
            .map(|metadata| metadata.id.clone())
            .collect::<Vec<_>>();
        for provider_id in provider_ids {
            let processor = BrowserBridgeCredentialProcessor::new(
                provider_id.clone(),
                providers.clone(),
                SqliteBrowserBridgeSessionRepository::new(database.clone()),
                secret_store.browser_bridge_commands(provider_id.clone()),
                SqliteTaskQueryRepository::new(database.clone()),
                SqliteProviderAccountRepository::new(database.clone()),
                secret_store.clone(),
                BrowserBridgeCredentialProcessorConfig {
                    worker_id: format!(
                        "asterismd-browser-credential-{}-{provider_id}",
                        std::process::id()
                    ),
                    claim_ttl,
                    retry_policy,
                },
            )
            .map(|processor| {
                processor.with_protocol_observations(Arc::new(
                    SqliteProtocolObservationRepository::new(database.clone()),
                ))
            });
            let Ok(processor) = processor else {
                tracing::error!(
                    provider = %provider_id,
                    "BrowserBridge credential processor configuration is invalid"
                );
                continue;
            };
            match processor.tick(chrono::Utc::now()).await {
                Ok(report) if report != BrowserBridgeCredentialTickReport::default() => {
                    tracing::info!(
                        provider = %provider_id,
                        selected = report.selected,
                        committed = report.committed,
                        conflicted = report.conflicted,
                        retry_scheduled = report.retry_scheduled,
                        dead_lettered = report.dead_lettered,
                        failed = report.failed,
                        "BrowserBridge credential processor tick completed"
                    );
                }
                Ok(_) => {}
                Err(error) => {
                    tracing::error!(
                        provider = %provider_id,
                        %error,
                        "BrowserBridge credential processor tick failed"
                    );
                }
            }
        }
    }
    tracing::info!("BrowserBridge credential processor stopped");
}

fn start_browser_bridge_workflow_processor(
    database: &Database,
    providers: Arc<ProviderRegistry>,
    secret_store: Option<SqliteSecretStore>,
    config: &Config,
    background_tick_lock: BackgroundTickLock,
    shutdown: watch::Receiver<bool>,
) -> Option<tokio::task::JoinHandle<()>> {
    let secret_store = secret_store?;
    let has_workflow_results = providers.metadata().any(|metadata| {
        providers
            .get(&metadata.id)
            .and_then(|entry| entry.browser_bridge.as_ref())
            .is_some_and(|capability| {
                !capability
                    .browser_bridge_intermediate_result_types()
                    .is_empty()
                    || !capability
                        .browser_bridge_execution_result_types()
                        .is_empty()
            })
    });
    if !has_workflow_results {
        return None;
    }
    Some(tokio::spawn(run_browser_bridge_workflow_processor(
        database.clone(),
        providers,
        secret_store,
        RetryPolicy {
            max_attempts: config.scheduler.retry_max_attempts,
            initial_delay_seconds: config.scheduler.retry_initial_delay_seconds,
            multiplier: config.scheduler.retry_multiplier,
            max_delay_seconds: config.scheduler.retry_max_delay_seconds,
        },
        std::time::Duration::from_secs(config.scheduler.claim_ttl_seconds),
        background_tick_lock,
        shutdown,
    )))
}

async fn run_browser_bridge_workflow_processor(
    database: Database,
    providers: Arc<ProviderRegistry>,
    secret_store: SqliteSecretStore,
    retry_policy: RetryPolicy,
    claim_ttl: std::time::Duration,
    background_tick_lock: BackgroundTickLock,
    mut shutdown: watch::Receiver<bool>,
) {
    let mut interval = tokio::time::interval(BROWSER_BRIDGE_CREDENTIAL_TICK_INTERVAL);
    interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
    loop {
        let should_tick = tokio::select! {
            biased;
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    break;
                }
                false
            }
            _ = interval.tick() => true,
        };
        if !should_tick {
            continue;
        }
        let _tick_guard = background_tick_lock.lock().await;
        let provider_ids = providers
            .metadata()
            .filter(|metadata| {
                providers
                    .get(&metadata.id)
                    .and_then(|entry| entry.browser_bridge.as_ref())
                    .is_some_and(|capability| {
                        !capability
                            .browser_bridge_intermediate_result_types()
                            .is_empty()
                            || !capability
                                .browser_bridge_execution_result_types()
                                .is_empty()
                    })
            })
            .map(|metadata| metadata.id.clone())
            .collect::<Vec<_>>();
        for provider_id in provider_ids {
            let processor = BrowserBridgeWorkflowProcessor::new(
                provider_id.clone(),
                providers.clone(),
                SqliteBrowserBridgeSessionRepository::new(database.clone()),
                secret_store.browser_bridge_commands(provider_id.clone()),
                SqliteTaskQueryRepository::new(database.clone()),
                SqliteProviderAccountRepository::new(database.clone()),
                BrowserBridgeWorkflowProcessorConfig {
                    worker_id: format!(
                        "asterismd-browser-workflow-{}-{provider_id}",
                        std::process::id()
                    ),
                    claim_ttl,
                    retry_policy,
                },
            )
            .map(|processor| {
                processor.with_protocol_observations(Arc::new(
                    SqliteProtocolObservationRepository::new(database.clone()),
                ))
            });
            let Ok(processor) = processor else {
                tracing::error!(
                    provider = %provider_id,
                    "BrowserBridge workflow processor configuration is invalid"
                );
                continue;
            };
            match processor.tick(chrono::Utc::now()).await {
                Ok(report) if report != BrowserBridgeWorkflowTickReport::default() => {
                    tracing::info!(
                        provider = %provider_id,
                        selected = report.selected,
                        intermediate_committed = report.intermediate_committed,
                        terminal_committed = report.terminal_committed,
                        conflicted = report.conflicted,
                        retry_scheduled = report.retry_scheduled,
                        dead_lettered = report.dead_lettered,
                        failed = report.failed,
                        "BrowserBridge workflow processor tick completed"
                    );
                }
                Ok(_) => {}
                Err(error) => {
                    tracing::error!(
                        provider = %provider_id,
                        %error,
                        "BrowserBridge workflow processor tick failed"
                    );
                }
            }
        }
    }
    tracing::info!("BrowserBridge workflow processor stopped");
}

async fn run_scan_scheduler(
    worker: DaemonScanWorker,
    database: Database,
    providers: Arc<ProviderRegistry>,
    api_state: ApiState,
    tick_interval: std::time::Duration,
    background_tick_lock: BackgroundTickLock,
    mut shutdown: watch::Receiver<bool>,
) {
    let mut interval = tokio::time::interval(tick_interval);
    interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
    loop {
        let should_tick = tokio::select! {
            biased;
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    break;
                }
                false
            }
            _ = interval.tick() => true,
        };
        if !should_tick {
            continue;
        }
        let _tick_guard = background_tick_lock.lock().await;
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
                if report.completed > 0 {
                    run_course_automation_tick(&database, providers.clone(), &api_state).await;
                }
            }
            Ok(_) => {}
            Err(error) => {
                tracing::error!(%error, "scan scheduler tick failed");
            }
        }
    }
    tracing::info!("scan scheduler stopped");
}

async fn run_course_automation_tick(
    database: &Database,
    providers: Arc<ProviderRegistry>,
    api_state: &ApiState,
) {
    let now = chrono::Utc::now();
    let plans = match SqliteCourseAutomationPlanRepository::new(database.clone())
        .list_effective_course_automation_plans(now)
        .await
    {
        Ok(plans) => plans,
        Err(error) => {
            tracing::error!(%error, "course automation policy read failed");
            return;
        }
    };
    let tasks = SqliteTaskQueryRepository::new(database.clone());
    let service = ExecutionRequestService::new(
        tasks.clone(),
        SqliteExecutionRepository::new(database.clone()),
        SqliteProviderAccountRepository::new(database.clone()),
        SqliteProviderRuntimeSettingsRepository::new(database.clone()),
        SqliteQuestionSnapshotRepository::new(database.clone()),
        providers,
        FormalAssessmentPolicy::default(),
    );
    for plan in plans {
        let PlanScope::Course(course_id) = plan.scope else {
            continue;
        };
        let provider_id: Option<String> = match sqlx::query_scalar(
            "SELECT account.provider_id FROM courses AS course INNER JOIN provider_accounts AS account ON account.id = course.provider_account_id WHERE course.id = ? AND account.owner_user_id = ?",
        )
        .bind(course_id.to_string())
        .bind(plan.owner_user_id.to_string())
        .fetch_optional(database.pool())
        .await
        {
            Ok(value) => value,
            Err(error) => {
                tracing::error!(%error, course_id = %course_id, "course automation Provider lookup failed");
                continue;
            }
        };
        let Some(provider_id) = provider_id else {
            continue;
        };
        let mut offset = 0;
        loop {
            let page = match tasks
                .list_owned_course_tasks(plan.owner_user_id, course_id, 200, offset)
                .await
            {
                Ok(page) => page,
                Err(error) => {
                    tracing::error!(%error, course_id = %course_id, "course automation task read failed");
                    break;
                }
            };
            let count = page.items.len();
            for task in page.items {
                if !course_automation_task_is_safe(database, &provider_id, &task).await {
                    continue;
                }
                let mut capabilities = Vec::new();
                if task
                    .capabilities
                    .contains(&TaskCapability::ResourceExecution)
                {
                    capabilities.push(TaskCapability::ResourceExecution);
                }
                if task.capabilities.contains(&TaskCapability::DurationReport) {
                    capabilities.push(TaskCapability::DurationReport);
                }
                if capabilities.is_empty() {
                    continue;
                }
                let idempotency_key = format!(
                    "course-patrol:{}:{}",
                    task.id,
                    task.updated_at.timestamp_micros()
                );
                let correlation_id = format!("course-patrol:{}", task.id);
                if provider_id == "chaoxing"
                    && task
                        .capabilities
                        .contains(&TaskCapability::QuestionInventory)
                {
                    match api_state
                        .schedule_automatic_chaoxing_task(
                            plan.owner_user_id,
                            task.id,
                            &idempotency_key,
                            &correlation_id,
                        )
                        .await
                    {
                        Ok(Some(execution_id)) => {
                            tracing::info!(course_id = %course_id, task_id = %task.id, %execution_id, "course patrol scheduled answered Chaoxing chapter");
                            continue;
                        }
                        Ok(None) => {}
                        Err(error_code) => {
                            tracing::warn!(course_id = %course_id, task_id = %task.id, %error_code, "course patrol deferred Chaoxing chapter answer preparation");
                            continue;
                        }
                    }
                }
                if provider_id == "uai" && task.source_type == SourceType::Discussion {
                    match api_state
                        .schedule_automatic_uai_discussion(
                            plan.owner_user_id,
                            task.id,
                            &idempotency_key,
                            &correlation_id,
                            plan.schedule_policy.ai_profile.as_deref(),
                        )
                        .await
                    {
                        Ok(execution_id) => {
                            tracing::info!(course_id = %course_id, task_id = %task.id, %execution_id, "course patrol scheduled required UAI discussion");
                        }
                        Err(error_code) => {
                            tracing::warn!(course_id = %course_id, task_id = %task.id, %error_code, "course patrol deferred required UAI discussion");
                        }
                    }
                    continue;
                }
                let split_welearn_duration = provider_id == "welearn"
                    && capabilities.contains(&TaskCapability::ResourceExecution)
                    && capabilities.contains(&TaskCapability::DurationReport);
                let execution_capabilities = if split_welearn_duration {
                    vec![TaskCapability::ResourceExecution]
                } else {
                    capabilities.clone()
                };
                let duration_idempotency_key = format!("{idempotency_key}:duration");
                let duration_correlation_id = format!("{correlation_id}:duration");
                let result = service
                    .execute(ExecuteTaskCommand {
                        owner_id: plan.owner_user_id,
                        task_id: task.id,
                        requested_capabilities: execution_capabilities,
                        submission_draft_id: None,
                        invocation_draft_id: None,
                        strict_completion_retry: None,
                        score_improvement_retake: None,
                        billing: None,
                        ai_selection: Some(ExecutionAiSelectionInput {
                            profile: "economy".to_owned(),
                            route: if provider_id == "cidaren" {
                                "timed"
                            } else {
                                "untimed"
                            }
                            .to_owned(),
                        }),
                        request_source: RequestSource::Scheduler,
                        actor: AuditActor::User(plan.owner_user_id),
                        idempotency_key,
                        correlation_id,
                        requested_at: now,
                    })
                    .await;
                match result {
                    Ok(result) if result.created => {
                        tracing::info!(course_id = %course_id, task_id = %task.id, execution_id = %result.execution.id, "course patrol scheduled task")
                    }
                    Ok(_) => {}
                    Err(error) => {
                        tracing::warn!(course_id = %course_id, task_id = %task.id, %error, "course patrol skipped unschedulable task")
                    }
                }
                if split_welearn_duration {
                    let duration_result = service
                        .execute(ExecuteTaskCommand {
                            owner_id: plan.owner_user_id,
                            task_id: task.id,
                            requested_capabilities: vec![TaskCapability::DurationReport],
                            submission_draft_id: None,
                            invocation_draft_id: None,
                            strict_completion_retry: None,
                            score_improvement_retake: None,
                            billing: None,
                            ai_selection: Some(ExecutionAiSelectionInput {
                                profile: "economy".to_owned(),
                                route: "untimed".to_owned(),
                            }),
                            request_source: RequestSource::Scheduler,
                            actor: AuditActor::User(plan.owner_user_id),
                            idempotency_key: duration_idempotency_key,
                            correlation_id: duration_correlation_id,
                            requested_at: now,
                        })
                        .await;
                    if let Err(error) = duration_result {
                        tracing::warn!(course_id = %course_id, task_id = %task.id, %error, "course patrol skipped separate WELearn duration action");
                    }
                }
            }
            offset += count as u64;
            if count == 0 || offset >= page.total {
                break;
            }
        }
    }
}

async fn course_automation_task_is_safe(
    database: &Database,
    provider_id: &str,
    task: &asterism_domain::Task,
) -> bool {
    if task.assessment_class != AssessmentClass::Routine
        || !matches!(
            task.remote_state,
            RemoteState::Pending | RemoteState::InProgress
        )
        || !matches!(
            task.orchestration_state,
            OrchestrationState::Discovered | OrchestrationState::Ready | OrchestrationState::Failed
        )
        || (task.source_type == SourceType::Discussion && provider_id != "uai")
    {
        return false;
    }
    if provider_id == "chaoxing" && task.source_type != SourceType::Chapter {
        return false;
    }
    if provider_id == "uai" {
        let Some(snapshot_id) = task.latest_snapshot_id else {
            return false;
        };
        let raw: Option<String> = sqlx::query_scalar(
            "SELECT remote_raw_sanitized_json FROM task_snapshots WHERE id = ? AND task_id = ?",
        )
        .bind(snapshot_id.to_string())
        .bind(task.id.to_string())
        .fetch_optional(database.pool())
        .await
        .ok()
        .flatten();
        return raw
            .and_then(|value| serde_json::from_str::<serde_json::Value>(&value).ok())
            .and_then(|value| {
                value
                    .pointer("/provider_summary/required")
                    .and_then(serde_json::Value::as_bool)
            })
            == Some(true);
    }
    true
}

async fn run_answer_history_worker(
    worker: DaemonAnswerHistoryWorker,
    tick_interval: std::time::Duration,
    background_tick_lock: BackgroundTickLock,
    mut shutdown: watch::Receiver<bool>,
) {
    let mut interval = tokio::time::interval(tick_interval);
    interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
    loop {
        let should_tick = tokio::select! {
            biased;
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    break;
                }
                false
            }
            _ = interval.tick() => true,
        };
        if !should_tick {
            continue;
        }
        let _tick_guard = background_tick_lock.lock().await;
        match worker.tick_once(chrono::Utc::now()).await {
            Ok(report) if report != AnswerHistoryHarvestTickReport::default() => {
                tracing::info!(
                    claimed = report.claimed,
                    completed = report.completed,
                    yielded = report.yielded,
                    retry_scheduled = report.retry_scheduled,
                    dead_lettered = report.dead_lettered,
                    imported_tasks = report.imported_tasks,
                    "answer history worker tick completed"
                );
            }
            Ok(_) => {}
            Err(error) => {
                tracing::error!(%error, "answer history worker tick failed");
            }
        }
    }
    tracing::info!("answer history worker stopped");
}

async fn run_execution_scheduler(
    database: Database,
    worker: DaemonExecutionWorker,
    tick_interval: std::time::Duration,
    background_tick_lock: BackgroundTickLock,
    mut shutdown: watch::Receiver<bool>,
) {
    let mut interval = tokio::time::interval(tick_interval);
    interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
    loop {
        let should_tick = tokio::select! {
            biased;
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    break;
                }
                false
            }
            _ = interval.tick() => true,
        };
        if !should_tick {
            continue;
        }
        let _tick_guard = background_tick_lock.lock().await;
        let now = chrono::Utc::now();
        match database.recover_stale_work(now).await {
            Ok(report) if report != RecoveryReport::default() => {
                tracing::warn!(
                    executions = report.executions_marked_recovering,
                    execution_leases = report.expired_execution_leases_removed,
                    scheduler_claims = report.scheduler_claims_requeued,
                    scheduler_jobs_cancelled = report.scheduler_jobs_cancelled,
                    recovery_jobs = report.recovery_jobs_enqueued,
                    "runtime recovery sweep completed"
                );
            }
            Ok(_) => {}
            Err(error) => {
                tracing::error!(%error, "runtime recovery sweep failed");
                continue;
            }
        }
        match worker.tick_once(now).await {
            Ok(report) if report != ExecutionSchedulerTickReport::default() => {
                tracing::info!(
                    claimed = report.claimed,
                    succeeded = report.succeeded,
                    retry_scheduled = report.retry_scheduled,
                    human_required = report.human_required,
                    failed = report.failed,
                    deferred = report.deferred,
                    dead_lettered = report.dead_lettered,
                    already_terminal = report.already_terminal,
                    "execution scheduler tick completed"
                );
            }
            Ok(_) => {}
            Err(error) => {
                tracing::error!(%error, "execution scheduler tick failed");
            }
        }
    }
    tracing::info!("execution scheduler stopped");
}

async fn run_outbox_dispatcher(
    dispatcher: DaemonOutboxDispatcher,
    tick_interval: std::time::Duration,
    background_tick_lock: BackgroundTickLock,
    mut shutdown: watch::Receiver<bool>,
) {
    let mut interval = tokio::time::interval(tick_interval);
    interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
    loop {
        let should_tick = tokio::select! {
            biased;
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    break;
                }
                false
            }
            _ = interval.tick() => true,
        };
        if !should_tick {
            continue;
        }
        let _tick_guard = background_tick_lock.lock().await;
        match dispatcher.dispatch_once(chrono::Utc::now()).await {
            Ok(report) if report != DispatchReport::default() => {
                tracing::info!(
                    claimed = report.claimed,
                    delivered = report.delivered,
                    retry_pending = report.retry_pending,
                    dead_lettered = report.dead_lettered,
                    "outbox dispatch tick completed"
                );
            }
            Ok(_) => {}
            Err(error) => {
                tracing::error!(%error, "outbox dispatch tick failed");
            }
        }
    }
    tracing::info!("outbox dispatcher stopped");
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
    async fn scheduler_workers_stop_before_database_shutdown() {
        let database = Database::connect("sqlite::memory:").await.unwrap();
        database.migrate().await.unwrap();
        let (shutdown_sender, shutdown_receiver) = watch::channel(false);
        let providers = Arc::new(ProviderRegistry::default());
        let background_tick_lock = Arc::new(Mutex::new(()));
        let outbox_handle = start_outbox_dispatcher(
            &database,
            EventBus::new(8),
            background_tick_lock.clone(),
            shutdown_receiver.clone(),
        )
        .unwrap();
        let scan_handle = start_scan_scheduler(
            &database,
            providers.clone(),
            ApiState::new(database.clone(), providers.clone(), 3_600, false),
            &Config::default(),
            background_tick_lock.clone(),
            shutdown_receiver.clone(),
        )
        .unwrap()
        .unwrap();
        let execution_handle = start_execution_scheduler(
            &database,
            providers,
            None,
            &Config::default(),
            background_tick_lock,
            shutdown_receiver,
        )
        .unwrap()
        .unwrap();

        shutdown_sender.send(true).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(1), scan_handle)
            .await
            .unwrap()
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(1), execution_handle)
            .await
            .unwrap()
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(1), outbox_handle)
            .await
            .unwrap()
            .unwrap();
        database.close().await;
    }

    #[tokio::test]
    async fn browser_bridge_credential_processor_starts_only_for_terminal_provider() {
        let database = Database::connect("sqlite::memory:").await.unwrap();
        database.migrate().await.unwrap();
        let keyring = load_secret_keyring(
            Some("key-a".to_owned()),
            Some(SecretString::new(format!(
                "key-a={}",
                STANDARD.encode([7; 32])
            ))),
        )
        .unwrap()
        .unwrap();
        let store = SqliteSecretStore::new(database.clone(), keyring);
        let mut config = Config::default();
        config.providers.enable_development_cidaren = true;
        let providers = Arc::new(build_provider_registry(&config, Some(&store), &[]).unwrap());
        let (shutdown_sender, shutdown_receiver) = watch::channel(false);
        let background_tick_lock = Arc::new(Mutex::new(()));
        let handle = start_browser_bridge_credential_processor(
            &database,
            providers,
            Some(store),
            &config,
            background_tick_lock,
            shutdown_receiver,
        )
        .expect("Cidaren declares a credential-terminal BrowserBridge result");
        shutdown_sender.send(true).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(1), handle)
            .await
            .unwrap()
            .unwrap();
        database.close().await;
    }

    #[tokio::test]
    async fn browser_bridge_workflow_processor_starts_for_uai_result_inboxes() {
        let database = Database::connect("sqlite::memory:").await.unwrap();
        database.migrate().await.unwrap();
        let keyring = load_secret_keyring(
            Some("key-a".to_owned()),
            Some(SecretString::new(format!(
                "key-a={}",
                STANDARD.encode([8; 32])
            ))),
        )
        .unwrap()
        .unwrap();
        let store = SqliteSecretStore::new(database.clone(), keyring);
        let mut config = Config::default();
        config.providers.enable_development_uai = true;
        let providers = Arc::new(build_provider_registry(&config, Some(&store), &[]).unwrap());
        let (shutdown_sender, shutdown_receiver) = watch::channel(false);
        let background_tick_lock = Arc::new(Mutex::new(()));
        let handle = start_browser_bridge_workflow_processor(
            &database,
            providers,
            Some(store),
            &config,
            background_tick_lock,
            shutdown_receiver,
        )
        .expect("UAI declares intermediate and terminal BrowserBridge results");
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
            "--scheduler-execution-concurrency-limit=24",
            "--enable-development-chaoxing=false",
            "--enable-development-welearn=true",
            "--enable-development-uai=true",
            "--enable-development-cidaren=true",
        ])
        .unwrap();
        assert_eq!(arguments.scheduler_enabled, Some(false));
        assert_eq!(arguments.scheduler_tick_interval_seconds, Some(9));
        assert_eq!(arguments.scheduler_claim_limit, Some(2));
        assert_eq!(arguments.scheduler_execution_concurrency_limit, Some(24));
        assert_eq!(arguments.scheduler_materialize_limit, None);
        assert_eq!(arguments.enable_development_chaoxing, Some(false));
        assert_eq!(arguments.enable_development_welearn, Some(true));
        assert_eq!(arguments.enable_development_uai, Some(true));
        assert_eq!(arguments.enable_development_cidaren, Some(true));
    }

    #[test]
    fn secret_keyring_configuration_is_environment_only_and_redacted() {
        assert!(load_secret_keyring(None, None).unwrap().is_none());
        assert!(load_secret_keyring(Some("key-a".to_owned()), None).is_err());

        let encoded = format!(
            "key-a={},key-b={}",
            STANDARD.encode([7; 32]),
            STANDARD.encode([9; 32])
        );
        let keyring = load_secret_keyring(
            Some("key-b".to_owned()),
            Some(SecretString::new(encoded.clone())),
        )
        .unwrap()
        .unwrap();
        let debug = format!("{keyring:?}");
        assert!(debug.contains("key-a"));
        assert!(debug.contains("key-b"));
        assert!(!debug.contains(&encoded));

        let invalid = "not-a-base64-key";
        let error = load_secret_keyring(
            Some("key-a".to_owned()),
            Some(SecretString::new(format!("key-a={invalid}"))),
        )
        .unwrap_err();
        assert!(!error.to_string().contains(invalid));
    }

    #[tokio::test]
    async fn development_provider_registration_is_explicit_and_requires_secrets() {
        let default_registry = build_provider_registry(&Config::default(), None, &[]).unwrap();
        assert!(default_registry.is_empty());

        let mut config = Config::default();
        config.providers.enable_development_chaoxing = true;
        assert!(build_provider_registry(&config, None, &[]).is_err());
        config = Config::default();
        config.providers.enable_development_welearn = true;
        assert!(build_provider_registry(&config, None, &[]).is_err());
        config = Config::default();
        config.providers.enable_development_uai = true;
        assert!(build_provider_registry(&config, None, &[]).is_err());
        config = Config::default();
        config.providers.enable_development_cidaren = true;
        assert!(build_provider_registry(&config, None, &[]).is_err());

        let database = Database::connect("sqlite::memory:").await.unwrap();
        let keyring = load_secret_keyring(
            Some("key-a".to_owned()),
            Some(SecretString::new(format!(
                "key-a={}",
                STANDARD.encode([7; 32])
            ))),
        )
        .unwrap()
        .unwrap();
        let store = SqliteSecretStore::new(database.clone(), keyring);

        config = Config::default();
        config.providers.enable_development_uai = true;
        let uai_only = build_provider_registry(&config, Some(&store), &[]).unwrap();
        assert!(
            uai_only
                .get(&ProviderId::new("chaoxing").unwrap())
                .is_none()
        );
        assert!(uai_only.get(&ProviderId::new("welearn").unwrap()).is_none());
        assert!(uai_only.get(&ProviderId::new("uai").unwrap()).is_some());
        assert!(uai_only.get(&ProviderId::new("cidaren").unwrap()).is_none());

        config.providers.enable_development_chaoxing = true;
        config.providers.enable_development_welearn = true;
        config.providers.enable_development_uai = true;
        config.providers.enable_development_cidaren = true;
        let registry = build_provider_registry(&config, Some(&store), &[]).unwrap();
        let provider = registry.get(&ProviderId::new("chaoxing").unwrap()).unwrap();
        assert_eq!(
            provider.metadata.verification,
            asterism_provider_api::VerificationLevel::Development
        );
        let provider = registry.get(&ProviderId::new("cidaren").unwrap()).unwrap();
        assert_eq!(
            provider.metadata.verification,
            asterism_provider_api::VerificationLevel::Development
        );
        let provider = registry.get(&ProviderId::new("welearn").unwrap()).unwrap();
        assert_eq!(
            provider.metadata.verification,
            asterism_provider_api::VerificationLevel::Development
        );
        let provider = registry.get(&ProviderId::new("uai").unwrap()).unwrap();
        assert_eq!(
            provider.metadata.verification,
            asterism_provider_api::VerificationLevel::Development
        );
        database.close().await;
    }
}
