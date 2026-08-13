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
pub(crate) const DURATION_PROTOCOL_MODE_KEY: &str = "duration.protocol_mode";
pub(crate) const RESOURCE_SCORE_MODE_KEY: &str = "execution.completion_score_mode";
pub(crate) const RESOURCE_SCORE_PERCENT_KEY: &str = "execution.completion_score_percent";
pub(crate) const RESOURCE_SCORE_MIN_PERCENT_KEY: &str = "execution.completion_score_min_percent";
pub(crate) const RESOURCE_SCORE_MAX_PERCENT_KEY: &str = "execution.completion_score_max_percent";
pub(crate) const RESOURCE_COMPLETION_SEQUENCE_KEY: &str = "execution.completion_sequence";
pub(crate) const RESOURCE_COMPLETION_TIME_MODE_KEY: &str = "execution.completion_time_mode";
pub(crate) const RESOURCE_COMPLETION_CMI_FORMAT_KEY: &str = "execution.completion_cmi_format";
pub(crate) const RESOURCE_COMPLETION_WRITE_MODE_KEY: &str = "execution.completion_write_mode";
pub(crate) const RESOURCE_MUTATION_PROFILE_KEY: &str = "execution.mutation_profile";

const RESOURCE_SCORE_FIXED: &str = "fixed";
const RESOURCE_SCORE_RANDOM_RANGE: &str = "random_range";
const RESOURCE_SCORE_GAUSSIAN_RANDOM_RANGE: &str = "gaussian_random_range";
const RESOURCE_COMPLETION_SELECTED_SCORE: &str = "selected_score";
const RESOURCE_COMPLETION_CURRENT_DONOR_DUAL_SAVE: &str = "current_donor_dual_save_100";
const RESOURCE_COMPLETION_AUTO_TIME: &str = "auto";
const RESOURCE_COMPLETION_ZERO_TIME: &str = "zero_time";
const RESOURCE_COMPLETION_PRESERVE_FRESH_TIME: &str = "preserve_fresh_time";
const DURATION_PROTOCOL_PRESERVE_FRESH: &str = "preserve_fresh";
const DURATION_PROTOCOL_CLIENT_COUNTER: &str = "client_counter";
const DURATION_PROTOCOL_IMPLICIT_SERVER: &str = "implicit_server";
const RESOURCE_COMPLETION_CMI_JSON: &str = "json";
const RESOURCE_COMPLETION_CMI_INTERACTION_INFO: &str = "interaction_info_suffix";
const RESOURCE_COMPLETION_SET_THEN_SAVE: &str = "set_then_save";
const RESOURCE_COMPLETION_SAVE_ONLY: &str = "save_only";
const RESOURCE_MUTATION_CURRENT_FULL_SIMPLE: &str = "current_full_simple_referer";
const RESOURCE_MUTATION_LEGACY_MINIMAL_TASK: &str = "legacy_minimal_task_referer";
pub(crate) const MIN_DURATION_REPORT_SECONDS: u64 = 1;
pub(crate) const MAX_DURATION_REPORT_SECONDS: u64 = 7_200;
pub(crate) const MIN_DURATION_HEARTBEAT_SECONDS: u64 = 1;
pub(crate) const MAX_DURATION_HEARTBEAT_SECONDS: u64 = 90;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum WellearnDurationTarget {
    Fixed(u64),
    RandomRange { minimum: u64, maximum: u64 },
}

/// Donor-evidenced heartbeat and finalization wire protocol.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WellearnDurationProtocolMode {
    /// YZBRH sends an immediate preserved-state keep, interval keeps and a
    /// preserve-only final save.
    PreserveFresh,
    /// Current Fanyuchang sends one client-counter keep per second and leaves
    /// final completion to the following `ResourceExecution` step.
    ClientCounter,
    /// `Auto_WeLearn` waits before interval keeps, omits explicit time fields and
    /// finishes with a bare save.
    ImplicitServer,
}

impl WellearnDurationProtocolMode {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::PreserveFresh => DURATION_PROTOCOL_PRESERVE_FRESH,
            Self::ClientCounter => DURATION_PROTOCOL_CLIENT_COUNTER,
            Self::ImplicitServer => DURATION_PROTOCOL_IMPLICIT_SERVER,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum WellearnResourceScore {
    Fixed(u8),
    UniformRandomRange { minimum: u8, maximum: u8 },
    GaussianRandomRange { minimum: u8, maximum: u8 },
}

/// Donor-evidenced write sequence used by one `ResourceExecution`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WellearnResourceCompletionSequence {
    /// The YZBRH and `Auto_WeLearn` sequence ends after saving the selected score.
    SelectedScore,
    /// Current Fanyuchang performs the selected-score sequence, then issues a
    /// second save with score 100 and therefore has a final goal of 100.
    CurrentDonorDualSave100,
}

impl WellearnResourceCompletionSequence {
    pub(crate) const fn final_score(self, selected_score: u8) -> u8 {
        match self {
            Self::SelectedScore => selected_score,
            Self::CurrentDonorDualSave100 => 100,
        }
    }

    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::SelectedScore => RESOURCE_COMPLETION_SELECTED_SCORE,
            Self::CurrentDonorDualSave100 => RESOURCE_COMPLETION_CURRENT_DONOR_DUAL_SAVE,
        }
    }
}

/// Donor-evidenced CMI time fields used by `setscoinfo` during completion.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WellearnResourceCompletionTimeMode {
    /// Infer fresh-time only from the exact immutable duration-then-completion
    /// step context, otherwise use the normal donor zero-time document.
    Auto,
    /// Normal completion paths in all three donors write both time fields as zero.
    ZeroTime,
    /// Current Fanyuchang's duration-then-completion path carries the newly
    /// accumulated session and total time into the completion CMI document.
    PreserveFreshTime,
}

impl WellearnResourceCompletionTimeMode {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Auto => RESOURCE_COMPLETION_AUTO_TIME,
            Self::ZeroTime => RESOURCE_COMPLETION_ZERO_TIME,
            Self::PreserveFreshTime => RESOURCE_COMPLETION_PRESERVE_FRESH_TIME,
        }
    }
}

/// Donor-evidenced `setscoinfo.data` envelope.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WellearnResourceCompletionCmiFormat {
    /// Current Fanyuchang submits the serialized CMI JSON directly.
    Json,
    /// YZBRH and `Auto_WeLearn` append the literal `[INTERACTIONINFO]` marker.
    InteractionInfoSuffix,
}

impl WellearnResourceCompletionCmiFormat {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Json => RESOURCE_COMPLETION_CMI_JSON,
            Self::InteractionInfoSuffix => RESOURCE_COMPLETION_CMI_INTERACTION_INFO,
        }
    }
}

/// Donor-evidenced completion mutation family.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WellearnResourceCompletionWriteMode {
    /// Exercise completion donors submit CMI and then save progress/score.
    SetThenSave,
    /// `Auto_WeLearn`'s duration-completion path directly saves the final tuple.
    SaveOnly,
}

impl WellearnResourceCompletionWriteMode {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::SetThenSave => RESOURCE_COMPLETION_SET_THEN_SAVE,
            Self::SaveOnly => RESOURCE_COMPLETION_SAVE_ONLY,
        }
    }
}

/// Donor-evidenced `ResourceExecution` payload/Referer family.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WellearnResourceMutationProfile {
    /// Current Fanyuchang sends `classid`/`tid=-1` on start and uses the simple
    /// `StudyCourse` Referer for every mutation.
    CurrentFullSimpleReferer,
    /// YZBRH and `Auto_WeLearn` send only SCO identity on start and use the
    /// task-specific `StudyCourse` Referer for every mutation.
    LegacyMinimalTaskReferer,
}

impl WellearnResourceMutationProfile {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::CurrentFullSimpleReferer => RESOURCE_MUTATION_CURRENT_FULL_SIMPLE,
            Self::LegacyMinimalTaskReferer => RESOURCE_MUTATION_LEGACY_MINIMAL_TASK,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct WellearnRuntimeSettings {
    pub(crate) duration_report: WellearnDurationTarget,
    pub(crate) duration_heartbeat_interval_seconds: u64,
    pub(crate) duration_protocol_mode: WellearnDurationProtocolMode,
    pub(crate) resource_score: WellearnResourceScore,
    pub(crate) resource_completion_sequence: WellearnResourceCompletionSequence,
    pub(crate) resource_completion_time_mode: WellearnResourceCompletionTimeMode,
    pub(crate) resource_completion_cmi_format: WellearnResourceCompletionCmiFormat,
    pub(crate) resource_completion_write_mode: WellearnResourceCompletionWriteMode,
    pub(crate) resource_mutation_profile: WellearnResourceMutationProfile,
}

impl WellearnRuntimeSettings {
    #[allow(
        clippy::too_many_lines,
        reason = "resolution validates every interdependent WELearn execution setting in one boundary"
    )]
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
        let duration_protocol_mode = match settings.choice(DURATION_PROTOCOL_MODE_KEY) {
            Some(DURATION_PROTOCOL_PRESERVE_FRESH) => WellearnDurationProtocolMode::PreserveFresh,
            Some(DURATION_PROTOCOL_CLIENT_COUNTER) => WellearnDurationProtocolMode::ClientCounter,
            Some(DURATION_PROTOCOL_IMPLICIT_SERVER) => WellearnDurationProtocolMode::ImplicitServer,
            _ => {
                return Err(ProviderError::new(
                    ProviderErrorKind::Internal,
                    "WELearn duration protocol mode is unavailable",
                ));
            }
        };
        if (duration_protocol_mode == WellearnDurationProtocolMode::ClientCounter
            && duration_heartbeat_interval_seconds != 1)
            || (duration_protocol_mode == WellearnDurationProtocolMode::ImplicitServer
                && duration_heartbeat_interval_seconds != 60)
        {
            return Err(ProviderError::new(
                ProviderErrorKind::Internal,
                "WELearn donor duration protocol requires its evidenced heartbeat interval",
            ));
        }
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
        let resource_completion_sequence = match settings.choice(RESOURCE_COMPLETION_SEQUENCE_KEY) {
            Some(RESOURCE_COMPLETION_SELECTED_SCORE) => {
                WellearnResourceCompletionSequence::SelectedScore
            }
            Some(RESOURCE_COMPLETION_CURRENT_DONOR_DUAL_SAVE) => {
                WellearnResourceCompletionSequence::CurrentDonorDualSave100
            }
            _ => {
                return Err(ProviderError::new(
                    ProviderErrorKind::Internal,
                    "WELearn completion sequence is unavailable",
                ));
            }
        };
        let resource_completion_time_mode = match settings.choice(RESOURCE_COMPLETION_TIME_MODE_KEY)
        {
            Some(RESOURCE_COMPLETION_AUTO_TIME) => WellearnResourceCompletionTimeMode::Auto,
            Some(RESOURCE_COMPLETION_ZERO_TIME) => WellearnResourceCompletionTimeMode::ZeroTime,
            Some(RESOURCE_COMPLETION_PRESERVE_FRESH_TIME) => {
                WellearnResourceCompletionTimeMode::PreserveFreshTime
            }
            _ => {
                return Err(ProviderError::new(
                    ProviderErrorKind::Internal,
                    "WELearn completion time mode is unavailable",
                ));
            }
        };
        let resource_completion_cmi_format =
            match settings.choice(RESOURCE_COMPLETION_CMI_FORMAT_KEY) {
                Some(RESOURCE_COMPLETION_CMI_JSON) => WellearnResourceCompletionCmiFormat::Json,
                Some(RESOURCE_COMPLETION_CMI_INTERACTION_INFO) => {
                    WellearnResourceCompletionCmiFormat::InteractionInfoSuffix
                }
                _ => {
                    return Err(ProviderError::new(
                        ProviderErrorKind::Internal,
                        "WELearn completion CMI format is unavailable",
                    ));
                }
            };
        let resource_completion_write_mode = match settings
            .choice(RESOURCE_COMPLETION_WRITE_MODE_KEY)
        {
            Some(RESOURCE_COMPLETION_SET_THEN_SAVE) => {
                WellearnResourceCompletionWriteMode::SetThenSave
            }
            Some(RESOURCE_COMPLETION_SAVE_ONLY) => WellearnResourceCompletionWriteMode::SaveOnly,
            _ => {
                return Err(ProviderError::new(
                    ProviderErrorKind::Internal,
                    "WELearn completion write mode is unavailable",
                ));
            }
        };
        let resource_mutation_profile = match settings.choice(RESOURCE_MUTATION_PROFILE_KEY) {
            Some(RESOURCE_MUTATION_CURRENT_FULL_SIMPLE) => {
                WellearnResourceMutationProfile::CurrentFullSimpleReferer
            }
            Some(RESOURCE_MUTATION_LEGACY_MINIMAL_TASK) => {
                WellearnResourceMutationProfile::LegacyMinimalTaskReferer
            }
            _ => {
                return Err(ProviderError::new(
                    ProviderErrorKind::Internal,
                    "WELearn resource mutation profile is unavailable",
                ));
            }
        };
        Ok(Self {
            duration_report,
            duration_heartbeat_interval_seconds,
            duration_protocol_mode,
            resource_score,
            resource_completion_sequence,
            resource_completion_time_mode,
            resource_completion_cmi_format,
            resource_completion_write_mode,
            resource_mutation_profile,
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
        version: 13,
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
                key: DURATION_PROTOCOL_MODE_KEY.to_owned(),
                display_name: "时长协议模式".to_owned(),
                description: "选择 YZBRH 新鲜状态保留、当前 donor 客户端秒计数，或 Auto_WeLearn 隐式服务端计时协议。"
                    .to_owned(),
                kind: ProviderSettingKind::Choice {
                    options: BTreeSet::from([
                        DURATION_PROTOCOL_PRESERVE_FRESH.to_owned(),
                        DURATION_PROTOCOL_CLIENT_COUNTER.to_owned(),
                        DURATION_PROTOCOL_IMPLICIT_SERVER.to_owned(),
                    ]),
                },
                default: ProviderSettingValue::Choice(DURATION_PROTOCOL_PRESERVE_FRESH.to_owned()),
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
                scopes: provider_account_task_scopes.clone(),
                core_behavior: None,
            },
            ProviderSettingDefinition {
                key: RESOURCE_COMPLETION_SEQUENCE_KEY.to_owned(),
                display_name: "完成写入序列".to_owned(),
                description: "以所选分数结束，或复现当前 donor 在所选分数写入后再保存一次 100 分的双 save 序列。"
                    .to_owned(),
                kind: ProviderSettingKind::Choice {
                    options: BTreeSet::from([
                        RESOURCE_COMPLETION_SELECTED_SCORE.to_owned(),
                        RESOURCE_COMPLETION_CURRENT_DONOR_DUAL_SAVE.to_owned(),
                    ]),
                },
                default: ProviderSettingValue::Choice(
                    RESOURCE_COMPLETION_SELECTED_SCORE.to_owned(),
                ),
                scopes: provider_account_task_scopes.clone(),
                core_behavior: None,
            },
            ProviderSettingDefinition {
                key: RESOURCE_COMPLETION_TIME_MODE_KEY.to_owned(),
                display_name: "完成写入时间语义".to_owned(),
                description: "普通完成写入 donor 的零时间 CMI，或在时长后完成时把新鲜 CMI 的 session/total time 原样带入 setscoinfo。"
                    .to_owned(),
                kind: ProviderSettingKind::Choice {
                    options: BTreeSet::from([
                        RESOURCE_COMPLETION_AUTO_TIME.to_owned(),
                        RESOURCE_COMPLETION_ZERO_TIME.to_owned(),
                        RESOURCE_COMPLETION_PRESERVE_FRESH_TIME.to_owned(),
                    ]),
                },
                default: ProviderSettingValue::Choice(RESOURCE_COMPLETION_AUTO_TIME.to_owned()),
                scopes: provider_account_task_scopes.clone(),
                core_behavior: None,
            },
            ProviderSettingDefinition {
                key: RESOURCE_COMPLETION_CMI_FORMAT_KEY.to_owned(),
                display_name: "完成 CMI 信封".to_owned(),
                description: "发送当前 donor 的纯 JSON，或 YZBRH/Auto_WeLearn 的 JSON+[INTERACTIONINFO] 信封。"
                    .to_owned(),
                kind: ProviderSettingKind::Choice {
                    options: BTreeSet::from([
                        RESOURCE_COMPLETION_CMI_JSON.to_owned(),
                        RESOURCE_COMPLETION_CMI_INTERACTION_INFO.to_owned(),
                    ]),
                },
                default: ProviderSettingValue::Choice(RESOURCE_COMPLETION_CMI_JSON.to_owned()),
                scopes: provider_account_task_scopes.clone(),
                core_behavior: None,
            },
            ProviderSettingDefinition {
                key: RESOURCE_COMPLETION_WRITE_MODE_KEY.to_owned(),
                display_name: "完成写入模式".to_owned(),
                description: "执行练习 donor 的 setscoinfo 后 save，或 Auto_WeLearn 时长完成路径的直接 save。"
                    .to_owned(),
                kind: ProviderSettingKind::Choice {
                    options: BTreeSet::from([
                        RESOURCE_COMPLETION_SET_THEN_SAVE.to_owned(),
                        RESOURCE_COMPLETION_SAVE_ONLY.to_owned(),
                    ]),
                },
                default: ProviderSettingValue::Choice(RESOURCE_COMPLETION_SET_THEN_SAVE.to_owned()),
                scopes: provider_account_task_scopes.clone(),
                core_behavior: None,
            },
            ProviderSettingDefinition {
                key: RESOURCE_MUTATION_PROFILE_KEY.to_owned(),
                display_name: "完成变更报文".to_owned(),
                description: "选择当前 donor 全链路的完整启动身份+简单 Referer，或 YZBRH/Auto_WeLearn 的最小启动身份+任务 Referer。"
                    .to_owned(),
                kind: ProviderSettingKind::Choice {
                    options: BTreeSet::from([
                        RESOURCE_MUTATION_CURRENT_FULL_SIMPLE.to_owned(),
                        RESOURCE_MUTATION_LEGACY_MINIMAL_TASK.to_owned(),
                    ]),
                },
                default: ProviderSettingValue::Choice(
                    RESOURCE_MUTATION_CURRENT_FULL_SIMPLE.to_owned(),
                ),
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
    #[allow(
        clippy::too_many_lines,
        reason = "the test checks schema bounds, resolution and Core-owned settings together"
    )]
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
                duration_protocol_mode: WellearnDurationProtocolMode::PreserveFresh,
                resource_score: WellearnResourceScore::Fixed(82),
                resource_completion_sequence: WellearnResourceCompletionSequence::SelectedScore,
                resource_completion_time_mode: WellearnResourceCompletionTimeMode::Auto,
                resource_completion_cmi_format: WellearnResourceCompletionCmiFormat::Json,
                resource_completion_write_mode: WellearnResourceCompletionWriteMode::SetThenSave,
                resource_mutation_profile:
                    WellearnResourceMutationProfile::CurrentFullSimpleReferer,
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
                duration_protocol_mode: WellearnDurationProtocolMode::PreserveFresh,
                resource_score: WellearnResourceScore::Fixed(100),
                resource_completion_sequence: WellearnResourceCompletionSequence::SelectedScore,
                resource_completion_time_mode: WellearnResourceCompletionTimeMode::Auto,
                resource_completion_cmi_format: WellearnResourceCompletionCmiFormat::Json,
                resource_completion_write_mode: WellearnResourceCompletionWriteMode::SetThenSave,
                resource_mutation_profile:
                    WellearnResourceMutationProfile::CurrentFullSimpleReferer,
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
    fn all_donor_duration_wire_modes_are_explicit_and_interval_bound() {
        let schema = runtime_settings_schema();
        for (mode, interval, expected) in [
            (
                DURATION_PROTOCOL_PRESERVE_FRESH,
                30,
                WellearnDurationProtocolMode::PreserveFresh,
            ),
            (
                DURATION_PROTOCOL_CLIENT_COUNTER,
                1,
                WellearnDurationProtocolMode::ClientCounter,
            ),
            (
                DURATION_PROTOCOL_IMPLICIT_SERVER,
                60,
                WellearnDurationProtocolMode::ImplicitServer,
            ),
        ] {
            let task = ProviderRuntimeSettingsPatch {
                schema_version: schema.version,
                values: BTreeMap::from([
                    (
                        DURATION_PROTOCOL_MODE_KEY.to_owned(),
                        ProviderSettingValue::Choice(mode.to_owned()),
                    ),
                    (
                        DURATION_HEARTBEAT_INTERVAL_KEY.to_owned(),
                        ProviderSettingValue::DurationSeconds(interval),
                    ),
                ]),
            };
            let resolved = schema.resolve(None, None, Some(&task)).unwrap();
            assert_eq!(
                WellearnRuntimeSettings::resolve(&resolved)
                    .unwrap()
                    .duration_protocol_mode,
                expected
            );
        }

        for (mode, wrong_interval) in [
            (DURATION_PROTOCOL_CLIENT_COUNTER, 60),
            (DURATION_PROTOCOL_IMPLICIT_SERVER, 1),
        ] {
            let task = ProviderRuntimeSettingsPatch {
                schema_version: schema.version,
                values: BTreeMap::from([
                    (
                        DURATION_PROTOCOL_MODE_KEY.to_owned(),
                        ProviderSettingValue::Choice(mode.to_owned()),
                    ),
                    (
                        DURATION_HEARTBEAT_INTERVAL_KEY.to_owned(),
                        ProviderSettingValue::DurationSeconds(wrong_interval),
                    ),
                ]),
            };
            let resolved = schema.resolve(None, None, Some(&task)).unwrap();
            assert!(WellearnRuntimeSettings::resolve(&resolved).is_err());
        }
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

    #[test]
    fn current_donor_dual_save_sequence_is_explicit() {
        let schema = runtime_settings_schema();
        let task = ProviderRuntimeSettingsPatch {
            schema_version: schema.version,
            values: BTreeMap::from([(
                RESOURCE_COMPLETION_SEQUENCE_KEY.to_owned(),
                ProviderSettingValue::Choice(
                    RESOURCE_COMPLETION_CURRENT_DONOR_DUAL_SAVE.to_owned(),
                ),
            )]),
        };
        let resolved = schema.resolve(None, None, Some(&task)).unwrap();
        assert_eq!(
            WellearnRuntimeSettings::resolve(&resolved)
                .unwrap()
                .resource_completion_sequence,
            WellearnResourceCompletionSequence::CurrentDonorDualSave100
        );
    }

    #[test]
    fn historical_interaction_info_cmi_envelope_is_explicit() {
        let schema = runtime_settings_schema();
        let task = ProviderRuntimeSettingsPatch {
            schema_version: schema.version,
            values: BTreeMap::from([(
                RESOURCE_COMPLETION_CMI_FORMAT_KEY.to_owned(),
                ProviderSettingValue::Choice(RESOURCE_COMPLETION_CMI_INTERACTION_INFO.to_owned()),
            )]),
        };
        let resolved = schema.resolve(None, None, Some(&task)).unwrap();
        assert_eq!(
            WellearnRuntimeSettings::resolve(&resolved)
                .unwrap()
                .resource_completion_cmi_format,
            WellearnResourceCompletionCmiFormat::InteractionInfoSuffix
        );
    }

    #[test]
    fn auto_duration_completion_save_only_mode_is_explicit() {
        let schema = runtime_settings_schema();
        let task = ProviderRuntimeSettingsPatch {
            schema_version: schema.version,
            values: BTreeMap::from([(
                RESOURCE_COMPLETION_WRITE_MODE_KEY.to_owned(),
                ProviderSettingValue::Choice(RESOURCE_COMPLETION_SAVE_ONLY.to_owned()),
            )]),
        };
        let resolved = schema.resolve(None, None, Some(&task)).unwrap();
        assert_eq!(
            WellearnRuntimeSettings::resolve(&resolved)
                .unwrap()
                .resource_completion_write_mode,
            WellearnResourceCompletionWriteMode::SaveOnly
        );
    }

    #[test]
    fn historical_minimal_mutation_profile_is_explicit() {
        let schema = runtime_settings_schema();
        let task = ProviderRuntimeSettingsPatch {
            schema_version: schema.version,
            values: BTreeMap::from([(
                RESOURCE_MUTATION_PROFILE_KEY.to_owned(),
                ProviderSettingValue::Choice(RESOURCE_MUTATION_LEGACY_MINIMAL_TASK.to_owned()),
            )]),
        };
        let resolved = schema.resolve(None, None, Some(&task)).unwrap();
        assert_eq!(
            WellearnRuntimeSettings::resolve(&resolved)
                .unwrap()
                .resource_mutation_profile,
            WellearnResourceMutationProfile::LegacyMinimalTaskReferer
        );
    }

    #[test]
    fn duration_then_completion_time_semantics_are_explicit() {
        let schema = runtime_settings_schema();
        let task = ProviderRuntimeSettingsPatch {
            schema_version: schema.version,
            values: BTreeMap::from([(
                RESOURCE_COMPLETION_TIME_MODE_KEY.to_owned(),
                ProviderSettingValue::Choice(RESOURCE_COMPLETION_PRESERVE_FRESH_TIME.to_owned()),
            )]),
        };
        let resolved = schema.resolve(None, None, Some(&task)).unwrap();
        assert_eq!(
            WellearnRuntimeSettings::resolve(&resolved)
                .unwrap()
                .resource_completion_time_mode,
            WellearnResourceCompletionTimeMode::PreserveFreshTime
        );
    }

    #[test]
    fn donor_duration_completion_profiles_are_composable() {
        let schema = runtime_settings_schema();
        for (score, time_mode, expected_score, expected_time_mode) in [
            (
                0,
                RESOURCE_COMPLETION_ZERO_TIME,
                0,
                WellearnResourceCompletionTimeMode::ZeroTime,
            ),
            (
                100,
                RESOURCE_COMPLETION_PRESERVE_FRESH_TIME,
                100,
                WellearnResourceCompletionTimeMode::PreserveFreshTime,
            ),
        ] {
            let task = ProviderRuntimeSettingsPatch {
                schema_version: schema.version,
                values: BTreeMap::from([
                    (
                        RESOURCE_SCORE_PERCENT_KEY.to_owned(),
                        ProviderSettingValue::Integer(score),
                    ),
                    (
                        RESOURCE_COMPLETION_SEQUENCE_KEY.to_owned(),
                        ProviderSettingValue::Choice(RESOURCE_COMPLETION_SELECTED_SCORE.to_owned()),
                    ),
                    (
                        RESOURCE_COMPLETION_TIME_MODE_KEY.to_owned(),
                        ProviderSettingValue::Choice(time_mode.to_owned()),
                    ),
                ]),
            };
            let resolved =
                WellearnRuntimeSettings::resolve(&schema.resolve(None, None, Some(&task)).unwrap())
                    .unwrap();
            assert_eq!(
                resolved.resource_score,
                WellearnResourceScore::Fixed(expected_score)
            );
            assert_eq!(
                resolved.resource_completion_sequence,
                WellearnResourceCompletionSequence::SelectedScore
            );
            assert_eq!(resolved.resource_completion_time_mode, expected_time_mode);
        }
    }
}
