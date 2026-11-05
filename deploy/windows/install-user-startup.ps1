[CmdletBinding(SupportsShouldProcess)]
param(
    [Parameter(Mandatory)] [string] $AgentExe,
    [Parameter(Mandatory)] [string] $TrayExe,
    [string] $DataDir = "$env:LOCALAPPDATA\cofleet\cofleet\data",
    [string] $InstallDir = "$env:LOCALAPPDATA\CoFleet\bin",
    [switch] $Docker,
    [switch] $DockerObserve,
    [string] $WorkspaceImage,
    [switch] $BuildDeploy
)

$ErrorActionPreference = 'Stop'
if ($BuildDeploy -and -not $WorkspaceImage) { throw '-BuildDeploy requires -WorkspaceImage' }
if ($WorkspaceImage -and -not $Docker) { throw '-WorkspaceImage requires -Docker' }
$agentSource = (Resolve-Path -LiteralPath $AgentExe).Path
$traySource = (Resolve-Path -LiteralPath $TrayExe).Path
New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
$agent = Join-Path $InstallDir 'cofleet-agent.exe'
$tray = Join-Path $InstallDir 'cofleet-tray.exe'
if ($agentSource -ne $agent) { Copy-Item -Force -LiteralPath $agentSource -Destination $agent }
if ($traySource -ne $tray) { Copy-Item -Force -LiteralPath $traySource -Destination $tray }
$agentArguments = @('--data-dir', ('"{0}"' -f $DataDir))
if ($Docker) { $agentArguments += '--docker' }
if ($DockerObserve) { $agentArguments += '--docker-observe' }
if ($WorkspaceImage) { $agentArguments += @('--workspace-image', $WorkspaceImage) }
if ($BuildDeploy) { $agentArguments += '--build-deploy' }
$startup = [Environment]::GetFolderPath('Startup')
$shell = New-Object -ComObject WScript.Shell

foreach ($item in @(
    @{ Name = 'CoFleet Agent.lnk'; Target = $agent; Arguments = ($agentArguments -join ' ') },
    @{ Name = 'CoFleet Tray.lnk'; Target = $tray; Arguments = ('--data-dir "{0}"' -f $DataDir) }
)) {
    $path = Join-Path $startup $item.Name
    if ($PSCmdlet.ShouldProcess($path, 'Create current-user startup shortcut')) {
        $shortcut = $shell.CreateShortcut($path)
        $shortcut.TargetPath = $item.Target
        $shortcut.Arguments = $item.Arguments
        $shortcut.WorkingDirectory = Split-Path -Parent $item.Target
        $shortcut.Save()
        Write-Output $path
    }
}
