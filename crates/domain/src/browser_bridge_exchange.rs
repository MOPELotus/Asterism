use serde::{Deserialize, Serialize};

use crate::{BrowserBridgeSessionId, Timestamp};

const MAX_MESSAGE_TYPE_BYTES: usize = 96;

/// Durable metadata for one provider-owned `BrowserBridge` command/result pair.
///
/// The provider payload remains outside the Core domain. Providers validate
/// their typed payloads and persist only a stable digest here, allowing Core to
/// enforce ordering, binding, recovery and replay protection without storing
/// protocol-specific credentials or browser data.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BrowserBridgeExchange {
    pub session_id: BrowserBridgeSessionId,
    pub sequence: u64,
    pub command_type: String,
    pub command_digest: [u8; 32],
    pub result_type: Option<String>,
    pub result_digest: Option<[u8; 32]>,
    pub state: BrowserBridgeExchangeState,
    pub issued_at: Timestamp,
    pub completed_at: Option<Timestamp>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserBridgeExchangeState {
    Issued,
    Completed,
    Rejected,
}

impl BrowserBridgeExchange {
    /// Creates a new command record. Sequence assignment is performed by the
    /// persistence boundary, so callers must provide the expected next value.
    ///
    /// # Errors
    ///
    /// Returns an error when the sequence, command type or digest is invalid.
    pub fn issue(
        session_id: BrowserBridgeSessionId,
        sequence: u64,
        command_type: String,
        command_digest: [u8; 32],
        issued_at: Timestamp,
    ) -> Result<Self, BrowserBridgeExchangeError> {
        let exchange = Self {
            session_id,
            sequence,
            command_type,
            command_digest,
            result_type: None,
            result_digest: None,
            state: BrowserBridgeExchangeState::Issued,
            issued_at,
            completed_at: None,
        };
        exchange.validate()?;
        Ok(exchange)
    }

    /// Attaches one provider-validated result to the immutable command.
    ///
    /// # Errors
    ///
    /// Returns an error when the exchange is already terminal or the result is
    /// malformed or timestamped before the command.
    pub fn complete(
        &mut self,
        result_type: String,
        result_digest: [u8; 32],
        completed_at: Timestamp,
    ) -> Result<(), BrowserBridgeExchangeError> {
        self.finish(
            BrowserBridgeExchangeState::Completed,
            result_type,
            result_digest,
            completed_at,
        )
    }

    /// Records a typed provider rejection without treating it as a transport
    /// failure or retryable mutation.
    ///
    /// # Errors
    ///
    /// Returns an error when the exchange is already terminal or the result is
    /// malformed or timestamped before the command.
    pub fn reject(
        &mut self,
        result_type: String,
        result_digest: [u8; 32],
        completed_at: Timestamp,
    ) -> Result<(), BrowserBridgeExchangeError> {
        self.finish(
            BrowserBridgeExchangeState::Rejected,
            result_type,
            result_digest,
            completed_at,
        )
    }

    /// # Errors
    ///
    /// Returns an error when any sequence, type, digest, state or timestamp
    /// invariant is violated.
    pub fn validate(&self) -> Result<(), BrowserBridgeExchangeError> {
        if self.sequence == 0 || i64::try_from(self.sequence).is_err() {
            return Err(BrowserBridgeExchangeError::InvalidSequence);
        }
        validate_type(&self.command_type)?;
        if self.command_digest == [0; 32] {
            return Err(BrowserBridgeExchangeError::InvalidDigest);
        }
        match (
            self.state,
            &self.result_type,
            self.result_digest,
            self.completed_at,
        ) {
            (BrowserBridgeExchangeState::Issued, None, None, None) => {}
            (
                BrowserBridgeExchangeState::Completed | BrowserBridgeExchangeState::Rejected,
                Some(result_type),
                Some(result_digest),
                Some(completed_at),
            ) => {
                validate_type(result_type)?;
                if result_digest == [0; 32] || completed_at < self.issued_at {
                    return Err(BrowserBridgeExchangeError::InvalidResult);
                }
            }
            _ => return Err(BrowserBridgeExchangeError::InvalidResult),
        }
        Ok(())
    }

    fn finish(
        &mut self,
        state: BrowserBridgeExchangeState,
        result_type: String,
        result_digest: [u8; 32],
        completed_at: Timestamp,
    ) -> Result<(), BrowserBridgeExchangeError> {
        if self.state != BrowserBridgeExchangeState::Issued {
            return Err(BrowserBridgeExchangeError::AlreadyFinished);
        }
        self.state = state;
        self.result_type = Some(result_type);
        self.result_digest = Some(result_digest);
        self.completed_at = Some(completed_at);
        self.validate()
    }
}

fn validate_type(value: &str) -> Result<(), BrowserBridgeExchangeError> {
    if value.is_empty()
        || value.len() > MAX_MESSAGE_TYPE_BYTES
        || !value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_' | b'.')
        })
    {
        Err(BrowserBridgeExchangeError::InvalidMessageType)
    } else {
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum BrowserBridgeExchangeError {
    #[error("BrowserBridge exchange sequence must be a positive SQLite integer")]
    InvalidSequence,
    #[error("BrowserBridge exchange message type is invalid or unbounded")]
    InvalidMessageType,
    #[error("BrowserBridge exchange digest must be non-zero")]
    InvalidDigest,
    #[error("BrowserBridge exchange result is invalid or timestamped before its command")]
    InvalidResult,
    #[error("BrowserBridge exchange already has a terminal result")]
    AlreadyFinished,
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, Utc};

    use super::*;

    #[test]
    fn exchange_requires_one_bounded_result_and_preserves_command_identity() {
        let now = Utc::now();
        let mut exchange = BrowserBridgeExchange::issue(
            BrowserBridgeSessionId::new(),
            1,
            "cidaren.capture.snapshot".to_owned(),
            [1; 32],
            now,
        )
        .unwrap();
        exchange
            .complete(
                "capture.snapshot.accepted".to_owned(),
                [2; 32],
                now + Duration::seconds(1),
            )
            .unwrap();
        assert_eq!(exchange.state, BrowserBridgeExchangeState::Completed);
        assert_eq!(exchange.command_digest, [1; 32]);
        assert_eq!(
            exchange.complete("other".to_owned(), [3; 32], now + Duration::seconds(2)),
            Err(BrowserBridgeExchangeError::AlreadyFinished)
        );
    }

    #[test]
    fn exchange_rejects_unsafe_type_and_regressing_result() {
        let now = Utc::now();
        assert_eq!(
            BrowserBridgeExchange::issue(
                BrowserBridgeSessionId::new(),
                1,
                "Unsafe Type".to_owned(),
                [1; 32],
                now,
            ),
            Err(BrowserBridgeExchangeError::InvalidMessageType)
        );
        let mut exchange = BrowserBridgeExchange::issue(
            BrowserBridgeSessionId::new(),
            1,
            "uai.residence.pause".to_owned(),
            [1; 32],
            now,
        )
        .unwrap();
        assert_eq!(
            exchange.complete("rejected".to_owned(), [2; 32], now - Duration::seconds(1)),
            Err(BrowserBridgeExchangeError::InvalidResult)
        );
    }
}
