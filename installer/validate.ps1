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

Check "install root" (Test-Path $InstallRoot) $InstallRoot
Check "asterism config" (Test-Path (Join-Path $InstallRoot "asterism.toml")) "asterism.toml"
Check "secret file" (Test-Path (Join-Path $InstallRoot "secrets.env")) "secrets.env"
Check "worker venv" (Test-Path (Join-Path $InstallRoot ".venv-workers\Scripts\python.exe")) ".venv-workers"
Check "daemon binary" (Test-Path (Join-Path $SourceRoot "target\release\asterismd.exe")) "asterismd.exe"
Check "CLI binary" (Test-Path (Join-Path $SourceRoot "target\release\asterismctl.exe")) "asterismctl.exe"
Check "WebUI" (Test-Path (Join-Path $SourceRoot "web\dist\index.html")) "web/dist/index.html"
Check "Yunzai plugin" (Test-Path (Join-Path $SourceRoot "integrations\yunzai-plugin\apps\asterism.js")) "plugin source"

try {
    $health = Invoke-RestMethod -Uri ("http://" + $Bind + "/api/v1/system/health") -TimeoutSec 5
    Check "API health" ($health.status -eq "ok") ("status=" + $health.status)
} catch {
    Check "API health" $false $_.Exception.Message
}

$checks | Format-Table -AutoSize
if ($checks.Where({ -not $_.Passed }).Count -gt 0) {
    throw "安装验收失败：$($checks.Where({ -not $_.Passed }).Name -join ', ')"
}
Write-Host "Asterism 本机安装验收通过。真实 Provider/Yunzai 行为仍需现场验证。" -ForegroundColor Green
