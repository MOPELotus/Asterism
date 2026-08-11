use std::{fmt, sync::Arc};

use asterism_domain::SessionKind;
use asterism_provider_api::{ProviderContext, ProviderError, ProviderErrorKind, ProviderResult};
use asterism_secrets::{
    CredentialAcquisition, ProviderCredentialResolution, ProviderCredentialResolver, SecretPurpose,
    SecretStoreError,
};
use async_trait::async_trait;
use chrono::Utc;

use crate::{CidarenSessionResolver, CidarenTokenSession, metadata::PROVIDER_ID};

/// Cidaren session adapter backed by Core's provider-scoped credential
/// resolver. Plaintext exists only for one bounded operation.
pub struct StoredCidarenSessionResolver {
    credentials: Arc<dyn ProviderCredentialResolver>,
}

impl StoredCidarenSessionResolver {
    pub fn new(credentials: Arc<dyn ProviderCredentialResolver>) -> Self {
        Self { credentials }
    }
}

impl fmt::Debug for StoredCidarenSessionResolver {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StoredCidarenSessionResolver")
            .field("credentials", &"configured")
            .finish()
    }
}

#[async_trait]
impl CidarenSessionResolver for StoredCidarenSessionResolver {
    async fn resolve_session(
        &self,
        context: &ProviderContext,
    ) -> ProviderResult<CidarenTokenSession> {
        if context.provider_id.as_str() != PROVIDER_ID || context.credential_refs.is_empty() {
            return Err(invalid_stored_session());
        }
        let mut credentials = self
            .credentials
            .resolve_provider_credentials(ProviderCredentialResolution {
                provider_account_id: context.account_id,
                credential_refs: context.credential_refs.clone(),
                purposes: vec![SecretPurpose::ProviderAccessToken],
                correlation_id: context.correlation_id.clone(),
            })
            .await
            .map_err(|error| map_resolution_error(&error))?;
        if credentials.len() != 1 {
            return Err(invalid_stored_session());
        }
        let resolved = credentials.pop().expect("one credential was required");
        let metadata = &resolved.credential;
        if metadata.provider_account_id != context.account_id
            || metadata.secret.purpose != SecretPurpose::ProviderAccessToken
            || !context.credential_refs.contains(&metadata.secret.id)
            || metadata.session_kind != SessionKind::ProviderSpecific
            || metadata.acquired_via != CredentialAcquisition::ManualImport
            || metadata.is_expired_at(Utc::now())
        {
            return Err(invalid_stored_session());
        }
        let token = std::str::from_utf8(resolved.value.expose_secret())
            .map_err(|_| invalid_stored_session())?;
        CidarenTokenSession::try_new(token.to_owned()).map_err(|_| invalid_stored_session())
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
    ProviderError::new(kind, "Cidaren stored session could not be resolved")
}

fn invalid_stored_session() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::Authentication,
        "Cidaren account has no usable stored imported token",
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
        Valid,
        Duplicate,
        ForeignAccount,
        UnrequestedSecret,
        WrongPurpose,
        WrongKind,
        WrongOrigin,
        Expired,
        InvalidUtf8,
        StorageFailure,
    }

    #[derive(Debug)]
    struct FixtureResolver {
        behavior: FixtureBehavior,
        request: Mutex<Option<ProviderCredentialResolution>>,
        calls: AtomicUsize,
    }

    #[async_trait]
    impl ProviderCredentialResolver for FixtureResolver {
        async fn resolve_provider_credentials(
            &self,
            request: ProviderCredentialResolution,
        ) -> Result<Vec<ResolvedProviderCredential>, SecretStoreError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            *self.request.lock().unwrap() = Some(request.clone());
            let secret_id = request.credential_refs[0];
            let valid = || {
                resolved_credential(
                    request.provider_account_id,
                    secret_id,
                    SecretPurpose::ProviderAccessToken,
                    SessionKind::ProviderSpecific,
                    CredentialAcquisition::ManualImport,
                    None,
                    b"synthetic-user-token",
                )
            };
            match self.behavior {
                FixtureBehavior::Valid => Ok(vec![valid()]),
                FixtureBehavior::Duplicate => Ok(vec![valid(), valid()]),
                FixtureBehavior::ForeignAccount => Ok(vec![resolved_credential(
                    ProviderAccountId::new(),
                    secret_id,
                    SecretPurpose::ProviderAccessToken,
                    SessionKind::ProviderSpecific,
                    CredentialAcquisition::ManualImport,
                    None,
                    b"foreign-token",
                )]),
                FixtureBehavior::UnrequestedSecret => Ok(vec![resolved_credential(
                    request.provider_account_id,
                    SecretId::new(),
                    SecretPurpose::ProviderAccessToken,
                    SessionKind::ProviderSpecific,
                    CredentialAcquisition::ManualImport,
                    None,
                    b"unrequested-token",
                )]),
                FixtureBehavior::WrongPurpose => Ok(vec![resolved_credential(
                    request.provider_account_id,
                    secret_id,
                    SecretPurpose::ProviderRefreshToken,
                    SessionKind::ProviderSpecific,
                    CredentialAcquisition::ManualImport,
                    None,
                    b"refresh-token",
                )]),
                FixtureBehavior::WrongKind => Ok(vec![resolved_credential(
                    request.provider_account_id,
                    secret_id,
                    SecretPurpose::ProviderAccessToken,
                    SessionKind::BearerToken,
                    CredentialAcquisition::ManualImport,
                    None,
                    b"bearer-token",
                )]),
                FixtureBehavior::WrongOrigin => Ok(vec![resolved_credential(
                    request.provider_account_id,
                    secret_id,
                    SecretPurpose::ProviderAccessToken,
                    SessionKind::ProviderSpecific,
                    CredentialAcquisition::CaptureTool,
                    None,
                    b"captured-token",
                )]),
                FixtureBehavior::Expired => Ok(vec![resolved_credential(
                    request.provider_account_id,
                    secret_id,
                    SecretPurpose::ProviderAccessToken,
                    SessionKind::ProviderSpecific,
                    CredentialAcquisition::ManualImport,
                    Some(Timestamp::default()),
                    b"expired-token",
                )]),
                FixtureBehavior::InvalidUtf8 => Ok(vec![resolved_credential(
                    request.provider_account_id,
                    secret_id,
                    SecretPurpose::ProviderAccessToken,
                    SessionKind::ProviderSpecific,
                    CredentialAcquisition::ManualImport,
                    None,
                    &[0xff],
                )]),
                FixtureBehavior::StorageFailure => Err(SecretStoreError::Storage),
            }
        }
    }

    #[tokio::test]
    async fn resolves_only_the_context_bound_manual_token() {
        let credentials = Arc::new(FixtureResolver {
            behavior: FixtureBehavior::Valid,
            request: Mutex::new(None),
            calls: AtomicUsize::new(0),
        });
        let resolver = StoredCidarenSessionResolver::new(credentials.clone());
        let context = provider_context();
        let session = resolver.resolve_session(&context).await.unwrap();
        assert_eq!(session.expose_token(), "synthetic-user-token");
        assert!(!format!("{resolver:?}").contains("synthetic"));

        let request = credentials.request.lock().unwrap().clone().unwrap();
        assert_eq!(request.provider_account_id, context.account_id);
        assert_eq!(request.credential_refs, context.credential_refs);
        assert_eq!(request.purposes, [SecretPurpose::ProviderAccessToken]);
        assert_eq!(request.correlation_id, context.correlation_id);
    }

    #[tokio::test]
    async fn rejects_unbound_expired_or_malformed_credentials() {
        for behavior in [
            FixtureBehavior::Duplicate,
            FixtureBehavior::ForeignAccount,
            FixtureBehavior::UnrequestedSecret,
            FixtureBehavior::WrongPurpose,
            FixtureBehavior::WrongKind,
            FixtureBehavior::WrongOrigin,
            FixtureBehavior::Expired,
            FixtureBehavior::InvalidUtf8,
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
        let credentials = Arc::new(FixtureResolver {
            behavior: FixtureBehavior::Valid,
            request: Mutex::new(None),
            calls: AtomicUsize::new(0),
        });
        let resolver = StoredCidarenSessionResolver::new(credentials.clone());
        let mut context = provider_context();
        context.provider_id = ProviderId::new("other").unwrap();
        assert_eq!(
            resolver.resolve_session(&context).await.unwrap_err().kind,
            ProviderErrorKind::Authentication
        );
        assert_eq!(credentials.calls.load(Ordering::SeqCst), 0);

        assert_eq!(
            fixture_resolver(FixtureBehavior::StorageFailure)
                .resolve_session(&provider_context())
                .await
                .unwrap_err()
                .kind,
            ProviderErrorKind::Internal
        );
    }

    fn fixture_resolver(behavior: FixtureBehavior) -> StoredCidarenSessionResolver {
        StoredCidarenSessionResolver::new(Arc::new(FixtureResolver {
            behavior,
            request: Mutex::new(None),
            calls: AtomicUsize::new(0),
        }))
    }

    #[allow(clippy::too_many_arguments)]
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
            correlation_id: "cidaren-stored-session-test".to_owned(),
        }
    }
}
