use std::collections::BTreeSet;

use asterism_provider_api::{
    ProviderRuntimeSettingsSchema, ProviderSettingCoreBehavior, ProviderSettingDefinition,
    ProviderSettingKind, ProviderSettingScope, ProviderSettingValue,
};

pub(crate) const PROVIDER_EXECUTION_CONCURRENCY_KEY: &str = "execution.provider_concurrency";
pub(crate) const ACCOUNT_EXECUTION_CONCURRENCY_KEY: &str = "execution.account_concurrency";
pub(crate) const ACCOUNT_SCAN_INTERVAL_KEY: &str = "discovery.scan_interval";
pub(crate) const BROWSER_RESIDENCE_SECONDS_KEY: &str = "browser.residence_seconds";
pub(crate) const BROWSER_PLAY_VIDEO_KEY: &str = "browser.play_video";
pub(crate) const DISCUSSION_EMPTY_MODE_KEY: &str = "discussion.empty_mode";
pub(crate) const PRESET_EMPTY_MODE_KEY: &str = "preset.empty_mode";

pub(crate) fn runtime_settings_schema() -> ProviderRuntimeSettingsSchema {
    let provider_account_task_scopes = BTreeSet::from([
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
                description: "Core 同时向 UAI 发起的执行数量上限；真实并发验证前默认 1。"
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
                description: "同一 UAI 账号的并行执行上限，可按账号或具体任务覆盖；默认串行。"
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
                description: "Core Scheduler 周期检查 UAI 课程、单元及任务变化的默认间隔。"
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
                key: BROWSER_RESIDENCE_SECONDS_KEY.to_owned(),
                display_name: "页面驻留时长".to_owned(),
                description:
                    "BrowserBridge 在当前任务的 Micro、Tab 与 Task 页面间分配的总驻留秒数。"
                        .to_owned(),
                kind: ProviderSettingKind::DurationSeconds {
                    minimum: 60,
                    maximum: 28_800,
                    step: 60,
                },
                default: ProviderSettingValue::DurationSeconds(3_600),
                scopes: provider_account_task_scopes.clone(),
                core_behavior: None,
            },
            ProviderSettingDefinition {
                key: BROWSER_PLAY_VIDEO_KEY.to_owned(),
                display_name: "页面视频播放".to_owned(),
                description: "BrowserBridge 驻留时是否播放并保活当前页面的视频。".to_owned(),
                kind: ProviderSettingKind::Boolean,
                default: ProviderSettingValue::Boolean(false),
                scopes: provider_account_task_scopes.clone(),
                core_behavior: None,
            },
            empty_mode_definition(
                DISCUSSION_EMPTY_MODE_KEY,
                "讨论空执行模式",
                "直接标记使用无题体；占位答案复现 donor 的 instanceId=0 写法。",
                provider_account_task_scopes.clone(),
            ),
            empty_mode_definition(
                PRESET_EMPTY_MODE_KEY,
                "学习任务空执行模式",
                "无题标记使用原生空体；占位答案复现 Apache donor 的题型展开。",
                provider_account_task_scopes,
            ),
        ],
    }
}

fn empty_mode_definition(
    key: &str,
    display_name: &str,
    description: &str,
    scopes: BTreeSet<ProviderSettingScope>,
) -> ProviderSettingDefinition {
    ProviderSettingDefinition {
        key: key.to_owned(),
        display_name: display_name.to_owned(),
        description: description.to_owned(),
        kind: ProviderSettingKind::Choice {
            options: BTreeSet::from(["marker".to_owned(), "placeholder".to_owned()]),
        },
        default: ProviderSettingValue::Choice("marker".to_owned()),
        scopes,
        core_behavior: None,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use asterism_provider_api::ProviderRuntimeSettingsPatch;

    use super::*;

    #[test]
    fn browser_settings_are_bounded_and_resolve_task_overrides() {
        let schema = runtime_settings_schema();
        schema.validate().unwrap();
        let task = ProviderRuntimeSettingsPatch {
            schema_version: schema.version,
            values: BTreeMap::from([
                (
                    BROWSER_RESIDENCE_SECONDS_KEY.to_owned(),
                    ProviderSettingValue::DurationSeconds(1_200),
                ),
                (
                    BROWSER_PLAY_VIDEO_KEY.to_owned(),
                    ProviderSettingValue::Boolean(true),
                ),
                (
                    ACCOUNT_EXECUTION_CONCURRENCY_KEY.to_owned(),
                    ProviderSettingValue::Integer(2),
                ),
            ]),
        };
        let resolved = schema.resolve(None, None, Some(&task)).unwrap();

        assert_eq!(
            resolved.duration_seconds(BROWSER_RESIDENCE_SECONDS_KEY),
            Some(1_200)
        );
        assert_eq!(resolved.boolean(BROWSER_PLAY_VIDEO_KEY), Some(true));
        assert_eq!(resolved.choice(DISCUSSION_EMPTY_MODE_KEY), Some("marker"));
        assert_eq!(resolved.choice(PRESET_EMPTY_MODE_KEY), Some("marker"));
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
                BROWSER_RESIDENCE_SECONDS_KEY.to_owned(),
                ProviderSettingValue::DurationSeconds(30),
            )]),
        };
        assert!(
            schema
                .validate_patch(ProviderSettingScope::Task, &invalid)
                .is_err()
        );
    }
}
