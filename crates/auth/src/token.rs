use asterism_secrets::SecretString;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use rand_core::{OsRng, RngCore};
use sha2::{Digest, Sha256};

const TOKEN_RANDOM_BYTES: usize = 32;

#[derive(Clone, Debug)]
pub struct OpaqueTokenService {
    prefix: &'static str,
}

impl OpaqueTokenService {
    /// Creates a generator with an ASCII token-family prefix such as `ast_ws`.
    ///
    /// # Errors
    ///
    /// Returns [`TokenError::InvalidPrefix`] if the prefix is empty, too long,
    /// or contains characters other than lowercase ASCII, digits, and `_`.
    pub fn new(prefix: &'static str) -> Result<Self, TokenError> {
        let valid = !prefix.is_empty()
            && prefix.len() <= 16
            && prefix
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_');
        if valid {
            Ok(Self { prefix })
        } else {
            Err(TokenError::InvalidPrefix)
        }
    }

    /// Generates 256 random bits and returns the one-time plaintext plus its
    /// storage-safe SHA-256 digest.
    pub fn generate(&self) -> (SecretString, TokenDigest) {
        let mut random = [0_u8; TOKEN_RANDOM_BYTES];
        OsRng.fill_bytes(&mut random);
        let plaintext = format!("{}_{}", self.prefix, URL_SAFE_NO_PAD.encode(random));
        random.fill(0);
        let digest = Self::digest_bytes(plaintext.as_bytes());
        (SecretString::new(plaintext), digest)
    }

    pub fn digest(&self, token: &SecretString) -> TokenDigest {
        Self::digest_bytes(token.expose_secret().as_bytes())
    }

    fn digest_bytes(value: &[u8]) -> TokenDigest {
        TokenDigest(Sha256::digest(value).into())
    }
}

#[derive(Clone, Copy, Eq, Hash, PartialEq)]
pub struct TokenDigest([u8; 32]);

impl TokenDigest {
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl std::fmt::Debug for TokenDigest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("TokenDigest([REDACTED])")
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum TokenError {
    #[error("token prefix must contain 1-16 lowercase ASCII letters, digits, or underscores")]
    InvalidPrefix,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_tokens_are_unique_and_only_digests_are_debuggable() {
        let service = OpaqueTokenService::new("ast_ws").unwrap();
        let (first, first_digest) = service.generate();
        let (second, second_digest) = service.generate();
        assert!(first.expose_secret().starts_with("ast_ws_"));
        assert_ne!(first.expose_secret(), second.expose_secret());
        assert_ne!(first_digest, second_digest);
        assert_eq!(service.digest(&first), first_digest);
        assert_eq!(format!("{first_digest:?}"), "TokenDigest([REDACTED])");
    }
}
