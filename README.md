# Asterism

[![CI](https://github.com/MOPELotus/Asterism/actions/workflows/ci.yml/badge.svg)](https://github.com/MOPELotus/Asterism/actions/workflows/ci.yml)
![Rust 1.97+](https://img.shields.io/badge/Rust-1.97%2B-000000?logo=rust)
![Status: Phase 0](https://img.shields.io/badge/status-Phase%200-yellow)

> Unified learning task orchestration across multiple platforms, powered by Rust.

Asterism 是一个基于 Rust 的多平台学习任务聚合与调度服务。它将不同平台的课程、任务、进度和执行能力归一到统一的领域模型中，并由同一套 Core 为 CLI、WebUI 和 Asterism-Plugin 提供能力。

项目的核心目标是聚合并自动化学习平台中机械、重复、低价值且占用大量时间的操作，
尽可能完整复用和迁移成熟上游已经实现的能力，把用户时间从平台操作中释放出来。
用户是否启用某项能力属于产品调用与授权问题；Provider 是否准确实现 donor 已知能力属于
协议和工程问题，两者严格分离。Provider 不替用户做价值判断，也不因为能力会修改
completion/progress/score、自动答题或提交、需要浏览器/Capture，或者实现复杂而裁剪能力。

项目不以“接口返回成功”作为 Provider 已受支持的标准。一个 Provider 只有在能力覆盖、状态映射、错误分类、Fixture、真实账号验证和运行记录均达到要求后，才会被标记为可用。

> [!IMPORTANT]
> Asterism 正处于 **Phase 0 Core 基座开发阶段**，尚未宣布任何学习平台 Provider 已完成。当前代码用于架构建设和开发验证，不代表可用于生产环境。

## 设计目标

- 坚持 upstream-first：上游有可靠能力证据就审计、语义映射并完整迁移，不增加主观价值筛选步骤；
- 以统一模型聚合多平台课程、任务、进度和截止时间；
- 将远端任务事实与本地调度、审批、重试和执行状态严格分离；
- 通过 Capability 模型描述 Provider 的真实能力，而不是伪造统一接口；
- 支持自动执行、延迟审批、手动审批和仅通知等策略；
- 以事务化点数、调度、执行租约和 Event Outbox 保证关键状态一致性；
- 为敏感凭据、非幂等 mutation、浏览器自动化和公网部署设置明确可靠性与安全边界；
- 第一批 Provider 完成后立即准备 OpenAPI Client Generation，并建设正式 WebUI 与 Asterism-Plugin；第二批完成后再冻结兼容基线。

## 当前进度

Phase 0 已建立并持续完善以下基础：

- Rust 2024 workspace 与清晰的 crate 边界；
- Task / Execution、Remote State / Orchestration State 等独立领域模型；
- 有界且脱敏的 Question / AnswerCandidate / SubmissionDraft 领域模型、独立 AnswerSource、按 Task/Provider 绑定的不可变 QuestionSnapshot，以及按 Snapshot/Question 绑定并整批事务持久化的多来源候选答案；
- owner-scoped、整批校验且只读的 Provider 题目发现/解析编排，以及成功后原子落盘并返回快照身份的 HTTP/CLI 读取入口；
- 绑定显式 QuestionSnapshot、仅接受 ProviderNative 来源且与执行/提交策略解耦的候选答案解析编排；
- 使用 Task Read 权限、显式 Task/Snapshot 双重身份且不借用执行入口的 Provider-native 候选答案 HTTP/CLI 解析入口；
- owner-scoped、再次核验 Task/Snapshot 绑定且不调用 Provider 的已持久化多来源候选答案 HTTP/CLI 读取入口；
- 固定 Manual 来源与 Core provenance、显式绑定 owner/Task/Snapshot/Question 且拒绝 Unknown 的手工候选答案 Core/HTTP/CLI 写入链；
- 将 Selected、Conflict、Missing 明确分离且不能把未决状态伪装成已选答案的可审计 AnswerResolutionPlan 领域模型；
- 仅在全部已知归一化答案达成共识时推荐候选、冲突与缺失保持未决且不持久化 winner 的来源中立 Core AnswerResolver；
- 按显式 Task/Snapshot 只读生成保守 AnswerResolutionPlan 的 HTTP/OpenAPI/CLI 审阅入口；
- 排除快照临时身份但纳入完整脱敏题面语义、用于保守 LocalCache 匹配的版本化 QuestionContentFingerprint；
- 为新快照持久化强题目指纹，并仅按 owner/Task/时间/双侧唯一性读取历史直接答案证据的 LocalCache 存储边界；
- 将精确历史直接证据重新绑定到当前 Question、由 Core 固定 LocalCache provenance 且顺序重试幂等的保守答案缓存导入编排；
- 以显式 Task/Snapshot 导入 LocalCache 候选、返回本次新增证据且不隐式解析答案的 HTTP/OpenAPI/CLI 入口；
- 通过数据库复合外键绑定 QuestionSnapshot / Question / AnswerCandidate、整份校验后原子持久化且 owner-scoped 读取的不可变 SubmissionDraft；
- 要求每题恰好一个显式已存候选、仅生成安全 Provider payload preview 且不触发远端写入的 SubmissionDraft Core 编排；
- 使用 Task Read 权限和显式 Task/Snapshot/Candidate 身份构建草稿、按 Task/Snapshot/Draft 精确读取草稿的 HTTP/CLI 入口；
- 不以远端回执代替验证的 SubmissionResult / VerificationSnapshot 领域模型，以及互相独立的 SubmissionExecute / SubmissionVerify Provider 槽位；
- 通过数据库复合外键绑定 SubmissionDraft / Execution / ExecutionAttempt / Task、整份验证后不可变持久化且 owner-scoped 读取的 SubmissionResult；
- 将一个不可变 SubmissionDraft 最多冻结到一个 Execution、先持久化有界 Attempt Receipt 再独立 Verify，并在任何歧义或 Pending 后只验证而绝不重提的 Submission worker；
- 将调用方显式选择的可执行 Capability 子集规范化并冻结到 `Execution.requested_capabilities`，幂等重放必须匹配同一子集，Worker 不会因 Task 同时广告其他写入能力而顺带执行；
- 以 `ExecutionVerify` 标记至少一个具有目标验证路径的非幂等 TaskExecution action，并由 Provider 按冻结 action 决定是否需要 `verify_execution`：每个 Execution 最多调用一次远端变更，随后按同一设置和目标 fresh rebind 验证；返回歧义、未达目标或崩溃统一进入无重放路径的验证恢复；已知 Completed 只复核、不写入；
- 按 Task / QuestionSnapshot / SubmissionDraft / SubmissionResult 完整身份链读取不可变题目、草稿和验证结果的 owner-scoped HTTP 审计入口，以及现有 Draft/Result CLI 入口；
- Capability-based Provider API、Metadata 与 Registry，包括与远端执行解耦的题目发现、解析、Provider-native 候选答案解析和只读提交草稿构建槽位；
- 课程发现到后续 capability 的短命、脱敏且不持久化路由上下文；
- Account > Provider > Global 覆盖的 NetworkProfile 与集中 HTTP Client 构建；
- 先完整采集后事务提交的 Provider Course / Task inventory 编排；
- Provider 执行调度、可续租执行状态机、正式测评保护与崩溃恢复语义；
- SQLite migration、WAL 策略、事务仓储和并发测试；
- 版本化任务指纹、脱敏扫描快照、类型化差异与事务化扫描入库；
- 幂等执行请求、执行租约、周期扫描物化、隔离认领的 worker 与事务化 Event Outbox；
- owner-scoped 且带持久幂等回执的 Task 批准、取消、延迟、忽略 Core Action；批准只释放编排等待态而不绕过正式测评保护，延迟同步更新 Execution/Job，取消只接受未领取的远端工作并原子撤销 Job、Execution 与积分预留；
- 点数 grant / reserve / commit / release 流程、不可变流水，以及 Quote + Reserve + Execution 原子调度边界；
- SecretStore 抽象、Argon2id 密码、服务端 Session、scoped Service Token 与登录限速；
- password/hash 永不出管理边界、带 revision 冲突检测和最后活跃 Master 保护的用户管理 API，以及按权限全局/owner 隔离的脱敏 Audit 查询；Service Token 增加仅返回元数据的分页管理面，owner-bound 管理令牌只能列出、派生和撤销同一 owner 的令牌；
- 内部 Axum API、OpenAPI 入口、健康检查与 HTTP-only CLI；OpenAPI 统一声明稳定错误响应、`X-Request-ID` 与限流 `Retry-After`，并以契约测试校验 operationId、路径参数、请求体、响应及本地 `$ref` 的客户端生成完整性；健康/认证、Provider 管理与 scan report、Master 分层运行设置、credit、任务/执行/SSE、题目/候选/解析计划、Submission Draft/Result 及 owner-scoped BrowserBridge session policy 等主管理面已声明强类型成功响应，并由离线 Rust 导出、固定版本 Hey API SDK 生成、关键类型断言和 strict TypeScript 编译组成 CI 闭环；Capture/Bootstrap 与后续 BrowserBridge helper execution 的完整强类型客户端契约同属当前实现范围，不再作为后置例外；
- Auth Bootstrap 配对、状态事件、Provider 服务端验证与原子凭据提交；
- owner-scoped 人工扫描 API / CLI 与同事务扫描审计；
- Chaoxing 能力级上游审计、独立 Chapter / Resource / Work / Exam TaskInventory、按稳定身份重新发现并经 Core 校验的 TaskDetail / Progress API 与 CLI、四类任务的模块化只读进度复核、明确区分 Chapter Work 与独立 Work / Exam 的题面离线解析、独立 Work 原生题目读取与无值 SubmissionBuild，以及限定单选/多选/判断的单次原生 SubmissionExecute 和逐题服务端答案 SubmissionVerify；Exam Pending/goTest 任务现已接入独立的 cover -> one-shot start -> attempt-bound mobile Question 读取链和 donor 字段族的 value-free SubmissionBuild，验证码/人脸/考试码分支进入 typed BrowserRequired；提交 JSON 仅形成 Receipt，prompt/editor 不算完成，歧义恢复不重提；Document / Read / Video 原生执行、有界 Work 详情状态复核、Cookie 自动续登与显式开发验证入口均保持 Development；Exam answer/save/submit、Chapter Work、Live、QR、BrowserBridge/Capture fallback 及其他 donor 已知能力继续纳入当前开发，真实账号只读/另行授权写入验证仍待完成；
- WELearn clean-room 已覆盖三个 pinned donor 的 Password/OIDC、ImportedCookie 与 Capture-assisted/external-browser Cookie 认证，Core 持久会话解析/续登，Course/Unit/SCO inventory、fresh TaskDetail/CMI Progress、canonical DurationRead，fixed/random DurationReport，以及直接 `start -> setscoinfo -> save` completion/progress/score ResourceExecution；ResourceExecution 使用冻结设置重算同一目标并 fresh CMI 验证 exact completion/progress/score，DurationReport 保持独立的 preservation/time-change 验证且歧义写入不重放；平台/账号/任务级运行设置、Capture v4/v5 acquisition-method 配方与响应 readiness 已接入。验证码/短信在没有 donor solver 协议证据时保持 typed HumanRequired，而不是能力政策裁剪；Provider batch plan 已冻结 Unit/SCO membership、顺序、visibility/completion 观测及 Auto 聚合与子项目标，fixed/range duration 与 score 仍由各 child Execution 的不可变设置解析，剩余共享 parent/child durable batch/API 与真实账号脱敏验证继续推进；
- 多 Capability 执行由 Provider 显式给出阶段顺序，Core 在创建 Execution 时原子冻结阶段计划，并在每个远端调用前持久化 `issued` Attempt 绑定、按阶段独立验证后再推进；崩溃恢复只核验已经发出的阶段，不会重放可能已接受的非幂等写入。WELearn 因而可表达 donor 的“先时长、后完成/进度/分数”组合，而不是把两种不同成功语义塞进一次不可恢复调用；Provider 也可对精确 action set 声明有证据的 `NotOpen` 执行例外，Core 默认拒绝且始终阻止 `Expired` / `Removed`；
- UAI clean-room 已覆盖 Password / ImportedToken / Capture 认证编排、Core scoped 持久会话解析与原子自动续登、CourseResource / Unit / Section / Micro / Group inventory、fresh detail/progress/duration、完整加密 Question/Answer/Submission 与已验证 ResourceExecution；Provider-private Browser residence、discussion、upload 和媒体边界继续映射 donor 真实语义。共享 BrowserBridge 执行器、持久 Artifact/Draft、外部 AnswerResolve 下载与模型编排、DurationReport 的浏览器执行和真实账号验证仍在推进，不把共享 Core Gap 当成 Provider 结束理由；
- Cidaren 已覆盖 ImportedToken/Composite 会话、bounded `Student/Main` validation、Course/class/study inventory、fresh detail/progress/duration、当前 `jv=99` HKDF/AES-GCM Question/Answer/SubmissionBuild，以及 Provider-private Start/Verify/Submit/Skip/ChoseWord 状态机与 fresh SubmissionVerify；随机 marker 微信 OAuth、hash-only pending/CAS、一次性 callback 消费、P-256/ECDH/HKDF/AES-GCM token 解密和 Browser policy 亦已离线覆盖。剩余共享工作包括替代 donor XWeb Capture helper 执行、耐久 QuestionSession/Attempt continuation、BrowserBridge 执行器和真实账号验证；这些继续处于当前实现范围；
- rustfmt、Clippy 和全 workspace 测试组成的 CI 基线。

正在进行的工作以 [Phase 0 架构检查点](docs/architecture/phase-0-foundation.md) 为准。内部 API 在第二批 Provider 完成前仍可能发生不兼容变更。

## 架构

```text
CLI / WebUI / Asterism-Plugin
              │
       Versioned HTTP API
              │
     Core actions and policies
      ┌───────┼────────┐
      │       │        │
 Scheduler  Engine  Event Outbox
      │       │        │
      └───────┼────────┘
              │
       Provider capabilities
              │
 Native HTTP / BrowserBridge / Capture
              │
      External platforms
```

主要 workspace 成员：

| 路径 | 职责 |
|---|---|
| `crates/domain` | 与存储无关的领域实体、状态和不变量 |
| `crates/provider-api` | Provider Capability、Metadata、错误模型和注册表 |
| `crates/engine` | 执行状态转换、测评保护和可靠事件派发 |
| `crates/events` | 结构化实时领域事件 |
| `crates/networking` | NetworkProfile 覆盖、校验与共享 HTTP Client 构建 |
| `crates/scheduler` | 扫描、执行、重试和通知任务模型 |
| `crates/storage` | SQLite adapter、migration、事务仓储与恢复 |
| `crates/secrets` | 零化内存的 Secret 类型和 SecretStore 边界 |
| `crates/auth` | 密码、Token 和权限原语 |
| `crates/api` | Axum HTTP transport 与稳定错误封装 |
| `web` | OpenAPI-generated TypeScript Client 工具链与后续 Refine WebUI |
| `bins/asterismd` | 守护进程、数据库生命周期和 API 服务 |
| `bins/asterismctl` | 只通过 HTTP 调用 Core 的命令行客户端 |
| `bins/asterism-capture` | 按需运行、仅主动出站连接的本地认证辅助程序 |

更详细的边界说明见 [Phase 0 foundation](docs/architecture/phase-0-foundation.md)。

## Provider 路线

路线顺序由真实使用和持续验证条件决定，不按实现难度排列。

| 阶段 | Provider / 交付物 | 状态 |
|---|---|---|
| Phase 0 | Core、Storage、Scheduler、Auth、内部 API / CLI | 开发中 |
| 第一批 | `chaoxing`、`welearn`、`uai`、`cidaren` | 开发中（Chaoxing 已建立 Password → Cookie → Course / Chapter / Resource / Work / Exam 链路；WELearn 已覆盖 Password/OIDC、Capture Cookie、Course / Unit / SCO、fresh CMI、DurationReport 与 donor completion/progress/score execution；UAI 已覆盖 Password/JWT/Capture、层级 inventory、detail/progress/duration、加密 Question/Answer/Submission、exit-ticket、单一 oral、discussion 与上传前置链；Cidaren 已覆盖 token/composite Capture、class/study inventory、`jv=99` HKDF/AES-GCM、Question/Answer/SubmissionBuild、BrowserBridge 与一次性答题状态机。共享持久 Attempt/Artifact 合约、其余 donor 流程和真实账号验证仍在继续） |
| 公共交付面 | OpenAPI Client Generation Readiness、Refine v5 + shadcn/ui WebUI、Asterism-Plugin | 已并行开发；不作为第一批 Provider 能力留空或停止的理由 |
| 第二批 | `zhihuishu`、`zjy`、`icve` | 计划中 |
| 兼容性收口 | 稳定并冻结 API / OpenAPI 基线，完成 WebUI / Plugin 兼容性收口 | 第二批完成后开始 |
| 后续批次 | `fif`、`itest`、`utalk` 及其他已分配 Provider ID 的平台 | 规划中 |

第一批不会以 Demo 或功能残缺的 MVP 作为完成状态。Provider 的研究来源、采用方式、许可证和真实验证情况会记录在 [UPSTREAMS.md](UPSTREAMS.md)。

第一批实现以已审计上游能力尽可能完整迁移为完成标准。Native Rust HTTP 仍优先，
但 Capture、BrowserBridge、浏览器状态、OAuth、动态加密上下文和本地辅助均属于
当前实现范围；原生路径无法完整表达 donor 行为时，继续实现最小必要 fallback，
不能以 non-Capture、Native HTTP 或只读里程碑结束 Provider。

WebUI 采用 framework-first 原则，但不为复用而强行套用框架。默认直接复用 Refine v5 与 shadcn/ui 的 Layout、CRUD、DataTable、Form、Auth 和 Theme 基础设施，只调整 Asterism Theme / Branding 并实现必要的领域工作流；现成组件明显不适用时允许最小必要自定义，不进行缺少明确 UX 缺陷依据的主动重设计、美化或像素级循环。

## 快速开始

### 环境要求

- Rust 1.97 或更高版本；
- Cargo；
- Git。

SQLite 由应用直接管理，不需要单独安装数据库服务。

### 构建与启动

```bash
git clone https://github.com/MOPELotus/Asterism.git
cd Asterism
cargo build --workspace
cargo run -p asterismd
```

`asterismd` 默认监听 `127.0.0.1:8068`，并在当前目录使用 `asterism.db`。配置按 `CLI > 环境变量 > 配置文件 > 默认值` 合并；可复制 `asterism.example.toml` 为本地 `asterism.toml`，也可通过 `--config` 或 `ASTERISM_CONFIG` 指定文件。服务端变量包括 `ASTERISM_BIND`、`ASTERISM_DATABASE_URL`、`ASTERISM_SESSION_TTL_SECONDS` 和 `ASTERISM_SECURE_COOKIES`；统一调度器使用 `ASTERISM_SCHEDULER_*` 对应 `[scheduler]` 字段，并可由 `--scheduler-*` 参数覆盖。`execution_concurrency_limit` 是部署级全局硬上限，Provider/账号的后台设置只能在该上限内进一步收紧。普通配置文件不得保存凭据或其他 Secret。

Chaoxing、WELearn、UAI 与 Cidaren 仍处于 `Development` 且默认不注册。仅在本地真实账号验证时，才可通过 `[providers].enable_development_<provider> = true`、`ASTERISM_ENABLE_DEVELOPMENT_<PROVIDER>=true` 或 `--enable-development-<provider>` 分别显式启用 `chaoxing`、`welearn`、`uai` 或 `cidaren`；启用任一平台时必须同时配置 SecretStore keyring。这些开关只开放验证入口，不代表 Supported/Verified。具有已注册 Capture recipe 的 Provider 可通过独立 `asterism-capture` 辅助程序执行浏览器认证；该程序仍须使用 owner 创建的短期 Auth Bootstrap 会话。WELearn 仅能续登由原生 Password 登录形成的完整 Composite 凭据；单独导入的 Cookie 失效后仍需重新认证或导入。UAI 同样只续登完整 NativeProviderLogin username/password/composite 三件套，ManualImport JWT 不可续期；Cidaren 使用导入或 Capture 辅助获得的 token/composite session，当前不声明自动续期。

启用后可通过 CLI 完成 Password → Provider 会话 → 扫描的开发验证。先创建账号并启动认证会话，再把返回的 ID 代入后续命令：

```powershell
cargo run -p asterismctl -- provider-account create --provider chaoxing --name chaoxing-dev
cargo run -p asterismctl -- provider-account auth start <account-id> --method password
cargo run -p asterismctl -- provider-account credential import <account-id> --session <session-id> --purpose provider-username --purpose provider-password --auth-method password --session-kind provider-specific --acquired-via native-provider-login
cargo run -p asterismctl -- provider-account scan <account-id>
```

`credential import` 会按 `--purpose` 顺序隐藏提示输入，用户名和密码不会进入命令行参数；从非交互 stdin 读取时则要求每个字段独占一行。当前流程只用于开发验证，必须在留下脱敏运行记录并通过完整验收后，才能提升 Chaoxing 的支持状态。

SecretStore keyring 只从进程环境读取，不接受 TOML 或 CLI 参数。`ASTERISM_SECRET_ACTIVE_KEY_ID` 指定活动 key ID，`ASTERISM_SECRET_KEYS` 使用逗号分隔的 `<key-id>=<base64-encoded-32-byte-key>`；两者必须同时提供。轮换时保留旧 key 并添加新 key，再切换活动 ID；确认所有密文已轮换前不要移除旧 key。`/health` 的 `secret_store_configured` 只报告是否已配置，不返回 key ID 或 key material。

统一 Scheduler worker 默认启用，每 5 秒分别执行一次有界 Scan tick 与 Execution tick；默认每次最多领取一个 Scan Job，Execution worker 固定串行领取一个 `execution`、`retry` 或 `recovery` Job，避免长任务的后续预领取 claim 在等待时过期。每个 Execution tick 会先扫描失去租约的 Running Execution，原子转入 Recovering 并创建只读远端复核任务；普通资源执行只有远端完成才收口成功，明确 Pending 才可重新执行，无法判断则要求人工处理。调用方必须提交非空、唯一的可执行 Capability 子集，Core 将其规范化并冻结到 Execution，Provider 与 Recovery 全程只接收该子集。声明 `ExecutionVerify` 的 Task 可由 Provider 将 goal-bound verification 精确绑定到其中一个所选 action；需要验证的非幂等 action 以已持久化 Attempt 为最多一次边界，执行后必须 fresh rebind 同一目标，任何错误、未达目标或崩溃都只调度验证恢复而不创建写入 Retry。独立提交执行始终按冻结 Draft 调用一次 Submit、持久化回执、再调用 Verify；Submit 后的网络歧义、Pending、崩溃或重启只会反复调用 Verify，永远不会由 Recovery 或 Retry 重放提交。only-DurationReport 以时长证据而非完成状态作为目标，任何不确定写入直接要求人工处理且不会自动重跑。claim TTL 与 Execution lease 默认均为 300 秒，运行期间会一起续租。独立 Outbox dispatcher 每 250 毫秒领取最多 128 条已提交领域事件并派发到有界进程内实时总线；没有在线订阅者不会让已持久化事件无限重试，后续连接以查询快照重新同步。停止 `asterismd` 时会终止 SSE、停止新 tick，等待当前 Scan、Execution 或 Outbox 派发返回，再关闭数据库；具体安全边界与重试默认值见 `asterism.example.toml`。

另开一个终端检查服务：

```bash
cargo run -p asterismctl -- system health
```

首次启动后创建 Master。密码默认通过终端隐藏输入，命令会返回一个仅展示一次、默认有效期 30 天的 scoped Service Token：

```bash
cargo run -p asterismctl -- init --username master
```

请用 Secret Manager 保存返回的 Token，仅在调用期间注入 `ASTERISM_TOKEN`；不要把它写入 `asterism.toml`、命令行参数或版本库。PowerShell 示例：

```powershell
$env:ASTERISM_TOKEN = "<one-time token>"
cargo run -p asterismctl -- auth whoami
cargo run -p asterismctl -- provider list
cargo run -p asterismctl -- provider-account create --provider provider-alpha --name primary
cargo run -p asterismctl -- provider-account list
cargo run -p asterismctl -- provider-account schedule set <account-id> --interval-seconds 900
cargo run -p asterismctl -- task list --limit 50
cargo run -p asterismctl -- task execute <task-id> --capability resource_execution --idempotency-key manual-run-1
cargo run -p asterismctl -- execution list --limit 50
cargo run -p asterismctl -- execution get <execution-id>
cargo run -p asterismctl -- execution logs <execution-id> --limit 50 --offset 0
Remove-Item Env:ASTERISM_TOKEN
```

自动化环境可给 `init` 或 `auth login` 添加 `--password-stdin`，从标准输入读取单行密码，避免把密码放进进程参数。当前内部接口及其请求结构可从 `/api/v1/openapi.json` 查看。

需要本地辅助的 Auth Bootstrap 会话可由 `asterism-capture` 认领。手动导入命令只在 argv 中接收 session ID、认证类型和字段用途；pairing token、账号显示名、可选 tenant 与凭据值按顺序从隐藏终端输入或 stdin 读取，不提供 Secret 命令行参数：

```bash
cargo run -p asterism-capture -- \
  --url https://asterism.example \
  manual \
  --session-id <session-uuid> \
  --auth-method assisted-session \
  --session-kind cookie \
  --field cookie
```

Provider 已声明 Capture recipe 时可使用 `automatic`：它启动独立临时 Chromium/Edge profile，连接仅限 loopback 的随机端口 DevTools；recipe 将 HTTPS 顶层导航 origin 与更窄的 Secret read origin 分开，第三方 OAuth 页面不会因获准导航而取得 request header、LocalStorage、SessionStorage 或 Cookie 读取权。helper 只采集明确声明的来源，并在同一 target/document snapshot 完整且通过版本化 readiness gate 后提交；gate 可绑定当前 document loader 的精确请求，或精确 2xx 响应 MIME，从而拒绝“匿名 Cookie 已存在但接口仍返回登录 HTML”的假完成。未通过命令行传递 pairing token 或浏览器 Secret；显式浏览器路径只用于自动发现不适用的安装位置。

```bash
cargo run -p asterism-capture -- \
  --url https://asterism.example \
  automatic \
  --session-id <session-uuid>
```

复合凭据可按输入顺序重复 `--field`，需要 tenant 时添加 `--with-tenant`。`automatic` 同样可加 `--with-tenant`，或以 `--browser-path` 指定 Chromium/Edge。Capture 只主动连接配置的 Asterism HTTPS 地址和自身启动的 loopback DevTools；明文 Asterism HTTP 仅能通过显式开发开关连接 loopback。会话到期、服务端拒绝 access token、提交成功、本地按下 Ctrl+C 或独立浏览器退出时，Capture 会立即丢弃本地访问材料、终止辅助浏览器并尝试删除临时 profile；服务端会话取消仍由已认证的 owner 通过 WebUI / CLI 发起。

Provider Account 的 owner 始终由认证身份决定，CLI 和 API 都不接受调用方指定 `owner_id`。`--provider` 必须使用小写 canonical `ProviderId`；账号展示名属于本地用户数据，不作为项目内的平台名称或标识。

Task 仍只能由 Provider 扫描链路写入；读取和执行则统一通过 owner-scoped Core Action。远端账户完成认证且对应 Provider 已注册 inventory capability 后，可运行 `provider-account scan <account-id>`；`task list` 支持 `--account`、`--limit` 和 `--offset`。`task progress <task-id>` 与 `task duration <task-id>` 分别走独立的只读 capability 和 API，读取时长不会创建 Execution、上报时长或改变远端状态。`task execute <task-id> --capability <action> --idempotency-key <key>` 会原子创建并调度 Execution，`--capability` 可重复并冻结本次明确选择的 action；独立提交任务选择 `submission_execute` 后还必须传入 `--submission-draft <draft-id>`，Core 会冻结 Draft 与 Execution 的绑定并拒绝同一 Draft 通过新幂等键重复调度。等待人工批准的 Task 不能再用 execute 绕过批准：CLI/API/WebUI 共用 `approve`、`cancel`、`delay --until <rfc3339>` 与 `ignore` 动作，并要求调用方为每个语义请求提供可重用的幂等 key。批准只回到 Ready，正式测评写入仍受独立 Core policy 阻止；延迟只移动尚未领取的 Scheduled Execution 与 Job；取消拒绝已领取、Running 或 Recovering 的远端工作，避免本地假取消。调用方重试同一语义请求时必须复用原 key 和原 capability/Draft/动作参数，Core 会返回原结果，跨任务、换 capability、换 Draft 或换动作复用则拒绝。`execution list` 按创建时间稳定分页列出同一 owner 的 Execution，并可用 `--task` 缩小到单个 Task；`execution get <execution-id>` 返回当前结构化进度和按尝试序号排列的 Attempt 历史，`execution logs` 按稳定时间顺序分页读取脱敏日志。`GET /api/v1/executions/{execution_id}/stream` 提供 `snapshot` 起始帧以及该 Execution 的实时状态、进度和结构化日志事件；它不提供持久事件重放，收到 `resync` 或重新连接时应重新读取 detail，并按需读取 logs。不存在或属于其他 owner 的 ID 均不会泄露。正式测评默认在创建 Scheduler Job 前拦截。返回值始终分别保留远端状态、编排状态、来源模块与任务性质，不从其中任一字段推断另一字段。

Provider 执行期间只能通过 Core 注入的 `ExecutionEventSink` 上报进度或诊断日志，不能直接写数据库或实时连接。诊断日志由 Core 生成时间和标准阶段，并再次校验当前 Attempt、Execution lease、单行文本、Provider trace、8 KiB 脱敏 metadata 与敏感字段名；每个 Attempt 最多接受 1000 条 Provider 日志。日志历史行和 `ExecutionLogged` Outbox 事件在同一事务提交，Provider payload、Cookie、Token 和 Password 不属于该接口的合法输入。

周期扫描当前仅由 Master 通过 `provider-account schedule get <account-id>` 和 `provider-account schedule set <account-id> [--interval-seconds <seconds>]` 管理；省略间隔时，Core 会解析并采用当时该 Provider/ProviderAccount 的后台巡查默认值，显式间隔仍可覆盖本次 Schedule，添加 `--disabled` 可保留配置但停止物化任务。接口同时返回 Master 期望间隔、Provider 最小间隔和 Core 实际采用的间隔，普通用户第一阶段不展示也不能修改该设置，Provider 自身不创建独立 cron 或后台循环。

Provider 技术运行参数属于 Master 后台控制面：可设置平台默认值，并按需对 ProviderAccount 或单个 Task 做更具体覆盖；视频并发/线程数、播放速度、章节及其他任务的周期巡查只是示例，实际字段由各 Provider 的版本化 schema 定义并受安全上限约束。Provider API 已提供布尔、整数、千分位定点小数、秒级时长与受限选项类型，以及 Provider / ProviderAccount / Task 作用域声明和逐字段覆盖解析；不接受自由字符串或任意 JSON。Core 已按三个作用域持久化校验后的局部覆盖，使用 optimistic revision 防止后台并发覆盖，并在同一事务写入不含配置值的 Audit。第一阶段普通用户不展示也不能修改这些参数。周期巡查仍必须统一进入 Core Scheduler，Provider 不得据此自行创建后台循环；标准 Core 执行 Action 会原子解析并冻结最终设置，Worker 只把该 Execution 的版本化快照传给 Provider，Retry 不受后台后续修改影响。Provider 可通过受限的 portable behavior 将自定义字段声明为平台并发、账号并发或账号巡查间隔，Core 不按字段名猜语义；Worker 以同一原子准入门同时执行全局、Provider、ProviderAccount 三层限制，并在等待期间续租，Schedule API 则可在未给显式间隔时采用当前后台巡查默认值。`chaoxing` schema 已接入平台/账号执行并发、账号巡查间隔、视频倍速和进度上报间隔；同一账号默认仍为 1，Master 可在 schema 安全上限内设置平台默认、账号或单 Task 覆盖。

Master 控制面使用 `/api/v1/admin/providers/{provider_id}/runtime-settings`、`/api/v1/admin/provider-accounts/{account_id}/runtime-settings` 与 `/api/v1/admin/tasks/{task_id}/runtime-settings` 读取或替换各层覆盖；Provider schema 可从前一路径下的 `/schema` 读取。读取结果同时返回三层 override、最终解析值和逐字段来源。Provider 默认值仅允许 Master Web Session 管理；owner-bound `ProviderManage` 服务令牌只能管理其 owner 的账号和任务覆盖。

Credits 读面使用 `GET /api/v1/credits/account`、`GET /api/v1/credits/transactions` 和 `GET /api/v1/credits/reservations`。后两者采用显式 `limit` / `offset` 分页；Reservation 响应同时返回执行当时固定的 PriceQuote 与 `pricing_revision`。这些接口只允许 `ReadOwnCredits` Web 用户或 owner-bound `CreditRead` 服务令牌读取自己的数据，不提供扣点、调价或任意结算入口。

也可以直接访问：

```text
GET http://127.0.0.1:8068/health
GET http://127.0.0.1:8068/api/v1/system/health
GET http://127.0.0.1:8068/api/v1/openapi.json
```

日志级别通过 `RUST_LOG` 控制，例如：

```powershell
$env:RUST_LOG = "asterism=debug,tower_http=info"
cargo run -p asterismd
```

## 开发

提交前执行完整质量检查：

```bash
cargo fmt --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
```

开发时必须遵守以下边界：

1. `Task` 不等于 `Execution`，远端状态不等于本地编排状态；
2. CLI、WebUI 和 Plugin 必须调用同一 Core action，不得各自实现业务逻辑；
3. `asterismd` 是正常部署下唯一的 SQLite 写入者；
4. Provider 实现前先完成 capability-level upstream audit；
5. 凭据、Cookie、Token 和真实账号 Fixture 不得进入 Git；
6. 未通过真实验证的 Provider 不得宣称为 Supported 或 Verified。

Commit 使用统一格式：

```text
<type>(<scope>): <简洁的中文动作描述>
```

例如：`feat(auth): 接入 Web Session 鉴权`、`docs(readme): 完善项目说明与开发指南`。一条提交只包含一个可独立验证的逻辑变更。

## 安全说明

- 默认 API 仅监听 loopback；在认证、TLS 和部署策略完善前不要直接暴露到公网；
- Secret 只允许通过 SecretStore 边界访问，不应作为普通字段记录或输出；
- Browser Runtime 与 Core 进程隔离，浏览器 Worker 不应获得数据库或主密钥；
- 正式测评相关执行必须经过独立策略保护，不能仅依赖任务来源或 UI 提示；
- 提交问题、日志和 Fixture 前请移除个人信息、账号标识及所有认证材料。

## 项目状态

Asterism 当前没有稳定版本或兼容性承诺。正式 WebUI 与 Asterism-Plugin 在第一批 Provider 完成后开始建设；公开 API 与 OpenAPI 兼容基线在第二批完成并经过真实验证后稳定和冻结。
