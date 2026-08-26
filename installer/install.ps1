[CmdletBinding()]
param(
    [string]$InstallRoot,
    [string]$SourceRoot,
    [string]$YunzaiRoot,
    [string]$Bind = "127.0.0.1:8068",
    [string]$WebUrl,
    [string]$DatabasePath,
    [switch]$SkipDependencyInstall,
    [switch]$SkipBuild,
    [switch]$RegisterTask,
    [switch]$NonInteractive
)

$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"

$currentIdentity = [Security.Principal.WindowsIdentity]::GetCurrent()
$principal = New-Object Security.Principal.WindowsPrincipal($currentIdentity)
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw "Windows 安装向导需要以管理员身份运行（会修改本地 ACL，并可选注册 SYSTEM 任务）。请右键 PowerShell 选择‘以管理员身份运行’后重试。"
}

function Write-Step([string]$Message) { Write-Host "`n==> $Message" -ForegroundColor Cyan }
function Refresh-ProcessPath {
    $machine = [Environment]::GetEnvironmentVariable("Path", "Machine")
    $user = [Environment]::GetEnvironmentVariable("Path", "User")
    $env:Path = "$machine;$user"
}
function Require-Command([string]$Name) {
    $command = Get-Command $Name -ErrorAction SilentlyContinue
    if (-not $command) { throw "未找到依赖 $Name。请安装后重新运行，或允许安装器使用 winget。" }
    return $command.Source
}
function Ask([string]$Prompt, [string]$Default) {
    if ($NonInteractive) { return $Default }
    $value = Read-Host "$Prompt [$Default]"
    if ([string]::IsNullOrWhiteSpace($value)) { return $Default }
    return $value.Trim()
}
function Ask-YesNo([string]$Prompt, [bool]$Default = $true) {
    if ($NonInteractive) { return $Default }
    $suffix = if ($Default) { "Y/n" } else { "y/N" }
    $value = Read-Host "$Prompt [$suffix]"
    if ([string]::IsNullOrWhiteSpace($value)) { return $Default }
    return $value.Trim().ToLowerInvariant() -in @("y", "yes", "是")
}
function Install-WithWinget([string]$Id, [string]$DisplayName) {
    if ($SkipDependencyInstall) { throw "缺少 $DisplayName，且已指定 -SkipDependencyInstall。" }
    if (-not (Get-Command winget -ErrorAction SilentlyContinue)) {
        throw "缺少 $DisplayName，系统也没有 winget；请手动安装后重试。"
    }
    Write-Host "安装 $DisplayName ..."
    & winget install --id $Id --exact --accept-source-agreements --accept-package-agreements
    if ($LASTEXITCODE -ne 0) { throw "$DisplayName 安装失败（winget exit $LASTEXITCODE）。" }
    Refresh-ProcessPath
}
function Ensure-Dependency([string]$Command, [string]$WingetId, [string]$DisplayName) {
    if (-not (Get-Command $Command -ErrorAction SilentlyContinue)) {
        Install-WithWinget $WingetId $DisplayName
    }
    Require-Command $Command | Out-Null
}
function New-SecretKey {
    $bytes = [byte[]]::new(32)
    [Security.Cryptography.RandomNumberGenerator]::Fill($bytes)
    try { return [Convert]::ToBase64String($bytes) } finally { [Array]::Clear($bytes, 0, $bytes.Length) }
}
function Set-PrivateFile([string]$Path, [string]$Content) {
    $parent = Split-Path -Parent $Path
    New-Item -ItemType Directory -Force $parent | Out-Null
    $Content | Set-Content -LiteralPath $Path -Encoding UTF8 -NoNewline
    $acl = Get-Acl -LiteralPath $Path
    $acl.SetAccessRuleProtection($true, $false)
    foreach ($identity in @(
        [System.Security.Principal.WindowsIdentity]::GetCurrent().Name,
        "NT AUTHORITY\SYSTEM",
        "BUILTIN\Administrators"
    )) {
        $rule = New-Object System.Security.AccessControl.FileSystemAccessRule($identity, "FullControl", "Allow")
        $acl.SetAccessRule($rule)
    }
    Set-Acl -LiteralPath $Path -AclObject $acl
}

Write-Step "收集安装参数"
$scriptRoot = Split-Path -Parent $MyInvocation.MyCommand.Path
$SourceRoot = if ($SourceRoot) { (Resolve-Path $SourceRoot).Path } else { (Resolve-Path (Join-Path $scriptRoot "..")).Path }
$InstallRoot = if ($InstallRoot) { [IO.Path]::GetFullPath($InstallRoot) } else { Ask "安装根目录" (Join-Path $SourceRoot "runtime") }
$YunzaiRoot = if ($YunzaiRoot) { [IO.Path]::GetFullPath($YunzaiRoot) } else { Ask "Yunzai 根目录（可留空跳过插件安装）" "" }
$WebUrl = if ($WebUrl) { $WebUrl } else { Ask "Web URL（反代地址可稍后填写）" ("http://" + $Bind) }
$DatabasePath = if ($DatabasePath) { [IO.Path]::GetFullPath($DatabasePath) } else { Join-Path $InstallRoot "asterism.db" }
$InstallRoot = [IO.Path]::GetFullPath($InstallRoot)
New-Item -ItemType Directory -Force $InstallRoot | Out-Null
New-Item -ItemType Directory -Force (Join-Path $InstallRoot "logs") | Out-Null

Write-Step "检测/安装 Windows 依赖"
Ensure-Dependency git Git.Git "Git"
Ensure-Dependency cargo Rustlang.Rustup "Rust/Cargo"
Ensure-Dependency node OpenJS.NodeJS.LTS "Node.js"
Ensure-Dependency npm OpenJS.NodeJS.LTS "npm"
Ensure-Dependency py Python.Python.3.12 "Python"
if (-not (Get-Command schtasks -ErrorAction SilentlyContinue)) { Write-Warning "未找到 schtasks；将跳过 Windows 任务注册。"; $RegisterTask = $false }

Write-Step "准备源码和 Python Worker 环境"
if ((Resolve-Path $SourceRoot).Path -ne $SourceRoot) { throw "源码目录解析失败。" }
$venv = Join-Path $InstallRoot ".venv-workers"
if (-not (Test-Path (Join-Path $venv "Scripts\python.exe"))) { & py -m venv $venv }
$python = Join-Path $venv "Scripts\python.exe"
foreach ($requirements in @("workers\chaoxing\requirements.txt", "workers\welearn\requirements.txt", "workers\uai\requirements.txt", "workers\cidaren\requirements.txt")) {
    $file = Join-Path $SourceRoot $requirements
    if (Test-Path $file) { & $python -m pip install -r $file }
}

Write-Step "检测 Chromium 兼容浏览器"
$browserCandidates = @(
    (Get-Command msedge.exe -ErrorAction SilentlyContinue).Source,
    (Get-Command chrome.exe -ErrorAction SilentlyContinue).Source,
    "${env:ProgramFiles(x86)}\Microsoft\Edge\Application\msedge.exe",
    "${env:ProgramFiles}\Microsoft\Edge\Application\msedge.exe",
    "${env:ProgramFiles}\Google\Chrome\Application\chrome.exe"
) | Where-Object { $_ -and (Test-Path $_) }
$browser = $browserCandidates | Select-Object -First 1
if (-not $browser) {
    if (Ask-YesNo "未找到 Edge/Chrome，是否尝试通过 winget 安装 Edge" $true) {
        Install-WithWinget "Microsoft.Edge" "Microsoft Edge"
        $browser = @("${env:ProgramFiles(x86)}\Microsoft\Edge\Application\msedge.exe", "${env:ProgramFiles}\Microsoft\Edge\Application\msedge.exe") | Where-Object { Test-Path $_ } | Select-Object -First 1
    }
}
if (-not $browser) { Write-Warning "未配置 Chromium 浏览器；Chaoxing/UAI 的浏览器兜底能力将保持不可用。" }

Write-Step "生成本地配置"
$configPath = Join-Path $InstallRoot "asterism.toml"
$dbUrl = "sqlite://" + ($DatabasePath -replace '\\', '/')
$secretPath = Join-Path $InstallRoot "secrets.env"
if (-not (Test-Path $secretPath)) {
    $secret = New-SecretKey
    Set-PrivateFile $secretPath @"
ASTERISM_SECRET_ACTIVE_KEY_ID=prod-v1
ASTERISM_SECRET_KEYS=prod-v1=$secret
"@
}
$enabled = @{}
foreach ($provider in @("chaoxing", "welearn", "uai", "cidaren")) { $enabled[$provider] = Ask-YesNo "启用 $provider" $true }
$config = @"
[server]
bind = "$Bind"
session_ttl_seconds = 43200
secure_cookies = false

[database]
url = "$dbUrl"

[ai]
remote_store = false

[ai.gpt_router]
base_url = ""
api_key_env = "ASTERISM_GPT_ROUTER_API_KEY"
protocol = "responses"

[ai.deepseek]
base_url = "https://api.deepseek.com"
api_key_env = "ASTERISM_DEEPSEEK_API_KEY"
protocol = "chat_completions"

[ai.kimi]
base_url = "https://api.moonshot.cn/v1"
api_key_env = "ASTERISM_KIMI_API_KEY"
protocol = "chat_completions"

[providers]
enable_development_chaoxing = $($enabled.chaoxing.ToString().ToLowerInvariant())
enable_development_welearn = $($enabled.welearn.ToString().ToLowerInvariant())
enable_development_uai = $($enabled.uai.ToString().ToLowerInvariant())
enable_development_cidaren = $($enabled.cidaren.ToString().ToLowerInvariant())

[scheduler]
enabled = true
tick_interval_seconds = 5
materialize_limit = 100
claim_limit = 1
execution_concurrency_limit = 32
claim_ttl_seconds = 300
retry_max_attempts = 5
retry_initial_delay_seconds = 30
retry_multiplier = 2
retry_max_delay_seconds = 1800
"@
if (Test-Path $configPath) { Copy-Item $configPath "$configPath.before-install-$(Get-Date -Format yyyyMMdd-HHmmss).bak" }
Set-PrivateFile $configPath $config

if (-not $SkipBuild) {
    Write-Step "构建 Asterism 和 WebUI"
    Push-Location $SourceRoot
    try {
        & cargo build --release --workspace
        if ($LASTEXITCODE -ne 0) { throw "Rust 构建失败。" }
        Push-Location web
        try {
            & npm ci
            & npm run typecheck
            & npm run build
            if ($LASTEXITCODE -ne 0) { throw "WebUI 构建失败。" }
        } finally { Pop-Location }
    } finally { Pop-Location }
}

Write-Step "生成启动脚本"
$runScript = Join-Path $InstallRoot "run-asterism.ps1"
$daemon = Join-Path $SourceRoot "target\release\asterismd.exe"
$envLines = Get-Content $secretPath | Where-Object { $_ -match '^ASTERISM_' } | ForEach-Object {
    $parts = $_ -split '=', 2
    "`$env:$($parts[0]) = '$($parts[1].Replace("'", "''"))'"
}
$runContent = @"
`$ErrorActionPreference = "Stop"
$($envLines -join "`n")
`$env:ASTERISM_UAI_WORKER_PYTHON = "$python"
$(if ($browser) { "`$env:ASTERISM_CHAOXING_BROWSER_EXECUTABLE = `"$browser`"`n`$env:ASTERISM_UAI_BROWSER_EXECUTABLE = `"$browser`"" } else { "" })
& "$daemon" --config "$configPath" --web-dist "$(Join-Path $SourceRoot 'web\dist')" --uai-worker-python "$python" *>> "$(Join-Path $InstallRoot 'logs\asterismd.log')"
"@
Set-PrivateFile $runScript $runContent

if ($YunzaiRoot -and (Test-Path $YunzaiRoot)) {
    Write-Step "安装 Yunzai 插件"
    $pluginTarget = Join-Path $YunzaiRoot "plugins\asterism-plugin"
    New-Item -ItemType Directory -Force (Split-Path -Parent $pluginTarget) | Out-Null
    if (Test-Path $pluginTarget) { Copy-Item $pluginTarget "$pluginTarget.before-install-$(Get-Date -Format yyyyMMdd-HHmmss)" -Recurse }
    Copy-Item (Join-Path $SourceRoot "integrations\yunzai-plugin\*") $pluginTarget -Recurse -Force
    Write-Warning "Yunzai 插件已复制，但 Service Token 需在首次 asterismctl init 后写入插件 config/asterism.json。"
}

if ($RegisterTask) {
    Write-Step "注册 Windows 任务"
    $taskName = "Asterism"
    & schtasks /Create /TN $taskName /SC ONSTART /RU SYSTEM /TR "powershell.exe -NoProfile -ExecutionPolicy Bypass -File `"$runScript`"" /F | Out-Host
    if ($LASTEXITCODE -ne 0) { throw "Windows 任务注册失败。" }
}

Write-Step "运行数据库迁移和初始账号向导"
if (-not (Test-Path (Join-Path $SourceRoot "target\release\asterismctl.exe"))) { throw "未找到 asterismctl.exe。请不要使用 -SkipBuild，或先完成构建。" }
$daemonProcess = Start-Process -FilePath "powershell.exe" -ArgumentList @("-NoProfile", "-ExecutionPolicy", "Bypass", "-File", $runScript) -WorkingDirectory $SourceRoot -WindowStyle Hidden -PassThru
for ($attempt = 0; $attempt -lt 30; $attempt++) {
    try {
        $health = Invoke-RestMethod -Uri ("http://" + $Bind + "/api/v1/system/health") -TimeoutSec 2
        if ($health.status -eq "ok") { break }
    } catch { Start-Sleep -Seconds 1 }
    if ($attempt -eq 29) { throw "asterismd 未能在 30 秒内通过健康检查（PID $($daemonProcess.Id)）。" }
}
Write-Host "请在下一步为首次 Master 输入密码："
$env:ASTERISM_CONFIG = $configPath
& (Join-Path $SourceRoot "target\release\asterismctl.exe") init --username master --url ("http://" + $Bind)

Write-Host "`n安装完成。" -ForegroundColor Green
Write-Host "本地 WebUI/API：$WebUrl"
Write-Host "健康检查：http://$Bind/api/v1/system/health"
Write-Host "反向代理、公网 HTTPS 和域名映射未由安装器处理。"
