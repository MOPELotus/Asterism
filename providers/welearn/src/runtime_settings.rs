use std::collections::BTreeSet;

use asterism_provider_api::{
    ProviderError, ProviderErrorKind, ProviderResult, ProviderRuntimeSettingsSchema,
    ProviderSettingCoreBehavior, ProviderSettingDefinition, ProviderSettingKind,
    ProviderSettingScope, ProviderSettingValue, ResolvedProviderRuntimeSettings,
};

pub(crate) const PROVIDER_EXECUTION_CONCURRENCY_KEY: &str = "execution.provider_concurrency";
pub(crate) const ACCOUNT_EXECUTION_CONCURRENCY_KEY: &str = "execution.account_concurrency";
pub(crate) const ACCOUNT_SCAN_INTERVAL_KEY: &str = "discovery.scan_interval";
pub(crate) const DURATION_REPORT_SECONDS_KEY: &str = "duration.report_seconds";
pub(crate) const DURATION_HEARTBEAT_INTERVAL_KEY: &str = "duration.heartbeat_interval";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct WellearnRuntimeSettings {
    pub(crate) duration_report_seconds: u64,
    pub(crate) duration_heartbeat_interval_seconds: u64,
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
        let duration_report_seconds = settings
            .duration_seconds(DURATION_REPORT_SECONDS_KEY)
            .ok_or_else(|| {
                ProviderError::new(
                    ProviderErrorKind::Internal,
                    "WELearn duration report length is unavailable",
                )
            })?;
        let duration_heartbeat_interval_seconds = settings
            .duration_seconds(DURATION_HEARTBEAT_INTERVAL_KEY)
            .ok_or_else(|| {
                ProviderError::new(
                    ProviderErrorKind::Internal,
                    "WELearn duration heartbeat interval is unavailable",
                )
            })?;
        Ok(Self {
            duration_report_seconds,
            duration_heartbeat_interval_seconds,
        })
    }
}

pub(crate) fn runtime_settings_schema() -> ProviderRuntimeSettingsSchema {
    let provider_account_task_scopes = BTreeSet::from([
        ProviderSettingScope::Provider,
        ProviderSettingScope::ProviderAccount,
        ProviderSettingScope::Task,
    ]);
    ProviderRuntimeSettingsSchema {
        version: 1,
        definitions: vec![
            ProviderSettingDefinition {
                key: PROVIDER_EXECUTION_CONCURRENCY_KEY.to_owned(),
                display_name: "平台执行并发".to_owned(),
                description: "Core 同时向 WELearn 发起的执行数量上限；真实并发验证前默认 1。"
                    .to_owned(),
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
                description: "同一 WELearn 账号的并行执行上限，可按账号或具体任务覆盖；默认串行。"
                    .to_owned(),
                kind: ProviderSettingKind::Integer {
                    minimum: 1,
                    maximum: 4,
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
                key: DURATION_REPORT_SECONDS_KEY.to_owned(),
                display_name: "单次时长上报".to_owned(),
                description: "一次 WELearn 学习会话实际等待并上报的秒数，可按账号或具体任务覆盖。"
                    .to_owned(),
                kind: ProviderSettingKind::DurationSeconds {
                    minimum: 60,
                    maximum: 7_200,
                    step: 60,
                },
                default: ProviderSettingValue::DurationSeconds(600),
                scopes: provider_account_task_scopes.clone(),
                core_behavior: None,
            },
            ProviderSettingDefinition {
                key: DURATION_HEARTBEAT_INTERVAL_KEY.to_owned(),
                display_name: "时长心跳间隔".to_owned(),
                description: "WELearn 学习会话两次时长心跳之间的真实等待秒数，受安全范围限制。"
                    .to_owned(),
                kind: ProviderSettingKind::DurationSeconds {
                    minimum: 30,
                    maximum: 90,
                    step: 5,
                },
                default: ProviderSettingValue::DurationSeconds(60),
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
            ]),
        };
        let resolved = schema.resolve(None, None, Some(&task)).unwrap();
        assert_eq!(
            WellearnRuntimeSettings::resolve(&resolved).unwrap(),
            WellearnRuntimeSettings {
                duration_report_seconds: 1_200,
                duration_heartbeat_interval_seconds: 45,
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
                ProviderSettingValue::DurationSeconds(25),
            )]),
        };
        assert!(
            schema
                .validate_patch(ProviderSettingScope::Task, &invalid)
                .is_err()
        );
    }
}
