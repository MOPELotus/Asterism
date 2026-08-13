use std::fmt;

use aes_gcm::{
    Aes256Gcm, Nonce,
    aead::{Aead, KeyInit, Payload},
};
use asterism_provider_api::{
    CredentialReplacement, ProviderError, ProviderErrorKind, ProviderResult,
};
use asterism_secrets::{CredentialField, SecretPurpose, SecretString, SecretValue};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use hkdf::Hkdf;
use http::HeaderValue;
use p256::{
    PublicKey,
    ecdh::EphemeralSecret,
    pkcs8::{DecodePublicKey, EncodePublicKey},
};
use rand_core::OsRng;
use serde::Serialize;
use serde_json::Value;
use sha2::Sha256;
use zeroize::{Zeroize, Zeroizing};

use crate::CidarenTokenSession;

pub(crate) const LOGIN_VERSION: &str = "2.7.0.260715_01";
const CRYPTO_VERSION: &str = "v1";
#[cfg(test)]
const LOGIN_PATH: &str = "/Wechat/V2/LoginByWechatCode";
const LOGIN_AAD: &str = "vcg-auth:POST:/Wechat/V2/LoginByWechatCode";
const LOGIN_HKDF_INFO: &[u8] = b"vcg-auth-aes";
const SIGNING_SUFFIX: &str = "ajfajfamsnfaflfasakljdlalkflak";
const MAX_OAUTH_CODE_BYTES: usize = 512;
const MAX_LOGIN_RESPONSE_BYTES: usize = 256 * 1_024;
const MAX_HANDSHAKE_FIELD_BYTES: usize = 4 * 1_024;
const MAX_LOGIN_CIPHERTEXT_BYTES: usize = 256 * 1_024;
const MAX_LOGIN_PLAINTEXT_BYTES: usize = 256 * 1_024;
const AES_GCM_NONCE_BYTES: usize = 12;
const AES_256_KEY_BYTES: usize = 32;

/// One short-lived, single-use `WeChat` OAuth authorization code.
///
/// Core owns durable claim/consume semantics. This Provider value only keeps
/// the plaintext code redacted and zeroized while constructing one request.
pub(crate) struct CidarenOauthCode(SecretString);

impl CidarenOauthCode {
    /// Validates the callback artifact without assuming its current 32-byte
    /// representation will never change.
    ///
    /// # Errors
    ///
    /// Returns Authentication for empty, oversized, whitespace-padded or
    /// header/query-unsafe callback material.
    pub(crate) fn try_new(code: impl Into<String>) -> ProviderResult<Self> {
        let mut code = code.into();
        let valid = !code.is_empty()
            && code.len() <= MAX_OAUTH_CODE_BYTES
            && code.trim() == code
            && code.is_ascii()
            && code.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~')
            });
        if !valid {
            code.zeroize();
            return Err(invalid_oauth_code());
        }
        Ok(Self(SecretString::new(code)))
    }

    pub(crate) fn expose_secret(&self) -> &str {
        self.0.expose_secret()
    }
}

impl fmt::Debug for CidarenOauthCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("CidarenOauthCode([REDACTED])")
    }
}

/// Bounded browser/client facts which the current H5 request interceptor binds
/// to the V2 exchange. They are short-lived request context, not durable
/// account credentials.
pub(crate) struct CidarenOauthClientContext {
    authorization_v: SecretString,
    user_agent: String,
    abc: String,
}

impl CidarenOauthClientContext {
    /// Builds the verified OAuth bootstrap context. The current V2 login sends
    /// `Authorization-v: 00`; `Abc` is derived from the exact User-Agent as the
    /// first-party client does.
    ///
    /// # Errors
    ///
    /// Returns Authentication for an empty, oversized or header-unsafe
    /// User-Agent.
    pub(crate) fn try_for_oauth_bootstrap(user_agent: impl Into<String>) -> ProviderResult<Self> {
        Self::try_new("00", user_agent)
    }

    /// Validates an explicit Capture/browser `Authorization-v` value and exact
    /// User-Agent. `Abc` is derived locally as the first-party client does.
    ///
    /// # Errors
    ///
    /// Returns Authentication for empty, oversized or header-unsafe values.
    pub(crate) fn try_new(
        authorization_v: impl Into<String>,
        user_agent: impl Into<String>,
    ) -> ProviderResult<Self> {
        let mut authorization_v = authorization_v.into();
        let user_agent = user_agent.into();
        let valid_authorization = !authorization_v.is_empty()
            && authorization_v.len() <= 512
            && authorization_v.trim() == authorization_v
            && !authorization_v.chars().any(char::is_control)
            && HeaderValue::from_str(&authorization_v).is_ok();
        let valid_user_agent = !user_agent.is_empty()
            && user_agent.len() <= 4 * 1_024
            && user_agent.trim() == user_agent
            && !user_agent.chars().any(char::is_control)
            && HeaderValue::from_str(&user_agent).is_ok();
        if !valid_authorization || !valid_user_agent {
            authorization_v.zeroize();
            return Err(invalid_client_context());
        }
        let abc = format!("{:x}", md5::compute(user_agent.as_bytes()));
        Ok(Self {
            authorization_v: SecretString::new(authorization_v),
            user_agent,
            abc,
        })
    }

    pub(crate) fn authorization_header(&self) -> ProviderResult<HeaderValue> {
        let mut header = HeaderValue::from_str(self.authorization_v.expose_secret())
            .map_err(|_| invalid_client_context())?;
        header.set_sensitive(true);
        Ok(header)
    }

    pub(crate) fn user_agent_header(&self) -> ProviderResult<HeaderValue> {
        HeaderValue::from_str(&self.user_agent).map_err(|_| invalid_client_context())
    }

    pub(crate) fn abc_header(&self) -> ProviderResult<HeaderValue> {
        HeaderValue::from_str(&self.abc).map_err(|_| invalid_client_context())
    }
}

impl fmt::Debug for CidarenOauthClientContext {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("CidarenOauthClientContext([REDACTED])")
    }
}

/// One freshly generated P-256 client bootstrap, bound to exactly one OAuth
/// code exchange. It is intentionally neither cloneable nor serializable.
pub(crate) struct CidarenOauthBootstrap {
    private_key: EphemeralSecret,
    public_key_spki: String,
}

impl CidarenOauthBootstrap {
    /// Generates the exact WebCrypto-compatible P-256/SPKI bootstrap used by
    /// the current Cidaren H5 client.
    ///
    /// # Errors
    ///
    /// Returns Internal if the generated public key cannot be DER encoded.
    pub(crate) fn generate() -> ProviderResult<Self> {
        let private_key = EphemeralSecret::random(&mut OsRng);
        let public_key = PublicKey::from(&private_key);
        let public_key_spki = public_key
            .to_public_key_der()
            .map_err(|_| internal_crypto_error())?;
        Ok(Self {
            private_key,
            public_key_spki: STANDARD.encode(public_key_spki.as_bytes()),
        })
    }

    /// Freezes one signed request body around this bootstrap's public key.
    /// The OAuth code remains redacted and zeroized in memory.
    ///
    /// # Errors
    ///
    /// Returns Internal for a zero timestamp.
    pub(crate) fn build_request(
        &self,
        code: CidarenOauthCode,
        timestamp_millis: u64,
    ) -> ProviderResult<CidarenOauthLoginRequest> {
        if timestamp_millis == 0 {
            return Err(ProviderError::new(
                ProviderErrorKind::Internal,
                "Cidaren OAuth exchange requires a non-zero timestamp",
            ));
        }
        let signature_input = Zeroizing::new(format!(
            "code={}&cpub_k={}&cpub_v={CRYPTO_VERSION}&timestamp={timestamp_millis}&version={LOGIN_VERSION}{SIGNING_SUFFIX}",
            code.expose_secret(),
            self.public_key_spki
        ));
        let sign = format!("{:x}", md5::compute(signature_input.as_bytes()));
        Ok(CidarenOauthLoginRequest {
            code,
            public_key_spki: self.public_key_spki.clone(),
            timestamp_millis,
            sign,
        })
    }

    /// Authenticates and decrypts one response from this exact bootstrap.
    /// Consuming `self` prevents accidentally pairing a response with another
    /// client private key or reusing the key for a second code exchange.
    ///
    /// # Errors
    ///
    /// Returns Authentication for a rejected code and `InvalidResponse` for
    /// protocol drift, invalid P-256 material, failed AES-GCM authentication,
    /// an invalid token or any oversized field.
    pub(crate) fn complete(
        self,
        mut response_document: Vec<u8>,
    ) -> ProviderResult<CidarenOauthLoginMaterial> {
        let result = self.complete_inner(&response_document);
        response_document.zeroize();
        result
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the authenticated login transcript is kept sequential so every bound protocol check remains auditable"
    )]
    fn complete_inner(self, response_document: &[u8]) -> ProviderResult<CidarenOauthLoginMaterial> {
        if response_document.is_empty() || response_document.len() > MAX_LOGIN_RESPONSE_BYTES {
            return Err(invalid_login_response());
        }
        let mut root = ZeroizingJson::new(
            serde_json::from_slice::<Value>(response_document)
                .map_err(|_| invalid_login_response())?,
        );
        let object = root
            .value()
            .as_object()
            .ok_or_else(invalid_login_response)?;
        let code = object
            .get("code")
            .and_then(Value::as_i64)
            .ok_or_else(invalid_login_response)?;
        if code != 1 {
            return Err(ProviderError::new(
                ProviderErrorKind::Authentication,
                "Cidaren rejected or expired the WeChat OAuth authorization code",
            ));
        }
        let data = object
            .get("data")
            .and_then(Value::as_object)
            .ok_or_else(invalid_login_response)?;
        if data.get("handshake_required").and_then(Value::as_bool) != Some(false)
            || data.get("encrypted").and_then(Value::as_bool) != Some(true)
        {
            return Err(protocol_drift());
        }
        let handshake = data
            .get("handshake")
            .and_then(Value::as_object)
            .ok_or_else(protocol_drift)?;
        let payload = data
            .get("payload")
            .and_then(Value::as_object)
            .ok_or_else(protocol_drift)?;
        require_version(handshake.get("version"))?;
        require_version(payload.get("version"))?;

        let server_public_key = decode_bounded_base64(
            handshake.get("spub_k"),
            MAX_HANDSHAKE_FIELD_BYTES,
            protocol_drift,
        )?;
        let server_public_key =
            PublicKey::from_public_key_der(&server_public_key).map_err(|_| protocol_drift())?;
        let salt = Zeroizing::new(decode_bounded_base64(
            handshake.get("salt"),
            MAX_HANDSHAKE_FIELD_BYTES,
            protocol_drift,
        )?);
        let shared_secret = self.private_key.diffie_hellman(&server_public_key);
        let shared_secret = Zeroizing::new(shared_secret.raw_secret_bytes().to_vec());
        let hkdf = Hkdf::<Sha256>::new(Some(salt.as_slice()), shared_secret.as_slice());
        let mut key = Zeroizing::new([0_u8; AES_256_KEY_BYTES]);
        hkdf.expand(LOGIN_HKDF_INFO, key.as_mut())
            .map_err(|_| invalid_login_response())?;

        let aad = payload
            .get("aad")
            .and_then(Value::as_str)
            .filter(|value| *value == LOGIN_AAD)
            .ok_or_else(protocol_drift)?;
        let iv = decode_bounded_base64(
            payload.get("iv"),
            AES_GCM_NONCE_BYTES,
            invalid_login_response,
        )?;
        if iv.len() != AES_GCM_NONCE_BYTES {
            return Err(invalid_login_response());
        }
        let ciphertext = Zeroizing::new(decode_bounded_base64(
            payload
                .get("cipher_text")
                .or_else(|| payload.get("cipherText")),
            MAX_LOGIN_CIPHERTEXT_BYTES,
            invalid_login_response,
        )?);
        let cipher =
            Aes256Gcm::new_from_slice(key.as_slice()).map_err(|_| internal_crypto_error())?;
        let plaintext = cipher
            .decrypt(
                Nonce::from_slice(&iv),
                Payload {
                    msg: ciphertext.as_slice(),
                    aad: aad.as_bytes(),
                },
            )
            .map_err(|_| invalid_login_response())?;
        let mut plaintext = Zeroizing::new(plaintext);
        if plaintext.is_empty() || plaintext.len() > MAX_LOGIN_PLAINTEXT_BYTES {
            return Err(invalid_login_response());
        }
        let session = ZeroizingJson::new(
            serde_json::from_slice::<Value>(plaintext.as_slice())
                .map_err(|_| invalid_login_response())?,
        );
        plaintext.zeroize();
        let token = session
            .value()
            .as_object()
            .and_then(|object| object.get("token"))
            .and_then(Value::as_str)
            .ok_or_else(invalid_login_response)?
            .to_owned();
        let crypto_value = ZeroizingJson::new(serde_json::json!({
            "login_info": {
                "a": format!("h{}", STANDARD.encode(shared_secret.as_slice())),
                "b": format!("a{}", STANDARD.encode(salt.as_slice())),
            }
        }));
        let crypto_document =
            serde_json::to_vec(crypto_value.value()).map_err(|_| internal_crypto_error())?;
        let material = CidarenOauthLoginMaterial {
            token: SecretString::new(token),
            crypto_document: SecretValue::new(crypto_document),
        };
        material.token_session()?;
        root.zeroize_now();
        Ok(material)
    }
}

impl fmt::Debug for CidarenOauthBootstrap {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CidarenOauthBootstrap")
            .field("private_key", &"[REDACTED]")
            .field("public_key_spki", &self.public_key_spki)
            .finish()
    }
}

/// Exact signed request body for `LoginByWechatCode`. `app_type=1` is added
/// by the HTTP boundary, matching the current H5 request interceptor; it is
/// deliberately not part of the signed object.
pub(crate) struct CidarenOauthLoginRequest {
    code: CidarenOauthCode,
    public_key_spki: String,
    timestamp_millis: u64,
    sign: String,
}

impl CidarenOauthLoginRequest {
    #[cfg(test)]
    pub(crate) const fn path() -> &'static str {
        LOGIN_PATH
    }

    #[cfg(test)]
    pub(crate) fn public_key_spki(&self) -> &str {
        &self.public_key_spki
    }

    /// Serializes the bounded wire body only at the native HTTP boundary.
    ///
    /// # Errors
    ///
    /// Returns Internal if serialization unexpectedly fails.
    pub(crate) fn body_bytes(&self) -> ProviderResult<Zeroizing<Vec<u8>>> {
        #[derive(Serialize)]
        struct Body<'a> {
            code: &'a str,
            cpub_k: &'a str,
            cpub_v: &'static str,
            timestamp: u64,
            version: &'static str,
            sign: &'a str,
            app_type: u8,
        }
        serde_json::to_vec(&Body {
            code: self.code.expose_secret(),
            cpub_k: &self.public_key_spki,
            cpub_v: CRYPTO_VERSION,
            timestamp: self.timestamp_millis,
            version: LOGIN_VERSION,
            sign: &self.sign,
            app_type: 1,
        })
        .map(Zeroizing::new)
        .map_err(|_| internal_crypto_error())
    }
}

impl fmt::Debug for CidarenOauthLoginRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CidarenOauthLoginRequest")
            .field("code", &"[REDACTED]")
            .field("public_key_spki", &self.public_key_spki)
            .field("timestamp_millis", &self.timestamp_millis)
            .field("sign", &self.sign)
            .finish()
    }
}

/// Authenticated token plus standard crypto material produced by one native
/// V2 OAuth exchange. Both fields remain zeroizing secrets.
pub(crate) struct CidarenOauthLoginMaterial {
    token: SecretString,
    crypto_document: SecretValue,
}

impl CidarenOauthLoginMaterial {
    /// Creates the normal account-bound Provider session for immediate fresh
    /// `Student/Main` validation.
    ///
    /// # Errors
    ///
    /// Returns Authentication if the decrypted result does not form the exact
    /// Composite session shape.
    pub(crate) fn token_session(&self) -> ProviderResult<CidarenTokenSession> {
        CidarenTokenSession::try_new_captured(
            self.token.expose_secret().to_owned(),
            self.crypto_document.expose_secret(),
        )
    }

    /// Converts a freshly validated login into Core's atomic replacement
    /// shape. Core still owns persistence and `AuthSession` binding.
    pub(crate) fn into_credential_replacement(self) -> CredentialReplacement {
        CredentialReplacement {
            session_kind: asterism_domain::SessionKind::Composite,
            fields: vec![
                CredentialField {
                    purpose: SecretPurpose::ProviderAccessToken,
                    value: SecretValue::new(self.token.expose_secret().as_bytes().to_vec()),
                },
                CredentialField {
                    purpose: SecretPurpose::ProviderCompositeSession,
                    value: self.crypto_document,
                },
            ],
        }
    }
}

impl fmt::Debug for CidarenOauthLoginMaterial {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("CidarenOauthLoginMaterial([REDACTED])")
    }
}

fn require_version(value: Option<&Value>) -> ProviderResult<()> {
    if value.and_then(Value::as_str) == Some(CRYPTO_VERSION) {
        Ok(())
    } else {
        Err(protocol_drift())
    }
}

fn decode_bounded_base64(
    value: Option<&Value>,
    maximum: usize,
    error: fn() -> ProviderError,
) -> ProviderResult<Vec<u8>> {
    let encoded = value
        .and_then(Value::as_str)
        .filter(|value| {
            !value.is_empty()
                && value.len() <= maximum.saturating_mul(2).saturating_add(16)
                && value.is_ascii()
                && !value.bytes().any(|byte| byte.is_ascii_whitespace())
        })
        .ok_or_else(error)?;
    let decoded = STANDARD.decode(encoded).map_err(|_| error())?;
    if decoded.is_empty() || decoded.len() > maximum {
        Err(error())
    } else {
        Ok(decoded)
    }
}

struct ZeroizingJson(Value);

impl ZeroizingJson {
    const fn new(value: Value) -> Self {
        Self(value)
    }

    const fn value(&self) -> &Value {
        &self.0
    }

    fn zeroize_now(&mut self) {
        zeroize_json(&mut self.0);
    }
}

impl Drop for ZeroizingJson {
    fn drop(&mut self) {
        self.zeroize_now();
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

fn invalid_oauth_code() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::Authentication,
        "Cidaren WeChat OAuth callback code is invalid or oversized",
    )
}

fn invalid_client_context() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::Authentication,
        "Cidaren OAuth callback has no valid browser/device binding",
    )
}

fn invalid_login_response() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::InvalidResponse,
        "Cidaren V2 OAuth login response failed bounded authenticated decoding",
    )
}

fn protocol_drift() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::ProtocolDrift,
        "Cidaren V2 OAuth login handshake no longer matches the audited protocol",
    )
}

fn internal_crypto_error() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::Internal,
        "Cidaren V2 OAuth crypto initialization failed",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_matches_current_signed_v2_shape_and_redacts_code() {
        let bootstrap = CidarenOauthBootstrap::generate().unwrap();
        let request = bootstrap
            .build_request(
                CidarenOauthCode::try_new("synthetic-oauth-code").unwrap(),
                1_786_444_968_188,
            )
            .unwrap();
        assert_eq!(CidarenOauthLoginRequest::path(), LOGIN_PATH);
        let body: Value = serde_json::from_slice(request.body_bytes().unwrap().as_slice()).unwrap();
        assert_eq!(body["code"], "synthetic-oauth-code");
        assert_eq!(body["cpub_v"], CRYPTO_VERSION);
        assert_eq!(body["version"], LOGIN_VERSION);
        assert_eq!(body["app_type"], 1);
        let spki = STANDARD.decode(body["cpub_k"].as_str().unwrap()).unwrap();
        PublicKey::from_public_key_der(&spki).unwrap();
        let signature_input = format!(
            "code=synthetic-oauth-code&cpub_k={}&cpub_v=v1&timestamp=1786444968188&version={LOGIN_VERSION}{SIGNING_SUFFIX}",
            body["cpub_k"].as_str().unwrap()
        );
        assert_eq!(
            body["sign"],
            format!("{:x}", md5::compute(signature_input.as_bytes()))
        );
        assert!(!format!("{request:?}").contains("synthetic-oauth-code"));
    }

    #[test]
    fn oauth_client_context_derives_exact_abc_and_redacts_header_context() {
        let context = CidarenOauthClientContext::try_new(
            "synthetic-device-code",
            "Mozilla/5.0 synthetic-cidaren-client",
        )
        .unwrap();
        assert_eq!(
            context.abc_header().unwrap(),
            format!(
                "{:x}",
                md5::compute(b"Mozilla/5.0 synthetic-cidaren-client")
            )
        );
        assert!(context.authorization_header().unwrap().is_sensitive());
        assert_eq!(
            context.user_agent_header().unwrap(),
            "Mozilla/5.0 synthetic-cidaren-client"
        );
        assert!(!format!("{context:?}").contains("synthetic"));
        let bootstrap = CidarenOauthClientContext::try_for_oauth_bootstrap(
            "Mozilla/5.0 synthetic-cidaren-client",
        )
        .unwrap();
        assert_eq!(bootstrap.authorization_header().unwrap(), "00");
        assert_eq!(
            bootstrap.abc_header().unwrap(),
            context.abc_header().unwrap()
        );
        for (authorization, user_agent) in [
            ("", "Mozilla/5.0"),
            (" padded ", "Mozilla/5.0"),
            ("device", ""),
            ("device", "bad\nagent"),
        ] {
            assert!(CidarenOauthClientContext::try_new(authorization, user_agent).is_err());
        }
        assert!(CidarenOauthClientContext::try_new("x".repeat(513), "Mozilla/5.0").is_err());
        assert!(CidarenOauthClientContext::try_new("device", "x".repeat(4 * 1_024 + 1)).is_err());
    }

    #[test]
    fn native_exchange_decrypts_webcrypto_compatible_session() {
        let bootstrap = CidarenOauthBootstrap::generate().unwrap();
        let request = bootstrap
            .build_request(
                CidarenOauthCode::try_new("synthetic-code").unwrap(),
                1_786_444_968_188,
            )
            .unwrap();
        let response = encrypted_response(request.public_key_spki(), "synthetic-user-token");
        let material = bootstrap.complete(response).unwrap();
        let session = material.token_session().unwrap();
        assert_eq!(session.expose_token(), "synthetic-user-token");
        assert!(session.crypto_context().is_some());
        assert!(!format!("{material:?}").contains("synthetic"));
        let replacement = material.into_credential_replacement();
        assert_eq!(replacement.fields.len(), 2);
        assert_eq!(
            replacement
                .fields
                .iter()
                .map(|field| field.purpose)
                .collect::<Vec<_>>(),
            [
                SecretPurpose::ProviderAccessToken,
                SecretPurpose::ProviderCompositeSession,
            ]
        );
    }

    #[test]
    fn native_exchange_fails_closed_on_rejection_drift_and_tampering() {
        for code in ["", " padded ", "bad/code", "x\n", &"x".repeat(513)] {
            assert!(CidarenOauthCode::try_new(code).is_err());
        }

        let rejected = CidarenOauthBootstrap::generate()
            .unwrap()
            .complete(br#"{"code":11003,"msg":"expired","data":null}"#.to_vec())
            .unwrap_err();
        assert_eq!(rejected.kind, ProviderErrorKind::Authentication);

        let bootstrap = CidarenOauthBootstrap::generate().unwrap();
        let request = bootstrap
            .build_request(CidarenOauthCode::try_new("code-a").unwrap(), 1)
            .unwrap();
        let mut response = encrypted_response(request.public_key_spki(), "token-a");
        let position = response
            .windows(b"cipher_text".len())
            .position(|window| window == b"cipher_text")
            .unwrap();
        let tail = &mut response[position + b"cipher_text".len()..];
        let encoded = tail
            .iter_mut()
            .find(|byte| byte.is_ascii_alphabetic())
            .unwrap();
        *encoded = if *encoded == b'A' { b'B' } else { b'A' };
        assert_eq!(
            bootstrap.complete(response).unwrap_err().kind,
            ProviderErrorKind::InvalidResponse
        );

        let bootstrap = CidarenOauthBootstrap::generate().unwrap();
        let request = bootstrap
            .build_request(CidarenOauthCode::try_new("code-b").unwrap(), 2)
            .unwrap();
        let response = encrypted_response(request.public_key_spki(), "token-b");
        let mut response: Value = serde_json::from_slice(&response).unwrap();
        response["data"]["handshake"]["version"] = Value::String("v2".to_owned());
        assert_eq!(
            bootstrap
                .complete(serde_json::to_vec(&response).unwrap())
                .unwrap_err()
                .kind,
            ProviderErrorKind::ProtocolDrift
        );

        for (code, token) in [
            ("code-empty-token", ""),
            ("code-control-token", "bad\ntoken"),
        ] {
            let bootstrap = CidarenOauthBootstrap::generate().unwrap();
            let request = bootstrap
                .build_request(CidarenOauthCode::try_new(code).unwrap(), 3)
                .unwrap();
            let response = encrypted_response(request.public_key_spki(), token);
            assert_eq!(
                bootstrap.complete(response).unwrap_err().kind,
                ProviderErrorKind::Authentication
            );
        }
    }

    fn encrypted_response(client_spki: &str, token: &str) -> Vec<u8> {
        let client_public =
            PublicKey::from_public_key_der(&STANDARD.decode(client_spki).unwrap()).unwrap();
        let server_private = EphemeralSecret::random(&mut OsRng);
        let server_public = PublicKey::from(&server_private);
        let server_spki = server_public.to_public_key_der().unwrap();
        let shared_secret = server_private.diffie_hellman(&client_public);
        let salt = b"synthetic-login-salt";
        let hkdf = Hkdf::<Sha256>::new(Some(salt), shared_secret.raw_secret_bytes().as_slice());
        let mut key = [0_u8; AES_256_KEY_BYTES];
        hkdf.expand(LOGIN_HKDF_INFO, &mut key).unwrap();
        let cipher = Aes256Gcm::new_from_slice(&key).unwrap();
        let iv = [7_u8; AES_GCM_NONCE_BYTES];
        let plaintext = serde_json::to_vec(&serde_json::json!({
            "token": token,
            "ucd": "synthetic-ucd",
            "subscribe": "1"
        }))
        .unwrap();
        let ciphertext = cipher
            .encrypt(
                Nonce::from_slice(&iv),
                Payload {
                    msg: &plaintext,
                    aad: LOGIN_AAD.as_bytes(),
                },
            )
            .unwrap();
        key.zeroize();
        serde_json::to_vec(&serde_json::json!({
            "code": 1,
            "msg": "success",
            "data": {
                "handshake_required": false,
                "handshake": {
                    "salt": STANDARD.encode(salt),
                    "version": CRYPTO_VERSION,
                    "spub_k": STANDARD.encode(server_spki.as_bytes())
                },
                "encrypted": true,
                "payload": {
                    "version": CRYPTO_VERSION,
                    "iv": STANDARD.encode(iv),
                    "cipher_text": STANDARD.encode(ciphertext),
                    "aad": LOGIN_AAD
                }
            },
            "jv": "0",
            "cv": "0"
        }))
        .unwrap()
    }
}
