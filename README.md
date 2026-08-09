# Asterism

[![CI](https://github.com/MOPELotus/Asterism/actions/workflows/ci.yml/badge.svg)](https://github.com/MOPELotus/Asterism/actions/workflows/ci.yml)
![Rust 1.97+](https://img.shields.io/badge/Rust-1.97%2B-000000?logo=rust)
![Status: Phase 0](https://img.shields.io/badge/status-Phase%200-yellow)

> Unified learning task orchestration across multiple platforms, powered by Rust.

Asterism 是一个基于 Rust 的多平台学习任务聚合与调度服务。它将不同平台的课程、任务、进度和执行能力归一到统一的领域模型中，并由同一套 Core 为 CLI、WebUI 和 Asterism-Plugin 提供能力。

项目不以“接口返回成功”作为 Provider 已受支持的标准。一个 Provider 只有在能力覆盖、状态映射、错误分类、Fixture、真实账号验证和运行记录均达到要求后，才会被标记为可用。

> [!IMPORTANT]
> Asterism 正处于 **Phase 0 Core 基座开发阶段**，尚未宣布任何学习平台 Provider 已完成。当前代码用于架构建设和开发验证，不代表可用于生产环境。

## 设计目标

- 以统一模型聚合多平台课程、任务、进度和截止时间；
- 将远端任务事实与本地调度、审批、重试和执行状态严格分离；
- 通过 Capability 模型描述 Provider 的真实能力，而不是伪造统一接口；
- 支持自动执行、延迟审批、手动审批和仅通知等策略；
- 以事务化点数、调度、执行租约和 Event Outbox 保证关键状态一致性；
- 为敏感凭据、正式测评、浏览器自动化和公网部署设置明确安全边界；
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
- 按 Task / QuestionSnapshot / SubmissionDraft / SubmissionResult 完整身份链只读验证结果的 HTTP/CLI 审计入口；
- Capability-based Provider API、Metadata 与 Registry，包括与远端执行解耦的题目发现、解析、Provider-native 候选答案解析和只读提交草稿构建槽位；
- 课程发现到后续 capability 的短命、脱敏且不持久化路由上下文；
- Account > Provider > Global 覆盖的 NetworkProfile 与集中 HTTP Client 构建；
- 先完整采集后事务提交的 Provider Course / Task inventory 编排；
- Provider 执行调度、可续租执行状态机、正式测评保护与崩溃恢复语义；
- SQLite migration、WAL 策略、事务仓储和并发测试；
- 版本化任务指纹、脱敏扫描快照、类型化差异与事务化扫描入库；
- 幂等执行请求、执行租约、周期扫描物化、隔离认领的 worker 与事务化 Event Outbox；
- 点数 grant / reserve / commit / release 流程、不可变流水，以及 Quote + Reserve + Execution 原子调度边界；
- SecretStore 抽象、Argon2id 密码、服务端 Session、scoped Service Token 与登录限速；
- 内部 Axum API、OpenAPI 入口、健康检查与 HTTP-only CLI；
- Auth Bootstrap 配对、状态事件、Provider 服务端验证与原子凭据提交；
- owner-scoped 人工扫描 API / CLI 与同事务扫描审计；
- Chaoxing 能力级上游审计、独立 Chapter / Resource / Work / Exam TaskInventory、按稳定身份重新发现并经 Core 校验的 TaskDetail / Progress API 与 CLI、四类任务的模块化只读进度复核、明确区分 Chapter Work 与独立 Work / Exam 的题面离线解析、仅在新鲜详情确认可作答后开放的独立 Work 原生题目读取与无值 SubmissionBuild 预览、Document / Read / Video 原生执行、有界 Work 详情状态复核、Cookie 自动续登与显式开发验证入口；
- WELearn clean-room 上游审计与有界 Course / Unit / SCO 离线解析，完成状态、可见性和未确认单位的 donor 时长事实保持独立，尚未注册原生运行时能力；
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
 Native HTTP / BrowserBridge
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
| `bins/asterismd` | 守护进程、数据库生命周期和 API 服务 |
| `bins/asterismctl` | 只通过 HTTP 调用 Core 的命令行客户端 |
| `bins/asterism-capture` | 按需运行、仅主动出站连接的本地认证辅助程序 |

更详细的边界说明见 [Phase 0 foundation](docs/architecture/phase-0-foundation.md)。

## Provider 路线

路线顺序由真实使用和持续验证条件决定，不按实现难度排列。

| 阶段 | Provider / 交付物 | 状态 |
|---|---|---|
| Phase 0 | Core、Storage、Scheduler、Auth、内部 API / CLI | 开发中 |
| 第一批 | `chaoxing`、`welearn`、`uai`、`cidaren` | 开发中（Chaoxing 已建立 Password → Cookie → Course / Chapter / Resource / Work / Exam 的开发验证链路；WELearn 已建立 Course / Unit / SCO 离线解析边界；均未完成真实账号验证） |
| 第一批后交付 | OpenAPI Client Generation Readiness、Refine v5 + shadcn/ui WebUI、Asterism-Plugin | 第一批完成后立即开始 |
| 第二批 | `zhihuishu`、`zjy`、`icve` | 计划中 |
| 兼容性收口 | 稳定并冻结 API / OpenAPI 基线，完成 WebUI / Plugin 兼容性收口 | 第二批完成后开始 |
| 后续批次 | `fif`、`itest`、`utalk` 及其他已分配 Provider ID 的平台 | 规划中 |

第一批不会以 Demo 或功能残缺的 MVP 作为完成状态。Provider 的研究来源、采用方式、许可证和真实验证情况会记录在 [UPSTREAMS.md](UPSTREAMS.md)。

当前开发顺序只推进第一批中不依赖 Capture 的 Native HTTP、手工
Credential/Session Import 与离线 Fixture 能力。必须启动本地 Capture、浏览器抓取
或系统代理才能完成的 Provider 路径明确延后；对应验收项保持未完成，也不会因此
提前标记 Provider 为 `Verified`。仓库中已有的通用 Capture 基座保留，但不在这一
阶段继续扩展平台专用链路。

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

Chaoxing 仍处于 `Development` 且默认不注册。仅在本地真实账号验证时，才可通过 `[providers].enable_development_chaoxing = true`、`ASTERISM_ENABLE_DEVELOPMENT_CHAOXING=true` 或 `--enable-development-chaoxing` 显式启用；启用时必须同时配置 SecretStore keyring。这个开关只开放验证入口，不代表 Supported/Verified，也不会启用 Capture。

启用后可通过 CLI 完成 Password → Cookie → 扫描的开发验证。先创建账号并启动认证会话，再把返回的 ID 代入后续命令：

```powershell
cargo run -p asterismctl -- provider-account create --provider chaoxing --name chaoxing-dev
cargo run -p asterismctl -- provider-account auth start <account-id> --method password
cargo run -p asterismctl -- provider-account credential import <account-id> --session <session-id> --purpose provider-username --purpose provider-password --auth-method password --session-kind provider-specific --acquired-via native-provider-login
cargo run -p asterismctl -- provider-account scan <account-id>
```

`credential import` 会按 `--purpose` 顺序隐藏提示输入，用户名和密码不会进入命令行参数；从非交互 stdin 读取时则要求每个字段独占一行。当前流程只用于开发验证，必须在留下脱敏运行记录并通过完整验收后，才能提升 Chaoxing 的支持状态。

SecretStore keyring 只从进程环境读取，不接受 TOML 或 CLI 参数。`ASTERISM_SECRET_ACTIVE_KEY_ID` 指定活动 key ID，`ASTERISM_SECRET_KEYS` 使用逗号分隔的 `<key-id>=<base64-encoded-32-byte-key>`；两者必须同时提供。轮换时保留旧 key 并添加新 key，再切换活动 ID；确认所有密文已轮换前不要移除旧 key。`/health` 的 `secret_store_configured` 只报告是否已配置，不返回 key ID 或 key material。

统一 Scheduler worker 默认启用，每 5 秒分别执行一次有界 Scan tick 与 Execution tick；默认每次最多领取一个 Scan Job，Execution worker 固定串行领取一个 `execution`、`retry` 或 `recovery` Job，避免长任务的后续预领取 claim 在等待时过期。每个 Execution tick 会先扫描失去租约的 Running Execution，原子转入 Recovering 并创建只读远端复核任务；远端完成才收口成功，明确 Pending 才重新执行，无法判断则要求人工处理。claim TTL 与 Execution lease 默认均为 300 秒，运行期间会一起续租。独立 Outbox dispatcher 每 250 毫秒领取最多 128 条已提交领域事件并派发到有界进程内实时总线；没有在线订阅者不会让已持久化事件无限重试，后续连接以查询快照重新同步。停止 `asterismd` 时会终止 SSE、停止新 tick，等待当前 Scan、Execution 或 Outbox 派发返回，再关闭数据库；具体安全边界与重试默认值见 `asterism.example.toml`。

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
cargo run -p asterismctl -- task execute <task-id> --idempotency-key manual-run-1
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
  --auth-method imported-cookie \
  --session-kind cookie \
  --field cookie
```

复合凭据可按输入顺序重复 `--field`，需要 tenant 时添加 `--with-tenant`。Capture 只主动连接配置的 Asterism HTTPS 地址；明文 HTTP 仅能通过显式开发开关连接 loopback。会话到期、服务端拒绝 access token、提交成功或本地按下 Ctrl+C 时，Capture 会立即丢弃本地访问材料；服务端会话取消仍由已认证的 owner 通过 WebUI / CLI 发起。

Provider Account 的 owner 始终由认证身份决定，CLI 和 API 都不接受调用方指定 `owner_id`。`--provider` 必须使用小写 canonical `ProviderId`；账号展示名属于本地用户数据，不作为项目内的平台名称或标识。

Task 仍只能由 Provider 扫描链路写入；读取和执行则统一通过 owner-scoped Core Action。远端账户完成认证且对应 Provider 已注册 inventory capability 后，可运行 `provider-account scan <account-id>`；`task list` 支持 `--account`、`--limit` 和 `--offset`。`task execute <task-id> --idempotency-key <key>` 会原子创建并调度 Execution；调用方重试同一语义请求时必须复用该 key，Core 会返回原 Execution，跨任务复用则拒绝。`execution list` 按创建时间稳定分页列出同一 owner 的 Execution，并可用 `--task` 缩小到单个 Task；`execution get <execution-id>` 返回当前结构化进度和按尝试序号排列的 Attempt 历史，`execution logs` 按稳定时间顺序分页读取脱敏日志。`GET /api/v1/executions/{execution_id}/stream` 提供 `snapshot` 起始帧以及该 Execution 的实时状态、进度和结构化日志事件；它不提供持久事件重放，收到 `resync` 或重新连接时应重新读取 detail，并按需读取 logs。不存在或属于其他 owner 的 ID 均不会泄露。正式测评默认在创建 Scheduler Job 前拦截。返回值始终分别保留远端状态、编排状态、来源模块与任务性质，不从其中任一字段推断另一字段。

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
