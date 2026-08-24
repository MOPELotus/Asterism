# Asterism

[![CI](https://github.com/MOPELotus/Asterism/actions/workflows/ci.yml/badge.svg)](https://github.com/MOPELotus/Asterism/actions/workflows/ci.yml)
[![Rust 1.97+](https://img.shields.io/badge/Rust-1.97%2B-000000?logo=rust)](https://www.rust-lang.org/)
[![Status: Pre-release](https://img.shields.io/badge/status-pre--release-orange)](#发布路线)
[![Last commit](https://img.shields.io/github/last-commit/MOPELotus/Asterism)](https://github.com/MOPELotus/Asterism/commits/master/)
[![GitHub stars](https://img.shields.io/github/stars/MOPELotus/Asterism?style=flat)](https://github.com/MOPELotus/Asterism/stargazers)

**Unified learning task orchestration across multiple platforms, powered by Rust.**

Asterism 是一个基于 Rust 的多平台学习任务聚合与调度服务。它把平台账号、课程、任务、进度、执行、验证、审计和恢复统一到同一套 Core 中，并通过 HTTP API、CLI 与 WebUI 提供一致的操作入口。

> [!WARNING]
> Asterism 仍处于 pre-release 前的开发阶段。当前 Provider 均为 `Development`，默认关闭，尚未获得生产可用或 `Verified` 承诺。请勿在没有授权、备份和风险评估的情况下对真实平台任务执行写入操作。

## 能做什么

- 聚合多个学习平台的账号、课程与任务；
- 独立展示远端任务状态与本地编排状态；
- 执行周期扫描、任务调度、审批、延迟、取消与恢复；
- 以不可变 Draft、Attempt、Receipt 和独立 Verification 保护提交链路；
- 对非幂等操作执行 issue-before-send，并在网络歧义后只读恢复，避免盲目重放；
- 管理运行设置、点数、审计记录、执行日志与实时进度；
- 通过 Capture / BrowserBridge 辅助需要浏览器登录态或交互的流程；
- 从 Rust OpenAPI 源生成 TypeScript Client，供 WebUI 和外部集成复用。

## Provider 路线

当前 `0.0.1` 分支采用 upstream-first 路线：Asterism 保留 WebUI、账号、课程/任务、
题库、调度、Job 与日志控制面，四个平台的执行则通过薄 Worker 调用已审计 donor，
不要求把 donor 的协议和状态机重新移植为 Rust。完整约束见
[`docs/0.0.1/README.md`](docs/0.0.1/README.md)。

| 批次 | Provider | 状态 |
|---|---|---|
| 第一批 | `chaoxing`、`welearn`、`uai`、`cidaren` | upstream 执行已接入；账号与只读链路已验证 |
| 第二批 | `zhihuishu`、`zjy`、`icve` | 计划中 |
| 后续批次 | `fif`、`itest`、`utalk` 及其他已分配 Provider ID 的平台 | 未来 |

第一批 Provider 仅在本地验证时显式启用。代码和离线测试不等于真实账号验证，也不代表平台协议长期稳定。

## 发布路线

首次 pre-release 的门槛是：

1. 发布范围内的真实账号登录、Session、账号绑定和安全只读链路完成验证；
2. 公共交付面可用，包括生成式 OpenAPI Client、WebUI 与 Asterism-Plugin/Yunzai；
3. 完整 CI、迁移、前端构建和发布产物检查通过；
4. Development Provider、未授权 mutation 和已知限制得到明确标注。

第二批 Provider 当前为计划中，不阻塞首次 pre-release；后续 Provider 批次与稳定 API / OpenAPI 兼容基线属于未来工作。首次 pre-release 只提供明确声明的预发布兼容范围。

## 架构概览

```text
CLI / WebUI / external integrations
                │
        Versioned HTTP API
                │
       Core actions and policy
      ┌─────────┼─────────┐
      │         │         │
 Scheduler   Engine   Event Outbox
      │         │         │
      └─────────┼─────────┘
                │
       Provider capabilities
                │
 Native HTTP / BrowserBridge / Capture
```

正常部署中，`asterismd` 是 SQLite 的唯一写入者。CLI、WebUI 和外部集成都通过同一 HTTP API 调用 Core，不应复制业务逻辑。

## 环境要求

- Rust 1.97 或更高版本；
- Cargo；
- Git；
- Node.js 22.18 或更高版本（仅 WebUI 开发需要）。

SQLite 由应用直接管理，无需单独安装数据库服务。

## 快速开始

克隆并构建：

```bash
git clone --recurse-submodules --branch 0.0.1 https://github.com/MOPELotus/Asterism.git
cd Asterism
cargo build --workspace
```

完整的生产构建、SecretStore、浏览器和反向代理步骤见
[`docs/0.0.1/DEPLOYMENT.md`](docs/0.0.1/DEPLOYMENT.md)。

已有 checkout 先同步 donor：

```bash
git submodule update --init --recursive
```

四个平台 Worker 使用 Python donor。建议建立同一个隔离环境并安装各 Worker 声明的
依赖；服务端用户负责部署，WebUI/QQ 用户不需要自行 clone donor：

```powershell
py -m venv .venv-workers
.\.venv-workers\Scripts\python.exe -m pip install -r workers\chaoxing\requirements.txt
.\.venv-workers\Scripts\python.exe -m pip install -r workers\welearn\requirements.txt
.\.venv-workers\Scripts\python.exe -m pip install -r workers\uai\requirements.txt
.\.venv-workers\Scripts\python.exe -m pip install -r workers\cidaren\requirements.txt
$env:ASTERISM_UAI_WORKER_PYTHON = '.venv-workers\Scripts\python.exe'
```

`asterismd` 默认从 `upstreams/` 下的固定 submodule 加载四个平台 donor；无需再传入
各 donor 路径。Chaoxing 的 DOM 兜底和 UAI 页面驻留还需要一个 Chromium 兼容浏览器，
通过以下两个环境变量指向同一个可执行文件即可：

```powershell
$env:ASTERISM_CHAOXING_BROWSER_EXECUTABLE = '<chromium-or-edge-path>'
$env:ASTERISM_UAI_BROWSER_EXECUTABLE = $env:ASTERISM_CHAOXING_BROWSER_EXECUTABLE
```

WELearn donor 当前未声明开源许可证。固定 submodule 便于当前授权环境复现运行，但在
取得上游授权或明确许可证前，不应把其源码随公开安装包重新分发。

复制示例配置并启动服务：

```bash
cp asterism.example.toml asterism.toml
cargo run -p asterismd
```

PowerShell：

```powershell
Copy-Item asterism.example.toml asterism.toml
cargo run -p asterismd
```

默认地址为 `http://127.0.0.1:8068`，默认数据库为当前目录下的 `asterism.db`。首次启动会自动执行数据库迁移。

在另一个终端检查服务并创建首个 Master：

```bash
cargo run -p asterismctl -- system health
cargo run -p asterismctl -- init --username master
```

`init` 会返回一个仅展示一次的 Service Token。请使用 Secret Manager 保存，并仅在调用期间通过 `ASTERISM_TOKEN` 注入：

```powershell
$env:ASTERISM_TOKEN = "<token>"
cargo run -p asterismctl -- auth whoami
cargo run -p asterismctl -- provider list
cargo run -p asterismctl -- provider-account list
cargo run -p asterismctl -- task list --limit 50
Remove-Item Env:ASTERISM_TOKEN
```

常用只读端点：

```text
GET /health
GET /api/v1/system/health
GET /api/v1/openapi.json
```

## WebUI

```bash
cd web
npm ci
npm run dev
```

生产构建：

```bash
npm run build
```

构建完成后，`asterismd` 默认直接从 `web/dist` 提供 WebUI，因此生产部署不需要再运行
Vite。若静态文件放在其他位置，可设置 `ASTERISM_WEB_DIST`；目录不存在时 daemon 会以
API-only 模式启动并记录警告。

WebUI 使用从 Asterism OpenAPI 自动生成的 TypeScript Client。修改 API 后应重新生成并执行类型检查。

## QQ 机器人交互

QQ 机器人交互部分基于 [MOPELotus/Miao-Yunzai](https://github.com/MOPELotus/Miao-Yunzai)
进行适配。当前薄插件位于
[`integrations/yunzai-plugin`](integrations/yunzai-plugin)，通过 owner-bound Service Token
调用 Asterism HTTP API，不在机器人插件中重复实现 Provider 业务逻辑。它已在当前 fork
HEAD `f9c5dbc1` 完成入口加载和真实只读 API 冒烟；安装、权限与命令见插件 README。

## SecretStore 与开发 Provider

普通 TOML 配置不得保存密码、Cookie、Token 或密钥。SecretStore keyring 只从环境变量读取：

```text
ASTERISM_SECRET_ACTIVE_KEY_ID=<active-key-id>
ASTERISM_SECRET_KEYS=<key-id>=<base64-encoded-32-byte-key>[,...]
```

四个 Development Provider 默认关闭。仅在本地、已获授权的验证环境中启用：

```toml
[providers]
enable_development_chaoxing = false
enable_development_welearn = false
enable_development_uai = false
enable_development_cidaren = false
```

也可使用对应的 `ASTERISM_ENABLE_DEVELOPMENT_<PROVIDER>` 环境变量或 `asterismd` CLI 参数。启用开关只开放开发验证入口，不代表 Supported 或 Verified。

需要浏览器辅助登录时，可使用独立的 `asterism-capture`。它只主动连接 Asterism 服务和自己启动的 loopback DevTools，不应获得数据库或主密钥。

## 配置

配置优先级为：

```text
CLI > 环境变量 > 配置文件 > 默认值
```

主要配置位于 `asterism.example.toml`：

- `[server]`：监听地址、Session 有效期、安全 Cookie；
- `[database]`：SQLite URL；
- `[providers]`：Development Provider 开关；
- `[scheduler]`：扫描、执行并发、认领 TTL 与重试策略。

默认仅监听 loopback。在完成 TLS、反向代理、Cookie 与部署安全配置前，不要直接暴露到公网。

## 开发与贡献

欢迎外部贡献者提交 Issue 和 Pull Request。开始较大改动前，建议先通过 Issue 说明使用场景、影响范围和验证方式。

贡献时请遵守以下原则：

1. Provider 业务逻辑使用 Rust 实现，并通过明确的 Capability 暴露；
2. 保持 Task、Execution、远端状态和本地编排状态相互独立；
3. 非幂等操作必须有持久化 issue/receipt/recovery 边界，不能依赖进程内重试；
4. Provider 不直接访问数据库、Scheduler、WebUI 或用户权限状态；
5. 不复制来源不明或许可证不兼容的第三方实现；协议迁移应采用可审计的 clean-room 方式；
6. 不提交真实账号数据、Cookie、Token、密码、私有响应或可识别个人身份的 Fixture；
7. 新功能必须包含与风险相称的测试，并保持错误与日志脱敏。

提交前运行：

```bash
cargo fmt --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
```

WebUI 或 OpenAPI 变更还应运行：

```bash
cd web
npm ci
npm audit --audit-level=high
npm run generate:api
npm run typecheck
npm run build
```

建议使用 Conventional Commits，例如：

```text
feat(api): add owner-scoped endpoint
fix(provider): reject ambiguous response
docs(readme): clarify local setup
```

## 安全

- 不要在 Issue、PR、日志或 Fixture 中公开凭据和真实账号响应；
- 不要把 Asterism 当作通用 Cookie、浏览器数据或 HTTPS 凭据抓取器；
- 对真实平台执行写入前，必须确认账号所有权、操作范围和授权；
- 正式测评相关操作必须保留独立的策略保护和人工确认边界；
- 发现安全问题时，请优先使用 GitHub Security Advisory 私下报告，而不是公开包含利用细节的 Issue。

## 许可证与第三方来源

仓库当前未包含统一的开源许可证文件，因此默认版权规则适用。Provider 可能参考不同许可证或无明确许可证的上游协议证据；贡献者不得直接复制第三方代码，并应在提交中说明必要的来源与许可证影响。

## 鸣谢

感谢以下项目提供的思路及技术支持：

- Chaoxing 相关：[Samueli924/chaoxing](https://github.com/Samueli924/chaoxing)、[surinrasu/CxKitty](https://github.com/surinrasu/CxKitty)、[Ylim314/chaoxing-sign](https://github.com/Ylim314/chaoxing-sign)、[superdaobo/mini-hbut](https://github.com/superdaobo/mini-hbut)、[LangHY/chaoxing-exam](https://github.com/LangHY/chaoxing-exam)、[CodFrm/cxmooc-tools](https://github.com/CodFrm/cxmooc-tools)、[ocsjs/ocsjs](https://github.com/ocsjs/ocsjs)、[iwillwill-ALLWILL/chaoxing-agent-skill](https://github.com/iwillwill-ALLWILL/chaoxing-agent-skill)
- WELearn 相关：[Fanyuchang2026/welearn-helper](https://github.com/Fanyuchang2026/welearn-helper)、[YZBRH/Welearn_helper](https://github.com/YZBRH/Welearn_helper)、[1q2w-c/Auto_WeLearn](https://github.com/1q2w-c/Auto_WeLearn)
- UAI 相关：[create-try-now/AutoFinish_UxiaoyuanAI](https://github.com/create-try-now/AutoFinish_UxiaoyuanAI)、[Duster-Cule/UnipusHelperPro](https://github.com/Duster-Cule/UnipusHelperPro)、[uxudjs/UnipusAIAutoPlayer](https://github.com/uxudjs/UnipusAIAutoPlayer)、[Zzj-klwgxdz/UnipusAI](https://github.com/Zzj-klwgxdz/UnipusAI)
- Cidaren 相关：[MOPELotus/Easy_Cidaren](https://github.com/MOPELotus/Easy_Cidaren)、[ularch/Easy_Cidaren](https://github.com/ularch/Easy_Cidaren)、[github123666/cidaren](https://github.com/github123666/cidaren)
- QQ 机器人交互适配：[MOPELotus/Miao-Yunzai](https://github.com/MOPELotus/Miao-Yunzai)

Asterism 对相关协议与行为采用独立实现。以上鸣谢不代表代码复制、官方隶属、背书或持续兼容承诺；各项目版权与许可证归其原作者及贡献者所有。
