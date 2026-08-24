# Asterism 0.0.1 部署

本页描述当前 upstream-backed 四 Provider 版本的单机部署。服务端运行
`asterismd`、SQLite、四个 Python Worker 和一个生产 WebUI；Web/QQ 用户不需要安装
donor 或 Python。

## 1. 获取源码和 donor

```bash
git clone --recurse-submodules --branch 0.0.1 https://github.com/MOPELotus/Asterism.git
cd Asterism
git submodule update --init --recursive
```

不要只复制主仓库目录。`upstreams/` 中固定 revision 的 donor 是运行时依赖。

## 2. 安装 Worker 运行时

Windows PowerShell：

```powershell
py -m venv .venv-workers
foreach ($file in @(
  'workers\chaoxing\requirements.txt',
  'workers\welearn\requirements.txt',
  'workers\uai\requirements.txt',
  'workers\cidaren\requirements.txt'
)) {
  .\.venv-workers\Scripts\python.exe -m pip install -r $file
}
$env:ASTERISM_UAI_WORKER_PYTHON = '.venv-workers\Scripts\python.exe'
```

Chaoxing DOM 兜底和 UAI 页面驻留需要 Chromium/Edge：

```powershell
$env:ASTERISM_CHAOXING_BROWSER_EXECUTABLE = 'C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe'
$env:ASTERISM_UAI_BROWSER_EXECUTABLE = $env:ASTERISM_CHAOXING_BROWSER_EXECUTABLE
```

## 3. 构建

```powershell
cargo build --release --workspace
Push-Location web
npm ci
npm run typecheck
npm run build
Pop-Location
```

生产 WebUI 位于 `web/dist`，由 `asterismd` 直接提供；不要另起 Vite 开发服务器。

## 4. 配置密钥和 Provider

复制 `asterism.example.toml` 为 `asterism.toml`，为当前授权部署启用四个 Development
Provider：

```toml
[providers]
enable_development_chaoxing = true
enable_development_welearn = true
enable_development_uai = true
enable_development_cidaren = true
```

生成只存在于部署环境的 32-byte SecretStore key：

```powershell
$bytes = [byte[]]::new(32)
[Security.Cryptography.RandomNumberGenerator]::Fill($bytes)
$key = [Convert]::ToBase64String($bytes)
$env:ASTERISM_SECRET_ACTIVE_KEY_ID = 'prod-v1'
$env:ASTERISM_SECRET_KEYS = "prod-v1=$key"
Remove-Variable bytes,key
```

将这两个值放入操作系统 Secret Manager 或服务管理器，不要写进 TOML、脚本或 Git。
轮换密钥前必须保留仍被数据库密文引用的旧 key id。

## 5. 启动与初始化

```powershell
.\target\release\asterismd.exe
```

首次初始化会在终端安全提示输入并确认 Master 密码，同时返回仅显示一次的 Service
Token：

```powershell
cargo run --release -p asterismctl -- init --username master
```

浏览器打开 `http://127.0.0.1:8068`。生产远程访问应让 `asterismd` 继续监听 loopback，
由 Caddy/Nginx 提供 HTTPS 反向代理；启用 HTTPS 后将 `secure_cookies` 设为 `true`。

## 6. 部署验收

```powershell
Invoke-RestMethod http://127.0.0.1:8068/api/v1/system/health
```

随后在 WebUI 中依次确认：

1. 添加四个平台账号并完成认证；
2. 账号页同步课程与任务；
3. 课程页能按平台语义展示章节、作业、考试或资源；
4. UAI 任务页能读取单任务和课程总时长；
5. Chaoxing 有题任务能进入题目快照与答案审核；
6. 执行页能显示 Job、实时日志和最终状态。

WELearn donor 当前没有声明开源许可证。当前私有授权部署可以固定调用该 submodule；
公开再分发包含 donor 源码的安装包前，应先取得许可证或上游许可。
