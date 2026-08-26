# Windows 安装向导

`install.ps1` 是 Asterism 0.0.1 的 Windows 优先安装入口。它会检测/安装 Git、Rust、Node.js、npm、Python，创建 Worker 虚拟环境，构建 Rust 与 WebUI，生成本地配置和密钥，运行数据库迁移，复制可选的 Yunzai 插件，并执行本地健康检查。

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

可使用 `-NonInteractive` 配合显式参数执行无人值守安装；`-SkipDependencyInstall` 只允许在依赖已经由管理员准备好时使用；`-SkipBuild` 只适用于已有 release 构建产物的升级/重配置。

也可以从 JSON 读取路径、Provider、群号和联系方式：

```powershell
.\installer\install.ps1 -ConfigFile .\installer\config.example.json
```

首次安装会提示设置 Master 密码，并只显示一次初始管理令牌。配置了 Yunzai 目录时，安装器会自动创建仅含 `qq_identity_assert`、`task_command_proxy`、`notification_delivery_report` 的网关令牌并写入插件配置；重复安装会保留已存在的网关配置，不会重复创建令牌。

安装器不会配置 Apache/Caddy/Nginx、域名、TLS 或任何反向代理。完成后请自行将反代指向输出的本地地址。安装日志位于安装根目录 `logs`，配置和密钥位于安装根目录且使用当前 Windows 用户 ACL 保护。

四个 Python Worker 会通过同一个安装器创建的 `.venv-workers` 启动；daemon 启动脚本显式传入该 Python 路径，不依赖系统 Python 的当前目录或 PATH。

安装完成后可运行本机验收：

```powershell
.\installer\validate.ps1 -InstallRoot 'C:\Asterism\runtime'
```

该脚本只检查本机文件、Worker venv、WebUI、二进制和 API health，不会伪造平台账号，也不会执行 Provider 写操作。

## 安全与回滚

- 已存在的配置会先备份，重复运行应保持幂等。
- 不会把密钥写入 Git、前端构建产物或普通日志。
- 卸载/回滚默认不删除 SQLite、题库和日志；请先备份再清理。
- 缺失依赖优先通过 winget 官方包安装；无 winget 或网络失败时，安装器会停止并给出人工安装提示，不下载未知二进制。
