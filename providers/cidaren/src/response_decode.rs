use asterism_provider_api::{ProviderError, ProviderErrorKind, ProviderResult};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::Value;
use zeroize::Zeroize;

use crate::CidarenCryptoContext;

const MAX_ENCODED_BYTES: usize = 2 * 1_024 * 1_024;
const MAX_DECODED_BYTES: usize = 2 * 1_024 * 1_024;

const JV_2_1254: &[usize] = &[0, 1, 2, 4, 5, 36, 47, 48, 59, 96, 107];
const JV_2_9214: &[usize] = &[0, 1, 2, 4, 5, 6, 7, 48, 49, 66, 149, 150, 284, 374, 375];
const JV_2_10232: &[usize] = &[0, 1, 2, 5, 6, 7, 8, 46, 65, 66, 199, 270, 328, 329];
const JV_2_10234: &[usize] = &[0, 1, 2, 4, 5, 6, 7, 46, 65, 66, 198, 270, 328, 329];
const JV_3_1021: &[usize] = &[0, 1, 2, 4, 5, 6, 7, 48, 49, 66, 150, 151, 284, 374, 375];

/// Strictly decodes Cidaren response data without captured crypto context.
///
/// Plain JSON and base64 `jv=0` are accepted. The exact legacy confusion-byte
/// variants frozen from donor evidence are also supported. `jv=99` fails
/// closed here because it must be decoded through [`decode_response_data`]
/// with fresh account-bound Capture or native-login context.
///
/// # Errors
///
/// Returns `Authentication` for `jv=99`, `ProtocolDrift` for unknown versions
/// and `InvalidResponse` for malformed, primitive or oversized payloads.
pub fn decode_legacy_response_data(data: &Value, jv: &str) -> ProviderResult<Value> {
    decode_response_data(data, jv, None)
}

/// Strictly decodes one Cidaren response using the exact donor-observed
/// encoding version and optional account-bound crypto context.
///
/// The current `jv=99` route is authenticated with HKDF-SHA256/AES-256-GCM and
/// never falls back to legacy heuristics. Unknown versions also fail closed.
///
/// # Errors
///
/// Returns a typed Provider error for a missing crypto context, unknown
/// version, malformed framing, authentication failure or oversized content.
pub fn decode_response_data(
    data: &Value,
    jv: &str,
    crypto: Option<&CidarenCryptoContext>,
) -> ProviderResult<Value> {
    let indices = match jv {
        "0" => None,
        "2_1254" => Some(JV_2_1254),
        "2_9214" => Some(JV_2_9214),
        "2_10232" => Some(JV_2_10232),
        "2_10234" => Some(JV_2_10234),
        "3_1021" => Some(JV_3_1021),
        "99" => {
            return crypto
                .ok_or_else(missing_crypto_context)?
                .decrypt_payload(data);
        }
        _ => {
            return Err(ProviderError::new(
                ProviderErrorKind::ProtocolDrift,
                "Cidaren response uses an unknown encoding version",
            ));
        }
    };

    if matches!(data, Value::Object(_) | Value::Array(_)) {
        return bounded_json_clone(data);
    }
    let encoded = data.as_str().filter(|value| {
        !value.is_empty()
            && value.len() <= MAX_ENCODED_BYTES
            && value.is_ascii()
            && !value.bytes().any(|byte| byte.is_ascii_whitespace())
    });
    let encoded = encoded.ok_or_else(invalid_encoded_response)?;
    if let Some(decoded) = decode_json(encoded.as_bytes()) {
        return Ok(decoded);
    }
    let Some(indices) = indices else {
        return Err(invalid_encoded_response());
    };
    let mut normalized = remove_confusion_bytes(encoded.as_bytes(), indices)?;
    let decoded = decode_json(&normalized).ok_or_else(invalid_encoded_response);
    normalized.zeroize();
    decoded
}

fn missing_crypto_context() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::Authentication,
        "Cidaren jv=99 response requires fresh account-bound crypto context",
    )
}

fn bounded_json_clone(data: &Value) -> ProviderResult<Value> {
    let mut encoded = serde_json::to_vec(data).map_err(|_| invalid_encoded_response())?;
    let within_limit = encoded.len() <= MAX_DECODED_BYTES;
    encoded.zeroize();
    if !within_limit {
        return Err(invalid_encoded_response());
    }
    Ok(data.clone())
}

fn decode_json(encoded: &[u8]) -> Option<Value> {
    let mut decoded = STANDARD.decode(encoded).ok()?;
    if decoded.is_empty() || decoded.len() > MAX_DECODED_BYTES {
        decoded.zeroize();
        return None;
    }
    let parsed = serde_json::from_slice::<Value>(&decoded)
        .ok()
        .filter(|value| matches!(value, Value::Object(_) | Value::Array(_)));
    decoded.zeroize();
    parsed
}

fn remove_confusion_bytes(encoded: &[u8], indices: &[usize]) -> ProviderResult<Vec<u8>> {
    if indices
        .last()
        .is_none_or(|maximum| *maximum >= encoded.len())
    {
        return Err(invalid_encoded_response());
    }
    let mut normalized = Vec::with_capacity(encoded.len().saturating_sub(indices.len()));
    let mut index_cursor = 0;
    for (index, byte) in encoded.iter().copied().enumerate() {
        if indices.get(index_cursor) == Some(&index) {
            index_cursor += 1;
        } else {
            normalized.push(byte);
        }
    }
    if index_cursor != indices.len() {
        normalized.zeroize();
        return Err(invalid_encoded_response());
    }
    Ok(normalized)
}

fn invalid_encoded_response() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::InvalidResponse,
        "Cidaren response data is malformed or exceeds the decode limit",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_json_and_plain_base64_are_bounded() {
        let value = serde_json::json!({"schema": "synthetic", "items": [1, 2, 3]});
        assert_eq!(decode_legacy_response_data(&value, "0").unwrap(), value);

        let encoded = STANDARD.encode(serde_json::to_vec(&value).unwrap());
        assert_eq!(
            decode_legacy_response_data(&Value::String(encoded), "0").unwrap(),
            value
        );
        assert!(decode_legacy_response_data(&Value::Null, "0").is_err());
        assert!(
            decode_legacy_response_data(&Value::String("x".repeat(MAX_ENCODED_BYTES + 1)), "0",)
                .is_err()
        );
    }

    #[test]
    fn exact_legacy_confusion_variants_decode_without_guessing() {
        let value = serde_json::json!({
            "schema": "synthetic.cidaren.response.v1",
            "padding": "x".repeat(512),
            "items": [{"id": "synthetic-1", "progress": 35}],
        });
        let encoded = STANDARD.encode(serde_json::to_vec(&value).unwrap());
        for (jv, indices) in [
            ("2_1254", JV_2_1254),
            ("2_9214", JV_2_9214),
            ("2_10232", JV_2_10232),
            ("2_10234", JV_2_10234),
            ("3_1021", JV_3_1021),
        ] {
            let confused = insert_confusion_bytes(encoded.as_bytes(), indices);
            assert_eq!(
                decode_legacy_response_data(
                    &Value::String(String::from_utf8(confused).unwrap()),
                    jv,
                )
                .unwrap(),
                value,
                "variant {jv}",
            );
        }
    }

    #[test]
    fn missing_capture_context_and_unknown_versions_fail_closed() {
        let payload = serde_json::json!({"payload": {"iv": "synthetic"}});
        assert_eq!(
            decode_legacy_response_data(&payload, "99")
                .unwrap_err()
                .kind,
            ProviderErrorKind::Authentication
        );
        assert_eq!(
            decode_legacy_response_data(&payload, "future")
                .unwrap_err()
                .kind,
            ProviderErrorKind::ProtocolDrift
        );
        assert!(
            decode_legacy_response_data(&Value::String("not-base64".to_owned()), "2_1254").is_err()
        );
    }

    fn insert_confusion_bytes(encoded: &[u8], indices: &[usize]) -> Vec<u8> {
        let mut confused = Vec::with_capacity(encoded.len() + indices.len());
        let mut source_cursor = 0;
        let mut index_cursor = 0;
        while source_cursor < encoded.len() || index_cursor < indices.len() {
            if indices.get(index_cursor) == Some(&confused.len()) {
                confused.push(b'A');
                index_cursor += 1;
            } else {
                confused.push(encoded[source_cursor]);
                source_cursor += 1;
            }
        }
        confused
    }
}
