use std::{fmt, sync::Arc};

use asterism_domain::SessionKind;
use asterism_provider_api::{ProviderContext, ProviderError, ProviderErrorKind, ProviderResult};
use asterism_secrets::{
    ProviderCredentialResolution, ProviderCredentialResolver, SecretPurpose, SecretStoreError,
};
use async_trait::async_trait;
use chrono::Utc;

use crate::{ChaoxingCookieSession, ChaoxingSessionResolver, metadata::PROVIDER_ID};

/// Chaoxing session adapter backed by Core's provider-scoped credential
/// resolver. It receives plaintext only for the duration of one operation and
/// never queries persistence directly.
pub struct StoredChaoxingSessionResolver {
    credentials: Arc<dyn ProviderCredentialResolver>,
}

impl StoredChaoxingSessionResolver {
    pub const fn new(credentials: Arc<dyn ProviderCredentialResolver>) -> Self {
        Self { credentials }
    }
}

impl fmt::Debug for StoredChaoxingSessionResolver {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StoredChaoxingSessionResolver")
            .field("credentials", &"configured")
            .finish()
    }
}

#[async_trait]
impl ChaoxingSessionResolver for StoredChaoxingSessionResolver {
    async fn resolve_session(
        &self,
        context: &ProviderContext,
    ) -> ProviderResult<ChaoxingCookieSession> {
        if context.provider_id.as_str() != PROVIDER_ID || context.credential_refs.is_empty() {
            return Err(invalid_stored_session());
        }
        let credentials = self
            .credentials
            .resolve_provider_credentials(ProviderCredentialResolution {
                provider_account_id: context.account_id,
                credential_refs: context.credential_refs.clone(),
                purposes: vec![SecretPurpose::ProviderCookie],
                correlation_id: context.correlation_id.clone(),
            })
            .await
            .map_err(|error| map_resolution_error(&error))?;
        let mut cookies = credentials.into_iter().filter(|resolved| {
            resolved.credential.provider_account_id == context.account_id
                && resolved.credential.secret.purpose == SecretPurpose::ProviderCookie
                && matches!(
                    resolved.credential.session_kind,
                    SessionKind::Cookie | SessionKind::Composite
                )
        });
        let cookie = cookies.next().ok_or_else(invalid_stored_session)?;
        if cookies.next().is_some() || cookie.credential.is_expired_at(Utc::now()) {
            return Err(invalid_stored_session());
        }
        let cookie_text = std::str::from_utf8(cookie.value.expose_secret())
            .map_err(|_| invalid_stored_session())?;
        ChaoxingCookieSession::try_new(cookie_text.to_owned())
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
    ProviderError::new(kind, "Chaoxing stored session could not be resolved")
}

fn invalid_stored_session() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::Authentication,
        "Chaoxing account has no usable stored Cookie session",
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
        MissingCookie,
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
            let provider_account_id = request.provider_account_id;
            *self.request.lock().unwrap() = Some(request);
            match self.behavior {
                FixtureBehavior::Cookie => Ok(vec![resolved_credential(
                    provider_account_id,
                    SecretPurpose::ProviderCookie,
                    b"_uid=SAFE_UID; uf=SAFE_UF",
                )]),
                FixtureBehavior::MissingCookie => Ok(vec![resolved_credential(
                    provider_account_id,
                    SecretPurpose::ProviderPassword,
                    b"private-password",
                )]),
                FixtureBehavior::StorageFailure => Err(SecretStoreError::Storage),
            }
        }
    }

    #[tokio::test]
    async fn resolves_only_the_context_bound_cookie() {
        let credentials = Arc::new(FixtureCredentialResolver {
            behavior: FixtureBehavior::Cookie,
            request: Mutex::new(None),
        });
        let resolver = StoredChaoxingSessionResolver::new(credentials.clone());
        let context = provider_context();
        let session = resolver.resolve_session(&context).await.unwrap();
        assert_eq!(session.expose_secret(), "_uid=SAFE_UID; uf=SAFE_UF");
        assert!(!format!("{resolver:?}").contains("SAFE_UID"));
        let request = credentials.request.lock().unwrap().clone().unwrap();
        assert_eq!(request.provider_account_id, context.account_id);
        assert_eq!(request.credential_refs, context.credential_refs);
        assert_eq!(request.purposes, [SecretPurpose::ProviderCookie]);
        assert_eq!(request.correlation_id, context.correlation_id);
    }

    #[tokio::test]
    async fn rejects_missing_cookie_wrong_provider_and_storage_failure() {
        let missing = StoredChaoxingSessionResolver::new(Arc::new(FixtureCredentialResolver {
            behavior: FixtureBehavior::MissingCookie,
            request: Mutex::new(None),
        }));
        assert_eq!(
            missing
                .resolve_session(&provider_context())
                .await
                .unwrap_err()
                .kind,
            ProviderErrorKind::Authentication
        );

        let storage = StoredChaoxingSessionResolver::new(Arc::new(FixtureCredentialResolver {
            behavior: FixtureBehavior::StorageFailure,
            request: Mutex::new(None),
        }));
        assert_eq!(
            storage
                .resolve_session(&provider_context())
                .await
                .unwrap_err()
                .kind,
            ProviderErrorKind::Internal
        );

        let mut wrong_provider = provider_context();
        wrong_provider.provider_id = ProviderId::new("welearn").unwrap();
        assert_eq!(
            missing
                .resolve_session(&wrong_provider)
                .await
                .unwrap_err()
                .kind,
            ProviderErrorKind::Authentication
        );
    }

    fn resolved_credential(
        provider_account_id: ProviderAccountId,
        purpose: SecretPurpose,
        value: &[u8],
    ) -> ResolvedProviderCredential {
        let now = Timestamp::default();
        ResolvedProviderCredential {
            credential: ProviderCredential {
                provider_account_id,
                secret: SecretRef {
                    id: SecretId::new(),
                    owner_user_id: UserId::new(),
                    purpose,
                    version: 1,
                    key_id: "key-a".to_owned(),
                    created_at: now,
                    updated_at: now,
                },
                session_kind: SessionKind::Composite,
                acquired_via: CredentialAcquisition::NativeProviderLogin,
                captured_at: now,
                expires_at: None,
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
            correlation_id: "chaoxing-stored-session-test".to_owned(),
        }
    }
}
