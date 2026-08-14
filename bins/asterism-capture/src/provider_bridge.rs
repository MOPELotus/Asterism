use std::time::Duration;

use anyhow::{Context, bail};
use asterism_domain::Timestamp;
use asterism_provider_cidaren::{
    CIDAREN_CAPTURE_COMMAND_TYPE, CidarenBrowserCaptureSource, CidarenBrowserCapturedValue,
    CidarenBrowserHelperProjection, project_browser_helper_command,
};
use asterism_secrets::SecretValue;
use chrono::Utc;

use crate::{
    BrowserBridgeCommand, BrowserBridgeDocumentBinding, BrowserBridgeReadSnapshot,
    ChromiumBrowserBridge,
};

const CIDAREN_READ_POLL_INTERVAL: Duration = Duration::from_millis(500);

/// One Provider-encoded result ready for the durable Core inbox.
pub struct BrowserBridgeProviderResult {
    result_type: &'static str,
    artifact: SecretValue,
    terminal: bool,
}

impl BrowserBridgeProviderResult {
    pub const fn result_type(&self) -> &'static str {
        self.result_type
    }

    pub const fn is_terminal(&self) -> bool {
        self.terminal
    }

    pub fn into_parts(self) -> (&'static str, SecretValue, bool) {
        (self.result_type, self.artifact, self.terminal)
    }
}

impl std::fmt::Debug for BrowserBridgeProviderResult {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BrowserBridgeProviderResult")
            .field("result_type", &self.result_type)
            .field("artifact", &"[REDACTED]")
            .field("terminal", &self.terminal)
            .finish()
    }
}

/// Executes one projected Cidaren Capture command against same-document,
/// policy-bound browser facts and returns its exact Provider result artifact.
///
/// # Errors
///
/// Rejects foreign command types/bindings, browser drift, expired sessions,
/// unsafe captured values or Provider result validation failure.
pub async fn handle_cidaren_browser_command(
    browser: &mut ChromiumBrowserBridge,
    command: BrowserBridgeCommand,
    expires_at: Timestamp,
) -> anyhow::Result<BrowserBridgeProviderResult> {
    if command.command_type != CIDAREN_CAPTURE_COMMAND_TYPE {
        bail!("Core dispatched a foreign command type to the Cidaren helper");
    }
    let binding = browser.document_binding().await?;
    let projection = project_browser_helper_command(
        command.command_artifact,
        command.command_digest,
        command.session_id,
        &binding.observed_origin,
        &binding.frame_id,
        command.sequence,
    )
    .context("Cidaren rejected the BrowserBridge command dispatch")?;
    let sources = projection
        .user_token_sources()
        .iter()
        .chain(projection.login_info_sources())
        .map(|source| source.into_read_source())
        .collect::<Vec<_>>();

    loop {
        if Utc::now() >= expires_at {
            bail!("Cidaren BrowserBridge session expired before one complete snapshot");
        }
        let mut snapshot = browser.read_snapshot(&sources).await?;
        require_same_binding(&binding, snapshot.binding())?;
        if let Some(captured) = capture_cidaren_snapshot(&projection, &mut snapshot) {
            let result = projection
                .into_result_artifact(captured)
                .context("Cidaren rejected the captured BrowserBridge snapshot")?;
            let result_type = result.result_type();
            return Ok(BrowserBridgeProviderResult {
                result_type,
                artifact: result.into_secret_value(),
                terminal: true,
            });
        }
        tokio::time::sleep(CIDAREN_READ_POLL_INTERVAL).await;
    }
}

fn require_same_binding(
    expected: &BrowserBridgeDocumentBinding,
    actual: &BrowserBridgeDocumentBinding,
) -> anyhow::Result<()> {
    if actual == expected {
        Ok(())
    } else {
        bail!("BrowserBridge document binding changed after command projection")
    }
}

fn capture_cidaren_snapshot(
    projection: &CidarenBrowserHelperProjection,
    snapshot: &mut BrowserBridgeReadSnapshot,
) -> Option<Vec<CidarenBrowserCapturedValue>> {
    let mut captured = Vec::with_capacity(2);
    captured.push(take_first_cidaren_source(
        snapshot,
        projection.user_token_sources(),
    )?);
    if !projection.login_info_sources().is_empty() {
        captured.push(take_first_cidaren_source(
            snapshot,
            projection.login_info_sources(),
        )?);
    }
    Some(captured)
}

fn take_first_cidaren_source(
    snapshot: &mut BrowserBridgeReadSnapshot,
    sources: &[CidarenBrowserCaptureSource],
) -> Option<CidarenBrowserCapturedValue> {
    sources.iter().find_map(|source| {
        snapshot
            .take(&source.into_read_source())
            .map(|secret| CidarenBrowserCapturedValue::from((*source, secret)))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_result_debug_never_exposes_artifact_bytes() {
        let result = BrowserBridgeProviderResult {
            result_type: "cidaren.capture.snapshot.result",
            artifact: SecretValue::new(b"captured-secret".to_vec()),
            terminal: true,
        };
        let debug = format!("{result:?}");
        assert!(debug.contains("cidaren.capture.snapshot.result"));
        assert!(!debug.contains("captured-secret"));
        assert!(result.is_terminal());
    }
}
