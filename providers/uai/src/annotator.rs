use std::time::{SystemTime, UNIX_EPOCH};

use asterism_provider_api::{ProviderError, ProviderErrorKind, ProviderResult};
use asterism_secrets::SecretString;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use hmac::{Hmac, Mac};
use serde::Serialize;
use sha2::Sha256;
use zeroize::Zeroize;

const JWT_HEADER: &[u8] = br#"{"alg":"HS256","typ":"JWT"}"#;
const ANNOTATOR_KEY: &[u8] = b"a824b379f126b8b7aa5e33dee83fb0a05aa7462c";
const ANNOTATOR_ISSUER: &str = "c4f772063dcfa98e9c50";
const ANNOTATOR_AUDIENCE: &str = "edx.unipus.cn";
const TOKEN_LIFETIME_MILLISECONDS: u64 = 31_536_000_000;

type HmacSha256 = Hmac<Sha256>;

pub(crate) fn generate_annotator_token(open_id: &str) -> ProviderResult<SecretString> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| token_error("UAI annotator-token clock is before the Unix epoch"))?
        .as_millis();
    let now = u64::try_from(now)
        .map_err(|_| token_error("UAI annotator-token timestamp exceeds the supported range"))?;
    generate_annotator_token_at(open_id, now)
}

fn generate_annotator_token_at(
    open_id: &str,
    now_milliseconds: u64,
) -> ProviderResult<SecretString> {
    let expiration = now_milliseconds
        .checked_add(TOKEN_LIFETIME_MILLISECONDS)
        .ok_or_else(|| token_error("UAI annotator-token expiration overflowed"))?;
    let claims = AnnotatorClaims {
        open_id,
        name: "",
        email: "",
        administrator: false,
        expiration,
        issuer: ANNOTATOR_ISSUER,
        audience: ANNOTATOR_AUDIENCE,
    };
    let mut payload = serde_json::to_vec(&claims)
        .map_err(|_| token_error("UAI annotator-token claims cannot be encoded"))?;
    let header = URL_SAFE_NO_PAD.encode(JWT_HEADER);
    let mut payload_segment = URL_SAFE_NO_PAD.encode(&payload);
    payload.zeroize();
    let mut signing_input = format!("{header}.{payload_segment}");
    payload_segment.zeroize();

    let mut mac = HmacSha256::new_from_slice(ANNOTATOR_KEY)
        .map_err(|_| token_error("UAI annotator-token signer cannot be initialized"))?;
    mac.update(signing_input.as_bytes());
    let signature = URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes());
    let token = format!("{signing_input}.{signature}");
    signing_input.zeroize();
    Ok(SecretString::new(token))
}

#[derive(Serialize)]
struct AnnotatorClaims<'a> {
    open_id: &'a str,
    name: &'static str,
    email: &'static str,
    administrator: bool,
    #[serde(rename = "exp")]
    expiration: u64,
    #[serde(rename = "iss")]
    issuer: &'static str,
    #[serde(rename = "aud")]
    audience: &'static str,
}

fn token_error(message: &'static str) -> ProviderError {
    ProviderError::new(ProviderErrorKind::Internal, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_matches_the_two_donor_hs256_contract() {
        let token = generate_annotator_token_at("synthetic-open-id", 1_700_000_000_000).unwrap();
        assert_eq!(
            token.expose_secret(),
            "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJvcGVuX2lkIjoic3ludGhldGljLW9wZW4taWQiLCJuYW1lIjoiIiwiZW1haWwiOiIiLCJhZG1pbmlzdHJhdG9yIjpmYWxzZSwiZXhwIjoxNzMxNTM2MDAwMDAwLCJpc3MiOiJjNGY3NzIwNjNkY2ZhOThlOWM1MCIsImF1ZCI6ImVkeC51bmlwdXMuY24ifQ.dfj4QmJiHmKy8EKt2hZjygu_A4G_dJhJhcsGScMT4T0"
        );
        assert!(!format!("{token:?}").contains("synthetic"));
    }

    #[test]
    fn expiration_overflow_fails_closed() {
        assert!(generate_annotator_token_at("synthetic", u64::MAX).is_err());
    }
}
