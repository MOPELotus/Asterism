use std::{collections::HashSet, str::FromStr};

use anyhow::{Context, bail};
use asterism_capture::{
    CaptureClient, CaptureCredentialAccepted, CaptureCredentialField, CaptureCredentialSubmission,
};
use asterism_domain::{
    AuthBootstrapClientEventKind, AuthBootstrapPurpose, AuthBootstrapSessionId, AuthMethod,
    ProviderAccountId, ProviderId, SessionKind, Timestamp,
};
use asterism_secrets::SecretPurpose;
use chrono::Utc;
use clap::{Args, ValueEnum};
use serde::Serialize;

use crate::input::{read_secret, read_text};

const MAX_BOOTSTRAP_TOKEN_BYTES: usize = 128;
const MAX_CREDENTIAL_FIELD_BYTES: usize = 1024 * 1024;
const MAX_DISPLAY_NAME_BYTES: usize = 128;
const MAX_TENANT_BYTES: usize = 256;

#[derive(Debug, Args)]
pub struct ManualCommand {
    /// Auth Bootstrap session UUID shown by the trusted Asterism frontend.
    #[arg(long)]
    session_id: String,

    /// Provider authentication method represented by the captured value.
    #[arg(long, value_enum)]
    auth_method: ManualAuthMethod,

    /// Runtime session shape represented by the captured value.
    #[arg(long, value_enum)]
    session_kind: ManualSessionKind,

    /// Credential field purpose; repeat for composite credentials.
    #[arg(long = "field", value_enum, required = true)]
    fields: Vec<ManualSecretPurpose>,

    /// Read optional tenant metadata as an additional hidden input.
    #[arg(long)]
    with_tenant: bool,

    /// Optional credential expiry timestamp in RFC 3339 form.
    #[arg(long)]
    expires_at: Option<Timestamp>,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum ManualAuthMethod {
    AssistedSession,
    ImportedCookie,
    ImportedToken,
}

impl From<ManualAuthMethod> for AuthMethod {
    fn from(value: ManualAuthMethod) -> Self {
        match value {
            ManualAuthMethod::AssistedSession => Self::AssistedSession,
            ManualAuthMethod::ImportedCookie => Self::ImportedCookie,
            ManualAuthMethod::ImportedToken => Self::ImportedToken,
        }
    }
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum ManualSessionKind {
    Cookie,
    BearerToken,
    Jwt,
    Composite,
    ProviderSpecific,
}

impl From<ManualSessionKind> for SessionKind {
    fn from(value: ManualSessionKind) -> Self {
        match value {
            ManualSessionKind::Cookie => Self::Cookie,
            ManualSessionKind::BearerToken => Self::BearerToken,
            ManualSessionKind::Jwt => Self::Jwt,
            ManualSessionKind::Composite => Self::Composite,
            ManualSessionKind::ProviderSpecific => Self::ProviderSpecific,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, ValueEnum)]
enum ManualSecretPurpose {
    Cookie,
    AccessToken,
    RefreshToken,
    CompositeSession,
}

impl From<ManualSecretPurpose> for SecretPurpose {
    fn from(value: ManualSecretPurpose) -> Self {
        match value {
            ManualSecretPurpose::Cookie => Self::ProviderCookie,
            ManualSecretPurpose::AccessToken => Self::ProviderAccessToken,
            ManualSecretPurpose::RefreshToken => Self::ProviderRefreshToken,
            ManualSecretPurpose::CompositeSession => Self::ProviderCompositeSession,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ManualCredentialSummary {
    session_id: AuthBootstrapSessionId,
    provider_id: ProviderId,
    provider_account_id: ProviderAccountId,
    credential_count: usize,
    session_kind: SessionKind,
    expires_at: Option<Timestamp>,
}

pub async fn run_manual(
    client: &CaptureClient,
    command: ManualCommand,
) -> anyhow::Result<ManualCredentialSummary> {
    validate_command(&command)?;
    let session_id = AuthBootstrapSessionId::from_str(&command.session_id)
        .context("session ID must be a valid UUID")?;
    let pairing_token = read_secret("Pairing token: ", MAX_BOOTSTRAP_TOKEN_BYTES)?;
    let mut claimed = client.claim(session_id, &pairing_token).await?;
    drop(pairing_token);
    client.poll_session(&mut claimed).await?;
    client
        .record_event(&mut claimed, AuthBootstrapClientEventKind::ClientReady)
        .await?;

    let display_name = if claimed.session().purpose == AuthBootstrapPurpose::AddAccount {
        Some(read_text("Account display name: ", MAX_DISPLAY_NAME_BYTES)?)
    } else {
        None
    };
    let tenant = command
        .with_tenant
        .then(|| read_secret("Tenant: ", MAX_TENANT_BYTES))
        .transpose()?;
    client
        .record_event(
            &mut claimed,
            AuthBootstrapClientEventKind::StageChanged {
                stage: "manual.input".to_owned(),
            },
        )
        .await?;
    let fields = command
        .fields
        .iter()
        .map(|purpose| {
            let value = read_secret(
                &format!(
                    "{}: ",
                    purpose.to_possible_value().expect("known value").get_name()
                ),
                MAX_CREDENTIAL_FIELD_BYTES,
            )?;
            CaptureCredentialField::new((*purpose).into(), value)
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    client
        .record_event(
            &mut claimed,
            AuthBootstrapClientEventKind::CredentialDetected,
        )
        .await?;
    client
        .record_event(&mut claimed, AuthBootstrapClientEventKind::Validating)
        .await?;
    let submission = CaptureCredentialSubmission::new(
        display_name,
        tenant,
        command.auth_method.into(),
        command.session_kind.into(),
        command.expires_at,
        fields,
    );
    let accepted = client.submit_credential(&mut claimed, submission).await?;
    Ok(summary(&accepted))
}

fn validate_command(command: &ManualCommand) -> anyhow::Result<()> {
    let unique = command.fields.iter().copied().collect::<HashSet<_>>();
    if unique.len() != command.fields.len() {
        bail!("manual credential fields must not repeat a purpose");
    }
    if command
        .expires_at
        .is_some_and(|expiry| expiry <= Utc::now())
    {
        bail!("credential expiry must be in the future");
    }
    Ok(())
}

fn summary(accepted: &CaptureCredentialAccepted) -> ManualCredentialSummary {
    ManualCredentialSummary {
        session_id: accepted.session.id,
        provider_id: accepted.session.provider_id.clone(),
        provider_account_id: accepted.provider_account_id,
        credential_count: accepted.credential_count,
        session_kind: accepted.status.kind,
        expires_at: accepted.status.expires_at,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manual_command_rejects_duplicate_fields_before_claiming() {
        let command = ManualCommand {
            session_id: AuthBootstrapSessionId::new().to_string(),
            auth_method: ManualAuthMethod::ImportedCookie,
            session_kind: ManualSessionKind::Cookie,
            fields: vec![ManualSecretPurpose::Cookie, ManualSecretPurpose::Cookie],
            with_tenant: false,
            expires_at: None,
        };
        assert!(validate_command(&command).is_err());
    }
}
