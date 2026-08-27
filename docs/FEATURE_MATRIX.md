# 功能接线矩阵

本表是桌面版必须保留的最低功能面，也是从 `0.0.1` 和 donor 接线时的防遗漏清单。迁移
期间在“状态”列累计填写 `upstream-proven`、`ported-unverified`、`desktop-wired`、
`fixture-verified`、`live-read`、`live-write` 或 `blocked`，不得只写笼统的“完成”。

初始状态统一为 `planned`。上游具备的能力在审计引用后标为 `upstream-proven`；Asterism
以前新增的能力标为 `ported-unverified`。两类能力接入桌面入口后再追加 `desktop-wired`。

## 桌面公共能力

| 功能 | 必须保留的行为 | 状态 |
|---|---|---|
| Profile | 四个平台各自支持多个明文本地 Profile 和独立会话 | `desktop-wired + fixture-verified` |
| 登录 | 复用平台原生密码、Cookie、Token、OAuth/微信流程及自动续期 | `upstream-proven + desktop-wired + fixture-verified` |
| 清单 | 手动刷新课程、平台原生任务层级和独立正式任务 | `upstream-proven + desktop-wired + fixture-verified` |
| 执行 | 单项和手动批量执行，账号隔离，进度、日志和取消 | `desktop-wired + fixture-verified` |
| 状态 | 长扫描保守游标与重试，一个阻断账号不影响其他账号 | `desktop-wired + fixture-verified` |
| 通知 | 可选的成功/失败终态通知，不包含自动巡检 | `desktop-wired + fixture-verified` |
| 错误 | 区分凭据、会话、验证码、网络、协议和上游服务错误 | `desktop-wired + fixture-verified` |

## `chaoxing`

| 功能 | 必须保留的行为 | 状态 |
|---|---|---|
| 课程界面 | 官方顺序章节/知识点树、全选，作业与考试独立列表 | `upstream-proven + desktop-wired + fixture-verified` |
| 完成状态 | 与官方状态一致，保留已完成历史、模块权重和成绩缺口 | `upstream-proven + desktop-wired + fixture-verified` |
| 音视频 | 视频和音频回退，可配置倍速/并发，执行后重新读取状态 | `upstream-proven + desktop-wired` |
| 文档/阅读 | 文档完成、普通阅读和累计阅读时长计分 | `upstream-proven + desktop-wired` |
| 直播 | 固定 1x 计算真实时长，不信任无效加速产生的假完成 | `upstream-proven + desktop-wired` |
| 章节答题 | 使用现有 work 路径读取、选答、编码和提交 | `upstream-proven + desktop-wired + fixture-verified` |
| 独立作业 | 填写和保存，最终提交前必须由本地操作者确认 | `ported-unverified + desktop-wired + fixture-verified` |
| 考试 | 限时/次数任务进入前确认，草稿，二次确认，原生提交 | `upstream-proven + desktop-wired + fixture-verified` |
| 讨论/签到 | 保留有证据且影响完成度的操作；生成文本须相关且自然 | `upstream-proven + desktop-wired` |
| 成绩构成 | 展示模块权重、当前成绩、完成条件和剩余缺口 | `upstream-proven + desktop-wired + fixture-verified` |
| 挑战模式 | 只执行已开放节点；三次有界重试、一次 Sol xhigh 升级、然后明确失败 | `ported-unverified + desktop-wired + fixture-verified` |
| 验证码 | 复用进度验证、Exam 滑块、活体/人脸路径，自动退避且不全局阻塞 | `upstream-proven + desktop-wired + fixture-verified` |
| 历史扫描 | 所有可登录账号的可恢复后台扫描，显示覆盖、游标和重试 | `ported-unverified + desktop-wired + fixture-verified` |

## `welearn`

| 功能 | 必须保留的行为 | 状态 |
|---|---|---|
| 登录 | donor SSO 密码/Cookie、会话初始化和有限课程探测重试 | `upstream-proven + desktop-wired + fixture-verified` |
| 清单 | 课程、单元、SCO 叶节点及官方完成状态 | `upstream-proven + desktop-wired + fixture-verified` |
| 完成率 | donor 直接完成/正确率路径，不受题型影响 | `upstream-proven + desktop-wired + fixture-verified` |
| 时长 | 独立 donor 时长路径和设置，不与完成率隐式合并 | `upstream-proven + desktop-wired + fixture-verified` |
| 验证 | 重新读取官方 SCO 状态；验证失败不盲目重放写入 | `upstream-proven + desktop-wired + fixture-verified` |

## `uai`

| 功能 | 必须保留的行为 | 状态 |
|---|---|---|
| 登录/清单 | API 与必要浏览器流程，课程、必做章节和原生活动类型 | `upstream-proven + desktop-wired + fixture-verified` |
| 完成 | 单账号串行执行所有影响完成度的必做活动 | `upstream-proven + desktop-wired + fixture-verified` |
| 时长 | 时长/驻留单独执行并重新读取 | `upstream-proven + desktop-wired + fixture-verified` |
| 讨论 | 获取新鲜题目/上下文，在平台页提供本次纯文本草稿输入并原生发布 | `upstream-proven + desktop-wired` |
| 特殊活动 | Talk、口语、上传仅在确定不影响完成度时忽略，否则走现有路径 | `upstream-proven + desktop-wired` |
| AI 边界 | donor 通用 AI 默认关闭；确需答案时使用共享本地策略 | `desktop-wired + fixture-verified` |

## `cidaren`

| 功能 | 必须保留的行为 | 状态 |
|---|---|---|
| 登录 | 维护版微信链接/二维码回调，不要求 MITM 或抓包 | `upstream-proven + desktop-wired + fixture-verified` |
| 清单 | 单元自学、班级学习和测试任务，保留原生结构 | `upstream-proven + desktop-wired + fixture-verified` |
| 执行 | 单账号严格串行，保持 topic code 等前后依赖；桌面通过短生命周期 loopback bridge 接入全局题库/AI | `upstream-proven + desktop-wired + fixture-verified` |
| 历史答案 | 将 `answer_lib` 延迟绑定到完整新题，仅作为低可信证据 | `upstream-proven + desktop-wired + fixture-verified` |
| 限时答题 | Instant 模型优先；65% 后准备回退，85% 采用最佳结果，末 15% 提交/重试 | `upstream-proven + desktop-wired + fixture-verified` |
| 不限时答题 | 使用一个配置模型和可选自然延时，不做无意义多模型并发 | `upstream-proven + desktop-wired + fixture-verified` |
| 结果 | 将进度、成绩和正误观察写回答案证据循环 | `upstream-proven + desktop-wired + fixture-verified` |

## 题库与答题

`welearn` 和 `uai` 的直接完成路径默认绕过题库；题库主要服务 `chaoxing`、`cidaren` 和确实需要答案
的特殊活动。

| 功能 | 必须保留的行为 | 状态 |
|---|---|---|
| 范围 | 当前部署环境全局复用，并按 Provider 分区 | `desktop-wired + fixture-verified` |
| 题目身份 | 保存完整题干祖先、共享材料、混编内容、附件、挖空/下划线和原生题型；过滤答案/状态等易变字段 | `desktop-wired + fixture-verified` |
| 排除项 | 题号、远端题目 ID、选项 ID/字母和显示顺序不得定义可复用身份 | `desktop-wired + fixture-verified` |
| 选项绑定 | 按规范化选项内容和媒体语义保存，唯一时才绑定回本次 Provider ID | `desktop-wired + fixture-verified` |
| 富媒体 | 保留文本、图片、公式、音视频、文件、共享选项、下划线和编号空格的顺序与归属 | `desktop-wired + fixture-verified` |
| 复杂题 | 连线、排序、组合、阅读、完形、计算、口语/听力和 Provider-native 题可操作 | `upstream-proven + desktop-wired` |
| 证据 | 保存平台、历史、人工和 AI 候选及每次正确/错误/未验证观察 | `desktop-wired + fixture-verified` |
| 复用 | 全部观察为正确时可复用；有对有错或多个正解时进入仲裁；纯错误为负证据 | `desktop-wired + fixture-verified` |
| 省钱组合 | Native、精确缓存、可选便宜验证、Luna 限时、Terra 不限时；Router 失败才使用配置的 DeepSeek 国内灾备 | `desktop-wired + fixture-verified` |
| GPT-only | Native 标答直接使用；精确缓存作为证据；限时 Luna，不限时 Sol xhigh，无国内灾备 | `desktop-wired + fixture-verified` |
| 模型输入 | 包含全部文本、材料、媒体归属、附件、原生题型、空格/下划线和已有证据 | `desktop-wired + fixture-verified` |
| 主观文本 | 相关、自然的纯文本；无 Markdown/系统/测试痕迹；差异化并检测重复 | `ported-unverified + desktop-wired` |
| 正式草稿 | 本地/远端只保存草稿，可编辑答案和来源，明确提交并回收结果 | `ported-unverified + desktop-wired + fixture-verified` |

## Checkpoint 规则

C2 完成时，每一行必须至少具有：

- `upstream-proven + desktop-wired`；或
- `ported-unverified + desktop-wired + fixture-verified`；或
- `blocked`，并记录明确原因和用户可见行为。

C4 只集中补充真实 `live-read`。首版不要求对写入能力补 `live-write`，但不得在文档或 UI
中把未验证写成已验证。任何功能不得仅因缺少真实账号或写入条件而从矩阵消失。
