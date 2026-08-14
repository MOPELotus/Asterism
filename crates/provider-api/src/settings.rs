use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

const MAX_SETTING_DEFINITIONS: usize = 128;
const MAX_SETTING_KEY_BYTES: usize = 80;
const MAX_SETTING_LABEL_BYTES: usize = 120;
const MAX_SETTING_DESCRIPTION_BYTES: usize = 400;
const MAX_CHOICE_OPTIONS: usize = 64;
const MAX_CHOICE_BYTES: usize = 120;
// The audited WELearn donors expose a bounded 1..=100 per-account worker
// range. Core keeps the value finite and admission-controlled, but must not
// make an evidenced Provider contract unrepresentable.
const MAX_PROVIDER_EXECUTION_CONCURRENCY: i64 = 100;
const MAX_ACCOUNT_EXECUTION_CONCURRENCY: i64 = 100;
const MIN_ACCOUNT_SCAN_INTERVAL_SECONDS: u64 = 60;
const MAX_ACCOUNT_SCAN_INTERVAL_SECONDS: u64 = 7 * 24 * 60 * 60;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderSettingScope {
    Provider,
    ProviderAccount,
    Task,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderRuntimeSettingSource {
    SchemaDefault,
    Provider,
    ProviderAccount,
    Task,
}

/// A portable Core scheduling behavior attached to one Provider-authored
/// setting. Core interprets this enum and never Provider-specific key names.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderSettingCoreBehavior {
    ProviderExecutionConcurrency,
    AccountExecutionConcurrency,
    AccountScanInterval,
    MinimumAnswerCoverage,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProviderExecutionConcurrency {
    pub provider: u32,
    pub account: u32,
}

impl Default for ProviderExecutionConcurrency {
    fn default() -> Self {
        Self {
            provider: 1,
            account: 1,
        }
    }
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub core_behavior: Option<ProviderSettingCoreBehavior>,
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
        let mut core_behaviors = BTreeSet::new();
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
            if let Some(behavior) = definition.core_behavior {
                if !core_behaviors.insert(behavior) {
                    return Err(ProviderSettingsError::DuplicateCoreBehavior { behavior });
                }
                validate_core_behavior(definition, behavior)?;
            }
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
        self.resolve_with_sources(provider, account, task)
            .map(|(resolved, _)| resolved)
    }

    /// Resolves settings together with the winning source of every field.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderSettingsError`] under the same conditions as
    /// [`Self::resolve`].
    pub fn resolve_with_sources(
        &self,
        provider: Option<&ProviderRuntimeSettingsPatch>,
        account: Option<&ProviderRuntimeSettingsPatch>,
        task: Option<&ProviderRuntimeSettingsPatch>,
    ) -> Result<
        (
            ResolvedProviderRuntimeSettings,
            BTreeMap<String, ProviderRuntimeSettingSource>,
        ),
        ProviderSettingsError,
    > {
        self.validate()?;
        let mut values = self
            .definitions
            .iter()
            .map(|definition| (definition.key.clone(), definition.default.clone()))
            .collect::<BTreeMap<_, _>>();
        let mut sources = self
            .definitions
            .iter()
            .map(|definition| {
                (
                    definition.key.clone(),
                    ProviderRuntimeSettingSource::SchemaDefault,
                )
            })
            .collect::<BTreeMap<_, _>>();

        for (scope, source, patch) in [
            (
                ProviderSettingScope::Provider,
                ProviderRuntimeSettingSource::Provider,
                provider,
            ),
            (
                ProviderSettingScope::ProviderAccount,
                ProviderRuntimeSettingSource::ProviderAccount,
                account,
            ),
            (
                ProviderSettingScope::Task,
                ProviderRuntimeSettingSource::Task,
                task,
            ),
        ] {
            if let Some(patch) = patch {
                self.validate_patch(scope, patch)?;
                for (key, value) in &patch.values {
                    values.insert(key.clone(), value.clone());
                    sources.insert(key.clone(), source);
                }
            }
        }

        Ok((
            ResolvedProviderRuntimeSettings {
                schema_version: self.version,
                values,
            },
            sources,
        ))
    }

    /// Validates a complete settings snapshot before Provider execution.
    ///
    /// Unlike an override patch, a resolved snapshot must contain exactly one
    /// value for every definition in this schema.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderSettingsError`] when the snapshot is incomplete,
    /// contains an unknown field, uses another schema version, or has an
    /// out-of-range value.
    pub fn validate_resolved(
        &self,
        resolved: &ResolvedProviderRuntimeSettings,
    ) -> Result<(), ProviderSettingsError> {
        self.validate()?;
        if resolved.schema_version != self.version {
            return Err(ProviderSettingsError::SchemaVersionMismatch);
        }
        for definition in &self.definitions {
            let value = resolved.values.get(&definition.key).ok_or_else(|| {
                ProviderSettingsError::MissingSetting {
                    key: definition.key.clone(),
                }
            })?;
            definition.kind.validate_value(&definition.key, value)?;
        }
        if let Some(key) = resolved.values.keys().find(|key| {
            !self
                .definitions
                .iter()
                .any(|definition| definition.key == **key)
        }) {
            return Err(ProviderSettingsError::UnknownSetting { key: key.clone() });
        }
        Ok(())
    }

    /// Resolves the Core-owned execution admission limits from a validated,
    /// immutable settings snapshot. Schemas without these optional hints keep
    /// the conservative one-at-a-time Provider and account defaults.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderSettingsError`] when the snapshot or a scheduling
    /// hint is incompatible with this schema.
    pub fn execution_concurrency(
        &self,
        resolved: &ResolvedProviderRuntimeSettings,
    ) -> Result<ProviderExecutionConcurrency, ProviderSettingsError> {
        self.validate_resolved(resolved)?;
        let mut limits = ProviderExecutionConcurrency::default();
        for definition in &self.definitions {
            let Some(behavior) = definition.core_behavior else {
                continue;
            };
            if matches!(
                behavior,
                ProviderSettingCoreBehavior::AccountScanInterval
                    | ProviderSettingCoreBehavior::MinimumAnswerCoverage
            ) {
                continue;
            }
            let value = resolved
                .integer(&definition.key)
                .and_then(|value| u32::try_from(value).ok())
                .ok_or_else(|| ProviderSettingsError::InvalidCoreBehavior {
                    key: definition.key.clone(),
                    behavior,
                })?;
            match behavior {
                ProviderSettingCoreBehavior::ProviderExecutionConcurrency => {
                    limits.provider = value;
                }
                ProviderSettingCoreBehavior::AccountExecutionConcurrency => {
                    limits.account = value;
                }
                ProviderSettingCoreBehavior::AccountScanInterval
                | ProviderSettingCoreBehavior::MinimumAnswerCoverage => unreachable!(),
            }
        }
        Ok(limits)
    }

    /// Returns the Provider-defined default scan interval for one account
    /// settings resolution. The Core Scheduler remains the sole owner of the
    /// actual schedule and enable/disable state.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderSettingsError`] when the snapshot or declared hint is
    /// incompatible with this schema.
    pub fn account_scan_interval(
        &self,
        resolved: &ResolvedProviderRuntimeSettings,
    ) -> Result<Option<u64>, ProviderSettingsError> {
        self.validate_resolved(resolved)?;
        self.definitions
            .iter()
            .find(|definition| {
                definition.core_behavior == Some(ProviderSettingCoreBehavior::AccountScanInterval)
            })
            .map(|definition| {
                resolved.duration_seconds(&definition.key).ok_or_else(|| {
                    ProviderSettingsError::InvalidCoreBehavior {
                        key: definition.key.clone(),
                        behavior: ProviderSettingCoreBehavior::AccountScanInterval,
                    }
                })
            })
            .transpose()
    }

    /// Resolves the minimum answered fraction Core must enforce against the
    /// complete immutable Question snapshot. Providers without this optional
    /// behavior retain the conservative 100% requirement.
    ///
    /// The value uses thousandths (`900` = 90%) so no floating-point policy is
    /// persisted or compared.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderSettingsError`] when the snapshot or declared hint is
    /// incompatible with this schema.
    pub fn minimum_answer_coverage_millis(
        &self,
        resolved: &ResolvedProviderRuntimeSettings,
    ) -> Result<u16, ProviderSettingsError> {
        self.validate_resolved(resolved)?;
        self.definitions
            .iter()
            .find(|definition| {
                definition.core_behavior == Some(ProviderSettingCoreBehavior::MinimumAnswerCoverage)
            })
            .map_or(Ok(1_000), |definition| {
                resolved
                    .decimal_millis(&definition.key)
                    .and_then(|value| u16::try_from(value).ok())
                    .ok_or_else(|| ProviderSettingsError::InvalidCoreBehavior {
                        key: definition.key.clone(),
                        behavior: ProviderSettingCoreBehavior::MinimumAnswerCoverage,
                    })
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

impl ResolvedProviderRuntimeSettings {
    #[must_use]
    pub fn boolean(&self, key: &str) -> Option<bool> {
        match self.values.get(key) {
            Some(ProviderSettingValue::Boolean(value)) => Some(*value),
            _ => None,
        }
    }

    #[must_use]
    pub fn integer(&self, key: &str) -> Option<i64> {
        match self.values.get(key) {
            Some(ProviderSettingValue::Integer(value)) => Some(*value),
            _ => None,
        }
    }

    #[must_use]
    pub fn decimal_millis(&self, key: &str) -> Option<i64> {
        match self.values.get(key) {
            Some(ProviderSettingValue::DecimalMillis(value)) => Some(*value),
            _ => None,
        }
    }

    #[must_use]
    pub fn duration_seconds(&self, key: &str) -> Option<u64> {
        match self.values.get(key) {
            Some(ProviderSettingValue::DurationSeconds(value)) => Some(*value),
            _ => None,
        }
    }

    #[must_use]
    pub fn choice(&self, key: &str) -> Option<&str> {
        match self.values.get(key) {
            Some(ProviderSettingValue::Choice(value)) => Some(value),
            _ => None,
        }
    }
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
    #[error("provider runtime settings declare Core behavior `{behavior:?}` more than once")]
    DuplicateCoreBehavior {
        behavior: ProviderSettingCoreBehavior,
    },
    #[error("provider runtime settings definition `{key}` has invalid Core behavior `{behavior:?}")]
    InvalidCoreBehavior {
        key: String,
        behavior: ProviderSettingCoreBehavior,
    },
    #[error("provider runtime settings definition `{key}` has invalid constraints")]
    InvalidConstraint { key: String },
    #[error("provider runtime settings patch uses a different schema version")]
    SchemaVersionMismatch,
    #[error("provider runtime settings patch has too many values")]
    TooManyValues,
    #[error("provider runtime settings key `{key}` is unknown")]
    UnknownSetting { key: String },
    #[error("resolved provider runtime settings key `{key}` is missing")]
    MissingSetting { key: String },
    #[error("provider runtime settings key `{key}` does not support scope `{scope:?}`")]
    UnsupportedScope {
        key: String,
        scope: ProviderSettingScope,
    },
    #[error("provider runtime settings value for `{key}` is invalid")]
    InvalidValue { key: String },
}

fn validate_core_behavior(
    definition: &ProviderSettingDefinition,
    behavior: ProviderSettingCoreBehavior,
) -> Result<(), ProviderSettingsError> {
    let maximum = match behavior {
        ProviderSettingCoreBehavior::ProviderExecutionConcurrency => {
            MAX_PROVIDER_EXECUTION_CONCURRENCY
        }
        ProviderSettingCoreBehavior::AccountExecutionConcurrency => {
            MAX_ACCOUNT_EXECUTION_CONCURRENCY
        }
        ProviderSettingCoreBehavior::AccountScanInterval
        | ProviderSettingCoreBehavior::MinimumAnswerCoverage => 0,
    };
    let valid_kind = match behavior {
        ProviderSettingCoreBehavior::ProviderExecutionConcurrency
        | ProviderSettingCoreBehavior::AccountExecutionConcurrency => matches!(
            definition.kind,
            ProviderSettingKind::Integer {
                minimum: 1,
                maximum: declared_maximum,
                step: 1,
            } if declared_maximum <= maximum
        ),
        ProviderSettingCoreBehavior::AccountScanInterval => matches!(
            definition.kind,
            ProviderSettingKind::DurationSeconds {
                minimum,
                maximum,
                ..
            } if minimum >= MIN_ACCOUNT_SCAN_INTERVAL_SECONDS
                && maximum <= MAX_ACCOUNT_SCAN_INTERVAL_SECONDS
        ),
        ProviderSettingCoreBehavior::MinimumAnswerCoverage => matches!(
            definition.kind,
            ProviderSettingKind::DecimalMillis {
                minimum,
                maximum,
                step: 1,
            } if minimum >= 1 && maximum <= 1_000
        ),
    };
    let valid_scopes = match behavior {
        ProviderSettingCoreBehavior::ProviderExecutionConcurrency => {
            definition.scopes.len() == 1
                && definition.scopes.contains(&ProviderSettingScope::Provider)
        }
        ProviderSettingCoreBehavior::AccountExecutionConcurrency
        | ProviderSettingCoreBehavior::MinimumAnswerCoverage => {
            definition.scopes.contains(&ProviderSettingScope::Provider)
        }
        ProviderSettingCoreBehavior::AccountScanInterval => {
            definition.scopes
                == BTreeSet::from([
                    ProviderSettingScope::Provider,
                    ProviderSettingScope::ProviderAccount,
                ])
        }
    };
    if valid_kind && valid_scopes {
        Ok(())
    } else {
        Err(ProviderSettingsError::InvalidCoreBehavior {
            key: definition.key.clone(),
            behavior,
        })
    }
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
                    core_behavior: None,
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
                    core_behavior: None,
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
            .resolve_with_sources(Some(&provider), Some(&account), Some(&task))
            .unwrap();

        assert_eq!(resolved.0.schema_version, 3);
        assert_eq!(
            resolved.0.values["video.max_concurrency"],
            ProviderSettingValue::Integer(1)
        );
        assert_eq!(
            resolved.0.values["video.playback_rate"],
            ProviderSettingValue::DecimalMillis(1_500)
        );
        assert_eq!(
            resolved.1["video.max_concurrency"],
            ProviderRuntimeSettingSource::Task
        );
        assert_eq!(
            resolved.1["video.playback_rate"],
            ProviderRuntimeSettingSource::ProviderAccount
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

    #[test]
    fn complete_snapshots_are_validated_and_expose_typed_readers() {
        let schema = schema();
        let resolved = schema.resolve(None, None, None).unwrap();
        schema.validate_resolved(&resolved).unwrap();
        assert_eq!(resolved.integer("video.max_concurrency"), Some(2));
        assert_eq!(resolved.decimal_millis("video.playback_rate"), Some(1_000));
        assert_eq!(resolved.boolean("video.max_concurrency"), None);
        assert_eq!(resolved.duration_seconds("missing"), None);
        assert_eq!(resolved.choice("missing"), None);

        let mut missing = resolved.clone();
        missing.values.remove("video.max_concurrency");
        assert!(matches!(
            schema.validate_resolved(&missing),
            Err(ProviderSettingsError::MissingSetting { .. })
        ));

        let mut unknown = resolved;
        unknown.values.insert(
            "execution.unknown".to_owned(),
            ProviderSettingValue::Boolean(true),
        );
        assert!(matches!(
            schema.validate_resolved(&unknown),
            Err(ProviderSettingsError::UnknownSetting { .. })
        ));
    }

    #[test]
    fn execution_concurrency_uses_declared_core_behaviors_and_safe_defaults() {
        let mut schema = schema();
        schema.definitions[0].core_behavior =
            Some(ProviderSettingCoreBehavior::AccountExecutionConcurrency);
        let resolved = schema
            .resolve(
                None,
                None,
                Some(&patch([(
                    "video.max_concurrency",
                    ProviderSettingValue::Integer(4),
                )])),
            )
            .unwrap();

        assert_eq!(
            schema.execution_concurrency(&resolved).unwrap(),
            ProviderExecutionConcurrency {
                provider: 1,
                account: 4,
            }
        );
    }

    #[test]
    fn core_behaviors_reject_duplicates_wrong_scopes_and_unsafe_caps() {
        let mut duplicate = schema();
        for definition in &mut duplicate.definitions {
            definition.kind = ProviderSettingKind::Integer {
                minimum: 1,
                maximum: 8,
                step: 1,
            };
            definition.default = ProviderSettingValue::Integer(1);
            definition.core_behavior =
                Some(ProviderSettingCoreBehavior::AccountExecutionConcurrency);
        }
        assert!(matches!(
            duplicate.validate(),
            Err(ProviderSettingsError::DuplicateCoreBehavior { .. })
        ));

        let mut wrong_scope = schema();
        wrong_scope.definitions[0].core_behavior =
            Some(ProviderSettingCoreBehavior::ProviderExecutionConcurrency);
        assert!(matches!(
            wrong_scope.validate(),
            Err(ProviderSettingsError::InvalidCoreBehavior { .. })
        ));

        let mut unsafe_cap = schema();
        unsafe_cap.definitions[0].kind = ProviderSettingKind::Integer {
            minimum: 1,
            maximum: 101,
            step: 1,
        };
        unsafe_cap.definitions[0].core_behavior =
            Some(ProviderSettingCoreBehavior::AccountExecutionConcurrency);
        assert!(matches!(
            unsafe_cap.validate(),
            Err(ProviderSettingsError::InvalidCoreBehavior { .. })
        ));
    }

    #[test]
    fn account_scan_interval_requires_a_bounded_account_overridable_duration() {
        let mut schema = schema();
        schema.definitions.push(ProviderSettingDefinition {
            key: "discovery.scan_interval".to_owned(),
            display_name: "Scan interval".to_owned(),
            description: "Default periodic account inventory interval.".to_owned(),
            kind: ProviderSettingKind::DurationSeconds {
                minimum: 300,
                maximum: 86_400,
                step: 300,
            },
            default: ProviderSettingValue::DurationSeconds(1_800),
            scopes: BTreeSet::from([
                ProviderSettingScope::Provider,
                ProviderSettingScope::ProviderAccount,
            ]),
            core_behavior: Some(ProviderSettingCoreBehavior::AccountScanInterval),
        });
        let resolved = schema.resolve(None, None, None).unwrap();
        assert_eq!(
            schema.account_scan_interval(&resolved).unwrap(),
            Some(1_800)
        );

        schema.definitions.last_mut().unwrap().scopes = BTreeSet::from([
            ProviderSettingScope::Provider,
            ProviderSettingScope::ProviderAccount,
            ProviderSettingScope::Task,
        ]);
        assert!(matches!(
            schema.validate(),
            Err(ProviderSettingsError::InvalidCoreBehavior { .. })
        ));
    }

    #[test]
    fn answer_coverage_is_fixed_point_bounded_and_defaults_to_complete() {
        let plain = schema();
        let resolved = plain.resolve(None, None, None).unwrap();
        assert_eq!(
            plain.minimum_answer_coverage_millis(&resolved).unwrap(),
            1_000
        );

        let mut partial = schema();
        partial.definitions.push(ProviderSettingDefinition {
            key: "submission.minimum_answer_coverage".to_owned(),
            display_name: "Minimum answer coverage".to_owned(),
            description: "Minimum answered fraction required before submission.".to_owned(),
            kind: ProviderSettingKind::DecimalMillis {
                minimum: 1,
                maximum: 1_000,
                step: 1,
            },
            default: ProviderSettingValue::DecimalMillis(900),
            scopes: BTreeSet::from([
                ProviderSettingScope::Provider,
                ProviderSettingScope::ProviderAccount,
                ProviderSettingScope::Task,
            ]),
            core_behavior: Some(ProviderSettingCoreBehavior::MinimumAnswerCoverage),
        });
        let resolved = partial.resolve(None, None, None).unwrap();
        assert_eq!(
            partial.minimum_answer_coverage_millis(&resolved).unwrap(),
            900
        );

        partial.definitions.last_mut().unwrap().kind = ProviderSettingKind::DecimalMillis {
            minimum: 0,
            maximum: 1_000,
            step: 1,
        };
        assert!(matches!(
            partial.validate(),
            Err(ProviderSettingsError::InvalidCoreBehavior { .. })
        ));
    }
}
