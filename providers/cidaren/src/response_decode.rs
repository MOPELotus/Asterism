use asterism_domain::{ProtocolObservationKind, ProtocolSurface};
use asterism_provider_api::{ProviderError, ProviderErrorKind, ProviderResult};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::{Map, Value, json};
use zeroize::Zeroize;

use crate::{
    CidarenCryptoContext,
    protocol_observation::{
        error_with_protocol_observation, json_value_kind, protocol_drift_with_observation,
    },
};

const MAX_ENCODED_BYTES: usize = 2 * 1_024 * 1_024;
const MAX_DECODED_BYTES: usize = 2 * 1_024 * 1_024;

const JV_2_1254: &[usize] = &[0, 1, 2, 4, 5, 36, 47, 48, 59, 96, 107];
const JV_2_9214: &[usize] = &[0, 1, 2, 4, 5, 6, 7, 48, 49, 66, 149, 150, 284, 374, 375];
const JV_2_10232: &[usize] = &[0, 1, 2, 5, 6, 7, 8, 46, 65, 66, 199, 270, 328, 329];
const JV_2_10234: &[usize] = &[0, 1, 2, 4, 5, 6, 7, 46, 65, 66, 198, 270, 328, 329];
const JV_3_1021_FIXED: &[usize] = &[0, 1, 2, 4, 5, 6, 7, 48, 49, 66, 150, 151, 284, 374, 375];
const JV_3_1021_SPANS: &[(usize, usize)] = &[(0, 1), (1, 2), (33, 1), (57, 1), (111, 1)];
const JV_3_1021_ORDER: &[usize; 5] = &[1, 3, 2, 0, 4];
const JV_3_2265_SPANS: &[(usize, usize)] = &[(0, 2), (1, 3), (33, 1), (57, 1), (121, 1)];
const JV_3_2277_SPANS: &[(usize, usize)] = &[(0, 3), (1, 3), (32, 2), (50, 1), (110, 1)];
const JV_3_2265_2277_ORDER: &[usize; 5] = &[3, 1, 0, 4, 2];

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
        "0" | "3_1021" | "3_2265" | "3_2277" => None,
        "2_1254" => Some(JV_2_1254),
        "2_9214" => Some(JV_2_9214),
        "2_10232" => Some(JV_2_10232),
        "2_10234" => Some(JV_2_10234),
        "99" => {
            return crypto
                .ok_or_else(missing_crypto_context)?
                .decrypt_payload(data)
                .map_err(|error| response_data_observation(error, data, jv));
        }
        _ => {
            return Err(protocol_drift_with_observation(
                "Cidaren response uses an unknown encoding version",
                ProtocolSurface::Other,
                ProtocolObservationKind::EndpointVersionDrift,
                json!({
                    "jv_shape": {
                        "ascii": jv.is_ascii(),
                        "byte_length": jv.len(),
                    }
                }),
            ));
        }
    };

    (|| {
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
        match jv {
            "3_1021" => decode_unique_candidates([
                remove_confusion_bytes(encoded.as_bytes(), JV_3_1021_FIXED),
                remove_confusion_and_unshuffle(
                    encoded.as_bytes(),
                    JV_3_1021_SPANS,
                    JV_3_1021_ORDER,
                ),
            ]),
            "3_2265" => decode_transformed(remove_confusion_and_unshuffle(
                encoded.as_bytes(),
                JV_3_2265_SPANS,
                JV_3_2265_2277_ORDER,
            )?),
            "3_2277" => decode_transformed(remove_confusion_and_unshuffle(
                encoded.as_bytes(),
                JV_3_2277_SPANS,
                JV_3_2265_2277_ORDER,
            )?),
            _ => {
                let indices = indices.ok_or_else(invalid_encoded_response)?;
                decode_transformed(remove_confusion_bytes(encoded.as_bytes(), indices)?)
            }
        }
    })()
    .map_err(|error| response_data_observation(error, data, jv))
}

fn response_data_observation(error: ProviderError, data: &Value, jv: &str) -> ProviderError {
    if error.protocol_observation.is_some()
        || !matches!(
            error.kind,
            ProviderErrorKind::ProtocolDrift | ProviderErrorKind::InvalidResponse
        )
    {
        return error;
    }
    let object = data.as_object();
    let payload = object
        .and_then(|object| object.get("payload"))
        .and_then(Value::as_object)
        .or(object);
    let ciphertext = payload.and_then(|payload| {
        payload
            .get("cipher_text")
            .or_else(|| payload.get("cipherText"))
    });
    error_with_protocol_observation(
        error,
        ProtocolSurface::Other,
        ProtocolObservationKind::UnknownResultShape,
        json!({
            "schema": "cidaren.response-data-observation.v1",
            "version_family": known_version_family(jv),
            "data_kind": json_value_kind(Some(data)),
            "data_object_fields": object.map(Map::len),
            "data_array_count": data.as_array().map(Vec::len),
            "encoded_ascii": data.as_str().map(str::is_ascii),
            "encoded_has_whitespace": data
                .as_str()
                .map(|value| value.bytes().any(|byte| byte.is_ascii_whitespace())),
            "encoded_length": data.as_str().map(str::len),
            "payload_kind": json_value_kind(
                object.and_then(|object| object.get("payload"))
            ),
            "payload_fields": payload.map(Map::len),
            "iv_kind": json_value_kind(payload.and_then(|payload| payload.get("iv"))),
            "iv_length": string_length(payload.and_then(|payload| payload.get("iv"))),
            "aad_kind": json_value_kind(payload.and_then(|payload| payload.get("aad"))),
            "aad_length": string_length(payload.and_then(|payload| payload.get("aad"))),
            "ciphertext_kind": json_value_kind(ciphertext),
            "ciphertext_length": string_length(ciphertext),
        }),
    )
}

fn known_version_family(jv: &str) -> &'static str {
    match jv {
        "0" => "plain_or_base64",
        "2_1254" | "2_9214" | "2_10232" | "2_10234" => "legacy_fixed_confusion",
        "3_1021" | "3_2265" | "3_2277" => "legacy_chunked_confusion",
        "99" => "authenticated_aes_gcm",
        _ => "unknown",
    }
}

fn string_length(value: Option<&Value>) -> Option<usize> {
    value.and_then(Value::as_str).map(str::len)
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

fn decode_transformed(mut normalized: Vec<u8>) -> ProviderResult<Value> {
    let decoded = decode_json(&normalized).ok_or_else(invalid_encoded_response);
    normalized.zeroize();
    decoded
}

fn decode_unique_candidates<const N: usize>(
    candidates: [ProviderResult<Vec<u8>>; N],
) -> ProviderResult<Value> {
    let mut accepted: Option<Value> = None;
    for candidate in candidates.into_iter().flatten() {
        let Some(mut decoded) = decode_transformed(candidate).ok() else {
            continue;
        };
        match accepted.as_mut() {
            None => accepted = Some(decoded),
            Some(existing) if existing == &decoded => zeroize_json_strings(&mut decoded),
            Some(existing) => {
                zeroize_json_strings(existing);
                zeroize_json_strings(&mut decoded);
                return Err(invalid_encoded_response());
            }
        }
    }
    accepted.ok_or_else(invalid_encoded_response)
}

fn zeroize_json_strings(value: &mut Value) {
    match value {
        Value::String(value) => value.zeroize(),
        Value::Array(values) => values.iter_mut().for_each(zeroize_json_strings),
        Value::Object(values) => values.values_mut().for_each(zeroize_json_strings),
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
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

fn remove_confusion_and_unshuffle(
    encoded: &[u8],
    spans: &[(usize, usize)],
    order: &[usize; 5],
) -> ProviderResult<Vec<u8>> {
    let mut stripped = encoded.to_vec();
    for &(start, count) in spans {
        let end = start
            .checked_add(count)
            .filter(|end| count > 0 && *end <= stripped.len())
            .ok_or_else(invalid_encoded_response)?;
        let mut next = Vec::with_capacity(stripped.len() - count);
        next.extend_from_slice(&stripped[..start]);
        next.extend_from_slice(&stripped[end..]);
        stripped.zeroize();
        stripped = next;
    }

    let chunk = stripped.len() / order.len();
    if chunk == 0 {
        stripped.zeroize();
        return Err(invalid_encoded_response());
    }
    let mut normalized = Vec::with_capacity(stripped.len());
    for destination in 0..order.len() {
        let source = order
            .iter()
            .position(|candidate| *candidate == destination)
            .ok_or_else(invalid_encoded_response)?;
        let start = source * chunk;
        normalized.extend_from_slice(&stripped[start..start + chunk]);
    }
    normalized.extend_from_slice(&stripped[chunk * order.len()..]);
    stripped.zeroize();
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
            ("3_1021", JV_3_1021_FIXED),
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
    fn current_public_jv3_variants_decode_exact_donor_transform() {
        let value = serde_json::json!({
            "schema": "synthetic.cidaren.jv3.v1",
            "padding": "x".repeat(512),
            "items": [{"id": "synthetic-1", "score": 96.5}],
        });
        for (jv, spans, order) in [
            ("3_1021", JV_3_1021_SPANS, JV_3_1021_ORDER),
            ("3_2265", JV_3_2265_SPANS, JV_3_2265_2277_ORDER),
            ("3_2277", JV_3_2277_SPANS, JV_3_2265_2277_ORDER),
        ] {
            let confused = encode_jv3_fixture(&value, spans, order);
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
    fn conflicting_transform_candidates_fail_closed() {
        let first = STANDARD.encode(br#"{"schema":"synthetic.one"}"#);
        let second = STANDARD.encode(br#"{"schema":"synthetic.two"}"#);
        assert!(
            decode_unique_candidates([Ok(first.into_bytes()), Ok(second.into_bytes())]).is_err()
        );

        let same = STANDARD.encode(br#"{"schema":"synthetic.same"}"#);
        assert_eq!(
            decode_unique_candidates([Ok(same.clone().into_bytes()), Ok(same.into_bytes())])
                .unwrap(),
            serde_json::json!({"schema": "synthetic.same"})
        );
    }

    #[test]
    fn missing_capture_context_and_unknown_versions_fail_closed() {
        let payload = serde_json::json!({"payload": {"iv": "synthetic"}});
        let missing_context = decode_legacy_response_data(&payload, "99").unwrap_err();
        assert_eq!(missing_context.kind, ProviderErrorKind::Authentication);
        assert!(missing_context.protocol_observation.is_none());
        let error = decode_legacy_response_data(&payload, "future").unwrap_err();
        assert_eq!(error.kind, ProviderErrorKind::ProtocolDrift);
        let observation = error.protocol_observation.unwrap();
        assert_eq!(observation.surface, ProtocolSurface::Other);
        assert_eq!(
            observation.kind,
            ProtocolObservationKind::EndpointVersionDrift
        );
        assert_eq!(
            observation.shape_sanitized,
            json!({"jv_shape": {"ascii": true, "byte_length": 6}})
        );
        let encoded = "must-not-cross-encoded";
        let error =
            decode_legacy_response_data(&Value::String(encoded.to_owned()), "2_1254").unwrap_err();
        assert_eq!(error.kind, ProviderErrorKind::InvalidResponse);
        let observation = error.protocol_observation.unwrap();
        assert_eq!(observation.surface, ProtocolSurface::Other);
        assert_eq!(
            observation.kind,
            ProtocolObservationKind::UnknownResultShape
        );
        assert_eq!(
            observation.shape_sanitized["version_family"],
            "legacy_fixed_confusion"
        );
        assert_eq!(observation.shape_sanitized["data_kind"], "string");
        assert_eq!(observation.shape_sanitized["encoded_ascii"], true);
        assert_eq!(observation.shape_sanitized["encoded_has_whitespace"], false);
        assert_eq!(observation.shape_sanitized["encoded_length"], encoded.len());
        let sanitized = serde_json::to_string(&observation.shape_sanitized).unwrap();
        assert!(!sanitized.contains(encoded));
        assert!(!sanitized.contains("2_1254"));
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

    fn encode_jv3_fixture(value: &Value, spans: &[(usize, usize)], order: &[usize; 5]) -> Vec<u8> {
        let mut document = serde_json::to_vec(value).unwrap();
        let encoded = loop {
            let encoded = STANDARD.encode(&document);
            if encoded.len().is_multiple_of(order.len()) {
                break encoded.into_bytes();
            }
            document.push(b' ');
        };
        let chunk = encoded.len() / order.len();
        let mut shuffled = Vec::with_capacity(encoded.len());
        for source in order {
            let start = *source * chunk;
            shuffled.extend_from_slice(&encoded[start..start + chunk]);
        }
        for &(start, count) in spans.iter().rev() {
            shuffled.splice(start..start, std::iter::repeat_n(b'!', count));
        }
        shuffled
    }
}
