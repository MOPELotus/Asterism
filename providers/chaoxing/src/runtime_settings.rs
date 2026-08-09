use std::collections::BTreeSet;

use asterism_provider_api::{
    ProviderError, ProviderErrorKind, ProviderResult, ProviderRuntimeSettingsSchema,
    ProviderSettingCoreBehavior, ProviderSettingDefinition, ProviderSettingKind,
    ProviderSettingScope, ProviderSettingValue, ResolvedProviderRuntimeSettings,
};

pub(crate) const PROVIDER_EXECUTION_CONCURRENCY_KEY: &str = "execution.provider_concurrency";
pub(crate) const ACCOUNT_EXECUTION_CONCURRENCY_KEY: &str = "execution.account_concurrency";
pub(crate) const VIDEO_PLAYBACK_RATE_KEY: &str = "video.playback_rate";
pub(crate) const VIDEO_PROGRESS_INTERVAL_KEY: &str = "video.progress_interval";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ChaoxingRuntimeSettings {
    pub(crate) video_playback_rate_millis: u16,
    pub(crate) video_progress_interval_seconds: u64,
}

impl ChaoxingRuntimeSettings {
    pub(crate) fn resolve(settings: &ResolvedProviderRuntimeSettings) -> ProviderResult<Self> {
        runtime_settings_schema()
            .validate_resolved(settings)
            .map_err(|_| {
                ProviderError::new(
                    ProviderErrorKind::Internal,
                    "Chaoxing received incompatible runtime settings",
                )
            })?;
        let playback_rate = settings
            .decimal_millis(VIDEO_PLAYBACK_RATE_KEY)
            .ok_or_else(|| {
                ProviderError::new(
                    ProviderErrorKind::Internal,
                    "Chaoxing video playback rate is unavailable",
                )
            })?;
        let playback_rate = u16::try_from(playback_rate).map_err(|_| {
            ProviderError::new(
                ProviderErrorKind::Internal,
                "Chaoxing video playback rate cannot be represented",
            )
        })?;
        let progress_interval_seconds = settings
            .duration_seconds(VIDEO_PROGRESS_INTERVAL_KEY)
            .ok_or_else(|| {
                ProviderError::new(
                    ProviderErrorKind::Internal,
                    "Chaoxing video progress interval is unavailable",
                )
            })?;
        Ok(Self {
            video_playback_rate_millis: playback_rate,
            video_progress_interval_seconds: progress_interval_seconds,
        })
    }
}

pub(crate) fn runtime_settings_schema() -> ProviderRuntimeSettingsSchema {
    let scopes = BTreeSet::from([
        ProviderSettingScope::Provider,
        ProviderSettingScope::ProviderAccount,
        ProviderSettingScope::Task,
    ]);
    ProviderRuntimeSettingsSchema {
        version: 3,
        definitions: vec![
            ProviderSettingDefinition {
                key: PROVIDER_EXECUTION_CONCURRENCY_KEY.to_owned(),
                display_name: "平台执行并发".to_owned(),
                description: "Core 同时向超星发起的执行数量上限；未完成真实并发验证前默认 1。"
                    .to_owned(),
                kind: ProviderSettingKind::Integer {
                    minimum: 1,
                    maximum: 32,
                    step: 1,
                },
                default: ProviderSettingValue::Integer(1),
                scopes: BTreeSet::from([ProviderSettingScope::Provider]),
                core_behavior: Some(ProviderSettingCoreBehavior::ProviderExecutionConcurrency),
            },
            ProviderSettingDefinition {
                key: ACCOUNT_EXECUTION_CONCURRENCY_KEY.to_owned(),
                display_name: "账号执行并发".to_owned(),
                description: "同一超星账号的并行执行上限，可按账号或具体任务覆盖；默认串行。"
                    .to_owned(),
                kind: ProviderSettingKind::Integer {
                    minimum: 1,
                    maximum: 4,
                    step: 1,
                },
                default: ProviderSettingValue::Integer(1),
                scopes: scopes.clone(),
                core_behavior: Some(ProviderSettingCoreBehavior::AccountExecutionConcurrency),
            },
            ProviderSettingDefinition {
                key: VIDEO_PLAYBACK_RATE_KEY.to_owned(),
                display_name: "视频播放速度".to_owned(),
                description: "视频进度按真实经过时间乘以该速度推进，最高 2.0 倍。".to_owned(),
                kind: ProviderSettingKind::DecimalMillis {
                    minimum: 1_000,
                    maximum: 2_000,
                    step: 50,
                },
                default: ProviderSettingValue::DecimalMillis(1_000),
                scopes: scopes.clone(),
                core_behavior: None,
            },
            ProviderSettingDefinition {
                key: VIDEO_PROGRESS_INTERVAL_KEY.to_owned(),
                display_name: "视频进度上报间隔".to_owned(),
                description: "两次视频进度上报之间的目标视频时长，受 Provider 安全范围限制。"
                    .to_owned(),
                kind: ProviderSettingKind::DurationSeconds {
                    minimum: 30,
                    maximum: 90,
                    step: 5,
                },
                default: ProviderSettingValue::DurationSeconds(60),
                scopes,
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
    fn video_settings_are_bounded_and_resolve_task_overrides() {
        let schema = runtime_settings_schema();
        schema.validate().unwrap();
        let task = ProviderRuntimeSettingsPatch {
            schema_version: schema.version,
            values: BTreeMap::from([
                (
                    VIDEO_PLAYBACK_RATE_KEY.to_owned(),
                    ProviderSettingValue::DecimalMillis(1_500),
                ),
                (
                    ACCOUNT_EXECUTION_CONCURRENCY_KEY.to_owned(),
                    ProviderSettingValue::Integer(3),
                ),
            ]),
        };
        let resolved = schema.resolve(None, None, Some(&task)).unwrap();
        assert_eq!(
            ChaoxingRuntimeSettings::resolve(&resolved).unwrap(),
            ChaoxingRuntimeSettings {
                video_playback_rate_millis: 1_500,
                video_progress_interval_seconds: 60,
            }
        );
        assert_eq!(
            schema.execution_concurrency(&resolved).unwrap(),
            asterism_provider_api::ProviderExecutionConcurrency {
                provider: 1,
                account: 3,
            }
        );

        let invalid = ProviderRuntimeSettingsPatch {
            schema_version: schema.version,
            values: BTreeMap::from([(
                VIDEO_PLAYBACK_RATE_KEY.to_owned(),
                ProviderSettingValue::DecimalMillis(2_050),
            )]),
        };
        assert!(
            schema
                .validate_patch(ProviderSettingScope::Task, &invalid)
                .is_err()
        );
    }
}
