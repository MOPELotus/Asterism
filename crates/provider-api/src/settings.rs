use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

const MAX_SETTING_DEFINITIONS: usize = 128;
const MAX_SETTING_KEY_BYTES: usize = 80;
const MAX_SETTING_LABEL_BYTES: usize = 120;
const MAX_SETTING_DESCRIPTION_BYTES: usize = 400;
const MAX_CHOICE_OPTIONS: usize = 64;
const MAX_CHOICE_BYTES: usize = 120;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderSettingScope {
    Provider,
    ProviderAccount,
    Task,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum ProviderSettingValue {
    Boolean(bool),
    Integer(i64),
    DecimalMillis(i64),
    DurationSeconds(u64),
    Choice(String),
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ProviderSettingKind {
    Boolean,
    Integer {
        minimum: i64,
        maximum: i64,
        step: u64,
    },
    DecimalMillis {
        minimum: i64,
        maximum: i64,
        step: u64,
    },
    DurationSeconds {
        minimum: u64,
        maximum: u64,
        step: u64,
    },
    Choice {
        options: BTreeSet<String>,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProviderSettingDefinition {
    pub key: String,
    pub display_name: String,
    pub description: String,
    pub kind: ProviderSettingKind,
    pub default: ProviderSettingValue,
    pub scopes: BTreeSet<ProviderSettingScope>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProviderRuntimeSettingsSchema {
    pub version: u32,
    pub definitions: Vec<ProviderSettingDefinition>,
}

impl Default for ProviderRuntimeSettingsSchema {
    fn default() -> Self {
        Self::empty()
    }
}

impl ProviderRuntimeSettingsSchema {
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            version: 1,
            definitions: Vec::new(),
        }
    }

    /// Validates the complete Provider-authored schema before registration.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderSettingsError`] when the schema is unversioned,
    /// ambiguous, unbounded or has an invalid default.
    pub fn validate(&self) -> Result<(), ProviderSettingsError> {
        if self.version == 0 {
            return Err(ProviderSettingsError::InvalidSchemaVersion);
        }
        if self.definitions.len() > MAX_SETTING_DEFINITIONS {
            return Err(ProviderSettingsError::TooManyDefinitions);
        }

        let mut keys = BTreeSet::new();
        for definition in &self.definitions {
            if !valid_key(&definition.key)
                || !bounded_text(&definition.display_name, MAX_SETTING_LABEL_BYTES)
                || !bounded_text(&definition.description, MAX_SETTING_DESCRIPTION_BYTES)
                || definition.scopes.is_empty()
                || !definition.scopes.contains(&ProviderSettingScope::Provider)
            {
                return Err(ProviderSettingsError::InvalidDefinition {
                    key: definition.key.clone(),
                });
            }
            if !keys.insert(definition.key.clone()) {
                return Err(ProviderSettingsError::DuplicateDefinition {
                    key: definition.key.clone(),
                });
            }
            definition.kind.validate(&definition.key)?;
            definition
                .kind
                .validate_value(&definition.key, &definition.default)?;
        }
        Ok(())
    }

    /// Validates one partial override at its explicit Core-owned scope.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderSettingsError`] for a schema revision mismatch,
    /// unknown key, unsupported scope or out-of-range value.
    pub fn validate_patch(
        &self,
        scope: ProviderSettingScope,
        patch: &ProviderRuntimeSettingsPatch,
    ) -> Result<(), ProviderSettingsError> {
        self.validate()?;
        if patch.schema_version != self.version {
            return Err(ProviderSettingsError::SchemaVersionMismatch);
        }
        if patch.values.len() > self.definitions.len() {
            return Err(ProviderSettingsError::TooManyValues);
        }

        for (key, value) in &patch.values {
            let definition = self
                .definitions
                .iter()
                .find(|definition| definition.key == *key)
                .ok_or_else(|| ProviderSettingsError::UnknownSetting { key: key.clone() })?;
            if !definition.scopes.contains(&scope) {
                return Err(ProviderSettingsError::UnsupportedScope {
                    key: key.clone(),
                    scope,
                });
            }
            definition.kind.validate_value(key, value)?;
        }
        Ok(())
    }

    /// Resolves defaults and increasingly specific Master-owned overrides.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderSettingsError`] when any supplied override does not
    /// match this schema and its declared scope.
    pub fn resolve(
        &self,
        provider: Option<&ProviderRuntimeSettingsPatch>,
        account: Option<&ProviderRuntimeSettingsPatch>,
        task: Option<&ProviderRuntimeSettingsPatch>,
    ) -> Result<ResolvedProviderRuntimeSettings, ProviderSettingsError> {
        self.validate()?;
        let mut values = self
            .definitions
            .iter()
            .map(|definition| (definition.key.clone(), definition.default.clone()))
            .collect::<BTreeMap<_, _>>();

        for (scope, patch) in [
            (ProviderSettingScope::Provider, provider),
            (ProviderSettingScope::ProviderAccount, account),
            (ProviderSettingScope::Task, task),
        ] {
            if let Some(patch) = patch {
                self.validate_patch(scope, patch)?;
                values.extend(patch.values.clone());
            }
        }

        Ok(ResolvedProviderRuntimeSettings {
            schema_version: self.version,
            values,
        })
    }
}

impl ProviderSettingKind {
    fn validate(&self, key: &str) -> Result<(), ProviderSettingsError> {
        match self {
            Self::Boolean => Ok(()),
            Self::Integer {
                minimum,
                maximum,
                step,
            }
            | Self::DecimalMillis {
                minimum,
                maximum,
                step,
            } if minimum <= maximum && *step > 0 => Ok(()),
            Self::DurationSeconds {
                minimum,
                maximum,
                step,
            } if minimum <= maximum && *step > 0 => Ok(()),
            Self::Choice { options }
                if !options.is_empty()
                    && options.len() <= MAX_CHOICE_OPTIONS
                    && options
                        .iter()
                        .all(|option| bounded_text(option, MAX_CHOICE_BYTES)) =>
            {
                Ok(())
            }
            _ => Err(ProviderSettingsError::InvalidConstraint {
                key: key.to_owned(),
            }),
        }
    }

    fn validate_value(
        &self,
        key: &str,
        value: &ProviderSettingValue,
    ) -> Result<(), ProviderSettingsError> {
        let valid = match (self, value) {
            (Self::Boolean, ProviderSettingValue::Boolean(_)) => true,
            (
                Self::Integer {
                    minimum,
                    maximum,
                    step,
                },
                ProviderSettingValue::Integer(value),
            )
            | (
                Self::DecimalMillis {
                    minimum,
                    maximum,
                    step,
                },
                ProviderSettingValue::DecimalMillis(value),
            ) => integral_step_matches(*value, *minimum, *maximum, *step),
            (
                Self::DurationSeconds {
                    minimum,
                    maximum,
                    step,
                },
                ProviderSettingValue::DurationSeconds(value),
            ) => unsigned_step_matches(*value, *minimum, *maximum, *step),
            (Self::Choice { options }, ProviderSettingValue::Choice(value)) => {
                options.contains(value)
            }
            _ => false,
        };
        if valid {
            Ok(())
        } else {
            Err(ProviderSettingsError::InvalidValue {
                key: key.to_owned(),
            })
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProviderRuntimeSettingsPatch {
    pub schema_version: u32,
    pub values: BTreeMap<String, ProviderSettingValue>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ResolvedProviderRuntimeSettings {
    pub schema_version: u32,
    pub values: BTreeMap<String, ProviderSettingValue>,
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ProviderSettingsError {
    #[error("provider runtime settings schema version must be positive")]
    InvalidSchemaVersion,
    #[error("provider runtime settings schema has too many definitions")]
    TooManyDefinitions,
    #[error("provider runtime settings definition `{key}` is invalid")]
    InvalidDefinition { key: String },
    #[error("provider runtime settings definition `{key}` is duplicated")]
    DuplicateDefinition { key: String },
    #[error("provider runtime settings definition `{key}` has invalid constraints")]
    InvalidConstraint { key: String },
    #[error("provider runtime settings patch uses a different schema version")]
    SchemaVersionMismatch,
    #[error("provider runtime settings patch has too many values")]
    TooManyValues,
    #[error("provider runtime settings key `{key}` is unknown")]
    UnknownSetting { key: String },
    #[error("provider runtime settings key `{key}` does not support scope `{scope:?}`")]
    UnsupportedScope {
        key: String,
        scope: ProviderSettingScope,
    },
    #[error("provider runtime settings value for `{key}` is invalid")]
    InvalidValue { key: String },
}

fn valid_key(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_SETTING_KEY_BYTES
        && value.as_bytes()[0].is_ascii_lowercase()
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-')
        })
}

fn bounded_text(value: &str, maximum_bytes: usize) -> bool {
    !value.is_empty()
        && value.len() <= maximum_bytes
        && value.trim() == value
        && !value.chars().any(char::is_control)
}

fn integral_step_matches(value: i64, minimum: i64, maximum: i64, step: u64) -> bool {
    value >= minimum
        && value <= maximum
        && (i128::from(value) - i128::from(minimum)) % i128::from(step) == 0
}

fn unsigned_step_matches(value: u64, minimum: u64, maximum: u64, step: u64) -> bool {
    value >= minimum && value <= maximum && (value - minimum).is_multiple_of(step)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schema() -> ProviderRuntimeSettingsSchema {
        ProviderRuntimeSettingsSchema {
            version: 3,
            definitions: vec![
                ProviderSettingDefinition {
                    key: "video.max_concurrency".to_owned(),
                    display_name: "Video concurrency".to_owned(),
                    description: "Maximum concurrent video work for this Provider.".to_owned(),
                    kind: ProviderSettingKind::Integer {
                        minimum: 1,
                        maximum: 8,
                        step: 1,
                    },
                    default: ProviderSettingValue::Integer(2),
                    scopes: BTreeSet::from([
                        ProviderSettingScope::Provider,
                        ProviderSettingScope::Task,
                    ]),
                },
                ProviderSettingDefinition {
                    key: "video.playback_rate".to_owned(),
                    display_name: "Playback rate".to_owned(),
                    description: "Fixed-point playback multiplier in thousandths.".to_owned(),
                    kind: ProviderSettingKind::DecimalMillis {
                        minimum: 1_000,
                        maximum: 2_000,
                        step: 250,
                    },
                    default: ProviderSettingValue::DecimalMillis(1_000),
                    scopes: BTreeSet::from([
                        ProviderSettingScope::Provider,
                        ProviderSettingScope::ProviderAccount,
                        ProviderSettingScope::Task,
                    ]),
                },
            ],
        }
    }

    fn patch(
        values: impl IntoIterator<Item = (&'static str, ProviderSettingValue)>,
    ) -> ProviderRuntimeSettingsPatch {
        ProviderRuntimeSettingsPatch {
            schema_version: 3,
            values: values
                .into_iter()
                .map(|(key, value)| (key.to_owned(), value))
                .collect(),
        }
    }

    #[test]
    fn increasingly_specific_overrides_resolve_field_by_field() {
        let schema = schema();
        let provider = patch([
            ("video.max_concurrency", ProviderSettingValue::Integer(4)),
            (
                "video.playback_rate",
                ProviderSettingValue::DecimalMillis(1_250),
            ),
        ]);
        let account = patch([(
            "video.playback_rate",
            ProviderSettingValue::DecimalMillis(1_500),
        )]);
        let task = patch([("video.max_concurrency", ProviderSettingValue::Integer(1))]);

        let resolved = schema
            .resolve(Some(&provider), Some(&account), Some(&task))
            .unwrap();

        assert_eq!(resolved.schema_version, 3);
        assert_eq!(
            resolved.values["video.max_concurrency"],
            ProviderSettingValue::Integer(1)
        );
        assert_eq!(
            resolved.values["video.playback_rate"],
            ProviderSettingValue::DecimalMillis(1_500)
        );
    }

    #[test]
    fn schema_and_patch_validation_fail_closed() {
        let mut invalid = schema();
        invalid.definitions[0].default = ProviderSettingValue::Integer(9);
        assert!(matches!(
            invalid.validate(),
            Err(ProviderSettingsError::InvalidValue { .. })
        ));

        let schema = schema();
        let account = patch([("video.max_concurrency", ProviderSettingValue::Integer(4))]);
        assert!(matches!(
            schema.validate_patch(ProviderSettingScope::ProviderAccount, &account),
            Err(ProviderSettingsError::UnsupportedScope { .. })
        ));

        let wrong_step = patch([(
            "video.playback_rate",
            ProviderSettingValue::DecimalMillis(1_100),
        )]);
        assert!(matches!(
            schema.validate_patch(ProviderSettingScope::Task, &wrong_step),
            Err(ProviderSettingsError::InvalidValue { .. })
        ));

        let unknown = patch([("credential.cookie", ProviderSettingValue::Boolean(true))]);
        assert!(matches!(
            schema.validate_patch(ProviderSettingScope::Provider, &unknown),
            Err(ProviderSettingsError::UnknownSetting { .. })
        ));
    }
}
