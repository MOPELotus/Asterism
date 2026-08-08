use serde::{Deserialize, Serialize};

use crate::{AuthBootstrapSessionId, Timestamp};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AuthBootstrapClientEvent {
    pub session_id: AuthBootstrapSessionId,
    pub sequence: u64,
    pub kind: AuthBootstrapClientEventKind,
    pub received_at: Timestamp,
}

impl AuthBootstrapClientEvent {
    /// Creates one server-timestamped, non-secret Capture status event.
    ///
    /// # Errors
    ///
    /// Rejects a zero or SQLite-unrepresentable sequence, unsafe stage/code
    /// labels, or progress outside 0-100.
    pub fn new(
        session_id: AuthBootstrapSessionId,
        sequence: u64,
        kind: AuthBootstrapClientEventKind,
        received_at: Timestamp,
    ) -> Result<Self, AuthBootstrapClientEventError> {
        let event = Self {
            session_id,
            sequence,
            kind,
            received_at,
        };
        event.validate()?;
        Ok(event)
    }

    /// Validates a persisted or transport-decoded event independently from
    /// construction.
    ///
    /// # Errors
    ///
    /// Rejects invalid sequence, labels, or progress values.
    pub fn validate(&self) -> Result<(), AuthBootstrapClientEventError> {
        if self.sequence == 0 || i64::try_from(self.sequence).is_err() {
            return Err(AuthBootstrapClientEventError::InvalidSequence);
        }
        match &self.kind {
            AuthBootstrapClientEventKind::StageChanged { stage } => {
                validate_label(stage)?;
            }
            AuthBootstrapClientEventKind::Progress { stage, percent } => {
                validate_label(stage)?;
                if *percent > 100 {
                    return Err(AuthBootstrapClientEventError::InvalidProgress);
                }
            }
            AuthBootstrapClientEventKind::Failed { code } => validate_label(code)?,
            AuthBootstrapClientEventKind::ClientReady
            | AuthBootstrapClientEventKind::CredentialDetected
            | AuthBootstrapClientEventKind::Validating
            | AuthBootstrapClientEventKind::ClientReportedAuthenticated => {}
        }
        Ok(())
    }

    /// Client-reported success is diagnostic only. Core must still run
    /// Provider-side credential validation before authenticating an account.
    pub const fn is_client_success_report(&self) -> bool {
        matches!(
            self.kind,
            AuthBootstrapClientEventKind::ClientReportedAuthenticated
        )
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum AuthBootstrapClientEventKind {
    ClientReady,
    StageChanged {
        stage: String,
    },
    Progress {
        stage: String,
        percent: u8,
    },
    CredentialDetected,
    Validating,
    #[serde(rename = "authenticated")]
    ClientReportedAuthenticated,
    Failed {
        code: String,
    },
}

fn validate_label(value: &str) -> Result<(), AuthBootstrapClientEventError> {
    let valid = !value.is_empty()
        && value.len() <= 64
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_' | b'.')
        });
    if valid {
        Ok(())
    } else {
        Err(AuthBootstrapClientEventError::InvalidLabel)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum AuthBootstrapClientEventError {
    #[error("authentication bootstrap event sequence must be a positive SQLite integer")]
    InvalidSequence,
    #[error("authentication bootstrap event labels must be bounded safe lowercase identifiers")]
    InvalidLabel,
    #[error("authentication bootstrap event progress must be between 0 and 100")]
    InvalidProgress,
}

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use super::*;

    #[test]
    fn event_accepts_bounded_status_without_client_timestamps_or_messages() {
        let event = AuthBootstrapClientEvent::new(
            AuthBootstrapSessionId::new(),
            7,
            AuthBootstrapClientEventKind::Progress {
                stage: "browser.callback".to_owned(),
                percent: 75,
            },
            Utc::now(),
        )
        .unwrap();
        assert_eq!(event.sequence, 7);
        assert!(!event.is_client_success_report());
        event.validate().unwrap();
    }

    #[test]
    fn client_authenticated_wire_signal_never_claims_core_success() {
        let event = AuthBootstrapClientEvent::new(
            AuthBootstrapSessionId::new(),
            1,
            AuthBootstrapClientEventKind::ClientReportedAuthenticated,
            Utc::now(),
        )
        .unwrap();
        let wire = serde_json::to_value(&event).unwrap();
        assert_eq!(wire["kind"]["type"], "authenticated");
        assert!(event.is_client_success_report());
    }

    #[test]
    fn event_rejects_unbounded_labels_progress_and_sequences() {
        let session_id = AuthBootstrapSessionId::new();
        let now = Utc::now();
        assert_eq!(
            AuthBootstrapClientEvent::new(
                session_id,
                0,
                AuthBootstrapClientEventKind::ClientReady,
                now,
            ),
            Err(AuthBootstrapClientEventError::InvalidSequence)
        );
        assert_eq!(
            AuthBootstrapClientEvent::new(
                session_id,
                1,
                AuthBootstrapClientEventKind::StageChanged {
                    stage: "Unsafe Stage".to_owned(),
                },
                now,
            ),
            Err(AuthBootstrapClientEventError::InvalidLabel)
        );
        assert_eq!(
            AuthBootstrapClientEvent::new(
                session_id,
                1,
                AuthBootstrapClientEventKind::Progress {
                    stage: "login".to_owned(),
                    percent: 101,
                },
                now,
            ),
            Err(AuthBootstrapClientEventError::InvalidProgress)
        );
    }
}
