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
const MAX_COMPLETION_ATTEMPTS: i64 = 100;
const MIN_COMPLETION_TIME_LIMIT_SECONDS: u64 = 60;
const MAX_COMPLETION_TIME_LIMIT_SECONDS: u64 = 30 * 24 * 60 * 60;

pub const STRICT_COMPLETION_ENABLED_KEY: &str = "core.strict_completion.enabled";
pub const SCORE_IMPROVEMENT_ENABLED_KEY: &str = "core.score_improvement.enabled";
pub const STRICT_COMPLETION_ATTEMPT_LIMIT_KEY: &str = "core.strict_completion.attempt_limit";
pub const SCORE_IMPROVEMENT_ATTEMPT_LIMIT_KEY: &str = "core.score_improvement.attempt_limit";
pub const SCORE_IMPROVEMENT_TARGET_KEY: &str = "core.score_improvement.target";
pub const STRICT_COMPLETION_TIME_LIMIT_KEY: &str = "core.strict_completion.time_limit";
pub const SCORE_IMPROVEMENT_TIME_LIMIT_KEY: &str = "core.score_improvement.time_limit";
pub const AI_EXECUTION_PROFILE_KEY: &str = "core.ai.execution_profile";
pub const AI_PROFILE_ECONOMY: &str = "economy";
pub const AI_PROFILE_GPT_ONLY: &str = "gpt_only";

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
    StrictCompletionEnabled,
    ScoreImprovementEnabled,
    StrictCompletionAttemptLimit,
    ScoreImprovementAttemptLimit,
    ScoreImprovementTarget,
    StrictCompletionTimeLimit,
    ScoreImprovementTimeLimit,
    AiExecutionProfile,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AiExecutionProfile {
    Economy,
    GptOnly,
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

    /// Appends the canonical Core-owned completion policy fields.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderSettingsError`] when a Provider schema collides with
    /// a reserved key/behavior or the combined schema exceeds its bounds.
    pub fn with_core_completion_policy(mut self) -> Result<Self, ProviderSettingsError> {
        self.definitions.extend(core_completion_definitions());
        self.validate()?;
        Ok(self)
    }

    /// Adds the product-owned answer execution combination. It inherits across
    /// Provider/account/Task scopes and is frozen with the existing Job
    /// runtime-settings snapshot.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderSettingsError`] when adding the Core setting makes the
    /// combined schema invalid or collides with an existing setting.
    pub fn with_core_ai_policy(mut self) -> Result<Self, ProviderSettingsError> {
        self.definitions.push(core_ai_profile_definition());
        self.validate()?;
        Ok(self)
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

    /// Restores canonical defaults that were introduced after an execution
    /// settings snapshot was frozen.
    ///
    /// Only missing Core-owned completion fields are restored. Provider
    /// fields, unknown values, schema revisions and malformed values remain
    /// fail-closed so this cannot broaden Provider execution authority.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderSettingsError`] when the frozen snapshot cannot be
    /// upgraded without changing Provider-owned settings.
    pub fn hydrate_frozen_core_defaults(
        &self,
        resolved: &ResolvedProviderRuntimeSettings,
    ) -> Result<ResolvedProviderRuntimeSettings, ProviderSettingsError> {
        self.validate()?;
        if resolved.schema_version != self.version {
            return Err(ProviderSettingsError::SchemaVersionMismatch);
        }
        let mut hydrated = resolved.clone();
        for definition in &self.definitions {
            if hydrated.values.contains_key(&definition.key) {
                continue;
            }
            if definition
                .core_behavior
                .is_some_and(is_hydratable_core_behavior)
            {
                hydrated
                    .values
                    .insert(definition.key.clone(), definition.default.clone());
            } else {
                return Err(ProviderSettingsError::MissingSetting {
                    key: definition.key.clone(),
                });
            }
        }
        self.validate_resolved(&hydrated)?;
        Ok(hydrated)
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
                    | ProviderSettingCoreBehavior::StrictCompletionEnabled
                    | ProviderSettingCoreBehavior::ScoreImprovementEnabled
                    | ProviderSettingCoreBehavior::StrictCompletionAttemptLimit
                    | ProviderSettingCoreBehavior::ScoreImprovementAttemptLimit
                    | ProviderSettingCoreBehavior::ScoreImprovementTarget
                    | ProviderSettingCoreBehavior::StrictCompletionTimeLimit
                    | ProviderSettingCoreBehavior::ScoreImprovementTimeLimit
                    | ProviderSettingCoreBehavior::AiExecutionProfile
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
                | ProviderSettingCoreBehavior::MinimumAnswerCoverage
                | ProviderSettingCoreBehavior::StrictCompletionEnabled
                | ProviderSettingCoreBehavior::ScoreImprovementEnabled
                | ProviderSettingCoreBehavior::StrictCompletionAttemptLimit
                | ProviderSettingCoreBehavior::ScoreImprovementAttemptLimit
                | ProviderSettingCoreBehavior::ScoreImprovementTarget
                | ProviderSettingCoreBehavior::StrictCompletionTimeLimit
                | ProviderSettingCoreBehavior::ScoreImprovementTimeLimit
                | ProviderSettingCoreBehavior::AiExecutionProfile => unreachable!(),
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

    /// Resolves the shared completion policy from one immutable settings snapshot.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderSettingsError`] when the snapshot or one mapped Core
    /// value is missing, malformed or produces an unrepresentable deadline.
    pub fn completion_policy_snapshot(
        &self,
        resolved: &ResolvedProviderRuntimeSettings,
        captured_at: asterism_domain::Timestamp,
    ) -> Result<asterism_domain::CompletionPolicySnapshot, ProviderSettingsError> {
        let resolved = self.hydrate_frozen_core_defaults(resolved)?;
        let strict_limit = completion_integer(
            self,
            &resolved,
            ProviderSettingCoreBehavior::StrictCompletionAttemptLimit,
            3,
        )?;
        let score_limit = completion_integer(
            self,
            &resolved,
            ProviderSettingCoreBehavior::ScoreImprovementAttemptLimit,
            1,
        )?;
        let target = completion_decimal(
            self,
            &resolved,
            ProviderSettingCoreBehavior::ScoreImprovementTarget,
            1_000,
        )?;
        let policy = asterism_domain::CompletionPolicySnapshot {
            strict_completion_enabled: completion_boolean(
                self,
                &resolved,
                ProviderSettingCoreBehavior::StrictCompletionEnabled,
                true,
            )?,
            score_improvement_enabled: completion_boolean(
                self,
                &resolved,
                ProviderSettingCoreBehavior::ScoreImprovementEnabled,
                true,
            )?,
            strict_attempt_limit: u32::try_from(strict_limit).map_err(|_| {
                invalid_completion_behavior(
                    ProviderSettingCoreBehavior::StrictCompletionAttemptLimit,
                )
            })?,
            score_improvement_attempt_limit: u32::try_from(score_limit).map_err(|_| {
                invalid_completion_behavior(
                    ProviderSettingCoreBehavior::ScoreImprovementAttemptLimit,
                )
            })?,
            score_target_millis: u16::try_from(target).map_err(|_| {
                invalid_completion_behavior(ProviderSettingCoreBehavior::ScoreImprovementTarget)
            })?,
            strict_expires_at: completion_deadline(
                self,
                &resolved,
                ProviderSettingCoreBehavior::StrictCompletionTimeLimit,
                captured_at,
            )?,
            score_improvement_expires_at: completion_deadline(
                self,
                &resolved,
                ProviderSettingCoreBehavior::ScoreImprovementTimeLimit,
                captured_at,
            )?,
            formal_retry_requires_confirmation: true,
            captured_at,
        };
        policy.validate().map_err(|_| {
            invalid_completion_behavior(ProviderSettingCoreBehavior::StrictCompletionEnabled)
        })?;
        Ok(policy)
    }

    /// Resolves the selected answer combination from an immutable settings
    /// snapshot. Older frozen snapshots are hydrated with the economy default.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderSettingsError`] when the snapshot contains an invalid
    /// or unsupported Core answer profile.
    pub fn ai_execution_profile(
        &self,
        resolved: &ResolvedProviderRuntimeSettings,
    ) -> Result<AiExecutionProfile, ProviderSettingsError> {
        let resolved = self.hydrate_frozen_core_defaults(resolved)?;
        match resolved.choice(AI_EXECUTION_PROFILE_KEY) {
            Some(AI_PROFILE_ECONOMY) => Ok(AiExecutionProfile::Economy),
            Some(AI_PROFILE_GPT_ONLY) => Ok(AiExecutionProfile::GptOnly),
            _ => Err(ProviderSettingsError::InvalidCoreBehavior {
                key: AI_EXECUTION_PROFILE_KEY.to_owned(),
                behavior: ProviderSettingCoreBehavior::AiExecutionProfile,
            }),
        }
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

fn core_completion_definitions() -> Vec<ProviderSettingDefinition> {
    let scopes = BTreeSet::from([
        ProviderSettingScope::Provider,
        ProviderSettingScope::ProviderAccount,
        ProviderSettingScope::Task,
    ]);
    vec![
        ProviderSettingDefinition {
            key: STRICT_COMPLETION_ENABLED_KEY.to_owned(),
            display_name: "Strict completion".to_owned(),
            description: "Continue bounded verified work until the Task is complete.".to_owned(),
            kind: ProviderSettingKind::Boolean,
            default: ProviderSettingValue::Boolean(true),
            scopes: scopes.clone(),
            core_behavior: Some(ProviderSettingCoreBehavior::StrictCompletionEnabled),
        },
        ProviderSettingDefinition {
            key: SCORE_IMPROVEMENT_ENABLED_KEY.to_owned(),
            display_name: "Score improvement".to_owned(),
            description: "Allow explicitly confirmed retakes after verified completion.".to_owned(),
            kind: ProviderSettingKind::Boolean,
            default: ProviderSettingValue::Boolean(true),
            scopes: scopes.clone(),
            core_behavior: Some(ProviderSettingCoreBehavior::ScoreImprovementEnabled),
        },
        ProviderSettingDefinition {
            key: STRICT_COMPLETION_ATTEMPT_LIMIT_KEY.to_owned(),
            display_name: "Strict completion attempt limit".to_owned(),
            description: "Maximum attempts in one Strict Completion workflow.".to_owned(),
            kind: ProviderSettingKind::Integer {
                minimum: 1,
                maximum: MAX_COMPLETION_ATTEMPTS,
                step: 1,
            },
            default: ProviderSettingValue::Integer(3),
            scopes: scopes.clone(),
            core_behavior: Some(ProviderSettingCoreBehavior::StrictCompletionAttemptLimit),
        },
        ProviderSettingDefinition {
            key: SCORE_IMPROVEMENT_ATTEMPT_LIMIT_KEY.to_owned(),
            display_name: "Score improvement attempt limit".to_owned(),
            description: "Maximum explicitly confirmed retakes in one workflow.".to_owned(),
            kind: ProviderSettingKind::Integer {
                minimum: 1,
                maximum: MAX_COMPLETION_ATTEMPTS,
                step: 1,
            },
            default: ProviderSettingValue::Integer(1),
            scopes: scopes.clone(),
            core_behavior: Some(ProviderSettingCoreBehavior::ScoreImprovementAttemptLimit),
        },
        ProviderSettingDefinition {
            key: SCORE_IMPROVEMENT_TARGET_KEY.to_owned(),
            display_name: "Score improvement target".to_owned(),
            description: "Target score fraction in thousandths.".to_owned(),
            kind: ProviderSettingKind::DecimalMillis {
                minimum: 1,
                maximum: 1_000,
                step: 1,
            },
            default: ProviderSettingValue::DecimalMillis(1_000),
            scopes: scopes.clone(),
            core_behavior: Some(ProviderSettingCoreBehavior::ScoreImprovementTarget),
        },
        ProviderSettingDefinition {
            key: STRICT_COMPLETION_TIME_LIMIT_KEY.to_owned(),
            display_name: "Strict completion time limit".to_owned(),
            description: "Maximum wall-clock duration of one Strict Completion workflow."
                .to_owned(),
            kind: ProviderSettingKind::DurationSeconds {
                minimum: MIN_COMPLETION_TIME_LIMIT_SECONDS,
                maximum: MAX_COMPLETION_TIME_LIMIT_SECONDS,
                step: 60,
            },
            default: ProviderSettingValue::DurationSeconds(7 * 24 * 60 * 60),
            scopes: scopes.clone(),
            core_behavior: Some(ProviderSettingCoreBehavior::StrictCompletionTimeLimit),
        },
        ProviderSettingDefinition {
            key: SCORE_IMPROVEMENT_TIME_LIMIT_KEY.to_owned(),
            display_name: "Score improvement time limit".to_owned(),
            description: "Maximum wall-clock duration of one Score Improvement workflow."
                .to_owned(),
            kind: ProviderSettingKind::DurationSeconds {
                minimum: MIN_COMPLETION_TIME_LIMIT_SECONDS,
                maximum: MAX_COMPLETION_TIME_LIMIT_SECONDS,
                step: 60,
            },
            default: ProviderSettingValue::DurationSeconds(24 * 60 * 60),
            scopes,
            core_behavior: Some(ProviderSettingCoreBehavior::ScoreImprovementTimeLimit),
        },
    ]
}

fn core_ai_profile_definition() -> ProviderSettingDefinition {
    ProviderSettingDefinition {
        key: AI_EXECUTION_PROFILE_KEY.to_owned(),
        display_name: "AI answer combination".to_owned(),
        description: "Economy trusts Provider standard answers and verified exact cache; GPT-only still trusts Provider standard answers but sends cache hits to GPT for verification.".to_owned(),
        kind: ProviderSettingKind::Choice {
            options: BTreeSet::from([
                AI_PROFILE_ECONOMY.to_owned(),
                AI_PROFILE_GPT_ONLY.to_owned(),
            ]),
        },
        default: ProviderSettingValue::Choice(AI_PROFILE_ECONOMY.to_owned()),
        scopes: BTreeSet::from([
            ProviderSettingScope::Provider,
            ProviderSettingScope::ProviderAccount,
            ProviderSettingScope::Task,
        ]),
        core_behavior: Some(ProviderSettingCoreBehavior::AiExecutionProfile),
    }
}

fn completion_definition(
    schema: &ProviderRuntimeSettingsSchema,
    behavior: ProviderSettingCoreBehavior,
) -> Option<&ProviderSettingDefinition> {
    schema
        .definitions
        .iter()
        .find(|definition| definition.core_behavior == Some(behavior))
}

fn is_hydratable_core_behavior(behavior: ProviderSettingCoreBehavior) -> bool {
    matches!(
        behavior,
        ProviderSettingCoreBehavior::StrictCompletionEnabled
            | ProviderSettingCoreBehavior::ScoreImprovementEnabled
            | ProviderSettingCoreBehavior::StrictCompletionAttemptLimit
            | ProviderSettingCoreBehavior::ScoreImprovementAttemptLimit
            | ProviderSettingCoreBehavior::ScoreImprovementTarget
            | ProviderSettingCoreBehavior::StrictCompletionTimeLimit
            | ProviderSettingCoreBehavior::ScoreImprovementTimeLimit
            | ProviderSettingCoreBehavior::AiExecutionProfile
    )
}

fn completion_boolean(
    schema: &ProviderRuntimeSettingsSchema,
    resolved: &ResolvedProviderRuntimeSettings,
    behavior: ProviderSettingCoreBehavior,
    fallback: bool,
) -> Result<bool, ProviderSettingsError> {
    completion_definition(schema, behavior).map_or(Ok(fallback), |definition| {
        resolved
            .boolean(&definition.key)
            .ok_or_else(|| invalid_completion_behavior(behavior))
    })
}

fn completion_integer(
    schema: &ProviderRuntimeSettingsSchema,
    resolved: &ResolvedProviderRuntimeSettings,
    behavior: ProviderSettingCoreBehavior,
    fallback: i64,
) -> Result<i64, ProviderSettingsError> {
    completion_definition(schema, behavior).map_or(Ok(fallback), |definition| {
        resolved
            .integer(&definition.key)
            .ok_or_else(|| invalid_completion_behavior(behavior))
    })
}

fn completion_decimal(
    schema: &ProviderRuntimeSettingsSchema,
    resolved: &ResolvedProviderRuntimeSettings,
    behavior: ProviderSettingCoreBehavior,
    fallback: i64,
) -> Result<i64, ProviderSettingsError> {
    completion_definition(schema, behavior).map_or(Ok(fallback), |definition| {
        resolved
            .decimal_millis(&definition.key)
            .ok_or_else(|| invalid_completion_behavior(behavior))
    })
}

fn completion_deadline(
    schema: &ProviderRuntimeSettingsSchema,
    resolved: &ResolvedProviderRuntimeSettings,
    behavior: ProviderSettingCoreBehavior,
    captured_at: asterism_domain::Timestamp,
) -> Result<Option<asterism_domain::Timestamp>, ProviderSettingsError> {
    let Some(definition) = completion_definition(schema, behavior) else {
        return Ok(None);
    };
    let seconds = resolved
        .duration_seconds(&definition.key)
        .and_then(|seconds| i64::try_from(seconds).ok())
        .ok_or_else(|| invalid_completion_behavior(behavior))?;
    captured_at
        .checked_add_signed(chrono::Duration::seconds(seconds))
        .map(Some)
        .ok_or_else(|| invalid_completion_behavior(behavior))
}

fn invalid_completion_behavior(behavior: ProviderSettingCoreBehavior) -> ProviderSettingsError {
    ProviderSettingsError::InvalidCoreBehavior {
        key: "core.completion".to_owned(),
        behavior,
    }
}

fn validate_core_behavior(
    definition: &ProviderSettingDefinition,
    behavior: ProviderSettingCoreBehavior,
) -> Result<(), ProviderSettingsError> {
    if valid_core_behavior_kind(definition, behavior)
        && valid_core_behavior_scopes(definition, behavior)
    {
        Ok(())
    } else {
        Err(ProviderSettingsError::InvalidCoreBehavior {
            key: definition.key.clone(),
            behavior,
        })
    }
}

fn valid_core_behavior_kind(
    definition: &ProviderSettingDefinition,
    behavior: ProviderSettingCoreBehavior,
) -> bool {
    let maximum = match behavior {
        ProviderSettingCoreBehavior::ProviderExecutionConcurrency => {
            MAX_PROVIDER_EXECUTION_CONCURRENCY
        }
        ProviderSettingCoreBehavior::AccountExecutionConcurrency => {
            MAX_ACCOUNT_EXECUTION_CONCURRENCY
        }
        ProviderSettingCoreBehavior::AccountScanInterval
        | ProviderSettingCoreBehavior::MinimumAnswerCoverage
        | ProviderSettingCoreBehavior::StrictCompletionEnabled
        | ProviderSettingCoreBehavior::ScoreImprovementEnabled
        | ProviderSettingCoreBehavior::StrictCompletionAttemptLimit
        | ProviderSettingCoreBehavior::ScoreImprovementAttemptLimit
        | ProviderSettingCoreBehavior::ScoreImprovementTarget
        | ProviderSettingCoreBehavior::StrictCompletionTimeLimit
        | ProviderSettingCoreBehavior::ScoreImprovementTimeLimit
        | ProviderSettingCoreBehavior::AiExecutionProfile => 0,
    };
    match behavior {
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
        ProviderSettingCoreBehavior::MinimumAnswerCoverage
        | ProviderSettingCoreBehavior::ScoreImprovementTarget => matches!(
            definition.kind,
            ProviderSettingKind::DecimalMillis {
                minimum,
                maximum,
                step: 1,
            } if minimum >= 1 && maximum <= 1_000
        ),
        ProviderSettingCoreBehavior::StrictCompletionEnabled
        | ProviderSettingCoreBehavior::ScoreImprovementEnabled => {
            matches!(definition.kind, ProviderSettingKind::Boolean)
        }
        ProviderSettingCoreBehavior::StrictCompletionAttemptLimit
        | ProviderSettingCoreBehavior::ScoreImprovementAttemptLimit => matches!(
            definition.kind,
            ProviderSettingKind::Integer {
                minimum: 1,
                maximum,
                step: 1,
            } if maximum <= MAX_COMPLETION_ATTEMPTS
        ),
        ProviderSettingCoreBehavior::StrictCompletionTimeLimit
        | ProviderSettingCoreBehavior::ScoreImprovementTimeLimit => matches!(
            definition.kind,
            ProviderSettingKind::DurationSeconds {
                minimum,
                maximum,
                ..
            } if minimum >= MIN_COMPLETION_TIME_LIMIT_SECONDS
                && maximum <= MAX_COMPLETION_TIME_LIMIT_SECONDS
        ),
        ProviderSettingCoreBehavior::AiExecutionProfile => matches!(
            &definition.kind,
            ProviderSettingKind::Choice { options }
                if options == &BTreeSet::from([
                    AI_PROFILE_ECONOMY.to_owned(),
                    AI_PROFILE_GPT_ONLY.to_owned(),
                ])
        ),
    }
}

fn valid_core_behavior_scopes(
    definition: &ProviderSettingDefinition,
    behavior: ProviderSettingCoreBehavior,
) -> bool {
    match behavior {
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
        ProviderSettingCoreBehavior::StrictCompletionEnabled
        | ProviderSettingCoreBehavior::ScoreImprovementEnabled
        | ProviderSettingCoreBehavior::StrictCompletionAttemptLimit
        | ProviderSettingCoreBehavior::ScoreImprovementAttemptLimit
        | ProviderSettingCoreBehavior::ScoreImprovementTarget
        | ProviderSettingCoreBehavior::StrictCompletionTimeLimit
        | ProviderSettingCoreBehavior::ScoreImprovementTimeLimit
        | ProviderSettingCoreBehavior::AiExecutionProfile => {
            definition.scopes
                == BTreeSet::from([
                    ProviderSettingScope::Provider,
                    ProviderSettingScope::ProviderAccount,
                    ProviderSettingScope::Task,
                ])
        }
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

    #[test]
    fn completion_policy_is_core_owned_inherited_and_frozen() {
        let schema = schema().with_core_completion_policy().unwrap();
        let provider = patch([(
            SCORE_IMPROVEMENT_TARGET_KEY,
            ProviderSettingValue::DecimalMillis(950),
        )]);
        let account = patch([(
            STRICT_COMPLETION_ATTEMPT_LIMIT_KEY,
            ProviderSettingValue::Integer(5),
        )]);
        let task = patch([(
            SCORE_IMPROVEMENT_ENABLED_KEY,
            ProviderSettingValue::Boolean(true),
        )]);
        let (resolved, sources) = schema
            .resolve_with_sources(Some(&provider), Some(&account), Some(&task))
            .unwrap();
        let captured_at = chrono::Utc::now();
        let policy = schema
            .completion_policy_snapshot(&resolved, captured_at)
            .unwrap();
        assert!(policy.strict_completion_enabled);
        assert!(policy.score_improvement_enabled);
        assert_eq!(policy.strict_attempt_limit, 5);
        assert_eq!(policy.score_improvement_attempt_limit, 1);
        assert_eq!(policy.score_target_millis, 950);
        assert_eq!(
            policy.strict_expires_at,
            Some(captured_at + chrono::Duration::days(7))
        );
        assert_eq!(
            policy.score_improvement_expires_at,
            Some(captured_at + chrono::Duration::days(1))
        );
        assert!(policy.formal_retry_requires_confirmation);
        assert_eq!(
            sources[SCORE_IMPROVEMENT_TARGET_KEY],
            ProviderRuntimeSettingSource::Provider
        );
        assert_eq!(
            sources[STRICT_COMPLETION_ATTEMPT_LIMIT_KEY],
            ProviderRuntimeSettingSource::ProviderAccount
        );
        assert_eq!(
            sources[SCORE_IMPROVEMENT_ENABLED_KEY],
            ProviderRuntimeSettingSource::Task
        );
    }

    #[test]
    fn provider_schema_cannot_claim_a_reserved_completion_key() {
        let mut collision = schema();
        collision.definitions.push(ProviderSettingDefinition {
            key: STRICT_COMPLETION_ENABLED_KEY.to_owned(),
            display_name: "Provider collision".to_owned(),
            description: "Provider-defined value using a Core-reserved key.".to_owned(),
            kind: ProviderSettingKind::Boolean,
            default: ProviderSettingValue::Boolean(false),
            scopes: BTreeSet::from([ProviderSettingScope::Provider]),
            core_behavior: None,
        });
        assert!(matches!(
            collision.with_core_completion_policy(),
            Err(ProviderSettingsError::DuplicateDefinition { .. })
        ));
    }

    #[test]
    fn frozen_snapshots_restore_only_later_core_completion_defaults() {
        let provider_schema = schema();
        let legacy = provider_schema.resolve(None, None, None).unwrap();
        let schema = provider_schema.with_core_completion_policy().unwrap();

        assert!(matches!(
            schema.validate_resolved(&legacy),
            Err(ProviderSettingsError::MissingSetting { .. })
        ));
        let hydrated = schema.hydrate_frozen_core_defaults(&legacy).unwrap();
        schema.validate_resolved(&hydrated).unwrap();
        assert_eq!(hydrated.boolean(STRICT_COMPLETION_ENABLED_KEY), Some(true));
        assert_eq!(hydrated.boolean(SCORE_IMPROVEMENT_ENABLED_KEY), Some(true));

        let mut missing_provider_value = legacy;
        missing_provider_value
            .values
            .remove("video.max_concurrency");
        assert!(matches!(
            schema.hydrate_frozen_core_defaults(&missing_provider_value),
            Err(ProviderSettingsError::MissingSetting { key })
                if key == "video.max_concurrency"
        ));
    }
}
