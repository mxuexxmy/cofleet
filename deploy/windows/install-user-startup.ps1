[CmdletBinding(SupportsShouldProcess)]
param(
    [Parameter(Mandatory)] [string] $AgentExe,
    [Parameter(Mandatory)] [string] $TrayExe,
    [string] $DataDir = "$env:LOCALAPPDATA\cofleet\cofleet\data",
    [string] $InstallDir = "$env:LOCALAPPDATA\CoFleet\bin"
)

$ErrorActionPreference = 'Stop'
$agentSource = (Resolve-Path -LiteralPath $AgentExe).Path
$traySource = (Resolve-Path -LiteralPath $TrayExe).Path
New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
$agent = Join-Path $InstallDir 'cofleet-agent.exe'
$tray = Join-Path $InstallDir 'cofleet-tray.exe'
if ($agentSource -ne $agent) { Copy-Item -Force -LiteralPath $agentSource -Destination $agent }
if ($traySource -ne $tray) { Copy-Item -Force -LiteralPath $traySource -Destination $tray }
$startup = [Environment]::GetFolderPath('Startup')
$shell = New-Object -ComObject WScript.Shell

foreach ($item in @(
    @{ Name = 'CoFleet Agent.lnk'; Target = $agent },
    @{ Name = 'CoFleet Tray.lnk'; Target = $tray }
)) {
    $path = Join-Path $startup $item.Name
    if ($PSCmdlet.ShouldProcess($path, 'Create current-user startup shortcut')) {
        $shortcut = $shell.CreateShortcut($path)
        $shortcut.TargetPath = $item.Target
        $shortcut.Arguments = '--data-dir "{0}"' -f $DataDir
        $shortcut.WorkingDirectory = Split-Path -Parent $item.Target
        $shortcut.Save()
        Write-Output $path
    }
}
