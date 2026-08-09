use std::{fmt, sync::Arc};

use asterism_domain::SessionKind;
use asterism_provider_api::{ProviderContext, ProviderError, ProviderErrorKind, ProviderResult};
use asterism_secrets::{
    CredentialAcquisition, ProviderCredentialResolution, ProviderCredentialResolver, SecretPurpose,
    SecretStoreError,
};
use async_trait::async_trait;
use chrono::Utc;

use crate::{UaiJwtSession, UaiSessionResolver, metadata::PROVIDER_ID};

/// UAI session adapter backed by Core's provider-scoped credential resolver.
/// Plaintext exists only for one bounded operation.
pub struct StoredUaiSessionResolver {
    credentials: Arc<dyn ProviderCredentialResolver>,
}

impl StoredUaiSessionResolver {
    pub fn new(credentials: Arc<dyn ProviderCredentialResolver>) -> Self {
        Self { credentials }
    }
}

impl fmt::Debug for StoredUaiSessionResolver {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StoredUaiSessionResolver")
            .field("credentials", &"configured")
            .finish()
    }
}

#[async_trait]
impl UaiSessionResolver for StoredUaiSessionResolver {
    async fn resolve_session(&self, context: &ProviderContext) -> ProviderResult<UaiJwtSession> {
        if context.provider_id.as_str() != PROVIDER_ID || context.credential_refs.is_empty() {
            return Err(invalid_stored_session());
        }
        let mut credentials = self
            .credentials
            .resolve_provider_credentials(ProviderCredentialResolution {
                provider_account_id: context.account_id,
                credential_refs: context.credential_refs.clone(),
                purposes: vec![SecretPurpose::ProviderCompositeSession],
                correlation_id: context.correlation_id.clone(),
            })
            .await
            .map_err(|error| map_resolution_error(&error))?;
        if credentials.len() != 1 {
            return Err(invalid_stored_session());
        }
        let resolved = credentials.pop().expect("one credential was required");
        let metadata = &resolved.credential;
        let valid_origin = matches!(
            (metadata.session_kind, metadata.acquired_via),
            (
                SessionKind::Composite,
                CredentialAcquisition::NativeProviderLogin
            ) | (SessionKind::Jwt, CredentialAcquisition::ManualImport)
        );
        if metadata.provider_account_id != context.account_id
            || metadata.secret.purpose != SecretPurpose::ProviderCompositeSession
            || !context.credential_refs.contains(&metadata.secret.id)
            || !valid_origin
            || metadata.is_expired_at(Utc::now())
        {
            return Err(invalid_stored_session());
        }
        UaiJwtSession::try_from_composite(resolved.value.expose_secret())
            .map_err(|_| invalid_stored_session())
    }
}

fn map_resolution_error(error: &SecretStoreError) -> ProviderError {
    let kind = match error {
        SecretStoreError::NotFound
        | SecretStoreError::Unauthorized
        | SecretStoreError::VersionConflict
        | SecretStoreError::InvalidValue
        | SecretStoreError::CredentialManaged
        | SecretStoreError::AccountMismatch => ProviderErrorKind::Authentication,
        SecretStoreError::KeyUnavailable
        | SecretStoreError::Storage
        | SecretStoreError::AuthenticationFailed => ProviderErrorKind::Internal,
    };
    ProviderError::new(kind, "UAI stored session could not be resolved")
}

fn invalid_stored_session() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::Authentication,
        "UAI account has no usable stored openid/JWT session",
    )
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    };

    use asterism_domain::{ProviderAccountId, ProviderId, SecretId, Timestamp, UserId};
    use asterism_secrets::{
        ProviderCredential, ResolvedProviderCredential, SecretRef, SecretValue,
    };

    use super::*;

    #[derive(Clone, Copy, Debug)]
    enum FixtureBehavior {
        Imported,
        Native,
        Duplicate,
        ForeignAccount,
        UnrequestedSecret,
        WrongPurpose,
        WrongOrigin,
        Expired,
        Malformed,
        StorageFailure,
    }

    #[derive(Debug)]
    struct FixtureCredentialResolver {
        behavior: FixtureBehavior,
        request: Mutex<Option<ProviderCredentialResolution>>,
        resolutions: AtomicUsize,
    }

    #[async_trait]
    impl ProviderCredentialResolver for FixtureCredentialResolver {
        async fn resolve_provider_credentials(
            &self,
            request: ProviderCredentialResolution,
        ) -> Result<Vec<ResolvedProviderCredential>, SecretStoreError> {
            self.resolutions.fetch_add(1, Ordering::SeqCst);
            *self.request.lock().unwrap() = Some(request.clone());
            let secret_id = request.credential_refs[0];
            let valid = |session_kind, acquired_via, value: &[u8]| {
                resolved_credential(
                    request.provider_account_id,
                    secret_id,
                    SecretPurpose::ProviderCompositeSession,
                    session_kind,
                    acquired_via,
                    None,
                    value,
                )
            };
            let document = br#"{"openid":"stored-open-id","jwt":"STORED_HEADER.STORED_PAYLOAD.STORED_SIGNATURE"}"#;
            match self.behavior {
                FixtureBehavior::Imported => Ok(vec![valid(
                    SessionKind::Jwt,
                    CredentialAcquisition::ManualImport,
                    document,
                )]),
                FixtureBehavior::Native => Ok(vec![valid(
                    SessionKind::Composite,
                    CredentialAcquisition::NativeProviderLogin,
                    document,
                )]),
                FixtureBehavior::Duplicate => Ok(vec![
                    valid(
                        SessionKind::Jwt,
                        CredentialAcquisition::ManualImport,
                        document,
                    ),
                    valid(
                        SessionKind::Jwt,
                        CredentialAcquisition::ManualImport,
                        document,
                    ),
                ]),
                FixtureBehavior::ForeignAccount => Ok(vec![resolved_credential(
                    ProviderAccountId::new(),
                    secret_id,
                    SecretPurpose::ProviderCompositeSession,
                    SessionKind::Jwt,
                    CredentialAcquisition::ManualImport,
                    None,
                    document,
                )]),
                FixtureBehavior::UnrequestedSecret => Ok(vec![resolved_credential(
                    request.provider_account_id,
                    SecretId::new(),
                    SecretPurpose::ProviderCompositeSession,
                    SessionKind::Jwt,
                    CredentialAcquisition::ManualImport,
                    None,
                    document,
                )]),
                FixtureBehavior::WrongPurpose => Ok(vec![resolved_credential(
                    request.provider_account_id,
                    secret_id,
                    SecretPurpose::ProviderAccessToken,
                    SessionKind::Jwt,
                    CredentialAcquisition::ManualImport,
                    None,
                    document,
                )]),
                FixtureBehavior::WrongOrigin => Ok(vec![valid(
                    SessionKind::Jwt,
                    CredentialAcquisition::NativeProviderLogin,
                    document,
                )]),
                FixtureBehavior::Expired => Ok(vec![resolved_credential(
                    request.provider_account_id,
                    secret_id,
                    SecretPurpose::ProviderCompositeSession,
                    SessionKind::Jwt,
                    CredentialAcquisition::ManualImport,
                    Some(Timestamp::default()),
                    document,
                )]),
                FixtureBehavior::Malformed => Ok(vec![valid(
                    SessionKind::Jwt,
                    CredentialAcquisition::ManualImport,
                    b"not-json",
                )]),
                FixtureBehavior::StorageFailure => Err(SecretStoreError::Storage),
            }
        }
    }

    #[tokio::test]
    async fn resolves_only_context_bound_imported_or_native_session() {
        for behavior in [FixtureBehavior::Imported, FixtureBehavior::Native] {
            let credentials = Arc::new(FixtureCredentialResolver {
                behavior,
                request: Mutex::new(None),
                resolutions: AtomicUsize::new(0),
            });
            let resolver = StoredUaiSessionResolver::new(credentials.clone());
            let context = provider_context();
            let session = resolver.resolve_session(&context).await.unwrap();
            assert_eq!(session.expose_open_id(), "stored-open-id");
            assert!(!format!("{resolver:?}").contains("stored-open-id"));
            let request = credentials.request.lock().unwrap().clone().unwrap();
            assert_eq!(request.provider_account_id, context.account_id);
            assert_eq!(request.credential_refs, context.credential_refs);
            assert_eq!(request.purposes, [SecretPurpose::ProviderCompositeSession]);
            assert_eq!(request.correlation_id, context.correlation_id);
        }
    }

    #[tokio::test]
    async fn rejects_unbound_expired_or_malformed_credentials() {
        for behavior in [
            FixtureBehavior::Duplicate,
            FixtureBehavior::ForeignAccount,
            FixtureBehavior::UnrequestedSecret,
            FixtureBehavior::WrongPurpose,
            FixtureBehavior::WrongOrigin,
            FixtureBehavior::Expired,
            FixtureBehavior::Malformed,
        ] {
            assert_eq!(
                fixture_resolver(behavior)
                    .resolve_session(&provider_context())
                    .await
                    .unwrap_err()
                    .kind,
                ProviderErrorKind::Authentication
            );
        }
    }

    #[tokio::test]
    async fn rejects_wrong_context_before_storage_and_sanitizes_storage_errors() {
        let credentials = Arc::new(FixtureCredentialResolver {
            behavior: FixtureBehavior::Imported,
            request: Mutex::new(None),
            resolutions: AtomicUsize::new(0),
        });
        let resolver = StoredUaiSessionResolver::new(credentials.clone());
        let mut wrong = provider_context();
        wrong.provider_id = ProviderId::new("welearn").unwrap();
        assert_eq!(
            resolver.resolve_session(&wrong).await.unwrap_err().kind,
            ProviderErrorKind::Authentication
        );
        assert_eq!(credentials.resolutions.load(Ordering::SeqCst), 0);

        assert_eq!(
            fixture_resolver(FixtureBehavior::StorageFailure)
                .resolve_session(&provider_context())
                .await
                .unwrap_err()
                .kind,
            ProviderErrorKind::Internal
        );
    }

    fn fixture_resolver(behavior: FixtureBehavior) -> StoredUaiSessionResolver {
        StoredUaiSessionResolver::new(Arc::new(FixtureCredentialResolver {
            behavior,
            request: Mutex::new(None),
            resolutions: AtomicUsize::new(0),
        }))
    }

    fn resolved_credential(
        provider_account_id: ProviderAccountId,
        secret_id: SecretId,
        purpose: SecretPurpose,
        session_kind: SessionKind,
        acquired_via: CredentialAcquisition,
        expires_at: Option<Timestamp>,
        value: &[u8],
    ) -> ResolvedProviderCredential {
        let now = Timestamp::default();
        ResolvedProviderCredential {
            credential: ProviderCredential {
                provider_account_id,
                secret: SecretRef {
                    id: secret_id,
                    owner_user_id: UserId::new(),
                    purpose,
                    version: 1,
                    key_id: "key-a".to_owned(),
                    created_at: now,
                    updated_at: now,
                },
                session_kind,
                acquired_via,
                captured_at: now,
                expires_at,
                updated_at: now,
            },
            value: SecretValue::new(value.to_vec()),
        }
    }

    fn provider_context() -> ProviderContext {
        ProviderContext {
            provider_id: ProviderId::new(PROVIDER_ID).unwrap(),
            account_id: ProviderAccountId::new(),
            credential_refs: vec![SecretId::new()],
            correlation_id: "uai-stored-session-test".to_owned(),
        }
    }
}
