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
    [switch]$ForceBuild,
    [switch]$RegisterTask,
    [switch]$NonInteractive
)

$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"

if ($SkipBuild -and $ForceBuild) {
    throw "-SkipBuild 与 -ForceBuild 不能同时使用。"
}

$script:installCommitted = $false
$script:installerDaemonProcess = $null
$script:installerDaemonOwned = $false
$script:installerDaemonChildPids = @()
$script:daemonPidsBefore = @()
$script:installerDaemonPath = $null
$script:installerConfigPath = $null

$currentIdentity = [Security.Principal.WindowsIdentity]::GetCurrent()
$principal = New-Object Security.Principal.WindowsPrincipal($currentIdentity)
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw "Windows 安装向导需要以管理员身份运行（会修改本地 ACL，并可选注册 SYSTEM 任务）。请右键 PowerShell 选择‘以管理员身份运行’后重试。"
}

function Write-Step([string]$Message) { Write-Host "`n==> $Message" -ForegroundColor Cyan }
function Invoke-NativeCommand([string]$DisplayName, [string]$FilePath, [string[]]$Arguments = @()) {
    Write-Host ("运行 {0} {1}" -f $DisplayName, (($Arguments | ForEach-Object { if ($_ -match '\s') { '"' + $_ + '"' } else { $_ } }) -join ' ')) -ForegroundColor DarkGray
    & $FilePath @Arguments
    $exitCode = $LASTEXITCODE
    if ($null -eq $exitCode) { $exitCode = 0 }
    if ($exitCode -ne 0) {
        throw "$DisplayName 失败（exit $exitCode）：$FilePath"
    }
}
function Invoke-NativeCapture([string]$DisplayName, [string]$FilePath, [string[]]$Arguments = @()) {
    $output = & $FilePath @Arguments 2>&1
    $exitCode = $LASTEXITCODE
    if ($null -eq $exitCode) { $exitCode = 0 }
    if ($exitCode -ne 0) {
        throw "$DisplayName 失败（exit $exitCode）：$FilePath"
    }
    return @($output)
}
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
        $manual = switch ($DisplayName) {
            "Git" { "https://git-scm.com/download/win"; break }
            "Rust/Cargo" { "https://rustup.rs/"; break }
            "Node.js" { "https://nodejs.org/en/download"; break }
            "Python" { "https://www.python.org/downloads/windows/"; break }
            "Microsoft Edge" { "https://www.microsoft.com/edge/download"; break }
            default { "请参阅项目安装文档中的依赖说明" }
        }
        throw "缺少 $DisplayName，且系统没有 winget。请手动安装后重新运行：$manual"
    }
    Write-Host "安装 $DisplayName ..."
    Invoke-NativeCommand "winget 安装 $DisplayName" "winget" @("install", "--id", $Id, "--exact", "--accept-source-agreements", "--accept-package-agreements")
    Refresh-ProcessPath
}
function Ensure-Dependency([string]$Command, [string]$WingetId, [string]$DisplayName) {
    if (-not (Get-Command $Command -ErrorAction SilentlyContinue)) {
        Install-WithWinget $WingetId $DisplayName
    }
    Require-Command $Command | Out-Null
}
function Ensure-MinimumVersion([string]$DisplayName, [string]$Command, [string[]]$Arguments, [version]$Minimum) {
    $raw = ((Invoke-NativeCapture "读取 $DisplayName 版本" $Command $Arguments) -join "").Trim() -replace '^[^0-9]*', ''
    $match = [regex]::Match($raw, '^\d+(\.\d+){1,3}')
    if (-not $match.Success -or ([version]$match.Value) -lt $Minimum) {
        throw "$DisplayName 版本过低或无法识别（当前：$raw，最低：$Minimum）。请从官方来源升级后重试。"
    }
}
function New-SecretKey {
    $bytes = [byte[]]::new(32)
    $rng = [Security.Cryptography.RandomNumberGenerator]::Create()
    try {
        $rng.GetBytes($bytes)
        return [Convert]::ToBase64String($bytes)
    } finally {
        $rng.Dispose()
        [Array]::Clear($bytes, 0, $bytes.Length)
    }
}
function Get-Sha256([string]$Path) {
    if (-not (Test-Path -LiteralPath $Path)) { return $null }
    return (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant()
}
function Get-GitHead([string]$Root) {
    try { return ((Invoke-NativeCapture "读取 Git HEAD" "git" @("-C", $Root, "rev-parse", "HEAD")) -join "").Trim() } catch { return $null }
}
function Get-BuildDirtyEntries([string]$Root) {
    $lines = try { Invoke-NativeCapture "检查源码工作区" "git" @("-C", $Root, "status", "--porcelain", "--untracked-files=all") } catch { @() }
    @($lines | Where-Object {
        $line = [string]$_
        if (-not $line -or $line.Length -lt 4) { return $false }
        $path = $line.Substring(3).Trim('"') -replace '\\', '/'
        return $path -match '^(Cargo\.(toml|lock)|bins/|crates/|providers/|migrations/|workers/uai/client-rs/|web/(?!dist/|node_modules/))'
    })
}
function Get-BuildFingerprint([string]$Root) {
    [ordered]@{
        sourceRoot = $Root
        gitHead = Get-GitHead $Root
        cargoLock = Get-Sha256 (Join-Path $Root "Cargo.lock")
        webLock = Get-Sha256 (Join-Path $Root "web\package-lock.json")
        dirty = @(Get-BuildDirtyEntries $Root)
    }
}
function Get-RequiredBuildArtifacts([string]$Root) {
    @(
        (Join-Path $Root "target\release\asterismd.exe"),
        (Join-Path $Root "target\release\asterismctl.exe"),
        (Join-Path $Root "web\dist\index.html")
    )
}
function Test-BuildArtifacts([string]$Root) {
    $missing = @(Get-RequiredBuildArtifacts $Root | Where-Object { -not (Test-Path -LiteralPath $_) -or ((Get-Item -LiteralPath $_).Length -le 0) })
    return [pscustomobject]@{ Valid = ($missing.Count -eq 0); Missing = $missing }
}
function Test-BuildCache([string]$Root, [string]$StampPath) {
    $artifacts = Test-BuildArtifacts $Root
    if (-not $artifacts.Valid) { return [pscustomobject]@{ Valid = $false; Reason = "缺少构建产物：$($artifacts.Missing -join ', ')" } }
    if (-not (Test-Path -LiteralPath $StampPath)) { return [pscustomobject]@{ Valid = $false; Reason = "没有 build stamp" } }
    try { $stamp = Get-Content -LiteralPath $StampPath -Raw | ConvertFrom-Json } catch { return [pscustomobject]@{ Valid = $false; Reason = "build stamp 无法解析" } }
    $current = Get-BuildFingerprint $Root
    if (-not $current.gitHead) { return [pscustomobject]@{ Valid = $false; Reason = "无法读取 Git HEAD" } }
    foreach ($key in @("sourceRoot", "gitHead", "cargoLock", "webLock")) {
        if ([string]$stamp.$key -ne [string]$current[$key]) { return [pscustomobject]@{ Valid = $false; Reason = "$key 已变化" } }
    }
    if (@($current.dirty).Count -gt 0 -or [bool]$stamp.dirty) { return [pscustomobject]@{ Valid = $false; Reason = "工作区存在影响构建的未提交修改" } }
    foreach ($artifact in @($stamp.artifacts)) {
        if (-not $artifact.path -or -not (Test-Path -LiteralPath $artifact.path) -or (Get-Sha256 $artifact.path) -ne [string]$artifact.sha256) {
            return [pscustomobject]@{ Valid = $false; Reason = "构建产物校验失败：$($artifact.path)" }
        }
    }
    [pscustomobject]@{ Valid = $true; Reason = "HEAD、lockfile、工作区和产物均匹配" }
}
function Write-BuildStamp([string]$Root, [string]$Path) {
    $fingerprint = Get-BuildFingerprint $Root
    $stamp = [ordered]@{
        schema = 1
        profile = "release"
        builtAtUtc = [DateTime]::UtcNow.ToString("o")
        sourceRoot = $fingerprint.sourceRoot
        gitHead = $fingerprint.gitHead
        cargoLock = $fingerprint.cargoLock
        webLock = $fingerprint.webLock
        dirty = (@($fingerprint.dirty).Count -gt 0)
        artifacts = @(Get-RequiredBuildArtifacts $Root | ForEach-Object {
            $fullPath = [IO.Path]::GetFullPath($_)
            [ordered]@{ path = $fullPath; sha256 = Get-Sha256 $fullPath }
        })
    }
    Set-PrivateFile $Path (($stamp | ConvertTo-Json -Depth 5) + "`n")
}
function Set-PrivateFile([string]$Path, [string]$Content, [switch]$WithBom) {
    $parent = Split-Path -Parent $Path
    New-Item -ItemType Directory -Force $parent | Out-Null
    if ($WithBom) {
        [IO.File]::WriteAllText($Path, $Content, [Text.UTF8Encoding]::new($true))
    } else {
        [IO.File]::WriteAllText($Path, $Content, [Text.UTF8Encoding]::new($false))
    }
    $acl = Get-Acl -LiteralPath $Path
    $acl.SetAccessRuleProtection($true, $false)
    foreach ($existingIdentity in @($acl.Access | Select-Object -ExpandProperty IdentityReference -Unique)) {
        $acl.PurgeAccessRules($existingIdentity)
    }
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
function Find-VsWhere {
    @(
        (Join-Path ${env:ProgramFiles(x86)} "Microsoft Visual Studio\Installer\vswhere.exe"),
        (Join-Path $env:ProgramFiles "Microsoft Visual Studio\Installer\vswhere.exe")
    ) | Where-Object { $_ -and (Test-Path -LiteralPath $_) } | Select-Object -First 1
}
function Find-MsvcInstallation {
    $paths = New-Object System.Collections.Generic.List[string]
    $vswhere = Find-VsWhere
    if ($vswhere) {
        try {
            $found = Invoke-NativeCapture "查找 Visual Studio 安装" $vswhere @("-latest", "-products", "*", "-requires", "Microsoft.VisualStudio.Component.VC.Tools.x86.x64", "-property", "installationPath")
            foreach ($line in $found) { if ($line -and (Test-Path -LiteralPath ([string]$line).Trim())) { $paths.Add(([string]$line).Trim()) } }
        } catch { Write-Warning "vswhere 未能返回完整安装信息，将继续检查标准安装目录：$($_.Exception.Message)" }
    }
    foreach ($base in @(
        (Join-Path ${env:ProgramFiles} "Microsoft Visual Studio\2022\BuildTools"),
        (Join-Path ${env:ProgramFiles} "Microsoft Visual Studio\2022\Community"),
        (Join-Path ${env:ProgramFiles} "Microsoft Visual Studio\2022\Professional"),
        (Join-Path ${env:ProgramFiles} "Microsoft Visual Studio\2022\Enterprise"),
        (Join-Path ${env:ProgramFiles(x86)} "Microsoft Visual Studio\2022\BuildTools")
    )) { if ($base -and (Test-Path -LiteralPath $base) -and -not $paths.Contains($base)) { $paths.Add($base) } }
    foreach ($path in $paths) {
        $msvc = @(Get-ChildItem -LiteralPath (Join-Path $path "VC\Tools\MSVC") -Directory -ErrorAction SilentlyContinue | Sort-Object Name -Descending | Select-Object -First 1)
        if (-not $msvc) { continue }
        $link = Join-Path $msvc.FullName "bin\Hostx64\x64\link.exe"
        $sdkLib = Join-Path ${env:ProgramFiles(x86)} "Windows Kits\10\Lib"
        $sdk = $sdkLib -and (Test-Path -LiteralPath $sdkLib) -and @(Get-ChildItem -LiteralPath $sdkLib -Directory -ErrorAction SilentlyContinue).Count -gt 0
        if ((Test-Path -LiteralPath $link) -and $sdk) {
            return [pscustomobject]@{ InstallationPath = $path; Link = $link; VsDevCmd = (Join-Path $path "Common7\Tools\VsDevCmd.bat") }
        }
    }
    return $null
}
function Ensure-MsvcToolchain {
    $toolchain = Find-MsvcInstallation
    if (-not $toolchain) {
        throw @"
未检测到可用的 MSVC Rust 构建工具链。x86_64-pc-windows-msvc 构建不仅需要 cargo/rustc，还需要：
Visual Studio Build Tools 2022 -> Desktop development with C++
并勾选 MSVC x64/x86 tools 与 Windows 10/11 SDK。
安装后重新打开 PowerShell 再运行安装器；Windows Server/LTSC 没有 winget 时请从
https://visualstudio.microsoft.com/visual-cpp-build-tools/ 手动安装。
"@
    }
    Write-Host "检测到 MSVC/Windows SDK：$($toolchain.InstallationPath)" -ForegroundColor DarkGray
    return $toolchain
}
function Import-VsDevEnvironment([object]$Toolchain) {
    if (-not $Toolchain -or -not (Test-Path -LiteralPath $Toolchain.VsDevCmd)) { return }
    $lines = & cmd.exe /d /s /c "call `"$($Toolchain.VsDevCmd)`" -arch=x64 >nul && set" 2>$null
    $exitCode = $LASTEXITCODE
    if ($exitCode -ne 0) { throw "加载 Visual Studio 开发环境失败（exit $exitCode）。" }
    foreach ($line in $lines) {
        $text = [string]$line
        $index = $text.IndexOf('=')
        if ($index -gt 0) { [Environment]::SetEnvironmentVariable($text.Substring(0, $index), $text.Substring($index + 1), "Process") }
    }
}
function Get-BindPort([string]$Value) {
    if ($Value -match ':(\d+)$') { return [int]$Matches[1] }
    throw "Bind 必须包含端口，例如 127.0.0.1:8068。"
}
function Get-LocalApiUrl([string]$Value) {
    if ($Value -match '^0\.0\.0\.0:(\d+)$') { return "http://127.0.0.1:$($Matches[1])" }
    if ($Value -match '^\[?::\]?:(\d+)$') { return "http://[::1]:$($Matches[1])" }
    return "http://$Value"
}
function Get-AsterismProcessInfo {
    @(Get-CimInstance Win32_Process -Filter "Name='asterismd.exe'" -ErrorAction SilentlyContinue | ForEach-Object {
        [pscustomobject]@{ Pid = [int]$_.ProcessId; ParentPid = [int]$_.ParentProcessId; CommandLine = [string]$_.CommandLine; ExecutablePath = [string]$_.ExecutablePath }
    })
}
function Get-ListeningProcessIds([int]$Port) {
    try { return @((Get-NetTCPConnection -LocalPort $Port -State Listen -ErrorAction Stop) | Select-Object -ExpandProperty OwningProcess -Unique) } catch {
        $lines = & netstat.exe -ano -p tcp 2>$null
        $code = $LASTEXITCODE
        if ($code -ne 0) { return @() }
        return @($lines | ForEach-Object {
            if (([string]$_) -match ("^\s*TCP\s+\S+:" + $Port + "\s+\S+\s+LISTENING\s+(\d+)\s*$")) { [int]$Matches[1] }
        } | Where-Object { $null -ne $_ } | Select-Object -Unique)
    }
}
function Stop-InstallerDaemon {
    if (-not $script:installerDaemonOwned) { return }
    $lateChildren = @(Get-AsterismProcessInfo | Where-Object {
        $script:daemonPidsBefore -notcontains $_.Pid -and
        $_.ExecutablePath -and $script:installerDaemonPath -and
        ((Resolve-Path $_.ExecutablePath -ErrorAction SilentlyContinue).Path -eq $script:installerDaemonPath) -and
        (($_.ParentPid -eq $script:installerDaemonProcess.Id) -or ($_.CommandLine -and $_.CommandLine -like "*--config*$script:installerConfigPath*"))
    } | Select-Object -ExpandProperty Pid)
    foreach ($processId in @($script:installerDaemonChildPids + $lateChildren | Select-Object -Unique)) {
        Stop-Process -Id $processId -Force -ErrorAction SilentlyContinue
    }
    if ($script:installerDaemonProcess) {
        Stop-Process -Id $script:installerDaemonProcess.Id -Force -ErrorAction SilentlyContinue
    }
    $script:installerDaemonOwned = $false
}
trap {
    $originalError = $_
    if (-not $script:installCommitted -and (Get-Command Stop-InstallerDaemon -ErrorAction SilentlyContinue)) { Stop-InstallerDaemon }
    throw $originalError
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
$databaseParent = Split-Path -Parent $DatabasePath
if ($databaseParent) { New-Item -ItemType Directory -Force $databaseParent | Out-Null }

Write-Step "检测/安装 Windows 依赖"
Ensure-Dependency git Git.Git "Git"
Ensure-Dependency cargo Rustlang.Rustup "Rust/Cargo"
Ensure-Dependency rustc Rustlang.Rustup "Rust/Cargo"
Ensure-MinimumVersion "Rust" "rustc" @("--version") ([version]"1.97.0")
Ensure-Dependency node OpenJS.NodeJS.LTS "Node.js"
Ensure-Dependency npm OpenJS.NodeJS.LTS "npm"
Ensure-MinimumVersion "Node.js" "node" @("--version") ([version]"22.18.0")
if (-not (Get-Command py -ErrorAction SilentlyContinue) -and -not (Get-Command python -ErrorAction SilentlyContinue)) {
    Install-WithWinget Python.Python.3.12 "Python"
}
$pythonLauncher = if (Get-Command python -ErrorAction SilentlyContinue) { (Get-Command python).Source } elseif (Get-Command py -ErrorAction SilentlyContinue) { (Get-Command py).Source } else { throw "未找到 Python 或 py launcher。" }
$pythonLauncherArgs = if ((Split-Path -Leaf $pythonLauncher) -like "py*") { @("-3") } else { @() }
Ensure-MinimumVersion "Python" $pythonLauncher (@($pythonLauncherArgs) + @("--version")) ([version]"3.12.0")
if (-not (Get-Command schtasks -ErrorAction SilentlyContinue)) { Write-Warning "未找到 schtasks；将跳过 Windows 任务注册。"; $RegisterTask = $false }
$buildStampPath = Join-Path $InstallRoot "build-stamp.json"
$buildCache = Test-BuildCache $SourceRoot $buildStampPath
if ($SkipBuild) {
    $artifactCheck = Test-BuildArtifacts $SourceRoot
    if (-not $artifactCheck.Valid) { throw "已指定 -SkipBuild，但构建产物不完整：$($artifactCheck.Missing -join ', ')" }
    Write-Host "已指定 -SkipBuild，使用现有构建产物（不检查源码匹配）。" -ForegroundColor Yellow
} elseif (-not $ForceBuild -and $buildCache.Valid) {
    Write-Host "检测到有效现有构建产物，跳过 Rust/WebUI 构建。" -ForegroundColor Green
} elseif ($ForceBuild) {
    Write-Host "已指定 -ForceBuild，无条件重新构建。" -ForegroundColor Yellow
} else {
    Write-Host "需要重新构建：$($buildCache.Reason)" -ForegroundColor Yellow
}

Write-Step "准备源码和 Python Worker 环境"
if ((Resolve-Path $SourceRoot).Path -ne $SourceRoot) { throw "源码目录解析失败。" }
$venv = Join-Path $InstallRoot ".venv-workers"
if (-not (Test-Path (Join-Path $venv "Scripts\python.exe"))) {
    $venvArgs = @($pythonLauncherArgs) + @("-m", "venv", "--copies", $venv)
    & $pythonLauncher @venvArgs
    if ($LASTEXITCODE -ne 0 -or -not (Test-Path (Join-Path $venv "Scripts\python.exe"))) {
        $pyFallback = Get-Command py -ErrorAction SilentlyContinue
        if ($pyFallback -and $pythonLauncher -ne $pyFallback.Source) {
            Invoke-NativeCommand "Python venv 备用创建" $pyFallback.Source @("-3", "-m", "venv", "--copies", $venv)
        }
    }
    if (-not (Test-Path (Join-Path $venv "Scripts\python.exe"))) {
        throw "Python venv 创建失败：$venv"
    }
}
$python = Join-Path $venv "Scripts\python.exe"
if (-not (Test-Path $python)) { throw "未找到 Worker Python：$python" }
$requirementFiles = @("workers\chaoxing\requirements.txt", "workers\welearn\requirements.txt", "workers\uai\requirements.txt", "workers\cidaren\requirements.txt") | ForEach-Object { Join-Path $SourceRoot $_ } | Where-Object { Test-Path -LiteralPath $_ }
$workerStampPath = Join-Path $InstallRoot "worker-dependencies-stamp.json"
$workerFingerprint = [ordered]@{
    python = ((Invoke-NativeCapture "读取 Worker Python 版本" $python @("--version")) -join "").Trim()
    requirements = @($requirementFiles | ForEach-Object { [ordered]@{ path = [IO.Path]::GetFullPath($_); sha256 = Get-Sha256 $_ } })
}
$workerDependenciesCurrent = $false
if (-not $ForceBuild -and (Test-Path -LiteralPath $workerStampPath)) {
    try {
        $existingWorkerStamp = Get-Content -LiteralPath $workerStampPath -Raw | ConvertFrom-Json
        $workerDependenciesCurrent = (($existingWorkerStamp | ConvertTo-Json -Depth 5 -Compress) -eq (($workerFingerprint | ConvertTo-Json -Depth 5 -Compress)))
    } catch { $workerDependenciesCurrent = $false }
}
if ($workerDependenciesCurrent) {
    Write-Host "检测到有效 Worker 依赖 stamp，跳过重复 pip install。" -ForegroundColor Green
} else {
    foreach ($file in $requirementFiles) {
        Invoke-NativeCommand "Python Worker 依赖安装 $file" $python @("-m", "pip", "install", "--disable-pip-version-check", "--progress-bar", "off", "-r", $file)
    }
    Set-PrivateFile $workerStampPath (($workerFingerprint | ConvertTo-Json -Depth 5) + "`n")
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
    $installBrowser = if ($NonInteractive) { $false } else { Ask-YesNo "未找到 Edge/Chrome，是否尝试通过 winget 安装 Edge" $true }
    if ($installBrowser) {
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
    $secret = $null
} else {
    $existingSecrets = Get-Content -LiteralPath $secretPath -Raw
    if ($existingSecrets -notmatch '(?m)^ASTERISM_SECRET_ACTIVE_KEY_ID=.+$' -or $existingSecrets -notmatch '(?m)^ASTERISM_SECRET_KEYS=.+$') {
        throw "现有 secrets.env 缺少必要键；为避免密钥丢失，安装器不会自动重建，请先修复或从备份恢复。"
    }
    Set-PrivateFile $secretPath $existingSecrets
    $existingSecrets = $null
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
if ((Test-Path $configPath) -and ((Get-Content -LiteralPath $configPath -Raw) -ne $config)) { Copy-Item $configPath "$configPath.before-install-$(Get-Date -Format yyyyMMdd-HHmmss-fffffff).bak" }
Set-PrivateFile $configPath $config

if (-not $SkipBuild -and ($ForceBuild -or -not $buildCache.Valid)) {
    Write-Step "构建 Asterism 和 WebUI"
    $msvcToolchain = Ensure-MsvcToolchain
    Import-VsDevEnvironment $msvcToolchain
    Push-Location $SourceRoot
    try {
        Invoke-NativeCommand "Rust release workspace 构建" "cargo" @("build", "--locked", "--release", "--workspace")
        Push-Location web
        try {
            Invoke-NativeCommand "WebUI npm ci" "npm" @("ci")
            Invoke-NativeCommand "WebUI typecheck" "npm" @("run", "typecheck")
            Invoke-NativeCommand "WebUI build" "npm" @("run", "build")
        } finally { Pop-Location }
    } finally { Pop-Location }
    $artifactCheck = Test-BuildArtifacts $SourceRoot
    if (-not $artifactCheck.Valid) { throw "构建结束但产物不完整：$($artifactCheck.Missing -join ', ')" }
    Write-BuildStamp $SourceRoot $buildStampPath
}

Write-Step "生成启动脚本"
$runScript = Join-Path $InstallRoot "run-asterism.ps1"
$daemon = Join-Path $SourceRoot "target\release\asterismd.exe"
$runContent = @"
`$ErrorActionPreference = "Stop"
foreach (`$line in Get-Content -LiteralPath "$secretPath") {
    if (`$line -match '^ASTERISM_[A-Z0-9_]+=') {
        `$parts = `$line -split '=', 2
        [Environment]::SetEnvironmentVariable(`$parts[0], `$parts[1], "Process")
    }
}
`$env:ASTERISM_UAI_WORKER_PYTHON = "$python"
$(if ($browser) { "`$env:ASTERISM_CHAOXING_BROWSER_EXECUTABLE = `"$browser`"`n`$env:ASTERISM_UAI_BROWSER_EXECUTABLE = `"$browser`"" } else { "" })
& "$daemon" --config "$configPath" --web-dist "$(Join-Path $SourceRoot 'web\dist')" --uai-worker-python "$python" *>> "$(Join-Path $InstallRoot 'logs\asterismd.log')"
`$exitCode = `$LASTEXITCODE
if (`$exitCode -ne 0) { throw "asterismd 退出（exit `$exitCode）" }
"@
Set-PrivateFile $runScript $runContent -WithBom

$pluginTarget = $null
if ($YunzaiRoot -and (Test-Path $YunzaiRoot)) {
    Write-Step "安装 Yunzai 插件"
    $pluginTarget = Join-Path $YunzaiRoot "plugins\asterism-plugin"
    New-Item -ItemType Directory -Force (Split-Path -Parent $pluginTarget) | Out-Null
    if (Test-Path $pluginTarget) { Copy-Item $pluginTarget "$pluginTarget.before-install-$(Get-Date -Format yyyyMMdd-HHmmss-fffffff)" -Recurse }
    New-Item -ItemType Directory -Force $pluginTarget | Out-Null
    Get-ChildItem -LiteralPath (Join-Path $SourceRoot "integrations\yunzai-plugin") -Force | ForEach-Object {
        Copy-Item -LiteralPath $_.FullName -Destination (Join-Path $pluginTarget $_.Name) -Recurse -Force
    }
    $AllowedGroups = if ($AllowedGroups) { $AllowedGroups } else { Ask "允许使用 Asterism 的群号（逗号分隔，留空表示全部）" "" }
    $NotificationGroups = if ($NotificationGroups) { $NotificationGroups } else { Ask "通知群号（逗号分隔，留空表示不投递）" "" }
    $AdminContact = if ($AdminContact) { $AdminContact } else { Ask "管理员联系方式" "" }
}

Write-Step "运行数据库迁移和初始账号向导"
if (-not (Test-Path (Join-Path $SourceRoot "target\release\asterismctl.exe"))) { throw "未找到 asterismctl.exe。请不要使用 -SkipBuild，或先完成构建。" }
$bootstrapOut = Join-Path $InstallRoot "logs\bootstrap.stdout.log"
$bootstrapErr = Join-Path $InstallRoot "logs\bootstrap.stderr.log"
$port = Get-BindPort $Bind
$apiUrl = Get-LocalApiUrl $Bind
$processBefore = @(Get-AsterismProcessInfo)
$script:daemonPidsBefore = @($processBefore.Pid)
$script:installerDaemonPath = (Resolve-Path $daemon).Path
$script:installerConfigPath = $configPath
$ownedExisting = @($processBefore | Where-Object { $_.CommandLine -and $_.CommandLine -like "*--config*$configPath*" -and $_.ExecutablePath -and ((Resolve-Path $_.ExecutablePath -ErrorAction SilentlyContinue).Path -eq (Resolve-Path $daemon).Path) })
$listeningPids = @(Get-ListeningProcessIds $port)
$ownedListener = @($ownedExisting | Where-Object { $listeningPids -contains $_.Pid })
if ($listeningPids.Count -gt 0 -and $ownedListener.Count -eq 0) {
    throw "端口 $Bind 已被占用，且无法确认是当前安装的 asterismd（仅依赖 health 不足以安全归属）。请停止占用端口的实例或改用其他 -Bind。"
}
$daemonProcess = $null
if ($ownedListener.Count -gt 0) {
    Write-Host "检测到当前安装配置已经有 asterismd 运行实例，复用该实例，不启动第二个 daemon。" -ForegroundColor Yellow
} else {
    $runScriptArgument = '"' + $runScript + '"'
    $daemonProcess = Start-Process -FilePath "powershell.exe" -ArgumentList @("-NoProfile", "-ExecutionPolicy", "Bypass", "-File", $runScriptArgument) -WorkingDirectory $SourceRoot -WindowStyle Hidden -RedirectStandardOutput $bootstrapOut -RedirectStandardError $bootstrapErr -PassThru
    $script:installerDaemonProcess = $daemonProcess
    $script:installerDaemonOwned = $true
    Start-Sleep -Milliseconds 500
    $processAfter = @(Get-AsterismProcessInfo)
    $script:installerDaemonChildPids = @($processAfter | Where-Object { $processBefore.Pid -notcontains $_.Pid -and $_.ExecutablePath -and ((Resolve-Path $_.ExecutablePath -ErrorAction SilentlyContinue).Path -eq (Resolve-Path $daemon).Path) } | Select-Object -ExpandProperty Pid)
}
for ($attempt = 0; $attempt -lt 120; $attempt++) {
    try {
        $health = Invoke-RestMethod -Uri ($apiUrl + "/api/v1/system/health") -TimeoutSec 2
        if ($health.status -eq "ok") { break }
    } catch { Start-Sleep -Seconds 1 }
    if ($attempt -eq 119) {
        $daemonLog = Join-Path $InstallRoot "logs\asterismd.log"
        $diagnosticFiles = @($daemonLog, $bootstrapOut, $bootstrapErr) | Where-Object { Test-Path $_ }
        $diagnostic = if ($diagnosticFiles) { ($diagnosticFiles | ForEach-Object { "--- $_ ---"; Get-Content -LiteralPath $_ -Tail 40 -ErrorAction SilentlyContinue }) -join "`n" } else { "（尚未生成 daemon/启动日志）" }
        $pidText = if ($daemonProcess) { [string]$daemonProcess.Id } else { ($ownedListener.Pid -join ',') }
        $state = if ($daemonProcess) { [string]((Get-Process -Id $daemonProcess.Id -ErrorAction SilentlyContinue).HasExited) } else { "existing" }
        throw "asterismd 未能在 120 秒内通过健康检查（PID $pidText，进程状态 $state）。`n$diagnostic"
    }
}
$env:ASTERISM_CONFIG = $configPath
$cli = Join-Path $SourceRoot "target\release\asterismctl.exe"
$currentProcesses = @(Get-AsterismProcessInfo)
$expectedListenerPids = @($ownedListener.Pid) + @($currentProcesses | Where-Object {
    $processBefore.Pid -notcontains $_.Pid -and $_.ExecutablePath -and
    ((Resolve-Path $_.ExecutablePath -ErrorAction SilentlyContinue).Path -eq (Resolve-Path $daemon).Path) -and
    (($_.ParentPid -eq $daemonProcess.Id) -or ($_.CommandLine -and $_.CommandLine -like "*--config*$configPath*"))
} | Select-Object -ExpandProperty Pid)
$actualListenerPids = @(Get-ListeningProcessIds $port)
if (@($actualListenerPids | Where-Object { $expectedListenerPids -contains $_ }).Count -eq 0) {
    throw "健康检查端口 $Bind 的监听进程不属于本次安装或已确认的当前实例；拒绝继续 Master/token 初始化。"
}
$health = Invoke-RestMethod -Uri ($apiUrl + "/api/v1/system/health") -TimeoutSec 5
if ($null -eq $health.master_initialized) {
    throw "健康端点不是当前 Asterism 版本，或监听地址已被其他实例占用；请更换 -Bind 或先停止占用端口的服务。"
}
$pluginConfigPath = if ($pluginTarget) { Join-Path $pluginTarget "config\asterism.json" } else { $null }
$needsGatewayToken = $pluginTarget -and ((-not $health.master_initialized) -or -not (Test-Path $pluginConfigPath))
$needsAdminAuthentication = (-not $health.master_initialized) -or $needsGatewayToken
$adminToken = $null
$adminTokenId = $null
$adminTokenRevoked = $false
$createdInitialMaster = $false
if ($needsAdminAuthentication) {
    if ($NonInteractive -and -not $MasterPasswordFile) {
        throw "无人值守安装需要通过 -MasterPasswordFile（或配置文件的 masterPasswordFile）提供 Master 密码；安装器不接受命令行明文密码。"
    }
    $authCommand = if ($health.master_initialized) { @("auth", "login") } else { @("init") }
    $bootstrapScopes = "system-read,provider-read,provider-manage,task-read,task-execute,credit-read,credit-manage,audit-read,service-token-manage,qq-identity-assert,task-command-proxy,notification-delivery-report"
    $authArgs = @("--url", $apiUrl) + @($authCommand) + @("--username", $MasterUsername, "--scope", $bootstrapScopes)
    $createdInitialMaster = -not $health.master_initialized
    if ($MasterPasswordFile) {
        $passwordLine = (Get-Content -LiteralPath $MasterPasswordFile -Raw).TrimEnd("`r", "`n")
        if ([string]::IsNullOrWhiteSpace($passwordLine)) { throw "Master 密码文件为空。" }
        try {
            $adminOutput = $passwordLine | & $cli @authArgs --password-stdin
        } finally {
            $passwordLine = $null
        }
    } else {
        Write-Host $(if ($createdInitialMaster) { "请为首次 Master 输入并确认密码：" } else { "请为现有 Master 输入密码，以创建缺失的 Yunzai 网关令牌：" })
        $adminOutput = & $cli @authArgs
    }
    if ($LASTEXITCODE -ne 0) { throw "Master 初始化或认证失败。" }
    $adminResult = ($adminOutput -join "`n") | ConvertFrom-Json
    $adminToken = $adminResult.token
    $adminTokenId = $adminResult.metadata.id
    if (-not $adminToken) { throw "asterismctl 未返回管理 Service Token。" }
    if ($createdInitialMaster -and -not $NonInteractive) {
        Write-Warning "以下初始管理 Service Token 只显示一次，请保存到密码管理器："
        $adminOutput | Write-Output
    }
}

if ($needsGatewayToken) {
    Write-Step "创建并写入 Yunzai 网关令牌"
    $env:ASTERISM_TOKEN = $adminToken
    $gatewayTokenId = $null
    $gatewayConfigSaved = $false
    try {
        $gatewayOutput = & $cli --url $apiUrl service-token create `
            --name yunzai-gateway `
            --scope provider-read,provider-manage,task-read,task-execute,qq-identity-assert,task-command-proxy,notification-delivery-report `
            --expires-in-seconds $GatewayTokenTtlSeconds
        if ($LASTEXITCODE -ne 0) { throw "Yunzai 网关 Service Token 创建失败。" }
        $gatewayResult = ($gatewayOutput -join "`n") | ConvertFrom-Json
        $gatewayToken = $gatewayResult.token
        $gatewayTokenId = $gatewayResult.metadata.id
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
        $gatewayConfigSaved = $true
        if (-not $createdInitialMaster -and $adminTokenId) {
            Invoke-NativeCommand "撤销临时管理 Service Token" $cli @("--url", $apiUrl, "service-token", "revoke", $adminTokenId)
            $adminTokenRevoked = $true
        } elseif ($createdInitialMaster -and $NonInteractive -and $adminTokenId) {
            Invoke-NativeCommand "撤销无人值守初始化管理 Service Token" $cli @("--url", $apiUrl, "service-token", "revoke", $adminTokenId)
            $adminTokenRevoked = $true
        }
    } catch {
        $originalError = $_
        if (-not $gatewayConfigSaved -and $gatewayTokenId) {
            try { Invoke-NativeCommand "回滚未落盘的 Yunzai 网关令牌" $cli @("--url", $apiUrl, "service-token", "revoke", $gatewayTokenId) } catch { Write-Warning "Yunzai 网关令牌回滚失败，请在管理后台撤销 token id $gatewayTokenId。" }
        }
        if (-not $createdInitialMaster -and $adminTokenId) {
            try { Invoke-NativeCommand "回滚临时管理 Service Token" $cli @("--url", $apiUrl, "service-token", "revoke", $adminTokenId) } catch { Write-Warning "临时管理令牌回滚失败，请在管理后台撤销 token id $adminTokenId。" }
        } elseif ($createdInitialMaster -and $NonInteractive -and $adminTokenId) {
            try { Invoke-NativeCommand "回滚无人值守初始化管理 Service Token" $cli @("--url", $apiUrl, "service-token", "revoke", $adminTokenId) } catch { Write-Warning "无人值守初始化管理令牌回滚失败，请在管理后台撤销 token id $adminTokenId。" }
        }
        throw $originalError
    } finally {
        Remove-Item Env:ASTERISM_TOKEN -ErrorAction SilentlyContinue
        $adminToken = $null
        $gatewayToken = $null
    }
}

if ($createdInitialMaster -and $NonInteractive -and -not $adminTokenRevoked -and $adminTokenId) {
    $env:ASTERISM_TOKEN = $adminToken
    try {
        Invoke-NativeCommand "撤销无人值守初始化管理 Service Token" $cli @("--url", $apiUrl, "service-token", "revoke", $adminTokenId)
        $adminTokenRevoked = $true
    } finally {
        Remove-Item Env:ASTERISM_TOKEN -ErrorAction SilentlyContinue
        $adminToken = $null
    }
}

if ($RegisterTask) {
    Write-Step "注册 Windows 任务"
    $taskName = "Asterism"
    Invoke-NativeCommand "注册 Windows 任务" "schtasks.exe" @("/Create", "/TN", $taskName, "/SC", "ONSTART", "/RU", "SYSTEM", "/TR", "powershell.exe -NoProfile -ExecutionPolicy Bypass -File `"$runScript`"", "/F")
}

Write-Host "`n安装完成。" -ForegroundColor Green
Write-Host "本地 WebUI/API：$WebUrl"
Write-Host "健康检查：$apiUrl/api/v1/system/health"
Write-Host "反向代理、公网 HTTPS 和域名映射未由安装器处理。"
$script:installCommitted = $true
