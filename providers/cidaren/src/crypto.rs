use std::fmt;

use aes_gcm::{
    Aes256Gcm, Nonce,
    aead::{Aead, KeyInit, Payload},
};
use asterism_provider_api::{ProviderError, ProviderErrorKind, ProviderResult};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use hkdf::Hkdf;
use serde_json::Value;
use sha2::Sha256;
use zeroize::{Zeroize, Zeroizing};

const MAX_CAPTURE_CONTEXT_BYTES: usize = 256 * 1_024;
const MAX_CAPTURE_FIELD_BYTES: usize = 64 * 1_024;
const MAX_AAD_BYTES: usize = 4 * 1_024;
const MAX_CIPHERTEXT_BYTES: usize = 2 * 1_024 * 1_024;
const MAX_PLAINTEXT_BYTES: usize = 2 * 1_024 * 1_024;
const AES_GCM_NONCE_BYTES: usize = 12;
const AES_256_KEY_BYTES: usize = 32;

/// Account-bound secret material captured from Cidaren browser storage.
///
/// The decoded values never enter persisted domain objects, logs, route
/// contexts or submission drafts. They are zeroized when the bounded Provider
/// session is dropped.
pub struct CidarenCryptoContext {
    shared_secret: Zeroizing<Vec<u8>>,
    salt: Zeroizing<Vec<u8>>,
}

impl CidarenCryptoContext {
    /// Parses the donor-observed `crypto.json` variants fail-closed.
    ///
    /// `login_info`, `CDR_LOGIN_INFO`, `loginInfo`, and a direct login-info
    /// object are accepted. Nested login info may itself be a JSON string.
    /// Field `a` carries base64 shared-secret bytes with an optional `h`
    /// prefix, while `b` carries base64 salt bytes with an optional `a`
    /// prefix.
    ///
    /// # Errors
    ///
    /// Returns Authentication for missing, malformed or oversized context.
    pub fn parse(document: &[u8]) -> ProviderResult<Self> {
        if document.is_empty() || document.len() > MAX_CAPTURE_CONTEXT_BYTES {
            return Err(invalid_capture_context());
        }
        let root = ZeroizingJsonValue::new(
            serde_json::from_slice(document).map_err(|_| invalid_capture_context())?,
        );
        let login_info = decode_login_info(root.as_value())?;
        Ok(Self {
            shared_secret: login_info.shared_secret,
            salt: login_info.salt,
        })
    }

    /// Decrypts one authenticated `jv=99` payload using the exact captured
    /// account context and response AAD.
    ///
    /// # Errors
    ///
    /// Returns `InvalidResponse` for malformed framing, invalid authentication,
    /// oversized plaintext or a non-JSON result.
    pub fn decrypt_payload(&self, data: &Value) -> ProviderResult<Value> {
        let payload = normalize_payload(data)?;
        let iv = decode_payload_field(payload.get("iv"), MAX_CAPTURE_FIELD_BYTES)?;
        if iv.len() != AES_GCM_NONCE_BYTES {
            return Err(invalid_encrypted_response());
        }
        let ciphertext = decode_payload_field(
            payload
                .get("cipher_text")
                .or_else(|| payload.get("cipherText")),
            MAX_CIPHERTEXT_BYTES,
        )?;
        let aad = payload
            .get("aad")
            .and_then(Value::as_str)
            .filter(|value| {
                !value.is_empty()
                    && value.len() <= MAX_AAD_BYTES
                    && value.trim() == *value
                    && !value.chars().any(char::is_control)
            })
            .ok_or_else(invalid_encrypted_response)?;
        let ucd = aad
            .rsplit(':')
            .next()
            .filter(|value| !value.is_empty() && value.len() <= 512)
            .ok_or_else(invalid_encrypted_response)?;

        let info = Zeroizing::new(format!("vcg-exam-aes:{ucd}"));
        let hkdf = Hkdf::<Sha256>::new(Some(self.salt.as_slice()), self.shared_secret.as_slice());
        let mut key = Zeroizing::new([0_u8; AES_256_KEY_BYTES]);
        hkdf.expand(info.as_bytes(), key.as_mut())
            .map_err(|_| invalid_encrypted_response())?;
        let cipher =
            Aes256Gcm::new_from_slice(key.as_slice()).map_err(|_| invalid_encrypted_response())?;
        let nonce = Nonce::from_slice(&iv);
        let mut plaintext = cipher
            .decrypt(
                nonce,
                Payload {
                    msg: &ciphertext,
                    aad: aad.as_bytes(),
                },
            )
            .map_err(|_| invalid_encrypted_response())?;
        if plaintext.is_empty() || plaintext.len() > MAX_PLAINTEXT_BYTES {
            plaintext.zeroize();
            return Err(invalid_encrypted_response());
        }
        let parsed = serde_json::from_slice::<Value>(&plaintext)
            .ok()
            .filter(|value| matches!(value, Value::Object(_) | Value::Array(_)))
            .ok_or_else(invalid_encrypted_response);
        plaintext.zeroize();
        parsed
    }
}

impl fmt::Debug for CidarenCryptoContext {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("CidarenCryptoContext([REDACTED])")
    }
}

struct DecodedLoginInfo {
    shared_secret: Zeroizing<Vec<u8>>,
    salt: Zeroizing<Vec<u8>>,
}

fn decode_login_info(root: &Value) -> ProviderResult<DecodedLoginInfo> {
    let root = root.as_object().ok_or_else(invalid_capture_context)?;
    let selected = ["login_info", "CDR_LOGIN_INFO", "loginInfo"]
        .into_iter()
        .find_map(|key| root.get(key));
    match selected {
        None => decode_login_info_fields(root),
        Some(Value::Object(object)) => decode_login_info_fields(object),
        Some(Value::String(encoded)) if encoded.len() <= MAX_CAPTURE_CONTEXT_BYTES => {
            let nested = ZeroizingJsonValue::new(
                serde_json::from_str::<Value>(encoded).map_err(|_| invalid_capture_context())?,
            );
            let object = nested
                .as_value()
                .as_object()
                .ok_or_else(invalid_capture_context)?;
            decode_login_info_fields(object)
        }
        Some(_) => Err(invalid_capture_context()),
    }
}

fn decode_login_info_fields(
    login_info: &serde_json::Map<String, Value>,
) -> ProviderResult<DecodedLoginInfo> {
    Ok(DecodedLoginInfo {
        shared_secret: decode_capture_field(login_info.get("a"), b'h')?,
        salt: decode_capture_field(login_info.get("b"), b'a')?,
    })
}

struct ZeroizingJsonValue(Value);

impl ZeroizingJsonValue {
    const fn new(value: Value) -> Self {
        Self(value)
    }

    const fn as_value(&self) -> &Value {
        &self.0
    }
}

impl Drop for ZeroizingJsonValue {
    fn drop(&mut self) {
        zeroize_json(&mut self.0);
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

fn decode_capture_field(value: Option<&Value>, prefix: u8) -> ProviderResult<Zeroizing<Vec<u8>>> {
    let encoded = value
        .and_then(Value::as_str)
        .filter(|value| {
            !value.is_empty()
                && value.len() <= MAX_CAPTURE_FIELD_BYTES
                && value.is_ascii()
                && !value.bytes().any(|byte| byte.is_ascii_whitespace())
        })
        .ok_or_else(invalid_capture_context)?;
    let encoded = encoded
        .as_bytes()
        .strip_prefix(&[prefix])
        .unwrap_or(encoded.as_bytes());
    let decoded = Zeroizing::new(
        STANDARD
            .decode(encoded)
            .map_err(|_| invalid_capture_context())?,
    );
    if decoded.is_empty() || decoded.len() > MAX_CAPTURE_FIELD_BYTES {
        return Err(invalid_capture_context());
    }
    Ok(decoded)
}

fn normalize_payload(data: &Value) -> ProviderResult<serde_json::Map<String, Value>> {
    let mut value = match data {
        Value::String(encoded) if encoded.len() <= MAX_CIPHERTEXT_BYTES => {
            serde_json::from_str::<Value>(encoded).map_err(|_| invalid_encrypted_response())?
        }
        value => value.clone(),
    };
    if let Some(payload) = value
        .as_object()
        .and_then(|object| object.get("payload"))
        .cloned()
    {
        value = payload;
    }
    value
        .as_object()
        .cloned()
        .ok_or_else(invalid_encrypted_response)
}

fn decode_payload_field(value: Option<&Value>, maximum: usize) -> ProviderResult<Vec<u8>> {
    let encoded = value
        .and_then(Value::as_str)
        .filter(|value| {
            !value.is_empty()
                && value.len() <= maximum.saturating_mul(2)
                && value.is_ascii()
                && !value.bytes().any(|byte| byte.is_ascii_whitespace())
        })
        .ok_or_else(invalid_encrypted_response)?;
    let decoded = STANDARD
        .decode(encoded)
        .map_err(|_| invalid_encrypted_response())?;
    if decoded.is_empty() || decoded.len() > maximum {
        return Err(invalid_encrypted_response());
    }
    Ok(decoded)
}

fn invalid_capture_context() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::Authentication,
        "Cidaren captured crypto context is missing, malformed or oversized",
    )
}

fn invalid_encrypted_response() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::InvalidResponse,
        "Cidaren jv=99 response failed authenticated bounded decoding",
    )
}

#[cfg(test)]
mod tests {
    use aes_gcm::aead::Aead;

    use super::*;

    #[test]
    fn capture_context_accepts_all_observed_wrappers_and_redacts_secrets() {
        let direct = serde_json::json!({
            "a": format!("h{}", STANDARD.encode(b"synthetic-shared-secret")),
            "b": format!("a{}", STANDARD.encode(b"synthetic-salt")),
        });
        for document in [
            direct.clone(),
            serde_json::json!({"login_info": direct}),
            serde_json::json!({"CDR_LOGIN_INFO": serde_json::to_string(&direct).unwrap()}),
            serde_json::json!({"loginInfo": direct}),
        ] {
            let context =
                CidarenCryptoContext::parse(&serde_json::to_vec(&document).unwrap()).unwrap();
            assert!(!format!("{context:?}").contains("synthetic"));
        }
    }

    #[test]
    fn exact_hkdf_aes_gcm_chain_decrypts_both_ciphertext_names() {
        let shared_secret = b"synthetic-shared-secret";
        let salt = b"synthetic-salt";
        let context_document = serde_json::json!({
            "login_info": {
                "a": format!("h{}", STANDARD.encode(shared_secret)),
                "b": format!("a{}", STANDARD.encode(salt)),
            }
        });
        let context =
            CidarenCryptoContext::parse(&serde_json::to_vec(&context_document).unwrap()).unwrap();
        let aad = "cidaren:synthetic-ucd";
        let hkdf = Hkdf::<Sha256>::new(Some(salt), shared_secret);
        let mut key = [0_u8; AES_256_KEY_BYTES];
        hkdf.expand(b"vcg-exam-aes:synthetic-ucd", &mut key)
            .unwrap();
        let cipher = Aes256Gcm::new_from_slice(&key).unwrap();
        let iv = [7_u8; AES_GCM_NONCE_BYTES];
        let plaintext = serde_json::to_vec(&serde_json::json!({
            "topic_code": "synthetic-topic",
            "answer_result": 1
        }))
        .unwrap();
        let encrypted = cipher
            .encrypt(
                Nonce::from_slice(&iv),
                Payload {
                    msg: &plaintext,
                    aad: aad.as_bytes(),
                },
            )
            .unwrap();
        key.zeroize();

        for key_name in ["cipher_text", "cipherText"] {
            let payload = serde_json::json!({
                "iv": STANDARD.encode(iv),
                key_name: STANDARD.encode(&encrypted),
                "aad": aad,
            });
            assert_eq!(
                context.decrypt_payload(&payload).unwrap()["topic_code"],
                "synthetic-topic"
            );
        }
    }

    #[test]
    fn malformed_context_or_authenticated_payload_fails_closed() {
        assert!(CidarenCryptoContext::parse(br#"{"a":"","b":""}"#).is_err());
        let context = CidarenCryptoContext::parse(
            &serde_json::to_vec(&serde_json::json!({
                "a": STANDARD.encode(b"secret"),
                "b": STANDARD.encode(b"salt"),
            }))
            .unwrap(),
        )
        .unwrap();
        assert!(
            context
                .decrypt_payload(&serde_json::json!({
                    "iv": STANDARD.encode([0_u8; AES_GCM_NONCE_BYTES]),
                    "cipher_text": STANDARD.encode([0_u8; 32]),
                    "aad": "missing-valid-tag:ucd"
                }))
                .is_err()
        );
    }
}
