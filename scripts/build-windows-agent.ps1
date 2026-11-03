[CmdletBinding()]
param([string] $Output)

$ErrorActionPreference = 'Stop'
$root = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
if (-not $Output) { $Output = Join-Path $root 'dist\windows-agent' }
Push-Location $root
try {
    cargo build --release --locked --bin cofleet-agent --bin cofleetctl
    if ($LASTEXITCODE -ne 0) { throw 'Rust release build failed' }
    cargo build --release --locked --features tray --bin cofleet-tray
    if ($LASTEXITCODE -ne 0) { throw 'Windows tray release build failed' }
    New-Item -ItemType Directory -Force -Path $Output | Out-Null
    foreach ($name in @('cofleet-agent.exe', 'cofleetctl.exe', 'cofleet-tray.exe')) {
        $source = Join-Path $root "target\release\$name"
        $destination = Join-Path $Output $name
        $same = (Test-Path -LiteralPath $destination) -and ((Get-FileHash $source).Hash -eq (Get-FileHash $destination).Hash)
        if (-not $same) { Copy-Item -Force -LiteralPath $source -Destination $destination }
    }
    Copy-Item -Force deploy\windows\install-agent.ps1, deploy\windows\uninstall-agent.ps1, deploy\windows\install-user-startup.ps1, deploy\windows\uninstall-user-startup.ps1 -Destination $Output
    Write-Output (Resolve-Path $Output).Path
} finally {
    Pop-Location
}
