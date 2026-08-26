# Windows 安装向导

`install.ps1` 是 Asterism 0.0.1 的 Windows 优先安装入口。它会检测 Git 和 Python、创建 Worker 虚拟环境、优先安装官方预编译 Rust/WebUI 产物、生成本地配置和密钥、运行数据库迁移、复制可选的 Yunzai 插件并执行本地健康检查。只有回退或显式选择源码构建时才检测/安装 Rust、MSVC/Windows SDK、Node.js 和 npm。

安装器脚本使用 UTF-8 BOM，同时支持 Windows PowerShell 5.1 和 PowerShell 7；CI 会用 Windows PowerShell 5.1 对仓库内全部 `.ps1` 做 parser validation。默认安装优先下载 CI 已验证并与当前 Git HEAD、`Cargo.lock`、`web/package-lock.json` 完全匹配的官方 Windows 预编译包，因此普通服务器部署不再需要本机编译 Rust/WebUI，也不要求安装 Rust、Node.js 或 Visual Studio Build Tools。Python 仍用于 Provider Worker。

只有指定 `-SourceBuild`/`-ForceBuild`，或当前提交尚无匹配预编译包时，才回退到源码构建。源码构建 Rust release binary 必须有 Visual Studio Build Tools 2022 的 **Desktop development with C++** 工作负载，并勾选 MSVC x64/x86 tools 和 Windows 10/11 SDK。安装器会通过 `vswhere`/Visual Studio 安装目录检查这些组件，不会把普通 PowerShell 中 `where.exe link` 找不到当成唯一判据。

## 交互安装

```powershell
Set-ExecutionPolicy -Scope Process Bypass
.\installer\install.ps1
```

常用参数：

```powershell
.\installer\install.ps1 `
  -InstallRoot 'C:\Asterism\runtime' `
  -YunzaiRoot 'C:\Yunzai' `
  -Bind '127.0.0.1:8068' `
  -RegisterTask
```

可使用 `-NonInteractive` 配合显式参数执行无人值守安装。首次初始化或补建 Yunzai 网关令牌时，还必须通过 `-MasterPasswordFile` 提供只含一行密码的本地文件；安装器刻意不接受命令行明文密码。`-SkipDependencyInstall` 只允许在依赖已经由管理员准备好时使用；`-SkipBuild` 只适用于已有 release 构建产物的升级/重配置，且会在缺少 `asterismd.exe`、`asterismctl.exe` 或 `web/dist/index.html` 时立即失败；`-SourceBuild` 强制使用本机源码工具链，`-ForceBuild` 还会忽略现有构建缓存。二者都不能和 `-SkipBuild` 同时使用。内网环境可用 `-PrebuiltArchive C:\path\asterism-windows-x64.zip` 指定提前下载的包；包内提交与当前源码不一致时会拒绝复用并回退源码构建。

默认运行会检查 `runtime/build-stamp.json`、Git HEAD、`Cargo.lock`、`web/package-lock.json`、影响构建的工作区修改以及 release/WebUI 产物。全部匹配时输出“检测到有效现有构建产物，跳过 Rust/WebUI 构建”；源码、锁文件、相关未提交修改或产物变化时只重建一次并更新 stamp。这样安装中途在初始化阶段失败后重新运行不会再次编译几十分钟。安装器会把 WebUI 的 OpenAPI 预生成也放在 cargo release profile，避免重复生成 debug 依赖。

当本地缓存无效且未强制源码构建时，安装器从 `MOPELotus/Asterism` 的 `windows-latest` Release 下载 `asterism-windows-x64.zip` 与独立 SHA-256 文件，先校验下载内容，再校验包内 Git HEAD 和两个 lockfile 指纹。只有三者完全匹配且工作区没有影响构建的修改时才安装预编译产物；否则清楚说明原因并回退源码构建。

也可以从 JSON 读取路径、Provider、群号和联系方式：

```powershell
.\installer\install.ps1 -ConfigFile .\installer\config.example.json
```

无人值守示例：

```powershell
.\installer\install.ps1 `
  -ConfigFile .\installer\config.example.json `
  -MasterPasswordFile C:\Asterism\bootstrap-password.txt `
  -NonInteractive
```

密码文件只用于向 `asterismctl --password-stdin` 传递密码，不会复制进安装目录或写入日志。部署后应删除该临时文件，或至少将 ACL 限制为管理员和 SYSTEM。

交互式首次安装会提示设置 Master 密码，并只显示一次初始管理令牌；无人值守安装不会把初始令牌输出到 CI/终端日志，完成内部初始化后会将它撤销。配置了 Yunzai 目录时，安装器会自动创建含 `provider_read`、`provider_manage`、`task_read`、`task_execute`、`qq_identity_assert`、`task_command_proxy`、`notification_delivery_report` 的最小网关令牌并写入插件配置；重复安装会保留已存在的网关配置，不会重复创建令牌。配置落盘失败时会尝试撤销本次创建的网关/临时管理令牌。`task_command_proxy` 只允许受信任的 Yunzai 网关在显式目标用户边界内执行代操作，普通 Service Token 不能借此跨 owner。

安装器不会配置 Apache/Caddy/Nginx、域名、TLS 或任何反向代理。完成后请自行将反代指向输出的本地地址。安装日志位于安装根目录 `logs`，配置和密钥位于安装根目录且使用当前 Windows 用户、SYSTEM、Administrators 的显式 ACL 保护。启动脚本运行时读取 `secrets.env`，不会再把密钥复制进另一个脚本文件。

四个 Python Worker 会通过同一个安装器创建的 `.venv-workers` 启动；daemon 启动脚本显式传入该 Python 路径，不依赖系统 Python 的当前目录或 PATH。

四个平台 donor 使用 Git submodule。安装器会在创建 Worker 环境前检查所需入口；缺失时先执行 `git submodule sync --recursive`，再执行 `git submodule update --init --recursive`，仍不完整则在启动服务前停止并列出缺失文件。不要使用 GitHub 的源码 ZIP 部署，因为 ZIP 不包含 submodule。仓库只固定远端仍可重现获取的 revision；必要的兼容修复保留在 Asterism 薄适配层，不把 gitlink 指向上游未发布或已回收的 commit。至少应存在：

- `upstreams\chaoxing-exam\cxapi\api.py`
- `upstreams\chaoxing\api\base.py`
- `upstreams\welearn\welearn_decompiled.py`
- `upstreams\uai\配置我运行我.py`
- `upstreams\uai-browser\unipus_ai_auto_player.user.js`
- `upstreams\cidaren\api\login.py`

生成的 `run-asterism.ps1` 会先切换到源码根目录，并把全部 Worker、donor 和元数据路径设为绝对路径。这保证 SYSTEM 启动任务不会因默认工作目录是 `C:\Windows\System32` 而找错文件，也避免中文 UAI 入口经过相对路径或当前代码页时失真。本地 Worker 文件/依赖缺失属于部署错误，不再对用户显示为远端平台不可用。

安装完成后可运行本机验收：

```powershell
.\installer\validate.ps1 -InstallRoot 'C:\Asterism\runtime'
```

该脚本只检查本机文件、Worker venv、WebUI、二进制、构建 stamp、PowerShell parser 和 API health，不会伪造平台账号，也不会执行 Provider 写操作。它还会检查关键配置路径、Yunzai 插件入口和 secret 文件权限；静态检查失败时不会触碰运行中的 Provider。

## 安全与回滚

- 已存在的配置会先备份，重复运行应保持幂等。
- 不会把密钥写入 Git、前端构建产物或普通日志。
- 卸载/回滚默认不删除 SQLite、题库和日志；请先备份再清理。
- 缺失依赖优先通过 winget 官方包安装；Windows Server/LTSC 没有 winget 时，安装器会给出逐项官方地址（Git、rustup、Node.js、Python、Edge、Visual Studio Build Tools）并停止，不下载未知二进制。
- Edge/Chrome 是 Chaoxing/UAI 浏览器兜底能力的可选依赖；无人值守服务器未安装浏览器时会明确警告并继续，不会因为没有 winget 阻断其余安装。
- 安装器会在启动前检查监听端口，并通过 asterismd 可执行路径和 `--config` 命令行确认当前实例归属；端口已有无法确认归属的实例时会明确停止，绝不只凭 health 的 `master_initialized` 创建 Master/token。
- 安装器只清理由本次启动且明确归属的临时 daemon；失败时清理 wrapper 和子 daemon，成功时按设计保留。已有同一安装配置的 daemon 会复用，不会重复启动或误杀。
- 已存在的 `secrets.env`、数据库、Yunzai 网关 token 和插件配置会保留；配置备份、任务注册和 venv 重跑均按幂等路径处理。
