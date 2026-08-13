use std::{path::PathBuf, str::FromStr};

use anyhow::Context;
use asterism_capture::{
    CaptureClient, CaptureCredentialAccepted, CaptureCredentialSubmission, ChromiumCapture,
};
use asterism_domain::{
    AuthBootstrapClientEventKind, AuthBootstrapPurpose, AuthBootstrapSessionId, ProviderAccountId,
    ProviderId, SessionKind, Timestamp,
};
use clap::Args;
use serde::Serialize;

use crate::input::{read_secret, read_text};

const MAX_BOOTSTRAP_TOKEN_BYTES: usize = 128;
const MAX_DISPLAY_NAME_BYTES: usize = 128;
const MAX_TENANT_BYTES: usize = 256;

#[derive(Debug, Args)]
pub struct AutomaticCommand {
    /// Auth Bootstrap session UUID shown by the trusted Asterism frontend.
    #[arg(long)]
    session_id: String,

    /// Explicit Chromium/Edge executable path when automatic discovery is unsuitable.
    #[arg(long)]
    browser_path: Option<PathBuf>,

    /// Read optional tenant metadata as an additional hidden input.
    #[arg(long)]
    with_tenant: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct AutomaticCredentialSummary {
    session_id: AuthBootstrapSessionId,
    provider_id: ProviderId,
    provider_account_id: ProviderAccountId,
    credential_count: usize,
    session_kind: SessionKind,
    expires_at: Option<Timestamp>,
}

pub async fn run_automatic(
    client: &CaptureClient,
    command: AutomaticCommand,
) -> anyhow::Result<AutomaticCredentialSummary> {
    let session_id = AuthBootstrapSessionId::from_str(&command.session_id)
        .context("session ID must be a valid UUID")?;
    let pairing_token = read_secret("Pairing token: ", MAX_BOOTSTRAP_TOKEN_BYTES).await?;
    let mut claimed = client.claim(session_id, &pairing_token).await?;
    drop(pairing_token);
    client.poll_session(&mut claimed).await?;
    client
        .record_event(&mut claimed, AuthBootstrapClientEventKind::ClientReady)
        .await?;

    let display_name = if claimed.session().purpose == AuthBootstrapPurpose::AddAccount {
        Some(read_text("Account display name: ", MAX_DISPLAY_NAME_BYTES).await?)
    } else {
        None
    };
    let tenant = if command.with_tenant {
        Some(read_secret("Tenant: ", MAX_TENANT_BYTES).await?)
    } else {
        None
    };
    client
        .record_event(
            &mut claimed,
            AuthBootstrapClientEventKind::StageChanged {
                stage: "browser.launch".to_owned(),
            },
        )
        .await?;
    let recipe = claimed.recipe().clone();
    let mut browser =
        ChromiumCapture::launch(recipe.clone(), command.browser_path.as_deref()).await?;
    let fields = browser.capture_until(claimed.session().expires_at).await?;
    browser.shutdown().await?;
    client
        .record_event(
            &mut claimed,
            AuthBootstrapClientEventKind::CredentialDetected,
        )
        .await?;
    client
        .record_event(&mut claimed, AuthBootstrapClientEventKind::Validating)
        .await?;
    let accepted = client
        .submit_credential(
            &mut claimed,
            CaptureCredentialSubmission::new(
                display_name,
                tenant,
                recipe.auth_method,
                recipe.session_kind,
                None,
                fields,
            ),
        )
        .await?;
    Ok(summary(&accepted))
}

fn summary(accepted: &CaptureCredentialAccepted) -> AutomaticCredentialSummary {
    AutomaticCredentialSummary {
        session_id: accepted.session.id,
        provider_id: accepted.session.provider_id.clone(),
        provider_account_id: accepted.provider_account_id,
        credential_count: accepted.credential_count,
        session_kind: accepted.status.kind,
        expires_at: accepted.status.expires_at,
    }
}
