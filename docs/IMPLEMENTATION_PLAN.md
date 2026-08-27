# Asterism 本地桌面版实施计划

## 1. 定位与迁移方法

Asterism 是在可信 Windows 机器上运行的本地桌面工具，不是面向公网的多用户服务。本地
操作者天然拥有原 Master 的全部产品能力，并可管理 `chaoxing`、`welearn`、`uai` 和 `cidaren` 各自
任意多个平台账号 Profile。

本次工作是**接线迁移**，不是四个平台的第二次重写：

1. 从 `0.0.1` 取回仍有产品价值的 Provider、Worker、题库、AI、任务与草稿代码。
2. 能直接改引用就直接引用；只有边界不兼容时才增加薄适配层。
3. 原本必须经过用户、权限、代操作或 Service Token 的路径改为固定本地操作者上下文，
   或直接调用其下层服务。
4. 保留各 donor 的原语言、运行环境、调用顺序和平台私有语义。
5. 不因改成桌面程序而重新分析协议、重写 Worker 或设计统一 Provider SDK。

`0.0.1` 是实现和迁移来源，不是要整体复制的架构。旧 WebUI、公开 API、权限、计费、
Scheduler 和分布式恢复层不得被顺带迁回。

### 移除的产品面

- Asterism 注册、登录、用户、角色、权限和代操作。
- QQ 身份绑定、点数、计费、充值、价格表和用量结算。
- Service Token、公开 REST/OpenAPI 和公网部署入口。
- 自动巡检、定时调度以及与本地工具无关的分布式 Job/Recovery。

### 保留的产品面

- 四个平台任意多个 Profile、平台原生登录和会话续期。
- 课程、章节、任务、作业、考试、刷新、单次执行和手动批量执行。
- 所有具备上游证据或已有实现的平台完成能力及私有配置。
- 全局题库、答案证据、AI 缓存、模型组合和复杂题。
- `chaoxing` 历史扫描、挑战模式、验证码、作业/考试草稿和提交确认。
- `welearn` 完成率/时长、`uai` 必做活动、`cidaren` 串行答题。
- 进度、结构化日志、取消以及可选的执行成功/失败通知。

## 2. 运行结构

```text
PyQt Fluent 桌面 UI
        |
本地控制器 / 旧能力薄适配层
        |---------------- 本地题库 SQLite
        |---------------- Profile / 状态 / 草稿 / 日志文件
        |
现有 Provider / Worker / 子进程
        |-- chaoxing：多个已审计 donor 与本地扩展
        |-- welearn：上游完成率与时长路径
        |-- uai：上游 API / 浏览器路径
        `-- cidaren：维护版后端与本地答案策略
```

桌面层只负责选择、启动、取消、日志、进度、设置和人工确认，不实现 Provider 协议。优先
保留现有进程和调用边界；只有现有边界无法被本地 UI 调用时，才增加最小 JSON/JSONL 适配。

如果旧代码需要 `actor`、`owner` 或权限上下文，本地模式提供唯一固定操作者，或绕过权限
外壳直接调用业务服务。不得在桌面版重新实现一套伪用户系统。

### 本地数据目录

```text
accounts/<provider>/<profile>.json   用户可编辑的明文账号与平台设置
state/<provider>/<profile>/          Cookie、Token、游标等生成状态
drafts/<provider>/<profile>/         待确认的正式作业/考试草稿
logs/<provider>/<profile>/           每次运行的 UTF-8 日志
data/question-bank.sqlite            题库、证据、AI 缓存和正式草稿索引
config.local.json                    模型、界面默认值和通知配置
```

配置、生成状态和日志分离。写入采用临时文件、刷新和原子替换。SQLite 不保存 Asterism
用户、权限、点数、调度或 Service Token。

## 3. 功能与验证状态

实现状态和验证状态必须分开，统一使用以下标记：

| 状态 | 含义 |
|---|---|
| `planned` | 尚未接入新桌面主线 |
| `upstream-proven` | 上游存在稳定实现，可作为能力可用的依据 |
| `ported-unverified` | Asterism 已有实现，但尚未做真实写入验证 |
| `desktop-wired` | 已可从新本地控制层或桌面 UI 到达 |
| `fixture-verified` | Fixture、模拟响应或离线调用链验证通过 |
| `live-read` | 新桌面版使用真实账号完成只读验证 |
| `live-write` | 新桌面版使用真实账号完成写入验证 |
| `blocked` | 存在明确且已记录的平台、上游、许可或环境阻断 |

一个功能可以同时具有多个状态，例如 `upstream-proven + desktop-wired + live-read`。
首个桌面版允许上游写入能力停留在 `upstream-proven + desktop-wired`，允许 Asterism 自研
写入能力停留在 `ported-unverified + desktop-wired + fixture-verified`。不得把它们宣传成
已经完成真实写入验证，也不得仅因暂时不能实测而重新实现或删除。

## 4. 工作阶段与 Checkpoint

### C0：路线与范围冻结（已完成）

- 建立无历史的新桌面主线。
- 将旧架构保存在 `0.0.1`。
- 锁定产品边界、功能矩阵和便携发布方向。

Checkpoint：新 `master` 只包含规划文件，不混入凭据、本地数据或旧运营层实现。

### C1：复原代码与本地运行骨架（已完成）

- 从 `0.0.1` 恢复实际需要的 Provider、Worker、题库、AI、任务和草稿模块。
- 接入本地 Profile、状态、日志、草稿和 SQLite。
- 用固定本地操作者或下层直调替代旧权限入口。
- 接通进程启动、进度、取消、错误分类、脱敏和进程树清理。
- 提供不依赖 UI 的本地调用/诊断入口。

Checkpoint：四个平台现有能力都能从本地入口加载；一个 Runner 的失败不会终止其他
Profile 或桌面进程；不需要 Asterism 用户、Token 或 Scheduler。

当前证据：四个固定 donor 均通过入口 hash 校验和无账号 `health`；原 Worker 53 项测试与
本地控制层与 GUI/AI/扫描控制层 58 项测试通过。Profile、会话、配置、草稿、题库、日志、超时、取消和 Windows
进程树兜底均已接入，且数据库中不存在用户、权限、计费或调度表。

### C2：四平台与共享能力接线

这不是重新开发阶段。逐项检查 `FEATURE_MATRIX.md`：

1. 来源代码、固定 donor 和运行资源存在。
2. 依赖能够加载，调用顺序没有因适配而改变。
3. 本地 Profile 能提供所需凭据和平台设置。
4. 调用不再依赖旧用户、权限、计费或代操作系统。
5. 进度、结果、交互请求和错误能返回桌面层。
6. 至少具备上游证据、旧实现证据或 Fixture 验证之一。

Checkpoint：矩阵全部达到 `upstream-proven` 或 `ported-unverified`，并全部
`desktop-wired`；不得存在只能从旧 WebUI 使用的遗留功能。

### C3：桌面 UI 与交互收口

- 实现 PyQt6 Fluent 模块化应用壳和首次启动流程。
- 首次启动显示本地数据目录、凭据边界、只读验证顺序和草稿确认规则；确认状态写入本地配置，
  不创建额外用户或权限实体。
- 完成四个平台、多 Profile、课程/任务、刷新、执行、取消、进度和日志页面。
- 完成独立作业/考试、待确认草稿、编辑和明确提交。
- 完成题库、证据、模型组合、扫描状态、重试和设置页面。
- 完成手动批量执行及可选成功/失败通知。
- 批量执行的 `chaoxing` 普通任务并发数由本次执行输入，不在桌面层设置任意产品上限；`uai` 与
  `cidaren` 仍由 Worker 强制单账号串行，`welearn` 遵循 donor 的执行约束。
- 批量执行按任务标识转发 Worker 的实时进度和日志；每次单项/批量执行可选择 `economy` 或
  `gpt_only` 答案组合，并由本地配置和 Profile 设置继续覆盖 Provider 默认值。
- `uai` 平台页提供可选的本次讨论/主观纯文本输入；留空时保持 donor 原生行为，填写时通过
  `generated_text` 设置传入 Worker，仅作用于本次执行，不自动写入 Profile。
- 支持跟随系统、浅色、深色主题；启动前配置 Qt 高 DPI 缩放，窗口尺寸和表格布局随显示器调整。
- 可选终态通知仅用于单项/批量执行，使用本地配置的无 shell 命令，仅传递事件、平台 ID、操作名和脱敏摘要；
  课程刷新、题目扫描和设置保存不会触发通知。
- `chaoxing` 章节任务执行前，桌面控制层会先读取题目并按全局题库优先、AI 其次准备答案；
  Worker 仍负责最终的题型编码、平台提交和完成状态读取。
- 挑战知识点若完成状态仍未达标，Worker 先执行其有界重试并返回升级标记；控制层消费该标记，
  仅再用 `gpt_only` 的 Sol xhigh 答案执行一次升级尝试，并通过内部标记防止无限重试。
- `cidaren` 任务执行时，桌面层按单次任务启动短生命周期的 loopback answer bridge：新题先走
  全局题库/AI 策略，Worker 仍保留 donor 的选项映射、提交顺序和限时回退；平台返回的正误
  观察写回本地证据。正式作业/考试只使用草稿中经人工确认的答案，不自动启动答题 bridge；
  普通任务的 bridge 使用随机 bearer ticket、仅绑定 `127.0.0.1`，任务结束即关闭。
- AI 组合通过 OpenAI-compatible Responses 请求接入：默认 `economy` 使用 `gpt_router`，
  `gpt_only` 使用 `gpt_site`；限时/不限时分别配置模型与 reasoning effort，国内 endpoint
  只作为主请求失败、超时或不可用时的灾备（默认模型名为 `deepseek-chat`，可在本地配置覆盖）。
  响应和用量写入本地 `ai_cache`，不默认保存到云端；富媒体题目保留图片/文件归属。

Checkpoint：所有矩阵能力均能从桌面 UI 到达。优先使用组件测试、数据驱动页面检查和
自动化 UI smoke，不要求机械地逐按钮人工验收。

### C4：集中真实只读验证

所有接线和 UI 完成后，一次性进行（当前可自动化的全号静默扫描仅覆盖 `chaoxing`）：

- 四个平台登录、会话续期、课程和任务同步。
- 各平台官方完成状态、时长、必做活动和历史结果读取。
- `chaoxing` 章节、作业、考试、成绩构成和可恢复扫描。
- 可从 `chaoxing` 页面手动启动全部已启用 Profile 的串行扫描；每个 Profile 独立保存游标，
  失败只标记该 Profile 并继续其余账号。
- 题库导入、内容匹配、答案证据与缓存读取。
- 多 Profile 切换、失败隔离、日志和断点续扫。

耗时很长的全量扫描只需验证启动、游标持久化、失败重试和续接，不等待整个账号完成。
本阶段不执行真实平台写入；写入能力保留其 `upstream-proven` 或
`ported-unverified` 状态。

Checkpoint：能够自动化读取的项目全部扫描；不能读取的项目有明确原因和 UI 状态；日志
和扫描产物不泄漏凭据。

### C5：Windows 便携发布

- 使用 Nuitka standalone 构建 x64 和 ARM64 ZIP。
- 打包桌面程序、现有 Runner、固定 donor、Qt/Python/native 运行库和浏览器资源。
- 在干净 Windows runner 上验证解压即用、中文路径、SQLite、子进程、取消和浏览器。
- 完成许可证、敏感信息和构建机路径审计。

当前已实现双架构 Workflow、Nuitka standalone 构建入口、冻结 Worker 可执行入口、资源
allowlist、SHA-256 manifest 与解压启动 smoke。由于固定 `welearn` donor 的许可仍为
`NOASSERTION`，构建会在编译前明确阻断；这是发布合规阻断，不是平台功能接线缺失。

Checkpoint：目标机器无需 Python、Node、Rust、管理员权限、服务注册或 PATH 修改即可
启动；两个架构均通过 `WINDOWS_PORTABLE_RELEASE.md` 的验收。

## 5. 版本节点

- `v0.2.0-alpha.1`：C1 完成。
- `v0.2.0-alpha.2`：C2 四平台和共享能力全部接线。
- `v0.2.0-beta.1`：C3 完成，桌面功能面收口。
- `v0.2.0-rc.1`：C4、C5 完成，生成双架构候选包。
- `v0.2.0`：首个正式本地桌面版本。

版本号表示桌面产品成熟度，不改变历史 `0.0.1`、`0.1.0` 分支或已有 tag。

## 6. 对外文档收口

开发期间以本文件和功能矩阵作为内部事实源。Beta 前整理简体中文对外文档：

- `README.md`：定位、截图、下载和快速开始。
- `docs/QUICK_START.md`：首次启动、数据目录和添加 Profile。
- `docs/USER_GUIDE.md`：完整桌面操作。
- `docs/PROVIDERS.md`：四个平台能力、限制和验证状态。
- `docs/QUESTION_BANK_AND_AI.md`：题库、证据、缓存、模型组合和隐私。
- `docs/DATA_AND_SECURITY.md`：明文凭据、本地备份、迁移和日志脱敏。
- `docs/TROUBLESHOOTING.md`：登录、验证码、网络、浏览器与协议变化。
- `docs/DEVELOPMENT.md`：模块、Runner 边界、测试和贡献说明。
- `docs/THIRD_PARTY_NOTICES.md`：donor 来源、版本、版权和许可证。
- `CHANGELOG.md`：用户可见的版本变化。

用户文档以操作结果为中心，不记录迁移争论和内部历史。产品稳定后再增加英文入口，避免
快速开发期同时维护两套容易失真的文档。
