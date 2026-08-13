use std::collections::BTreeSet;

use asterism_provider_api::{
    ProviderError, ProviderErrorKind, ProviderResult, ProviderRuntimeSettingsSchema,
    ProviderSettingCoreBehavior, ProviderSettingDefinition, ProviderSettingKind,
    ProviderSettingScope, ProviderSettingValue, ResolvedProviderRuntimeSettings,
};
use sha2::{Digest, Sha256};

pub(crate) const PROVIDER_EXECUTION_CONCURRENCY_KEY: &str = "execution.provider_concurrency";
pub(crate) const ACCOUNT_EXECUTION_CONCURRENCY_KEY: &str = "execution.account_concurrency";
pub(crate) const ACCOUNT_SCAN_INTERVAL_KEY: &str = "discovery.scan_interval";
pub(crate) const ANSWER_DELAY_MIN_SECONDS_KEY: &str = "answer.delay_min_seconds";
pub(crate) const ANSWER_DELAY_MAX_SECONDS_KEY: &str = "answer.delay_max_seconds";
pub(crate) const ANSWER_TIME_MIN_MILLIS_KEY: &str = "answer.reported_time_min_millis";
pub(crate) const ANSWER_TIME_MAX_MILLIS_KEY: &str = "answer.reported_time_max_millis";
pub(crate) const SKIP_TIME_MILLIS_KEY: &str = "answer.skip_reported_time_millis";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CidarenRuntimeSettings {
    pub(crate) answer_delay_min_seconds: u64,
    pub(crate) answer_delay_max_seconds: u64,
    pub(crate) answer_time_min_millis: u64,
    pub(crate) answer_time_max_millis: u64,
    pub(crate) skip_time_millis: u64,
}

impl CidarenRuntimeSettings {
    /// Validates and resolves one immutable Core-owned settings snapshot.
    ///
    /// # Errors
    ///
    /// Returns a sanitized internal error when the schema revision, value
    /// shapes or minimum/maximum relationships are incompatible.
    pub fn resolve(settings: &ResolvedProviderRuntimeSettings) -> ProviderResult<Self> {
        runtime_settings_schema()
            .validate_resolved(settings)
            .map_err(|_| incompatible_settings())?;
        let resolved = Self {
            answer_delay_min_seconds: duration(settings, ANSWER_DELAY_MIN_SECONDS_KEY)?,
            answer_delay_max_seconds: duration(settings, ANSWER_DELAY_MAX_SECONDS_KEY)?,
            answer_time_min_millis: integer(settings, ANSWER_TIME_MIN_MILLIS_KEY)?,
            answer_time_max_millis: integer(settings, ANSWER_TIME_MAX_MILLIS_KEY)?,
            skip_time_millis: integer(settings, SKIP_TIME_MILLIS_KEY)?,
        };
        if resolved.answer_delay_min_seconds > resolved.answer_delay_max_seconds
            || resolved.answer_time_min_millis > resolved.answer_time_max_millis
        {
            return Err(incompatible_settings());
        }
        Ok(resolved)
    }

    /// Selects one stable donor-compatible inter-step delay from the resolved
    /// bounds. Core can schedule this delay without sleeping inside Provider
    /// mutation code.
    pub fn answer_delay_seconds(&self, entropy: &[u8]) -> u64 {
        select_bounded(
            self.answer_delay_min_seconds,
            self.answer_delay_max_seconds,
            b"cidaren-answer-delay-v1",
            entropy,
        )
    }

    /// Selects one stable reported answer duration from the resolved bounds.
    /// The selected value is immutable for the same execution-step entropy.
    pub fn reported_answer_time_millis(&self, entropy: &[u8]) -> u64 {
        select_bounded(
            self.answer_time_min_millis,
            self.answer_time_max_millis,
            b"cidaren-reported-answer-time-v1",
            entropy,
        )
    }

    /// Returns the donor-observed fixed reported duration for `SkipAnswer`.
    pub const fn skip_reported_time_millis(&self) -> u64 {
        self.skip_time_millis
    }
}

fn select_bounded(minimum: u64, maximum: u64, domain: &[u8], entropy: &[u8]) -> u64 {
    if minimum == maximum {
        return minimum;
    }
    let digest = Sha256::new()
        .chain_update(domain)
        .chain_update([0])
        .chain_update(entropy)
        .finalize();
    let mut bytes = [0_u8; 8];
    bytes.copy_from_slice(&digest[..8]);
    minimum + u64::from_be_bytes(bytes) % (maximum - minimum + 1)
}

pub(crate) fn runtime_settings_schema() -> ProviderRuntimeSettingsSchema {
    let provider_account_task = BTreeSet::from([
        ProviderSettingScope::Provider,
        ProviderSettingScope::ProviderAccount,
        ProviderSettingScope::Task,
    ]);
    let mut definitions = scheduling_definitions(&provider_account_task);
    definitions.extend(answer_definitions(provider_account_task));
    ProviderRuntimeSettingsSchema {
        version: 2,
        definitions,
    }
}

fn scheduling_definitions(
    provider_account_task: &BTreeSet<ProviderSettingScope>,
) -> Vec<ProviderSettingDefinition> {
    vec![
        ProviderSettingDefinition {
            key: PROVIDER_EXECUTION_CONCURRENCY_KEY.to_owned(),
            display_name: "平台执行并发".to_owned(),
            description: "Core 同时向词达人发起的执行数量上限；真实并发验证前默认 1。".to_owned(),
            kind: ProviderSettingKind::Integer {
                minimum: 1,
                maximum: 16,
                step: 1,
            },
            default: ProviderSettingValue::Integer(1),
            scopes: BTreeSet::from([ProviderSettingScope::Provider]),
            core_behavior: Some(ProviderSettingCoreBehavior::ProviderExecutionConcurrency),
        },
        ProviderSettingDefinition {
            key: ACCOUNT_EXECUTION_CONCURRENCY_KEY.to_owned(),
            display_name: "账号执行并发".to_owned(),
            description: "同一词达人账号的并行执行上限，可按账号或具体任务覆盖；默认串行。"
                .to_owned(),
            kind: ProviderSettingKind::Integer {
                minimum: 1,
                maximum: 4,
                step: 1,
            },
            default: ProviderSettingValue::Integer(1),
            scopes: provider_account_task.clone(),
            core_behavior: Some(ProviderSettingCoreBehavior::AccountExecutionConcurrency),
        },
        ProviderSettingDefinition {
            key: ACCOUNT_SCAN_INTERVAL_KEY.to_owned(),
            display_name: "账号巡查间隔".to_owned(),
            description: "Core Scheduler 周期检查词达人课程、单元及班级任务变化的默认间隔。"
                .to_owned(),
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
        },
    ]
}

fn answer_definitions(
    provider_account_task: BTreeSet<ProviderSettingScope>,
) -> Vec<ProviderSettingDefinition> {
    vec![
        ProviderSettingDefinition {
            key: ANSWER_DELAY_MIN_SECONDS_KEY.to_owned(),
            display_name: "题间最短等待".to_owned(),
            description: "提交一题并进入下一题前的最短真实等待秒数，保留 donor 可配置行为。"
                .to_owned(),
            kind: ProviderSettingKind::DurationSeconds {
                minimum: 2,
                maximum: 300,
                step: 1,
            },
            default: ProviderSettingValue::DurationSeconds(2),
            scopes: provider_account_task.clone(),
            core_behavior: None,
        },
        ProviderSettingDefinition {
            key: ANSWER_DELAY_MAX_SECONDS_KEY.to_owned(),
            display_name: "题间最长等待".to_owned(),
            description: "提交一题并进入下一题前的最长真实等待秒数，不得小于最短等待。".to_owned(),
            kind: ProviderSettingKind::DurationSeconds {
                minimum: 2,
                maximum: 300,
                step: 1,
            },
            default: ProviderSettingValue::DurationSeconds(2),
            scopes: provider_account_task.clone(),
            core_behavior: None,
        },
        ProviderSettingDefinition {
            key: ANSWER_TIME_MIN_MILLIS_KEY.to_owned(),
            display_name: "答题上报最短时长".to_owned(),
            description:
                "SubmitAnswerAndSave 上报的最短毫秒数；默认 2500 对应 donor 配置 5 的 500ms 换算。"
                    .to_owned(),
            kind: ProviderSettingKind::Integer {
                minimum: 500,
                maximum: 300_000,
                step: 500,
            },
            default: ProviderSettingValue::Integer(2_500),
            scopes: provider_account_task.clone(),
            core_behavior: None,
        },
        ProviderSettingDefinition {
            key: ANSWER_TIME_MAX_MILLIS_KEY.to_owned(),
            display_name: "答题上报最长时长".to_owned(),
            description:
                "SubmitAnswerAndSave 上报的最长毫秒数；默认 7500 对应 donor 配置 15 的 500ms 换算。"
                    .to_owned(),
            kind: ProviderSettingKind::Integer {
                minimum: 500,
                maximum: 300_000,
                step: 500,
            },
            default: ProviderSettingValue::Integer(7_500),
            scopes: provider_account_task.clone(),
            core_behavior: None,
        },
        ProviderSettingDefinition {
            key: SKIP_TIME_MILLIS_KEY.to_owned(),
            display_name: "跳题上报时长".to_owned(),
            description: "SkipAnswer 使用的毫秒数；默认保留 donor 固定的 20000。".to_owned(),
            kind: ProviderSettingKind::Integer {
                minimum: 500,
                maximum: 300_000,
                step: 500,
            },
            default: ProviderSettingValue::Integer(20_000),
            scopes: provider_account_task,
            core_behavior: None,
        },
    ]
}

fn duration(settings: &ResolvedProviderRuntimeSettings, key: &str) -> ProviderResult<u64> {
    settings
        .duration_seconds(key)
        .ok_or_else(incompatible_settings)
}

fn integer(settings: &ResolvedProviderRuntimeSettings, key: &str) -> ProviderResult<u64> {
    settings
        .integer(key)
        .and_then(|value| u64::try_from(value).ok())
        .ok_or_else(incompatible_settings)
}

fn incompatible_settings() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::Internal,
        "Cidaren received incompatible runtime settings",
    )
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use asterism_provider_api::ProviderRuntimeSettingsPatch;

    use super::*;

    #[test]
    fn donor_timing_settings_are_bounded_and_task_overridable() {
        let schema = runtime_settings_schema();
        schema.validate().unwrap();
        assert_eq!(schema.version, 2);
        let task = ProviderRuntimeSettingsPatch {
            schema_version: schema.version,
            values: BTreeMap::from([
                (
                    ANSWER_DELAY_MAX_SECONDS_KEY.to_owned(),
                    ProviderSettingValue::DurationSeconds(4),
                ),
                (
                    ANSWER_TIME_MIN_MILLIS_KEY.to_owned(),
                    ProviderSettingValue::Integer(5_000),
                ),
                (
                    ANSWER_TIME_MAX_MILLIS_KEY.to_owned(),
                    ProviderSettingValue::Integer(10_000),
                ),
                (
                    ACCOUNT_EXECUTION_CONCURRENCY_KEY.to_owned(),
                    ProviderSettingValue::Integer(2),
                ),
            ]),
        };
        let resolved = schema.resolve(None, None, Some(&task)).unwrap();
        let task_settings = CidarenRuntimeSettings::resolve(&resolved).unwrap();
        assert_eq!(
            task_settings,
            CidarenRuntimeSettings {
                answer_delay_min_seconds: 2,
                answer_delay_max_seconds: 4,
                answer_time_min_millis: 5_000,
                answer_time_max_millis: 10_000,
                skip_time_millis: 20_000,
            }
        );
        assert!((2..=4).contains(&task_settings.answer_delay_seconds(b"synthetic-step")));
        assert_eq!(
            schema.execution_concurrency(&resolved).unwrap(),
            asterism_provider_api::ProviderExecutionConcurrency {
                provider: 1,
                account: 2,
            }
        );
        assert_eq!(
            schema.account_scan_interval(&resolved).unwrap(),
            Some(1_800)
        );

        let inverted = ProviderRuntimeSettingsPatch {
            schema_version: schema.version,
            values: BTreeMap::from([
                (
                    ANSWER_DELAY_MIN_SECONDS_KEY.to_owned(),
                    ProviderSettingValue::DurationSeconds(10),
                ),
                (
                    ANSWER_DELAY_MAX_SECONDS_KEY.to_owned(),
                    ProviderSettingValue::DurationSeconds(5),
                ),
            ]),
        };
        let resolved = schema.resolve(None, None, Some(&inverted)).unwrap();
        assert!(CidarenRuntimeSettings::resolve(&resolved).is_err());

        let zero_delay = ProviderRuntimeSettingsPatch {
            schema_version: schema.version,
            values: BTreeMap::from([(
                ANSWER_DELAY_MIN_SECONDS_KEY.to_owned(),
                ProviderSettingValue::DurationSeconds(0),
            )]),
        };
        assert!(schema.resolve(None, None, Some(&zero_delay)).is_err());
    }
}
