use argon2::{
    Algorithm, Argon2, Params, PasswordHash, PasswordHasher, PasswordVerifier, Version,
    password_hash::{SaltString, rand_core::OsRng},
};
use asterism_secrets::SecretString;

const MEMORY_COST_KIB: u32 = 19 * 1024;
const TIME_COST: u32 = 2;
const PARALLELISM: u32 = 1;

#[derive(Clone, Debug)]
pub struct Argon2idPasswordService {
    algorithm: Argon2<'static>,
}

impl Argon2idPasswordService {
    /// Builds the password service using the OWASP Argon2id minimum baseline:
    /// 19 MiB memory, two iterations, and one lane.
    ///
    /// # Errors
    ///
    /// Returns [`PasswordError`] if the compile-time parameters are rejected by
    /// the underlying Argon2 implementation.
    pub fn new() -> Result<Self, PasswordError> {
        let parameters = Params::new(MEMORY_COST_KIB, TIME_COST, PARALLELISM, None)?;
        Ok(Self {
            algorithm: Argon2::new(Algorithm::Argon2id, Version::V0x13, parameters),
        })
    }

    /// Hashes a password with a fresh random salt into PHC string format.
    ///
    /// # Errors
    ///
    /// Returns [`PasswordError`] when salt generation or Argon2 hashing fails.
    pub fn hash(&self, password: &SecretString) -> Result<String, PasswordError> {
        let salt = SaltString::generate(&mut OsRng);
        Ok(self
            .algorithm
            .hash_password(password.expose_secret().as_bytes(), &salt)?
            .to_string())
    }

    /// Verifies a password against a PHC-formatted hash.
    ///
    /// A password mismatch returns `Ok(false)` so callers can present one
    /// generic authentication error without leaking details.
    ///
    /// # Errors
    ///
    /// Returns [`PasswordError`] when the persisted PHC string is malformed or
    /// uses unsupported parameters.
    pub fn verify(
        &self,
        password: &SecretString,
        encoded_hash: &str,
    ) -> Result<bool, PasswordError> {
        let parsed = PasswordHash::new(encoded_hash)?;
        match self
            .algorithm
            .verify_password(password.expose_secret().as_bytes(), &parsed)
        {
            Ok(()) => Ok(true),
            Err(argon2::password_hash::Error::Password) => Ok(false),
            Err(error) => Err(error.into()),
        }
    }
}

impl Default for Argon2idPasswordService {
    fn default() -> Self {
        Self::new().expect("the fixed Argon2id parameters must be valid")
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PasswordError {
    #[error("invalid Argon2 parameters: {0}")]
    Parameters(#[from] argon2::Error),
    #[error("password hash operation failed: {0}")]
    PasswordHash(#[from] argon2::password_hash::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn password_round_trip_uses_argon2id_phc_format() {
        let service = Argon2idPasswordService::new().unwrap();
        let password = SecretString::new("correct horse battery staple");
        let encoded = service.hash(&password).unwrap();
        assert!(encoded.starts_with("$argon2id$v=19$m=19456,t=2,p=1$"));
        assert!(service.verify(&password, &encoded).unwrap());
        assert!(
            !service
                .verify(&SecretString::new("wrong"), &encoded)
                .unwrap()
        );
    }
}
