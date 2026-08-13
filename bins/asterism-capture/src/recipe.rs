use std::{collections::BTreeMap, fmt};

use anyhow::{Context, bail};
use asterism_provider_api::{CaptureRecipe, CaptureScalarSource, CaptureValueSource};
use asterism_secrets::{SecretPurpose, SecretString};
use serde::ser::{SerializeMap, Serializer};
use zeroize::Zeroizing;

use crate::CaptureCredentialField;

const MAX_BROWSER_BINDING_BYTES: usize = 256;
const MAX_CAPTURED_VALUE_BYTES: usize = 1024 * 1024;

/// One stable browser-document observation used to resolve exactly one
/// Provider recipe. Values from different observations cannot be combined.
pub struct CaptureSnapshot {
    recipe: CaptureRecipe,
    target_id: String,
    document_id: String,
    request_headers: BTreeMap<(String, String), SecretString>,
    local_storage: BTreeMap<(String, String), SecretString>,
    session_storage: BTreeMap<(String, String), SecretString>,
    cookie_headers: BTreeMap<String, SecretString>,
}

impl CaptureSnapshot {
    /// Starts one recipe-bound observation for a stable browser target and
    /// document. Browser adapters must discard the snapshot if either binding
    /// changes before acquisition finishes.
    ///
    /// # Errors
    ///
    /// Rejects an invalid recipe or unsafe/empty browser bindings.
    pub fn new(
        recipe: CaptureRecipe,
        target_id: impl Into<String>,
        document_id: impl Into<String>,
    ) -> anyhow::Result<Self> {
        recipe.validate().context("Capture recipe is invalid")?;
        let target_id = target_id.into();
        let document_id = document_id.into();
        if !valid_binding(&target_id) || !valid_binding(&document_id) {
            bail!("browser snapshot binding is invalid");
        }
        Ok(Self {
            recipe,
            target_id,
            document_id,
            request_headers: BTreeMap::new(),
            local_storage: BTreeMap::new(),
            session_storage: BTreeMap::new(),
            cookie_headers: BTreeMap::new(),
        })
    }

    pub fn recipe(&self) -> &CaptureRecipe {
        &self.recipe
    }

    pub fn target_id(&self) -> &str {
        &self.target_id
    }

    pub fn document_id(&self) -> &str {
        &self.document_id
    }

    /// Records an exact request header only when the origin/name pair is
    /// declared by the bound recipe.
    ///
    /// # Errors
    ///
    /// Rejects undeclared facts, empty/oversized values and duplicate facts.
    pub fn insert_request_header(
        &mut self,
        origin: &str,
        name: &str,
        value: SecretString,
    ) -> anyhow::Result<()> {
        let normalized_name = name.to_ascii_lowercase();
        if !self.recipe_declares_header(origin, &normalized_name) {
            bail!("browser snapshot contains an undeclared request header");
        }
        insert_secret(
            &mut self.request_headers,
            (origin.to_owned(), normalized_name),
            value,
        )
    }

    /// Records one exact local-storage value declared by the recipe.
    ///
    /// # Errors
    ///
    /// Rejects undeclared facts, empty/oversized values and duplicate facts.
    pub fn insert_local_storage(
        &mut self,
        origin: &str,
        key: &str,
        value: SecretString,
    ) -> anyhow::Result<()> {
        if !self.recipe_declares_storage(origin, key, true) {
            bail!("browser snapshot contains undeclared local storage");
        }
        insert_secret(
            &mut self.local_storage,
            (origin.to_owned(), key.to_owned()),
            value,
        )
    }

    /// Records one exact session-storage value declared by the recipe.
    ///
    /// # Errors
    ///
    /// Rejects undeclared facts, empty/oversized values and duplicate facts.
    pub fn insert_session_storage(
        &mut self,
        origin: &str,
        key: &str,
        value: SecretString,
    ) -> anyhow::Result<()> {
        if !self.recipe_declares_storage(origin, key, false) {
            bail!("browser snapshot contains undeclared session storage");
        }
        insert_secret(
            &mut self.session_storage,
            (origin.to_owned(), key.to_owned()),
            value,
        )
    }

    /// Records the canonical Cookie request-header value for a declared
    /// recipe origin.
    ///
    /// # Errors
    ///
    /// Rejects undeclared origins, empty/oversized values and duplicate facts.
    pub fn insert_cookie_header(
        &mut self,
        origin: &str,
        value: SecretString,
    ) -> anyhow::Result<()> {
        if !self.recipe.outputs.iter().any(|output| {
            output.sources.iter().any(|source| {
                matches!(source, CaptureValueSource::CookieHeader { origin: expected } if expected == origin)
            })
        }) {
            bail!("browser snapshot contains an undeclared Cookie origin");
        }
        insert_secret(&mut self.cookie_headers, origin.to_owned(), value)
    }

    /// Resolves ordered source alternatives without combining another browser
    /// observation. Optional missing outputs are omitted; required missing
    /// outputs are reported without exposing values.
    ///
    /// # Errors
    ///
    /// Returns an error if a resolved field cannot satisfy the bounded Core
    /// credential contract.
    pub fn resolve(&self) -> anyhow::Result<CaptureResolution> {
        let mut fields = Vec::with_capacity(self.recipe.outputs.len());
        let mut missing = Vec::new();
        for output in &self.recipe.outputs {
            let value = output
                .sources
                .iter()
                .find_map(|source| self.resolve_source(source));
            match value {
                Some(value) => fields.push(CaptureCredentialField::new(output.purpose, value)?),
                None if output.required => missing.push(output.purpose),
                None => {}
            }
        }
        if missing.is_empty() {
            Ok(CaptureResolution::Ready(fields))
        } else {
            Ok(CaptureResolution::Incomplete { missing })
        }
    }

    fn resolve_source(&self, source: &CaptureValueSource) -> Option<SecretString> {
        match source {
            CaptureValueSource::RequestHeader { origin, name } => self
                .request_headers
                .get(&(origin.clone(), name.to_ascii_lowercase()))
                .map(clone_secret),
            CaptureValueSource::LocalStorage { origin, key } => self
                .local_storage
                .get(&(origin.clone(), key.clone()))
                .map(clone_secret),
            CaptureValueSource::SessionStorage { origin, key } => self
                .session_storage
                .get(&(origin.clone(), key.clone()))
                .map(clone_secret),
            CaptureValueSource::CookieHeader { origin } => {
                self.cookie_headers.get(origin).map(clone_secret)
            }
            CaptureValueSource::JsonObject { fields } => {
                let resolved = fields
                    .iter()
                    .map(|field| {
                        let value = field
                            .sources
                            .iter()
                            .find_map(|source| self.resolve_scalar(source))?;
                        Some((field.name.as_str(), value))
                    })
                    .collect::<Option<Vec<_>>>()?;
                serialize_secret_object(&resolved).ok()
            }
        }
    }

    fn resolve_scalar(&self, source: &CaptureScalarSource) -> Option<SecretString> {
        match source {
            CaptureScalarSource::RequestHeader { origin, name } => self
                .request_headers
                .get(&(origin.clone(), name.to_ascii_lowercase()))
                .map(clone_secret),
            CaptureScalarSource::LocalStorage { origin, key } => self
                .local_storage
                .get(&(origin.clone(), key.clone()))
                .map(clone_secret),
            CaptureScalarSource::SessionStorage { origin, key } => self
                .session_storage
                .get(&(origin.clone(), key.clone()))
                .map(clone_secret),
        }
    }

    fn recipe_declares_header(&self, origin: &str, normalized_name: &str) -> bool {
        self.recipe.outputs.iter().any(|output| {
            output.sources.iter().any(|source| match source {
                CaptureValueSource::RequestHeader {
                    origin: expected_origin,
                    name,
                } => expected_origin == origin && name.eq_ignore_ascii_case(normalized_name),
                CaptureValueSource::JsonObject { fields } => fields.iter().any(|field| {
                    field.sources.iter().any(|source| {
                        matches!(
                            source,
                            CaptureScalarSource::RequestHeader { origin: expected_origin, name }
                                if expected_origin == origin
                                    && name.eq_ignore_ascii_case(normalized_name)
                        )
                    })
                }),
                CaptureValueSource::LocalStorage { .. }
                | CaptureValueSource::SessionStorage { .. }
                | CaptureValueSource::CookieHeader { .. } => false,
            })
        })
    }

    fn recipe_declares_storage(&self, origin: &str, key: &str, local: bool) -> bool {
        self.recipe.outputs.iter().any(|output| {
            output.sources.iter().any(|source| {
                source_matches_storage(source, origin, key, local)
                    || matches!(source, CaptureValueSource::JsonObject { fields } if fields.iter().any(|field| field.sources.iter().any(|source| scalar_matches_storage(source, origin, key, local))))
            })
        })
    }
}

impl fmt::Debug for CaptureSnapshot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CaptureSnapshot")
            .field("recipe", &self.recipe)
            .field("target_id", &self.target_id)
            .field("document_id", &self.document_id)
            .field("request_header_count", &self.request_headers.len())
            .field("local_storage_count", &self.local_storage.len())
            .field("session_storage_count", &self.session_storage.len())
            .field("cookie_header_count", &self.cookie_headers.len())
            .finish()
    }
}

#[derive(Debug)]
pub enum CaptureResolution {
    Incomplete { missing: Vec<SecretPurpose> },
    Ready(Vec<CaptureCredentialField>),
}

fn insert_secret<K: Ord>(
    values: &mut BTreeMap<K, SecretString>,
    key: K,
    value: SecretString,
) -> anyhow::Result<()> {
    if value.expose_secret().is_empty() || value.expose_secret().len() > MAX_CAPTURED_VALUE_BYTES {
        bail!("captured browser value is empty or oversized");
    }
    match values.entry(key) {
        std::collections::btree_map::Entry::Vacant(entry) => {
            entry.insert(value);
            Ok(())
        }
        std::collections::btree_map::Entry::Occupied(_) => {
            bail!("browser snapshot contains a duplicate fact")
        }
    }
}

fn clone_secret(value: &SecretString) -> SecretString {
    SecretString::new(value.expose_secret().to_owned())
}

fn serialize_secret_object(fields: &[(&str, SecretString)]) -> anyhow::Result<SecretString> {
    struct SecretObject<'a>(&'a [(&'a str, SecretString)]);

    impl serde::Serialize for SecretObject<'_> {
        fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
        where
            S: Serializer,
        {
            let mut map = serializer.serialize_map(Some(self.0.len()))?;
            for (name, value) in self.0 {
                map.serialize_entry(name, value.expose_secret())?;
            }
            map.end()
        }
    }

    let mut encoded = Zeroizing::new(Vec::new());
    serde_json::to_writer(&mut *encoded, &SecretObject(fields))
        .context("failed to encode captured composite session")?;
    let bytes = std::mem::take(&mut *encoded);
    let value = String::from_utf8(bytes).context("captured composite session is not UTF-8")?;
    Ok(SecretString::new(value))
}

fn source_matches_storage(
    source: &CaptureValueSource,
    origin: &str,
    key: &str,
    local: bool,
) -> bool {
    match source {
        CaptureValueSource::LocalStorage {
            origin: expected_origin,
            key: expected_key,
        } => local && expected_origin == origin && expected_key == key,
        CaptureValueSource::SessionStorage {
            origin: expected_origin,
            key: expected_key,
        } => !local && expected_origin == origin && expected_key == key,
        CaptureValueSource::RequestHeader { .. }
        | CaptureValueSource::CookieHeader { .. }
        | CaptureValueSource::JsonObject { .. } => false,
    }
}

fn scalar_matches_storage(
    source: &CaptureScalarSource,
    origin: &str,
    key: &str,
    local: bool,
) -> bool {
    match source {
        CaptureScalarSource::LocalStorage {
            origin: expected_origin,
            key: expected_key,
        } => local && expected_origin == origin && expected_key == key,
        CaptureScalarSource::SessionStorage {
            origin: expected_origin,
            key: expected_key,
        } => !local && expected_origin == origin && expected_key == key,
        CaptureScalarSource::RequestHeader { .. } => false,
    }
}

fn valid_binding(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_BROWSER_BINDING_BYTES
        && value.trim() == value
        && !value.chars().any(char::is_control)
}

#[cfg(test)]
mod tests {
    use asterism_domain::{AuthMethod, SessionKind};
    use asterism_provider_api::{CaptureCredentialOutput, CaptureJsonField, CaptureScalarSource};

    use super::*;

    const ORIGIN: &str = "https://provider.example";

    fn recipe() -> CaptureRecipe {
        CaptureRecipe {
            version: 1,
            start_url: format!("{ORIGIN}/login"),
            allowed_origins: vec![ORIGIN.to_owned()],
            poll_interval_millis: 500,
            auth_method: AuthMethod::AssistedSession,
            session_kind: SessionKind::Composite,
            outputs: vec![CaptureCredentialOutput {
                purpose: SecretPurpose::ProviderCompositeSession,
                required: true,
                sources: vec![CaptureValueSource::JsonObject {
                    fields: vec![
                        CaptureJsonField {
                            name: "openid".to_owned(),
                            sources: vec![CaptureScalarSource::RequestHeader {
                                origin: ORIGIN.to_owned(),
                                name: "u-openid".to_owned(),
                            }],
                        },
                        CaptureJsonField {
                            name: "jwt".to_owned(),
                            sources: vec![CaptureScalarSource::RequestHeader {
                                origin: ORIGIN.to_owned(),
                                name: "Authorization".to_owned(),
                            }],
                        },
                    ],
                }],
            }],
        }
    }

    #[test]
    fn one_snapshot_resolves_ordered_atomic_composite_without_debug_leakage() {
        let mut snapshot = CaptureSnapshot::new(recipe(), "target-1", "document-1").unwrap();
        snapshot
            .insert_request_header(ORIGIN, "U-OpenId", SecretString::new("openid-value"))
            .unwrap();
        assert!(matches!(
            snapshot.resolve().unwrap(),
            CaptureResolution::Incomplete { .. }
        ));
        snapshot
            .insert_request_header(ORIGIN, "Authorization", SecretString::new("jwt-value"))
            .unwrap();
        let CaptureResolution::Ready(fields) = snapshot.resolve().unwrap() else {
            panic!("complete same-snapshot fields must resolve");
        };
        assert_eq!(fields.len(), 1);
        assert_eq!(fields[0].purpose(), SecretPurpose::ProviderCompositeSession);
        assert!(!format!("{snapshot:?}").contains("jwt-value"));
        assert!(!format!("{fields:?}").contains("jwt-value"));
    }

    #[test]
    fn undeclared_or_duplicate_browser_facts_fail_closed() {
        let mut snapshot = CaptureSnapshot::new(recipe(), "target-1", "document-1").unwrap();
        assert!(
            snapshot
                .insert_request_header(
                    "https://foreign.example",
                    "Authorization",
                    SecretString::new("foreign"),
                )
                .is_err()
        );
        snapshot
            .insert_request_header(ORIGIN, "u-openid", SecretString::new("first"))
            .unwrap();
        assert!(
            snapshot
                .insert_request_header(ORIGIN, "U-OPENID", SecretString::new("second"))
                .is_err()
        );
    }
}
