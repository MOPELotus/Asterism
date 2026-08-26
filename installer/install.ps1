[CmdletBinding()]
param(
    [string]$InstallRoot,
    [string]$SourceRoot,
    [string]$YunzaiRoot,
    [string]$Bind = "127.0.0.1:8068",
    [string]$WebUrl,
    [string]$DatabasePath,
    [string]$ConfigFile,
    [string]$AllowedGroups,
    [string]$NotificationGroups,
    [string]$AdminContact,
    [string]$MasterUsername = "master",
    [string]$MasterPasswordFile,
    [int]$GatewayTokenTtlSeconds = 31536000,
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

$inputConfig = if ($ConfigFile) {
    Get-Content -LiteralPath (Resolve-Path $ConfigFile) -Raw | ConvertFrom-Json
} else { $null }
if (-not $InstallRoot -and $inputConfig.installRoot) { $InstallRoot = [string]$inputConfig.installRoot }
if (-not $SourceRoot -and $inputConfig.sourceRoot) { $SourceRoot = [string]$inputConfig.sourceRoot }
if (-not $YunzaiRoot -and $inputConfig.yunzaiRoot) { $YunzaiRoot = [string]$inputConfig.yunzaiRoot }
if (-not $PSBoundParameters.ContainsKey("Bind") -and $inputConfig.bind) { $Bind = [string]$inputConfig.bind }
if (-not $WebUrl -and $inputConfig.webUrl) { $WebUrl = [string]$inputConfig.webUrl }
if (-not $DatabasePath -and $inputConfig.databasePath) { $DatabasePath = [string]$inputConfig.databasePath }
if (-not $AllowedGroups -and $inputConfig.allowedGroups) { $AllowedGroups = @($inputConfig.allowedGroups) -join "," }
if (-not $NotificationGroups -and $inputConfig.notificationGroups) { $NotificationGroups = @($inputConfig.notificationGroups) -join "," }
if (-not $AdminContact -and $inputConfig.adminContact) { $AdminContact = [string]$inputConfig.adminContact }
if (-not $PSBoundParameters.ContainsKey("MasterUsername") -and $inputConfig.masterUsername) { $MasterUsername = [string]$inputConfig.masterUsername }
if (-not $MasterPasswordFile -and $inputConfig.masterPasswordFile) { $MasterPasswordFile = [string]$inputConfig.masterPasswordFile }
if (-not $PSBoundParameters.ContainsKey("GatewayTokenTtlSeconds") -and $inputConfig.gatewayTokenTtlSeconds) { $GatewayTokenTtlSeconds = [int]$inputConfig.gatewayTokenTtlSeconds }
if (-not $PSBoundParameters.ContainsKey("RegisterTask") -and $inputConfig.registerTask) { $RegisterTask = $true }

Write-Step "收集安装参数"
$scriptRoot = Split-Path -Parent $MyInvocation.MyCommand.Path
$SourceRoot = if ($SourceRoot) { (Resolve-Path $SourceRoot).Path } else { (Resolve-Path (Join-Path $scriptRoot "..")).Path }
$InstallRoot = if ($InstallRoot) { [IO.Path]::GetFullPath($InstallRoot) } else { Ask "安装根目录" (Join-Path $SourceRoot "runtime") }
$YunzaiRoot = if ($YunzaiRoot) { [IO.Path]::GetFullPath($YunzaiRoot) } else { Ask "Yunzai 根目录（可留空跳过插件安装）" "" }
$WebUrl = if ($WebUrl) { $WebUrl } else { Ask "Web URL（反代地址可稍后填写）" ("http://" + $Bind) }
$DatabasePath = if ($DatabasePath) { [IO.Path]::GetFullPath($DatabasePath) } else { Join-Path $InstallRoot "asterism.db" }
$MasterPasswordFile = if ($MasterPasswordFile) { (Resolve-Path $MasterPasswordFile).Path } else { $null }
$InstallRoot = [IO.Path]::GetFullPath($InstallRoot)
New-Item -ItemType Directory -Force $InstallRoot | Out-Null
New-Item -ItemType Directory -Force (Join-Path $InstallRoot "logs") | Out-Null

Write-Step "检测/安装 Windows 依赖"
Ensure-Dependency git Git.Git "Git"
Ensure-Dependency cargo Rustlang.Rustup "Rust/Cargo"
Ensure-Dependency node OpenJS.NodeJS.LTS "Node.js"
Ensure-Dependency npm OpenJS.NodeJS.LTS "npm"
if (-not (Get-Command py -ErrorAction SilentlyContinue) -and -not (Get-Command python -ErrorAction SilentlyContinue)) {
    Install-WithWinget Python.Python.3.12 "Python"
}
$pythonLauncher = if (Get-Command python -ErrorAction SilentlyContinue) { (Get-Command python).Source } elseif (Get-Command py -ErrorAction SilentlyContinue) { (Get-Command py).Source } else { throw "未找到 Python 或 py launcher。" }
$pythonLauncherArgs = if ((Split-Path -Leaf $pythonLauncher) -like "py*") { @("-3") } else { @() }
if (-not (Get-Command schtasks -ErrorAction SilentlyContinue)) { Write-Warning "未找到 schtasks；将跳过 Windows 任务注册。"; $RegisterTask = $false }

Write-Step "准备源码和 Python Worker 环境"
if ((Resolve-Path $SourceRoot).Path -ne $SourceRoot) { throw "源码目录解析失败。" }
$venv = Join-Path $InstallRoot ".venv-workers"
if (-not (Test-Path (Join-Path $venv "Scripts\python.exe"))) {
    $venvArgs = @($pythonLauncherArgs) + @("-m", "venv", "--copies", $venv)
    & $pythonLauncher @venvArgs
    if ($LASTEXITCODE -ne 0 -or -not (Test-Path (Join-Path $venv "Scripts\python.exe"))) {
        $pyFallback = Get-Command py -ErrorAction SilentlyContinue
        if ($pyFallback -and $pythonLauncher -ne $pyFallback.Source) {
            & $pyFallback.Source @("-3", "-m", "venv", "--copies", $venv)
        }
    }
    if (-not (Test-Path (Join-Path $venv "Scripts\python.exe"))) {
        throw "Python venv 创建失败：$venv"
    }
}
$python = Join-Path $venv "Scripts\python.exe"
if (-not (Test-Path $python)) { throw "未找到 Worker Python：$python" }
foreach ($requirements in @("workers\chaoxing\requirements.txt", "workers\welearn\requirements.txt", "workers\uai\requirements.txt", "workers\cidaren\requirements.txt")) {
    $file = Join-Path $SourceRoot $requirements
    if (Test-Path $file) {
        & $python -m pip install --disable-pip-version-check --progress-bar off -r $file
        if ($LASTEXITCODE -ne 0) { throw "Python Worker 依赖安装失败：$file" }
    }
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
foreach ($provider in @("chaoxing", "welearn", "uai", "cidaren")) {
    $defaultEnabled = if ($inputConfig.providers -and $null -ne $inputConfig.providers.$provider) { [bool]$inputConfig.providers.$provider } else { $true }
    $enabled[$provider] = Ask-YesNo "启用 $provider" $defaultEnabled
}
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

$pluginTarget = $null
if ($YunzaiRoot -and (Test-Path $YunzaiRoot)) {
    Write-Step "安装 Yunzai 插件"
    $pluginTarget = Join-Path $YunzaiRoot "plugins\asterism-plugin"
    New-Item -ItemType Directory -Force (Split-Path -Parent $pluginTarget) | Out-Null
    if (Test-Path $pluginTarget) { Copy-Item $pluginTarget "$pluginTarget.before-install-$(Get-Date -Format yyyyMMdd-HHmmss)" -Recurse }
    Get-ChildItem -LiteralPath (Join-Path $SourceRoot "integrations\yunzai-plugin") -Force | ForEach-Object {
        Copy-Item -LiteralPath $_.FullName -Destination (Join-Path $pluginTarget $_.Name) -Recurse -Force
    }
    $AllowedGroups = if ($AllowedGroups) { $AllowedGroups } else { Ask "允许使用 Asterism 的群号（逗号分隔，留空表示全部）" "" }
    $NotificationGroups = if ($NotificationGroups) { $NotificationGroups } else { Ask "通知群号（逗号分隔，留空表示不投递）" "" }
    $AdminContact = if ($AdminContact) { $AdminContact } else { Ask "管理员联系方式" "" }
}

if ($RegisterTask) {
    Write-Step "注册 Windows 任务"
    $taskName = "Asterism"
    & schtasks /Create /TN $taskName /SC ONSTART /RU SYSTEM /TR "powershell.exe -NoProfile -ExecutionPolicy Bypass -File `"$runScript`"" /F | Out-Host
    if ($LASTEXITCODE -ne 0) { throw "Windows 任务注册失败。" }
}

Write-Step "运行数据库迁移和初始账号向导"
if (-not (Test-Path (Join-Path $SourceRoot "target\release\asterismctl.exe"))) { throw "未找到 asterismctl.exe。请不要使用 -SkipBuild，或先完成构建。" }
$bootstrapOut = Join-Path $InstallRoot "logs\bootstrap.stdout.log"
$bootstrapErr = Join-Path $InstallRoot "logs\bootstrap.stderr.log"
$daemonProcess = Start-Process -FilePath "powershell.exe" -ArgumentList @("-NoProfile", "-ExecutionPolicy", "Bypass", "-File", $runScript) -WorkingDirectory $SourceRoot -WindowStyle Hidden -RedirectStandardOutput $bootstrapOut -RedirectStandardError $bootstrapErr -PassThru
for ($attempt = 0; $attempt -lt 30; $attempt++) {
    try {
        $health = Invoke-RestMethod -Uri ("http://" + $Bind + "/api/v1/system/health") -TimeoutSec 2
        if ($health.status -eq "ok") { break }
    } catch { Start-Sleep -Seconds 1 }
    if ($attempt -eq 29) {
        $daemonLog = Join-Path $InstallRoot "logs\asterismd.log"
        $diagnosticFiles = @($daemonLog, $bootstrapOut, $bootstrapErr) | Where-Object { Test-Path $_ }
        $diagnostic = if ($diagnosticFiles) { ($diagnosticFiles | ForEach-Object { "--- $_ ---"; Get-Content -LiteralPath $_ -Tail 40 -ErrorAction SilentlyContinue }) -join "`n" } else { "（尚未生成 daemon/启动日志）" }
        throw "asterismd 未能在 30 秒内通过健康检查（PID $($daemonProcess.Id)，进程状态 $((Get-Process -Id $daemonProcess.Id -ErrorAction SilentlyContinue).HasExited)）。`n$diagnostic"
    }
}
$env:ASTERISM_CONFIG = $configPath
$cli = Join-Path $SourceRoot "target\release\asterismctl.exe"
$apiUrl = "http://" + $Bind
$health = Invoke-RestMethod -Uri ("http://" + $Bind + "/api/v1/system/health") -TimeoutSec 5
if ($null -eq $health.master_initialized) {
    throw "健康端点不是当前 Asterism 版本，或监听地址已被其他实例占用；请更换 -Bind 或先停止占用端口的服务。"
}
$pluginConfigPath = if ($pluginTarget) { Join-Path $pluginTarget "config\asterism.json" } else { $null }
$needsGatewayToken = $pluginTarget -and ((-not $health.master_initialized) -or -not (Test-Path $pluginConfigPath))
$needsAdminAuthentication = (-not $health.master_initialized) -or $needsGatewayToken
$adminToken = $null
$adminTokenId = $null
$createdInitialMaster = $false
if ($needsAdminAuthentication) {
    if ($NonInteractive -and -not $MasterPasswordFile) {
        throw "无人值守安装需要通过 -MasterPasswordFile（或配置文件的 masterPasswordFile）提供 Master 密码；安装器不接受命令行明文密码。"
    }
    $authCommand = if ($health.master_initialized) { @("auth", "login") } else { @("init") }
    $createdInitialMaster = -not $health.master_initialized
    if ($MasterPasswordFile) {
        $passwordLine = (Get-Content -LiteralPath $MasterPasswordFile -Raw).TrimEnd("`r", "`n")
        if ([string]::IsNullOrWhiteSpace($passwordLine)) { throw "Master 密码文件为空。" }
        try {
            $adminOutput = $passwordLine | & $cli --url $apiUrl @authCommand --username $MasterUsername --password-stdin
        } finally {
            $passwordLine = $null
        }
    } else {
        Write-Host (if ($createdInitialMaster) { "请为首次 Master 输入并确认密码：" } else { "请为现有 Master 输入密码，以创建缺失的 Yunzai 网关令牌：" })
        $adminOutput = & $cli --url $apiUrl @authCommand --username $MasterUsername
    }
    if ($LASTEXITCODE -ne 0) { throw "Master 初始化或认证失败。" }
    $adminResult = ($adminOutput -join "`n") | ConvertFrom-Json
    $adminToken = $adminResult.token
    $adminTokenId = $adminResult.metadata.id
    if (-not $adminToken) { throw "asterismctl 未返回管理 Service Token。" }
    if ($createdInitialMaster) {
        Write-Warning "以下初始管理 Service Token 只显示一次，请保存到密码管理器："
        $adminOutput | Write-Output
    }
}

if ($needsGatewayToken) {
    Write-Step "创建并写入 Yunzai 网关令牌"
    $env:ASTERISM_TOKEN = $adminToken
    try {
        $gatewayOutput = & $cli --url $apiUrl service-token create `
            --name yunzai-gateway `
            --scope provider-read,provider-manage,task-read,task-execute,qq-identity-assert,task-command-proxy,notification-delivery-report `
            --expires-in-seconds $GatewayTokenTtlSeconds
        if ($LASTEXITCODE -ne 0) { throw "Yunzai 网关 Service Token 创建失败。" }
        $gatewayToken = (($gatewayOutput -join "`n") | ConvertFrom-Json).token
        if (-not $gatewayToken) { throw "asterismctl 未返回 Yunzai 网关令牌。" }
        $pluginConfig = [ordered]@{
            apiUrl = $apiUrl
            webUrl = $WebUrl
            token = $gatewayToken
            allowedGroups = @($AllowedGroups -split '[\s,]+' | Where-Object { $_ })
            notificationGroups = @($NotificationGroups -split '[\s,]+' | Where-Object { $_ })
            notificationIntervalMs = 30000
            adminContact = $AdminContact
            requestTimeoutMs = 180000
        } | ConvertTo-Json -Depth 4
        Set-PrivateFile $pluginConfigPath ($pluginConfig + "`n")
        if (-not $createdInitialMaster -and $adminTokenId) {
            & $cli --url $apiUrl service-token revoke $adminTokenId | Out-Null
        }
    } finally {
        Remove-Item Env:ASTERISM_TOKEN -ErrorAction SilentlyContinue
        $adminToken = $null
        $gatewayToken = $null
    }
}

Write-Host "`n安装完成。" -ForegroundColor Green
Write-Host "本地 WebUI/API：$WebUrl"
Write-Host "健康检查：http://$Bind/api/v1/system/health"
Write-Host "反向代理、公网 HTTPS 和域名映射未由安装器处理。"
