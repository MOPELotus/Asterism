//! Provider-specific subprocess client for the Asterism 0.0.1 UAI worker.

use std::{
    collections::BTreeMap,
    fmt,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader},
    process::Command,
    time,
};
use uuid::Uuid;

const UAI_PROTOCOL: &str = "asterism.uai.worker.v1";
const MAX_EVENT_BYTES: usize = 4 * 1024 * 1024;
const MAX_EVENTS: usize = 4_096;
const MAX_STDERR_CAPTURE_BYTES: usize = 64 * 1024;

#[derive(Clone)]
pub struct UaiWorkerClient {
    python: PathBuf,
    adapter: PathBuf,
    upstream: PathBuf,
    source_metadata: Option<PathBuf>,
    protocol: String,
    timeout: Duration,
    environment: BTreeMap<String, PathBuf>,
}

impl UaiWorkerClient {
    #[must_use]
    pub fn new(
        python: impl Into<PathBuf>,
        adapter: impl Into<PathBuf>,
        upstream: impl Into<PathBuf>,
    ) -> Self {
        Self {
            python: python.into(),
            adapter: adapter.into(),
            upstream: upstream.into(),
            source_metadata: None,
            protocol: UAI_PROTOCOL.to_owned(),
            timeout: Duration::from_secs(30),
            environment: BTreeMap::new(),
        }
    }

    /// Selects the exact Provider worker protocol expected on stdout.
    #[must_use]
    pub fn with_protocol(mut self, protocol: impl Into<String>) -> Self {
        self.protocol = protocol.into();
        self
    }

    #[must_use]
    pub fn with_source_metadata(mut self, source_metadata: impl Into<PathBuf>) -> Self {
        self.source_metadata = Some(source_metadata.into());
        self
    }

    #[must_use]
    pub const fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Adds one explicitly configured path to the worker subprocess environment.
    #[must_use]
    pub fn with_path_environment(
        mut self,
        name: impl Into<String>,
        value: impl Into<PathBuf>,
    ) -> Self {
        self.environment.insert(name.into(), value.into());
        self
    }

    /// Starts the configured worker and returns its pinned-source health report.
    ///
    /// # Errors
    ///
    /// Returns an error when the subprocess cannot be started, exceeds its
    /// limits or timeout, emits an invalid event, or reports a controlled
    /// worker failure.
    pub async fn health(&self) -> Result<UaiWorkerHealth, UaiWorkerClientError> {
        let result = self.invoke_result("health", json!({})).await?;
        serde_json::from_value(result).map_err(UaiWorkerClientError::InvalidHealthResult)
    }

    /// Invokes one thin worker operation and returns its terminal JSON value.
    ///
    /// # Errors
    ///
    /// Returns an error for process, timeout, protocol-binding, output-limit,
    /// decoding, or worker-reported failures.
    pub async fn invoke_result(
        &self,
        operation: &str,
        payload: Value,
    ) -> Result<Value, UaiWorkerClientError> {
        let events = self.invoke(operation, payload).await?;
        events
            .into_iter()
            .find_map(|event| match event.payload {
                WorkerEventPayload::Result { data } => Some(data),
                WorkerEventPayload::Log { .. }
                | WorkerEventPayload::Progress { .. }
                | WorkerEventPayload::Error { .. } => None,
            })
            .ok_or(UaiWorkerClientError::ResultMissing)
    }

    /// Invokes a worker and retains its bounded log/progress observations.
    pub async fn invoke_observed_result(
        &self,
        operation: &str,
        payload: Value,
    ) -> Result<WorkerInvocationResult, UaiWorkerClientError> {
        let events = self.invoke(operation, payload).await?;
        let mut result = None;
        let mut logs = Vec::new();
        let mut progress = Vec::new();
        for event in events {
            match event.payload {
                WorkerEventPayload::Log { level, message } => {
                    logs.push(WorkerLog { level, message });
                }
                WorkerEventPayload::Progress { current, total } => {
                    progress.push(WorkerProgress { current, total });
                }
                WorkerEventPayload::Result { data } => result = Some(data),
                WorkerEventPayload::Error { .. } => {}
            }
        }
        Ok(WorkerInvocationResult {
            data: result.ok_or(UaiWorkerClientError::ResultMissing)?,
            logs,
            progress,
        })
    }

    async fn invoke(
        &self,
        operation: &str,
        payload: Value,
    ) -> Result<Vec<WorkerEvent>, UaiWorkerClientError> {
        let request_id = Uuid::now_v7().to_string();
        match time::timeout(
            self.timeout,
            self.invoke_inner(&request_id, operation, payload),
        )
        .await
        {
            Ok(result) => result,
            Err(_) => Err(UaiWorkerClientError::Timeout(self.timeout)),
        }
    }

    async fn invoke_inner(
        &self,
        request_id: &str,
        operation: &str,
        payload: Value,
    ) -> Result<Vec<WorkerEvent>, UaiWorkerClientError> {
        let mut command = Command::new(&self.python);
        command
            .arg(&self.adapter)
            .arg("--upstream")
            .arg(&self.upstream)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if let Some(source_metadata) = &self.source_metadata {
            command.arg("--source-metadata").arg(source_metadata);
        }
        for (name, value) in &self.environment {
            command.env(name, value);
        }
        let mut child = command.spawn().map_err(UaiWorkerClientError::Spawn)?;
        let mut stdin = child
            .stdin
            .take()
            .ok_or(UaiWorkerClientError::PipeMissing)?;
        let stdout = child
            .stdout
            .take()
            .ok_or(UaiWorkerClientError::PipeMissing)?;
        let stderr = child
            .stderr
            .take()
            .ok_or(UaiWorkerClientError::PipeMissing)?;

        let request = WorkerRequest {
            request_id,
            operation,
            payload,
        };
        let mut request_bytes =
            serde_json::to_vec(&request).map_err(UaiWorkerClientError::Encode)?;
        request_bytes.push(b'\n');
        stdin
            .write_all(&request_bytes)
            .await
            .map_err(UaiWorkerClientError::Write)?;
        stdin
            .shutdown()
            .await
            .map_err(UaiWorkerClientError::Write)?;

        let stderr_task = tokio::spawn(capture_stderr(stderr));
        let events = read_events(stdout, &self.protocol, request_id, operation).await?;
        let status = child.wait().await.map_err(UaiWorkerClientError::Wait)?;
        let stderr = stderr_task
            .await
            .map_err(UaiWorkerClientError::StderrTask)?
            .map_err(UaiWorkerClientError::Read)?;

        if !status.success() && !events.iter().any(WorkerEvent::is_error) {
            return Err(UaiWorkerClientError::Exited {
                code: status.code(),
                stderr,
            });
        }
        if let Some((code, message)) = events.iter().find_map(WorkerEvent::remote_error) {
            return Err(UaiWorkerClientError::Remote {
                code: code.to_owned(),
                message: message.to_owned(),
            });
        }
        Ok(events)
    }

    #[must_use]
    pub fn python(&self) -> &Path {
        &self.python
    }

    #[must_use]
    pub fn adapter(&self) -> &Path {
        &self.adapter
    }

    #[must_use]
    pub fn upstream(&self) -> &Path {
        &self.upstream
    }
}

impl fmt::Debug for UaiWorkerClient {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UaiWorkerClient")
            .field("python", &self.python)
            .field("adapter", &self.adapter)
            .field("upstream", &self.upstream)
            .field("source_metadata", &self.source_metadata)
            .field("protocol", &self.protocol)
            .field("timeout", &self.timeout)
            .field(
                "environment_keys",
                &self.environment.keys().collect::<Vec<_>>(),
            )
            .finish()
    }
}

#[derive(Debug, Serialize)]
struct WorkerRequest<'a> {
    request_id: &'a str,
    operation: &'a str,
    payload: Value,
}

#[derive(Deserialize)]
struct WorkerEvent {
    protocol: String,
    request_id: String,
    operation: String,
    #[serde(flatten)]
    payload: WorkerEventPayload,
}

impl WorkerEvent {
    fn is_error(&self) -> bool {
        matches!(self.payload, WorkerEventPayload::Error { .. })
    }

    fn remote_error(&self) -> Option<(&str, &str)> {
        match &self.payload {
            WorkerEventPayload::Error { code, message } => Some((code, message)),
            WorkerEventPayload::Log { .. }
            | WorkerEventPayload::Progress { .. }
            | WorkerEventPayload::Result { .. } => None,
        }
    }
}

impl fmt::Debug for WorkerEvent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WorkerEvent")
            .field("protocol", &self.protocol)
            .field("request_id", &self.request_id)
            .field("operation", &self.operation)
            .field("payload", &"[REDACTED]")
            .finish()
    }
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum WorkerEventPayload {
    Log { level: String, message: String },
    Progress { current: u64, total: Option<u64> },
    Result { data: Value },
    Error { code: String, message: String },
}

#[derive(Debug)]
pub struct WorkerInvocationResult {
    pub data: Value,
    pub logs: Vec<WorkerLog>,
    pub progress: Vec<WorkerProgress>,
}

#[derive(Debug)]
pub struct WorkerLog {
    pub level: String,
    pub message: String,
}

#[derive(Debug)]
pub struct WorkerProgress {
    pub current: u64,
    pub total: Option<u64>,
}

async fn read_events(
    stdout: impl AsyncRead + Unpin,
    protocol: &str,
    request_id: &str,
    operation: &str,
) -> Result<Vec<WorkerEvent>, UaiWorkerClientError> {
    let mut lines = BufReader::new(stdout).lines();
    let mut events = Vec::new();
    while let Some(line) = lines
        .next_line()
        .await
        .map_err(UaiWorkerClientError::Read)?
    {
        if line.len() > MAX_EVENT_BYTES {
            return Err(UaiWorkerClientError::EventTooLarge(line.len()));
        }
        if events.len() >= MAX_EVENTS {
            return Err(UaiWorkerClientError::TooManyEvents);
        }
        let event: WorkerEvent =
            serde_json::from_str(&line).map_err(UaiWorkerClientError::InvalidEvent)?;
        if event.protocol != protocol
            || event.request_id != request_id
            || event.operation != operation
        {
            return Err(UaiWorkerClientError::EventBindingInvalid);
        }
        events.push(event);
    }
    Ok(events)
}

async fn capture_stderr(mut stderr: impl AsyncRead + Unpin) -> std::io::Result<String> {
    let mut captured = Vec::new();
    let mut buffer = [0_u8; 8 * 1024];
    loop {
        let read = stderr.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        let remaining = MAX_STDERR_CAPTURE_BYTES.saturating_sub(captured.len());
        captured.extend_from_slice(&buffer[..read.min(remaining)]);
    }
    Ok(String::from_utf8_lossy(&captured).into_owned())
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct UaiWorkerHealth {
    pub status: String,
    pub source: UaiWorkerSource,
    pub python: String,
    pub operations: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct UaiWorkerSource {
    pub name: String,
    pub revision: String,
    pub license: String,
    pub entrypoint_sha256: String,
}

#[derive(Debug, thiserror::Error)]
pub enum UaiWorkerClientError {
    #[error("failed to spawn UAI worker: {0}")]
    Spawn(std::io::Error),
    #[error("UAI worker pipe was not available")]
    PipeMissing,
    #[error("failed to encode UAI worker request: {0}")]
    Encode(serde_json::Error),
    #[error("failed to write UAI worker request: {0}")]
    Write(std::io::Error),
    #[error("failed to read UAI worker output: {0}")]
    Read(std::io::Error),
    #[error("failed to wait for UAI worker: {0}")]
    Wait(std::io::Error),
    #[error("UAI worker stderr task failed: {0}")]
    StderrTask(tokio::task::JoinError),
    #[error("UAI worker event is invalid: {0}")]
    InvalidEvent(serde_json::Error),
    #[error("UAI worker event binding is invalid")]
    EventBindingInvalid,
    #[error("UAI worker event exceeded the {MAX_EVENT_BYTES}-byte limit: {0}")]
    EventTooLarge(usize),
    #[error("UAI worker emitted too many events")]
    TooManyEvents,
    #[error("UAI worker timed out after {0:?}")]
    Timeout(Duration),
    #[error("UAI worker exited with code {code:?}: {stderr}")]
    Exited { code: Option<i32>, stderr: String },
    #[error("UAI worker failed ({code}): {message}")]
    Remote { code: String, message: String },
    #[error("UAI worker did not return a result")]
    ResultMissing,
    #[error("UAI worker health result is invalid: {0}")]
    InvalidHealthResult(serde_json::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace_path(relative: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../..")
            .join(relative)
    }

    #[tokio::test]
    async fn health_round_trips_through_the_python_adapter() {
        let client = UaiWorkerClient::new(
            "python",
            workspace_path("workers/uai/worker.py"),
            workspace_path("workers/uai/tests/fixtures/fake_upstream.py"),
        )
        .with_source_metadata(workspace_path(
            "workers/uai/tests/fixtures/fake_SOURCE.json",
        ));

        let health = client.health().await.unwrap();

        assert_eq!(health.status, "ok");
        assert_eq!(health.source.revision, "fixture-revision");
        assert!(
            health
                .operations
                .iter()
                .any(|operation| operation == "tasks")
        );
    }

    #[test]
    fn debug_output_contains_no_event_payload() {
        let event: WorkerEvent = serde_json::from_value(json!({
            "protocol": UAI_PROTOCOL,
            "request_id": "request-1",
            "operation": "health",
            "type": "result",
            "data": {"authorization": "secret-value"}
        }))
        .unwrap();

        let debug = format!("{event:?}");

        assert!(!debug.contains("secret-value"));
        assert!(debug.contains("[REDACTED]"));
    }
}
