use std::collections::BTreeSet;

use asterism_provider_api::{
    ProviderError, ProviderErrorKind, ProviderResult, ProviderRuntimeSettingsSchema,
    ProviderSettingCoreBehavior, ProviderSettingDefinition, ProviderSettingKind,
    ProviderSettingScope, ProviderSettingValue, ResolvedProviderRuntimeSettings,
};

pub(crate) const PROVIDER_EXECUTION_CONCURRENCY_KEY: &str = "execution.provider_concurrency";
pub(crate) const ACCOUNT_EXECUTION_CONCURRENCY_KEY: &str = "execution.account_concurrency";
pub(crate) const ACCOUNT_SCAN_INTERVAL_KEY: &str = "discovery.scan_interval";
pub(crate) const DURATION_REPORT_MODE_KEY: &str = "duration.report_mode";
pub(crate) const DURATION_REPORT_SECONDS_KEY: &str = "duration.report_seconds";
pub(crate) const DURATION_REPORT_MIN_SECONDS_KEY: &str = "duration.report_min_seconds";
pub(crate) const DURATION_REPORT_MAX_SECONDS_KEY: &str = "duration.report_max_seconds";
pub(crate) const DURATION_HEARTBEAT_INTERVAL_KEY: &str = "duration.heartbeat_interval";
pub(crate) const RESOURCE_SCORE_MODE_KEY: &str = "execution.completion_score_mode";
pub(crate) const RESOURCE_SCORE_PERCENT_KEY: &str = "execution.completion_score_percent";
pub(crate) const RESOURCE_SCORE_MIN_PERCENT_KEY: &str = "execution.completion_score_min_percent";
pub(crate) const RESOURCE_SCORE_MAX_PERCENT_KEY: &str = "execution.completion_score_max_percent";

const RESOURCE_SCORE_FIXED: &str = "fixed";
const RESOURCE_SCORE_RANDOM_RANGE: &str = "random_range";
const RESOURCE_SCORE_GAUSSIAN_RANDOM_RANGE: &str = "gaussian_random_range";
pub(crate) const MIN_DURATION_REPORT_SECONDS: u64 = 1;
pub(crate) const MAX_DURATION_REPORT_SECONDS: u64 = 7_200;
pub(crate) const MIN_DURATION_HEARTBEAT_SECONDS: u64 = 1;
pub(crate) const MAX_DURATION_HEARTBEAT_SECONDS: u64 = 90;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum WellearnDurationTarget {
    Fixed(u64),
    RandomRange { minimum: u64, maximum: u64 },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum WellearnResourceScore {
    Fixed(u8),
    UniformRandomRange { minimum: u8, maximum: u8 },
    GaussianRandomRange { minimum: u8, maximum: u8 },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct WellearnRuntimeSettings {
    pub(crate) duration_report: WellearnDurationTarget,
    pub(crate) duration_heartbeat_interval_seconds: u64,
    pub(crate) resource_score: WellearnResourceScore,
}

impl WellearnRuntimeSettings {
    pub(crate) fn resolve(settings: &ResolvedProviderRuntimeSettings) -> ProviderResult<Self> {
        runtime_settings_schema()
            .validate_resolved(settings)
            .map_err(|_| {
                ProviderError::new(
                    ProviderErrorKind::Internal,
                    "WELearn received incompatible runtime settings",
                )
            })?;
        let fixed_duration = settings
            .duration_seconds(DURATION_REPORT_SECONDS_KEY)
            .ok_or_else(|| {
                ProviderError::new(
                    ProviderErrorKind::Internal,
                    "WELearn duration report length is unavailable",
                )
            })?;
        let minimum_duration = bounded_duration(settings, DURATION_REPORT_MIN_SECONDS_KEY)?;
        let maximum_duration = bounded_duration(settings, DURATION_REPORT_MAX_SECONDS_KEY)?;
        if minimum_duration > maximum_duration {
            return Err(ProviderError::new(
                ProviderErrorKind::Internal,
                "WELearn duration report range is inverted",
            ));
        }
        let duration_report = match settings.choice(DURATION_REPORT_MODE_KEY) {
            Some(RESOURCE_SCORE_FIXED) => WellearnDurationTarget::Fixed(fixed_duration),
            Some(RESOURCE_SCORE_RANDOM_RANGE) => WellearnDurationTarget::RandomRange {
                minimum: minimum_duration,
                maximum: maximum_duration,
            },
            _ => {
                return Err(ProviderError::new(
                    ProviderErrorKind::Internal,
                    "WELearn duration report mode is unavailable",
                ));
            }
        };
        let duration_heartbeat_interval_seconds = settings
            .duration_seconds(DURATION_HEARTBEAT_INTERVAL_KEY)
            .ok_or_else(|| {
                ProviderError::new(
                    ProviderErrorKind::Internal,
                    "WELearn duration heartbeat interval is unavailable",
                )
            })?;
        let fixed_score = settings
            .integer(RESOURCE_SCORE_PERCENT_KEY)
            .and_then(|value| u8::try_from(value).ok())
            .filter(|value| *value <= 100)
            .ok_or_else(|| {
                ProviderError::new(
                    ProviderErrorKind::Internal,
                    "WELearn completion score is unavailable or out of range",
                )
            })?;
        let minimum_score = bounded_score(settings, RESOURCE_SCORE_MIN_PERCENT_KEY)?;
        let maximum_score = bounded_score(settings, RESOURCE_SCORE_MAX_PERCENT_KEY)?;
        if minimum_score > maximum_score {
            return Err(ProviderError::new(
                ProviderErrorKind::Internal,
                "WELearn completion score range is inverted",
            ));
        }
        let resource_score = match settings.choice(RESOURCE_SCORE_MODE_KEY) {
            Some(RESOURCE_SCORE_FIXED) => WellearnResourceScore::Fixed(fixed_score),
            Some(RESOURCE_SCORE_RANDOM_RANGE) => WellearnResourceScore::UniformRandomRange {
                minimum: minimum_score,
                maximum: maximum_score,
            },
            Some(RESOURCE_SCORE_GAUSSIAN_RANDOM_RANGE) => {
                WellearnResourceScore::GaussianRandomRange {
                    minimum: minimum_score,
                    maximum: maximum_score,
                }
            }
            _ => {
                return Err(ProviderError::new(
                    ProviderErrorKind::Internal,
                    "WELearn completion score mode is unavailable",
                ));
            }
        };
        Ok(Self {
            duration_report,
            duration_heartbeat_interval_seconds,
            resource_score,
        })
    }
}

fn bounded_duration(
    settings: &ResolvedProviderRuntimeSettings,
    key: &'static str,
) -> ProviderResult<u64> {
    settings.duration_seconds(key).ok_or_else(|| {
        ProviderError::new(
            ProviderErrorKind::Internal,
            "WELearn duration report range is unavailable",
        )
    })
}

fn bounded_score(
    settings: &ResolvedProviderRuntimeSettings,
    key: &'static str,
) -> ProviderResult<u8> {
    settings
        .integer(key)
        .and_then(|value| u8::try_from(value).ok())
        .filter(|value| *value <= 100)
        .ok_or_else(|| {
            ProviderError::new(
                ProviderErrorKind::Internal,
                "WELearn completion score range is unavailable or out of range",
            )
        })
}

#[allow(
    clippy::too_many_lines,
    reason = "the schema keeps all Master-overridable WELearn settings explicit and adjacent"
)]
pub(crate) fn runtime_settings_schema() -> ProviderRuntimeSettingsSchema {
    let provider_account_task_scopes = BTreeSet::from([
        ProviderSettingScope::Provider,
        ProviderSettingScope::ProviderAccount,
        ProviderSettingScope::Task,
    ]);
    ProviderRuntimeSettingsSchema {
        version: 7,
        definitions: vec![
            ProviderSettingDefinition {
                key: PROVIDER_EXECUTION_CONCURRENCY_KEY.to_owned(),
                display_name: "平台执行并发".to_owned(),
                description: "Core 同时向 WELearn 发起的执行数量上限；donor 可配置到 100，默认仍为 1。"
                    .to_owned(),
                kind: ProviderSettingKind::Integer {
                    minimum: 1,
                    maximum: 100,
                    step: 1,
                },
                default: ProviderSettingValue::Integer(1),
                scopes: BTreeSet::from([ProviderSettingScope::Provider]),
                core_behavior: Some(ProviderSettingCoreBehavior::ProviderExecutionConcurrency),
            },
            ProviderSettingDefinition {
                key: ACCOUNT_EXECUTION_CONCURRENCY_KEY.to_owned(),
                display_name: "账号执行并发".to_owned(),
                description: "同一 WELearn 账号的并行执行上限，可按账号或具体任务覆盖；donor 可配置到 100，默认串行。"
                    .to_owned(),
                kind: ProviderSettingKind::Integer {
                    minimum: 1,
                    maximum: 100,
                    step: 1,
                },
                default: ProviderSettingValue::Integer(1),
                scopes: provider_account_task_scopes.clone(),
                core_behavior: Some(ProviderSettingCoreBehavior::AccountExecutionConcurrency),
            },
            ProviderSettingDefinition {
                key: ACCOUNT_SCAN_INTERVAL_KEY.to_owned(),
                display_name: "账号巡查间隔".to_owned(),
                description: "Core Scheduler 周期检查 WELearn 课程、单元及任务变化的默认间隔。"
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
            ProviderSettingDefinition {
                key: DURATION_REPORT_MODE_KEY.to_owned(),
                display_name: "单次时长模式".to_owned(),
                description: "固定上报一个时长，或按 donor 行为从有界区间确定本次真实学习时长。"
                    .to_owned(),
                kind: ProviderSettingKind::Choice {
                    options: BTreeSet::from([
                        RESOURCE_SCORE_FIXED.to_owned(),
                        RESOURCE_SCORE_RANDOM_RANGE.to_owned(),
                    ]),
                },
                default: ProviderSettingValue::Choice(RESOURCE_SCORE_FIXED.to_owned()),
                scopes: provider_account_task_scopes.clone(),
                core_behavior: None,
            },
            ProviderSettingDefinition {
                key: DURATION_REPORT_SECONDS_KEY.to_owned(),
                display_name: "单次时长上报".to_owned(),
                description: "一次 WELearn 学习会话实际等待并上报的秒数，可按账号或具体任务覆盖。"
                    .to_owned(),
                kind: ProviderSettingKind::DurationSeconds {
                    minimum: MIN_DURATION_REPORT_SECONDS,
                    maximum: MAX_DURATION_REPORT_SECONDS,
                    step: 1,
                },
                default: ProviderSettingValue::DurationSeconds(600),
                scopes: provider_account_task_scopes.clone(),
                core_behavior: None,
            },
            ProviderSettingDefinition {
                key: DURATION_REPORT_MIN_SECONDS_KEY.to_owned(),
                display_name: "随机时长下限".to_owned(),
                description: "随机区间模式下本次实际学习并上报的最短秒数（含边界）。".to_owned(),
                kind: ProviderSettingKind::DurationSeconds {
                    minimum: MIN_DURATION_REPORT_SECONDS,
                    maximum: MAX_DURATION_REPORT_SECONDS,
                    step: 1,
                },
                default: ProviderSettingValue::DurationSeconds(300),
                scopes: provider_account_task_scopes.clone(),
                core_behavior: None,
            },
            ProviderSettingDefinition {
                key: DURATION_REPORT_MAX_SECONDS_KEY.to_owned(),
                display_name: "随机时长上限".to_owned(),
                description: "随机区间模式下本次实际学习并上报的最长秒数（含边界）。".to_owned(),
                kind: ProviderSettingKind::DurationSeconds {
                    minimum: MIN_DURATION_REPORT_SECONDS,
                    maximum: MAX_DURATION_REPORT_SECONDS,
                    step: 1,
                },
                default: ProviderSettingValue::DurationSeconds(900),
                scopes: provider_account_task_scopes.clone(),
                core_behavior: None,
            },
            ProviderSettingDefinition {
                key: DURATION_HEARTBEAT_INTERVAL_KEY.to_owned(),
                display_name: "时长心跳间隔".to_owned(),
                description: "WELearn 学习会话两次时长心跳之间的真实等待秒数，受安全范围限制。"
                    .to_owned(),
                kind: ProviderSettingKind::DurationSeconds {
                    minimum: MIN_DURATION_HEARTBEAT_SECONDS,
                    maximum: MAX_DURATION_HEARTBEAT_SECONDS,
                    step: 1,
                },
                default: ProviderSettingValue::DurationSeconds(60),
                scopes: provider_account_task_scopes.clone(),
                core_behavior: None,
            },
            ProviderSettingDefinition {
                key: RESOURCE_SCORE_MODE_KEY.to_owned(),
                display_name: "完成预设分数模式".to_owned(),
                description: "固定使用一个分数、按 Auto_WeLearn 均匀随机，或按当前 donor 的截断高斯分布确定每次执行的分数。"
                    .to_owned(),
                kind: ProviderSettingKind::Choice {
                    options: BTreeSet::from([
                        RESOURCE_SCORE_FIXED.to_owned(),
                        RESOURCE_SCORE_RANDOM_RANGE.to_owned(),
                        RESOURCE_SCORE_GAUSSIAN_RANDOM_RANGE.to_owned(),
                    ]),
                },
                default: ProviderSettingValue::Choice(RESOURCE_SCORE_FIXED.to_owned()),
                scopes: provider_account_task_scopes.clone(),
                core_behavior: None,
            },
            ProviderSettingDefinition {
                key: RESOURCE_SCORE_PERCENT_KEY.to_owned(),
                display_name: "完成预设分数".to_owned(),
                description: "固定模式下 WELearn SCO 完成度预设同时写入的整数分数。".to_owned(),
                kind: ProviderSettingKind::Integer {
                    minimum: 0,
                    maximum: 100,
                    step: 1,
                },
                default: ProviderSettingValue::Integer(100),
                scopes: provider_account_task_scopes.clone(),
                core_behavior: None,
            },
            ProviderSettingDefinition {
                key: RESOURCE_SCORE_MIN_PERCENT_KEY.to_owned(),
                display_name: "随机分数下限".to_owned(),
                description: "随机区间模式下允许写入的最低整数分数（含边界）。".to_owned(),
                kind: ProviderSettingKind::Integer {
                    minimum: 0,
                    maximum: 100,
                    step: 1,
                },
                default: ProviderSettingValue::Integer(70),
                scopes: provider_account_task_scopes.clone(),
                core_behavior: None,
            },
            ProviderSettingDefinition {
                key: RESOURCE_SCORE_MAX_PERCENT_KEY.to_owned(),
                display_name: "随机分数上限".to_owned(),
                description: "随机区间模式下允许写入的最高整数分数（含边界）。".to_owned(),
                kind: ProviderSettingKind::Integer {
                    minimum: 0,
                    maximum: 100,
                    step: 1,
                },
                default: ProviderSettingValue::Integer(100),
                scopes: provider_account_task_scopes,
                core_behavior: None,
            },
        ],
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use asterism_provider_api::ProviderRuntimeSettingsPatch;

    use super::*;

    #[test]
    fn duration_settings_are_bounded_and_resolve_task_overrides() {
        let schema = runtime_settings_schema();
        schema.validate().unwrap();
        let task = ProviderRuntimeSettingsPatch {
            schema_version: schema.version,
            values: BTreeMap::from([
                (
                    DURATION_REPORT_SECONDS_KEY.to_owned(),
                    ProviderSettingValue::DurationSeconds(1_200),
                ),
                (
                    DURATION_HEARTBEAT_INTERVAL_KEY.to_owned(),
                    ProviderSettingValue::DurationSeconds(45),
                ),
                (
                    ACCOUNT_EXECUTION_CONCURRENCY_KEY.to_owned(),
                    ProviderSettingValue::Integer(2),
                ),
                (
                    RESOURCE_SCORE_PERCENT_KEY.to_owned(),
                    ProviderSettingValue::Integer(82),
                ),
            ]),
        };
        let resolved = schema.resolve(None, None, Some(&task)).unwrap();
        assert_eq!(
            WellearnRuntimeSettings::resolve(&resolved).unwrap(),
            WellearnRuntimeSettings {
                duration_report: WellearnDurationTarget::Fixed(1_200),
                duration_heartbeat_interval_seconds: 45,
                resource_score: WellearnResourceScore::Fixed(82),
            }
        );
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

        let invalid = ProviderRuntimeSettingsPatch {
            schema_version: schema.version,
            values: BTreeMap::from([(
                DURATION_HEARTBEAT_INTERVAL_KEY.to_owned(),
                ProviderSettingValue::DurationSeconds(0),
            )]),
        };
        assert!(
            schema
                .validate_patch(ProviderSettingScope::Task, &invalid)
                .is_err()
        );

        let inverted = ProviderRuntimeSettingsPatch {
            schema_version: schema.version,
            values: BTreeMap::from([
                (
                    RESOURCE_SCORE_MODE_KEY.to_owned(),
                    ProviderSettingValue::Choice(RESOURCE_SCORE_RANDOM_RANGE.to_owned()),
                ),
                (
                    RESOURCE_SCORE_MIN_PERCENT_KEY.to_owned(),
                    ProviderSettingValue::Integer(90),
                ),
                (
                    RESOURCE_SCORE_MAX_PERCENT_KEY.to_owned(),
                    ProviderSettingValue::Integer(80),
                ),
            ]),
        };
        let resolved = schema.resolve(None, None, Some(&inverted)).unwrap();
        assert!(WellearnRuntimeSettings::resolve(&resolved).is_err());

        let inverted_duration = ProviderRuntimeSettingsPatch {
            schema_version: schema.version,
            values: BTreeMap::from([
                (
                    DURATION_REPORT_MODE_KEY.to_owned(),
                    ProviderSettingValue::Choice(RESOURCE_SCORE_RANDOM_RANGE.to_owned()),
                ),
                (
                    DURATION_REPORT_MIN_SECONDS_KEY.to_owned(),
                    ProviderSettingValue::DurationSeconds(1_200),
                ),
                (
                    DURATION_REPORT_MAX_SECONDS_KEY.to_owned(),
                    ProviderSettingValue::DurationSeconds(600),
                ),
            ]),
        };
        let resolved = schema
            .resolve(None, None, Some(&inverted_duration))
            .unwrap();
        assert!(WellearnRuntimeSettings::resolve(&resolved).is_err());
    }

    #[test]
    fn current_donor_short_session_and_one_second_heartbeat_are_expressible() {
        let schema = runtime_settings_schema();
        let task = ProviderRuntimeSettingsPatch {
            schema_version: schema.version,
            values: BTreeMap::from([
                (
                    DURATION_REPORT_SECONDS_KEY.to_owned(),
                    ProviderSettingValue::DurationSeconds(10),
                ),
                (
                    DURATION_HEARTBEAT_INTERVAL_KEY.to_owned(),
                    ProviderSettingValue::DurationSeconds(1),
                ),
            ]),
        };
        let resolved = schema.resolve(None, None, Some(&task)).unwrap();
        assert_eq!(
            WellearnRuntimeSettings::resolve(&resolved).unwrap(),
            WellearnRuntimeSettings {
                duration_report: WellearnDurationTarget::Fixed(10),
                duration_heartbeat_interval_seconds: 1,
                resource_score: WellearnResourceScore::Fixed(100),
            }
        );
    }

    #[test]
    fn current_donor_hundred_way_concurrency_is_expressible() {
        let schema = runtime_settings_schema();
        let provider = ProviderRuntimeSettingsPatch {
            schema_version: schema.version,
            values: BTreeMap::from([(
                PROVIDER_EXECUTION_CONCURRENCY_KEY.to_owned(),
                ProviderSettingValue::Integer(100),
            )]),
        };
        let account = ProviderRuntimeSettingsPatch {
            schema_version: schema.version,
            values: BTreeMap::from([(
                ACCOUNT_EXECUTION_CONCURRENCY_KEY.to_owned(),
                ProviderSettingValue::Integer(100),
            )]),
        };
        let resolved = schema
            .resolve(Some(&provider), Some(&account), None)
            .unwrap();
        assert_eq!(
            schema.execution_concurrency(&resolved).unwrap(),
            asterism_provider_api::ProviderExecutionConcurrency {
                provider: 100,
                account: 100,
            }
        );
    }

    #[test]
    fn current_and_historical_donor_score_distributions_are_explicit() {
        let schema = runtime_settings_schema();
        for (mode, expected) in [
            (
                RESOURCE_SCORE_RANDOM_RANGE,
                WellearnResourceScore::UniformRandomRange {
                    minimum: 70,
                    maximum: 90,
                },
            ),
            (
                RESOURCE_SCORE_GAUSSIAN_RANDOM_RANGE,
                WellearnResourceScore::GaussianRandomRange {
                    minimum: 70,
                    maximum: 90,
                },
            ),
        ] {
            let task = ProviderRuntimeSettingsPatch {
                schema_version: schema.version,
                values: BTreeMap::from([
                    (
                        RESOURCE_SCORE_MODE_KEY.to_owned(),
                        ProviderSettingValue::Choice(mode.to_owned()),
                    ),
                    (
                        RESOURCE_SCORE_MIN_PERCENT_KEY.to_owned(),
                        ProviderSettingValue::Integer(70),
                    ),
                    (
                        RESOURCE_SCORE_MAX_PERCENT_KEY.to_owned(),
                        ProviderSettingValue::Integer(90),
                    ),
                ]),
            };
            let resolved = schema.resolve(None, None, Some(&task)).unwrap();
            assert_eq!(
                WellearnRuntimeSettings::resolve(&resolved)
                    .unwrap()
                    .resource_score,
                expected
            );
        }
    }
}
