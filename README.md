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
- 在 Core 与前两批 Provider 完成后，逐步稳定 API 并建设 WebUI 与 Asterism-Plugin。

## 当前进度

Phase 0 已建立并持续完善以下基础：

- Rust 2024 workspace 与清晰的 crate 边界；
- Task / Execution、Remote State / Orchestration State 等独立领域模型；
- Capability-based Provider API、Metadata 与 Registry；
- 先完整采集后事务提交的 Provider Course / Task inventory 编排；
- 执行状态机、正式测评保护与崩溃恢复语义；
- SQLite migration、WAL 策略、事务仓储和并发测试；
- 版本化任务指纹、脱敏扫描快照、类型化差异与事务化扫描入库；
- 执行租约、Scheduler retry/dead-letter 与事务化 Event Outbox；
- 点数 grant / reserve / commit / release 流程与不可变流水；
- SecretStore 抽象、Argon2id 密码、服务端 Session、scoped Service Token 与登录限速；
- 内部 Axum API、OpenAPI 入口、健康检查与 HTTP-only CLI；
- owner-scoped 人工扫描 API / CLI 与同事务扫描审计；
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
| `crates/scheduler` | 扫描、执行、重试和通知任务模型 |
| `crates/storage` | SQLite adapter、migration、事务仓储与恢复 |
| `crates/secrets` | 零化内存的 Secret 类型和 SecretStore 边界 |
| `crates/auth` | 密码、Token 和权限原语 |
| `crates/api` | Axum HTTP transport 与稳定错误封装 |
| `bins/asterismd` | 守护进程、数据库生命周期和 API 服务 |
| `bins/asterismctl` | 只通过 HTTP 调用 Core 的命令行客户端 |

更详细的边界说明见 [Phase 0 foundation](docs/architecture/phase-0-foundation.md)。

## Provider 路线

路线顺序由真实使用和持续验证条件决定，不按实现难度排列。

| 阶段 | Provider / 交付物 | 状态 |
|---|---|---|
| Phase 0 | Core、Storage、Scheduler、Auth、内部 API / CLI | 开发中 |
| 第一批 | `chaoxing`、`welearn`、`uai`、`cidaren`、`zhihuishu` | 计划中 |
| 第二批 | `zjy`、`icve` | 计划中 |
| 对外接口阶段 | 稳定 API、正式 WebUI、Asterism-Plugin | 第二批完成后开始 |
| 后续批次 | `fif`、`itest`、`utalk` 及其他已分配 Provider ID 的平台 | 规划中 |

第一批不会以 Demo 或功能残缺的 MVP 作为完成状态。Provider 的研究来源、采用方式、许可证和真实验证情况会记录在 [UPSTREAMS.md](UPSTREAMS.md)。

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

`asterismd` 默认监听 `127.0.0.1:8068`，并在当前目录使用 `asterism.db`。配置按 `CLI > 环境变量 > 配置文件 > 默认值` 合并；可复制 `asterism.example.toml` 为本地 `asterism.toml`，也可通过 `--config` 或 `ASTERISM_CONFIG` 指定文件。当前支持的环境变量包括 `ASTERISM_BIND`、`ASTERISM_DATABASE_URL`、`ASTERISM_SESSION_TTL_SECONDS` 和 `ASTERISM_SECURE_COOKIES`。普通配置文件不得保存凭据或其他 Secret。

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
cargo run -p asterismctl -- task list --limit 50
Remove-Item Env:ASTERISM_TOKEN
```

自动化环境可给 `init` 或 `auth login` 添加 `--password-stdin`，从标准输入读取单行密码，避免把密码放进进程参数。当前内部接口及其请求结构可从 `/api/v1/openapi.json` 查看。

Provider Account 的 owner 始终由认证身份决定，CLI 和 API 都不接受调用方指定 `owner_id`。`--provider` 必须使用小写 canonical `ProviderId`；账号展示名属于本地用户数据，不作为项目内的平台名称或标识。

Task 接口当前只读，任务只能由 Provider 扫描链路写入。远端账户完成认证且对应 Provider 已注册 inventory capability 后，可运行 `provider-account scan <account-id>`；`task list` 支持 `--account`、`--limit` 和 `--offset`。返回值始终分别保留远端状态、编排状态、来源模块与任务性质，不从其中任一字段推断另一字段。

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

Asterism 当前没有稳定版本或兼容性承诺。公开 API、部署文档、正式 WebUI 和 Asterism-Plugin 会在 Core 与前两批 Provider 完成并经过真实验证后发布。
