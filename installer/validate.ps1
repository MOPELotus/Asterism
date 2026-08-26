[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$InstallRoot,
    [string]$SourceRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path,
    [string]$Bind = "127.0.0.1:8068"
)

$ErrorActionPreference = "Stop"
$checks = [System.Collections.Generic.List[object]]::new()
function Check([string]$Name, [bool]$Passed, [string]$Detail) {
    $checks.Add([pscustomobject]@{ Name = $Name; Passed = $Passed; Detail = $Detail })
}
function Get-LocalApiUrl([string]$Value) {
    if ($Value -match '^0\.0\.0\.0:(\d+)$') { return "http://127.0.0.1:$($Matches[1])" }
    if ($Value -match '^\[?::\]?:(\d+)$') { return "http://[::1]:$($Matches[1])" }
    return "http://$Value"
}

Check "install root" (Test-Path $InstallRoot) $InstallRoot
Check "asterism config" (Test-Path (Join-Path $InstallRoot "asterism.toml")) "asterism.toml"
Check "secret file" (Test-Path (Join-Path $InstallRoot "secrets.env")) "secrets.env"
Check "worker venv" (Test-Path (Join-Path $InstallRoot ".venv-workers\Scripts\python.exe")) ".venv-workers"
Check "daemon binary" (Test-Path (Join-Path $SourceRoot "target\release\asterismd.exe")) "asterismd.exe"
Check "CLI binary" (Test-Path (Join-Path $SourceRoot "target\release\asterismctl.exe")) "asterismctl.exe"
Check "WebUI" (Test-Path (Join-Path $SourceRoot "web\dist\index.html")) "web/dist/index.html"
Check "Yunzai plugin" (Test-Path (Join-Path $SourceRoot "integrations\yunzai-plugin\apps\asterism.js")) "plugin source"
Check "installer script" (Test-Path (Join-Path $SourceRoot "installer\install.ps1")) "installer/install.ps1"
Check "installer README" (Test-Path (Join-Path $SourceRoot "installer\README.md")) "installer/README.md"

$upstreamFiles = @(
    "upstreams\chaoxing\api\base.py",
    "upstreams\chaoxing-exam\cxapi\api.py",
    "upstreams\welearn\welearn_decompiled.py",
    "upstreams\uai\配置我运行我.py",
    "upstreams\uai-browser\unipus_ai_auto_player.user.js",
    "upstreams\cidaren\api\login.py"
)
foreach ($relative in $upstreamFiles) {
    Check "upstream Worker: $relative" (Test-Path -LiteralPath (Join-Path $SourceRoot $relative)) $relative
}
$runScriptPath = Join-Path $InstallRoot "run-asterism.ps1"
if (Test-Path -LiteralPath $runScriptPath) {
    $runScriptText = Get-Content -LiteralPath $runScriptPath -Raw
    Check "runtime working directory" ($runScriptText -match [regex]::Escape("Set-Location -LiteralPath `"$SourceRoot`"")) "run script pins SourceRoot"
    Check "runtime absolute upstream paths" ($runScriptText -match 'ASTERISM_CHAOXING_WORKER_UPSTREAM' -and $runScriptText -match 'ASTERISM_UAI_WORKER_UPSTREAM') "run script pins Worker paths"
} else {
    Check "runtime launch script" $false "run-asterism.ps1"
}

$secretPath = Join-Path $InstallRoot "secrets.env"
if (Test-Path -LiteralPath $secretPath) {
    $secretText = Get-Content -LiteralPath $secretPath -Raw
    Check "secret values" ($secretText -match '(?m)^ASTERISM_SECRET_ACTIVE_KEY_ID=.+$' -and $secretText -match '(?m)^ASTERISM_SECRET_KEYS=.+$') "required keys present (values hidden)"
    try {
        $secretAcl = Get-Acl -LiteralPath $secretPath
        Check "secret ACL inheritance" $secretAcl.AreAccessRulesProtected "inheritance disabled"
        $allowedSids = @(
            [System.Security.Principal.WindowsIdentity]::GetCurrent().User.Value,
            "S-1-5-18",
            "S-1-5-32-544"
        )
        $unexpectedAccess = @($secretAcl.Access | Where-Object {
            try { $sid = $_.IdentityReference.Translate([System.Security.Principal.SecurityIdentifier]).Value } catch { $sid = [string]$_.IdentityReference }
            $_.AccessControlType -eq [System.Security.AccessControl.AccessControlType]::Allow -and $allowedSids -notcontains $sid
        })
        Check "secret ACL principals" ($unexpectedAccess.Count -eq 0) $(if ($unexpectedAccess.Count -eq 0) { "current user/SYSTEM/Administrators only" } else { "unexpected explicit access rule" })
    } catch {
        Check "secret ACL" $false $_.Exception.Message
    }
}

foreach ($file in @(Get-ChildItem -LiteralPath $SourceRoot -Filter "*.ps1" -File -Recurse -ErrorAction SilentlyContinue | Where-Object { $_.FullName -notmatch "[\\/]node_modules[\\/]" -and $_.FullName -notmatch "[\\/]target[\\/]" })) {
    $tokens = $null
    $parseErrors = $null
    [System.Management.Automation.Language.Parser]::ParseFile($file.FullName, [ref]$tokens, [ref]$parseErrors) | Out-Null
    Check "PowerShell parser: $($file.FullName.Substring($SourceRoot.Length).TrimStart('\\','/'))" ($parseErrors.Count -eq 0) $(if ($parseErrors.Count -eq 0) { "parse ok" } else { ($parseErrors | Out-String).Trim() })
}

$stampPath = Join-Path $InstallRoot "build-stamp.json"
if (Test-Path -LiteralPath $stampPath) {
    try {
        $stamp = Get-Content -LiteralPath $stampPath -Raw | ConvertFrom-Json
        Check "build stamp" ($stamp.profile -eq "release" -and $stamp.schema -eq 1) "release stamp"
    } catch {
        Check "build stamp" $false $_.Exception.Message
    }
} else {
    Write-Warning "未找到 build-stamp.json。安装器的 -SkipBuild 可接受已有产物，但生产安装建议让安装器完成一次带 stamp 的 release 构建。"
}

try {
    $health = Invoke-RestMethod -Uri ((Get-LocalApiUrl $Bind) + "/api/v1/system/health") -TimeoutSec 5
    Check "API health" ($health.status -eq "ok") ("status=" + $health.status)
    Check "master initialized" ($health.master_initialized -eq $true) ("master_initialized=" + $health.master_initialized)
    Check "secret store" ($health.secret_store_configured -eq $true) ("secret_store_configured=" + $health.secret_store_configured)
} catch {
    Check "API health" $false $_.Exception.Message
}

$checks | Format-Table -AutoSize
if ($checks.Where({ -not $_.Passed }).Count -gt 0) {
    throw "安装验收失败：$($checks.Where({ -not $_.Passed }).Name -join ', ')"
}
Write-Host "Asterism 本机安装验收通过。真实 Provider/Yunzai 行为仍需现场验证。" -ForegroundColor Green
