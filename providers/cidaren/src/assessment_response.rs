use std::fmt;

use asterism_provider_api::{ProviderError, ProviderErrorKind, ProviderResult};
use serde_json::Value;
use zeroize::Zeroize;

use crate::{CidarenCryptoContext, decode_response_data};

const MAX_RESPONSE_BYTES: usize = 2 * 1_024 * 1_024;
const MAX_MESSAGE_BYTES: usize = 2_048;

/// Bounded donor acknowledgement classification. A terminal acknowledgement
/// is only a mutation receipt; it never substitutes for fresh verification.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CidarenAssessmentReceiptKind {
    Accepted,
    Completed,
    WordSelectionRequired,
}

/// Strict decoded result of one Cidaren assessment protocol call.
pub enum CidarenAssessmentResponse {
    Payload(CidarenDecodedAssessmentPayload),
    Receipt {
        kind: CidarenAssessmentReceiptKind,
        message_sanitized: Option<String>,
    },
}

impl fmt::Debug for CidarenAssessmentResponse {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Payload(_) => formatter.write_str("Payload([REDACTED])"),
            Self::Receipt {
                kind,
                message_sanitized,
            } => formatter
                .debug_struct("Receipt")
                .field("kind", kind)
                .field("has_message", &message_sanitized.is_some())
                .finish(),
        }
    }
}

/// Zeroizing ownership wrapper for one decoded topic/word payload.
pub struct CidarenDecodedAssessmentPayload(Value);

impl CidarenDecodedAssessmentPayload {
    pub fn as_value(&self) -> &Value {
        &self.0
    }
}

impl fmt::Debug for CidarenDecodedAssessmentPayload {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("CidarenDecodedAssessmentPayload([REDACTED])")
    }
}

impl Drop for CidarenDecodedAssessmentPayload {
    fn drop(&mut self) {
        zeroize_json(&mut self.0);
    }
}

/// Parses one bounded response envelope and dispatches its declared response
/// version to the exact legacy or captured `jv=99` decoder.
///
/// Localized completion messages and terminal codes are retained only as
/// acknowledgement facts. Callers must fresh-read the stable Task before
/// treating either as verified completion.
///
/// # Errors
///
/// Returns a typed Provider error for malformed framing, non-success codes,
/// missing data, crypto-context mismatch or protocol drift.
pub fn parse_assessment_response(
    document: &[u8],
    crypto: Option<&CidarenCryptoContext>,
) -> ProviderResult<CidarenAssessmentResponse> {
    if document.is_empty() || document.len() > MAX_RESPONSE_BYTES {
        return Err(invalid_response(
            "Cidaren assessment response is empty or exceeds the size limit",
        ));
    }
    let mut root: Value = serde_json::from_slice(document)
        .map_err(|_| invalid_response("Cidaren assessment response is not valid JSON"))?;
    let parsed = parse_root(&root, crypto);
    zeroize_json(&mut root);
    parsed
}

/// Parses the acknowledgement-only response emitted by the donor-observed
/// `SubmitChoseWord` mutation.
///
/// Unlike an assessment-step response, this endpoint does not need to return a
/// decoded topic payload. Accepting its bounded success envelope here keeps
/// the stricter `StartAnswer`/`VerifyAnswer`/advance parser fail-closed when
/// their required `data` field disappears.
///
/// # Errors
///
/// Returns a typed Provider error for malformed framing, non-success codes or
/// an invalid message. No response body or dynamic crypto material is retained.
pub fn parse_word_selection_response(document: &[u8]) -> ProviderResult<CidarenAssessmentResponse> {
    if document.is_empty() || document.len() > MAX_RESPONSE_BYTES {
        return Err(invalid_response(
            "Cidaren word-selection response is empty or exceeds the size limit",
        ));
    }
    let mut root: Value = serde_json::from_slice(document)
        .map_err(|_| invalid_response("Cidaren word-selection response is not valid JSON"))?;
    let parsed = parse_word_selection_root(&root);
    zeroize_json(&mut root);
    parsed
}

fn parse_root(
    root: &Value,
    crypto: Option<&CidarenCryptoContext>,
) -> ProviderResult<CidarenAssessmentResponse> {
    let object = root
        .as_object()
        .ok_or_else(|| protocol_drift("Cidaren assessment response is not an object"))?;
    let code = object
        .get("code")
        .and_then(Value::as_i64)
        .ok_or_else(|| protocol_drift("Cidaren assessment response has no numeric code"))?;
    let message = optional_message(object.get("msg"))?;

    if code == 20_004 {
        return Ok(CidarenAssessmentResponse::Receipt {
            kind: CidarenAssessmentReceiptKind::Completed,
            message_sanitized: message,
        });
    }
    if code == 20_001 && message.as_deref() == Some("需要选词！") {
        return Ok(CidarenAssessmentResponse::Receipt {
            kind: CidarenAssessmentReceiptKind::WordSelectionRequired,
            message_sanitized: message,
        });
    }
    if code != 1 && !(code == 20_001 && object.get("data").is_some_and(json_truthy)) {
        return Err(ProviderError::new(
            ProviderErrorKind::InvalidResponse,
            "Cidaren assessment endpoint returned a non-success code",
        ));
    }
    if message.as_deref() == Some("任务已完成！") {
        return Ok(CidarenAssessmentResponse::Receipt {
            kind: CidarenAssessmentReceiptKind::Completed,
            message_sanitized: message,
        });
    }
    if message.as_deref() == Some("需要选词！") {
        return Ok(CidarenAssessmentResponse::Receipt {
            kind: CidarenAssessmentReceiptKind::WordSelectionRequired,
            message_sanitized: message,
        });
    }
    let data = object
        .get("data")
        .filter(|value| !value.is_null())
        .ok_or_else(|| protocol_drift("Cidaren assessment response has no data"))?;
    let jv = match object.get("jv") {
        None | Some(Value::Null) => "0",
        Some(Value::String(value)) if !value.is_empty() && value.len() <= 64 => value,
        _ => {
            return Err(protocol_drift(
                "Cidaren assessment response has an invalid jv",
            ));
        }
    };
    Ok(CidarenAssessmentResponse::Payload(
        CidarenDecodedAssessmentPayload(decode_response_data(data, jv, crypto)?),
    ))
}

fn parse_word_selection_root(root: &Value) -> ProviderResult<CidarenAssessmentResponse> {
    let object = root
        .as_object()
        .ok_or_else(|| protocol_drift("Cidaren word-selection response is not an object"))?;
    let code = object
        .get("code")
        .and_then(Value::as_i64)
        .ok_or_else(|| protocol_drift("Cidaren word-selection response has no numeric code"))?;
    let message = optional_message(object.get("msg"))?;
    if code == 20_004 {
        return Ok(CidarenAssessmentResponse::Receipt {
            kind: CidarenAssessmentReceiptKind::Completed,
            message_sanitized: message,
        });
    }
    if code == 20_001 && message.as_deref() == Some("需要选词！") {
        return Ok(CidarenAssessmentResponse::Receipt {
            kind: CidarenAssessmentReceiptKind::WordSelectionRequired,
            message_sanitized: message,
        });
    }
    if code != 1 && !(code == 20_001 && object.get("data").is_some_and(json_truthy)) {
        return Err(ProviderError::new(
            ProviderErrorKind::InvalidResponse,
            "Cidaren word-selection endpoint returned a non-success code",
        ));
    }
    if message.as_deref() == Some("任务已完成！") {
        return Ok(CidarenAssessmentResponse::Receipt {
            kind: CidarenAssessmentReceiptKind::Completed,
            message_sanitized: message,
        });
    }
    Ok(CidarenAssessmentResponse::Receipt {
        kind: CidarenAssessmentReceiptKind::Accepted,
        message_sanitized: message,
    })
}

fn json_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(value) => value.as_f64().is_some_and(|value| value != 0.0),
        Value::String(value) => !value.is_empty(),
        Value::Array(value) => !value.is_empty(),
        Value::Object(value) => !value.is_empty(),
    }
}

fn optional_message(value: Option<&Value>) -> ProviderResult<Option<String>> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value))
            if !value.is_empty()
                && value.len() <= MAX_MESSAGE_BYTES
                && value.trim() == value
                && !value.chars().any(char::is_control) =>
        {
            Ok(Some(value.to_owned()))
        }
        _ => Err(protocol_drift(
            "Cidaren assessment response message is invalid",
        )),
    }
}

fn zeroize_json(value: &mut Value) {
    match value {
        Value::String(value) => value.zeroize(),
        Value::Array(values) => values.iter_mut().for_each(zeroize_json),
        Value::Object(values) => values.values_mut().for_each(zeroize_json),
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

fn protocol_drift(message: impl Into<String>) -> ProviderError {
    ProviderError::new(ProviderErrorKind::ProtocolDrift, message)
}

fn invalid_response(message: impl Into<String>) -> ProviderError {
    ProviderError::new(ProviderErrorKind::InvalidResponse, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    const START_ENVELOPE: &str =
        include_str!("../../../fixtures/providers/cidaren/questions/start-answer-envelope.json");

    #[test]
    fn payload_is_strictly_decoded_and_redacted() {
        let response = parse_assessment_response(START_ENVELOPE.as_bytes(), None).unwrap();
        let CidarenAssessmentResponse::Payload(payload) = response else {
            panic!("expected payload");
        };
        assert_eq!(payload.as_value()["topic_mode"], 17);
        assert!(!format!("{payload:?}").contains("synthetic-topic-code"));
    }

    #[test]
    fn donor_terminal_messages_are_receipts_not_verified_outcomes() {
        for (document, expected) in [
            (
                serde_json::json!({"code": 1, "msg": "任务已完成！"}),
                CidarenAssessmentReceiptKind::Completed,
            ),
            (
                serde_json::json!({"code": 1, "msg": "需要选词！"}),
                CidarenAssessmentReceiptKind::WordSelectionRequired,
            ),
            (
                serde_json::json!({"code": 20004, "msg": "synthetic terminal"}),
                CidarenAssessmentReceiptKind::Completed,
            ),
        ] {
            let encoded = serde_json::to_vec(&document).unwrap();
            let response = parse_assessment_response(&encoded, None).unwrap();
            assert!(matches!(
                response,
                CidarenAssessmentResponse::Receipt { kind, .. } if kind == expected
            ));
        }
        let selection_required = serde_json::json!({
            "code": 20001,
            "msg": "需要选词！",
            "data": null,
            "jv": "0",
            "cv": "0",
        });
        let encoded = serde_json::to_vec(&selection_required).unwrap();
        for response in [
            parse_assessment_response(&encoded, None).unwrap(),
            parse_word_selection_response(&encoded).unwrap(),
        ] {
            assert!(matches!(
                response,
                CidarenAssessmentResponse::Receipt {
                    kind: CidarenAssessmentReceiptKind::WordSelectionRequired,
                    ..
                }
            ));
        }
        for message in ["任务已完成！", "需要选词！"] {
            assert!(
                parse_assessment_response(
                    &serde_json::to_vec(&serde_json::json!({
                        "code": 0,
                        "msg": message,
                    }))
                    .unwrap(),
                    None,
                )
                .is_err()
            );
            assert!(
                parse_word_selection_response(
                    &serde_json::to_vec(&serde_json::json!({
                        "code": 0,
                        "msg": message,
                    }))
                    .unwrap(),
                )
                .is_err()
            );
        }
    }

    #[test]
    fn errors_unknown_versions_and_missing_crypto_fail_closed() {
        for document in [
            serde_json::json!({"code": 0, "msg": "synthetic failure"}),
            serde_json::json!({"code": 1, "msg": "synthetic success"}),
            serde_json::json!({"code": 1, "msg": "synthetic", "data": {}, "jv": "future"}),
        ] {
            assert!(
                parse_assessment_response(&serde_json::to_vec(&document).unwrap(), None).is_err()
            );
        }
        let encrypted = serde_json::json!({
            "code": 1,
            "msg": "synthetic",
            "data": {"iv": "synthetic"},
            "jv": "99"
        });
        assert_eq!(
            parse_assessment_response(&serde_json::to_vec(&encrypted).unwrap(), None)
                .unwrap_err()
                .kind,
            ProviderErrorKind::Authentication
        );
    }

    #[test]
    fn word_selection_accepts_only_its_acknowledgement_shape() {
        for document in [
            serde_json::json!({"code": 1, "msg": "synthetic accepted"}),
            serde_json::json!({"code": 20001, "msg": "synthetic accepted", "data": {"accepted": true}}),
        ] {
            assert!(matches!(
                parse_word_selection_response(&serde_json::to_vec(&document).unwrap()).unwrap(),
                CidarenAssessmentResponse::Receipt {
                    kind: CidarenAssessmentReceiptKind::Accepted,
                    ..
                }
            ));
        }
        for document in [
            serde_json::json!({"code": 20001, "msg": "synthetic", "data": null}),
            serde_json::json!({"code": 20001, "msg": "synthetic", "data": {}}),
            serde_json::json!({"code": 20001, "msg": "synthetic", "data": []}),
            serde_json::json!({"code": 20001, "msg": "synthetic", "data": ""}),
            serde_json::json!({"code": 20001, "msg": "synthetic", "data": 0}),
            serde_json::json!({"code": 20001, "msg": "synthetic", "data": false}),
        ] {
            assert!(
                parse_word_selection_response(&serde_json::to_vec(&document).unwrap()).is_err()
            );
        }
        assert!(matches!(
            parse_word_selection_response(br#"{"code":20004,"msg":"synthetic terminal"}"#).unwrap(),
            CidarenAssessmentResponse::Receipt {
                kind: CidarenAssessmentReceiptKind::Completed,
                ..
            }
        ));
        assert!(parse_word_selection_response(br#"{"code":0,"msg":"rejected"}"#).is_err());
        assert!(
            parse_assessment_response(br#"{"code":1,"msg":"synthetic accepted"}"#, None).is_err()
        );
    }
}
