use std::{fmt, sync::Arc};

use asterism_provider_api::{ProviderContext, ProviderError, ProviderErrorKind, ProviderResult};
use asterism_secrets::{
    ProviderCredentialResolution, ProviderCredentialResolver, SecretPurpose, SecretStoreError,
};
use async_trait::async_trait;
use chrono::Utc;

use crate::{WellearnCookieSession, WellearnSessionResolver, metadata::PROVIDER_ID};

/// `WELearn` session adapter backed by Core's provider-scoped credential
/// resolver. Plaintext exists only for the duration of one bounded operation.
pub struct StoredWellearnSessionResolver {
    credentials: Arc<dyn ProviderCredentialResolver>,
}

impl StoredWellearnSessionResolver {
    pub fn new(credentials: Arc<dyn ProviderCredentialResolver>) -> Self {
        Self { credentials }
    }
}

impl fmt::Debug for StoredWellearnSessionResolver {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StoredWellearnSessionResolver")
            .field("credentials", &"configured")
            .finish()
    }
}

#[async_trait]
impl WellearnSessionResolver for StoredWellearnSessionResolver {
    async fn resolve_session(
        &self,
        context: &ProviderContext,
    ) -> ProviderResult<WellearnCookieSession> {
        if context.provider_id.as_str() != PROVIDER_ID || context.credential_refs.is_empty() {
            return Err(invalid_stored_session());
        }
        let mut credentials = self
            .credentials
            .resolve_provider_credentials(ProviderCredentialResolution {
                provider_account_id: context.account_id,
                credential_refs: context.credential_refs.clone(),
                purposes: vec![SecretPurpose::ProviderCookie],
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
            || metadata.secret.purpose != SecretPurpose::ProviderCookie
            || !context.credential_refs.contains(&metadata.secret.id)
            || !matches!(
                metadata.session_kind,
                asterism_domain::SessionKind::Cookie | asterism_domain::SessionKind::Composite
            )
            || metadata.is_expired_at(Utc::now())
        {
            return Err(invalid_stored_session());
        }
        let cookie = std::str::from_utf8(resolved.value.expose_secret())
            .map_err(|_| invalid_stored_session())?;
        WellearnCookieSession::try_new(cookie.to_owned())
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
    ProviderError::new(kind, "WELearn stored session could not be resolved")
}

fn invalid_stored_session() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::Authentication,
        "WELearn account has no usable stored Cookie session",
    )
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use asterism_domain::{
        ProviderAccountId, ProviderId, SecretId, SessionKind, Timestamp, UserId,
    };
    use asterism_secrets::{
        CredentialAcquisition, ProviderCredential, ResolvedProviderCredential, SecretRef,
        SecretValue,
    };

    use super::*;

    #[derive(Clone, Copy, Debug)]
    enum FixtureBehavior {
        Cookie,
        Composite,
        MissingCookie,
        DuplicateCookie,
        ForeignAccount,
        UnrequestedSecret,
        ExpiredCookie,
        InvalidUtf8,
        StorageFailure,
    }

    #[derive(Debug)]
    struct FixtureCredentialResolver {
        behavior: FixtureBehavior,
        request: Mutex<Option<ProviderCredentialResolution>>,
    }

    #[async_trait]
    impl ProviderCredentialResolver for FixtureCredentialResolver {
        async fn resolve_provider_credentials(
            &self,
            request: ProviderCredentialResolution,
        ) -> Result<Vec<ResolvedProviderCredential>, SecretStoreError> {
            *self.request.lock().unwrap() = Some(request.clone());
            let secret_id = request.credential_refs[0];
            let valid = || {
                resolved_credential(
                    request.provider_account_id,
                    secret_id,
                    SecretPurpose::ProviderCookie,
                    SessionKind::Cookie,
                    None,
                    b"session=SAFE_SESSION",
                )
            };
            match self.behavior {
                FixtureBehavior::Cookie => Ok(vec![valid()]),
                FixtureBehavior::Composite => Ok(vec![resolved_credential(
                    request.provider_account_id,
                    secret_id,
                    SecretPurpose::ProviderCookie,
                    SessionKind::Composite,
                    None,
                    b"session=SAFE_COMPOSITE",
                )]),
                FixtureBehavior::MissingCookie => Ok(vec![resolved_credential(
                    request.provider_account_id,
                    secret_id,
                    SecretPurpose::ProviderPassword,
                    SessionKind::Composite,
                    None,
                    b"private-password",
                )]),
                FixtureBehavior::DuplicateCookie => Ok(vec![valid(), valid()]),
                FixtureBehavior::ForeignAccount => Ok(vec![resolved_credential(
                    ProviderAccountId::new(),
                    secret_id,
                    SecretPurpose::ProviderCookie,
                    SessionKind::Cookie,
                    None,
                    b"session=FOREIGN",
                )]),
                FixtureBehavior::UnrequestedSecret => Ok(vec![resolved_credential(
                    request.provider_account_id,
                    SecretId::new(),
                    SecretPurpose::ProviderCookie,
                    SessionKind::Cookie,
                    None,
                    b"session=UNREQUESTED",
                )]),
                FixtureBehavior::ExpiredCookie => Ok(vec![resolved_credential(
                    request.provider_account_id,
                    secret_id,
                    SecretPurpose::ProviderCookie,
                    SessionKind::Cookie,
                    Some(Timestamp::default()),
                    b"session=EXPIRED",
                )]),
                FixtureBehavior::InvalidUtf8 => Ok(vec![resolved_credential(
                    request.provider_account_id,
                    secret_id,
                    SecretPurpose::ProviderCookie,
                    SessionKind::Cookie,
                    None,
                    &[0xff],
                )]),
                FixtureBehavior::StorageFailure => Err(SecretStoreError::Storage),
            }
        }
    }

    #[tokio::test]
    async fn resolves_only_the_context_bound_cookie() {
        for (behavior, expected) in [
            (FixtureBehavior::Cookie, "session=SAFE_SESSION"),
            (FixtureBehavior::Composite, "session=SAFE_COMPOSITE"),
        ] {
            let credentials = Arc::new(FixtureCredentialResolver {
                behavior,
                request: Mutex::new(None),
            });
            let resolver = StoredWellearnSessionResolver::new(credentials.clone());
            let context = provider_context();
            let session = resolver.resolve_session(&context).await.unwrap();
            assert_eq!(session.expose_secret(), expected);
            assert!(!format!("{resolver:?}").contains("SAFE"));
            let request = credentials.request.lock().unwrap().clone().unwrap();
            assert_eq!(request.provider_account_id, context.account_id);
            assert_eq!(request.credential_refs, context.credential_refs);
            assert_eq!(request.purposes, [SecretPurpose::ProviderCookie]);
            assert_eq!(request.correlation_id, context.correlation_id);
        }
    }

    #[tokio::test]
    async fn rejects_unbound_expired_duplicate_or_malformed_credentials() {
        for behavior in [
            FixtureBehavior::MissingCookie,
            FixtureBehavior::DuplicateCookie,
            FixtureBehavior::ForeignAccount,
            FixtureBehavior::UnrequestedSecret,
            FixtureBehavior::ExpiredCookie,
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
    async fn rejects_wrong_context_before_storage_and_maps_storage_failures() {
        let credentials = Arc::new(FixtureCredentialResolver {
            behavior: FixtureBehavior::Cookie,
            request: Mutex::new(None),
        });
        let resolver = StoredWellearnSessionResolver::new(credentials.clone());
        let mut wrong_provider = provider_context();
        wrong_provider.provider_id = ProviderId::new("chaoxing").unwrap();
        assert_eq!(
            resolver
                .resolve_session(&wrong_provider)
                .await
                .unwrap_err()
                .kind,
            ProviderErrorKind::Authentication
        );
        assert!(credentials.request.lock().unwrap().is_none());

        assert_eq!(
            fixture_resolver(FixtureBehavior::StorageFailure)
                .resolve_session(&provider_context())
                .await
                .unwrap_err()
                .kind,
            ProviderErrorKind::Internal
        );
    }

    fn fixture_resolver(behavior: FixtureBehavior) -> StoredWellearnSessionResolver {
        StoredWellearnSessionResolver::new(Arc::new(FixtureCredentialResolver {
            behavior,
            request: Mutex::new(None),
        }))
    }

    fn resolved_credential(
        provider_account_id: ProviderAccountId,
        secret_id: SecretId,
        purpose: SecretPurpose,
        session_kind: SessionKind,
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
                acquired_via: CredentialAcquisition::ManualImport,
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
            correlation_id: "welearn-stored-session-test".to_owned(),
        }
    }
}
