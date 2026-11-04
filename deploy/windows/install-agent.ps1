#Requires -RunAsAdministrator
[CmdletBinding(SupportsShouldProcess)]
param(
    [Parameter(Mandatory)] [string] $AgentExe,
    [Parameter(Mandatory)] [string] $CtlExe,
    [string] $TrayExe,
    [Parameter(Mandatory)] [string] $Manager,
    [Parameter(Mandatory)] [string] $Name,
    [ValidateSet('development', 'lazycat')] [string] $Auth = 'development',
    [string] $DataDir = "$env:ProgramData\CoFleet",
    [string] $InstallDir = "$env:ProgramFiles\CoFleet",
    [switch] $Docker,
    [switch] $DockerObserve,
    [string] $WorkspaceImage,
    [switch] $BuildDeploy
)

$ErrorActionPreference = 'Stop'
$agentSource = (Resolve-Path -LiteralPath $AgentExe).Path
$ctlSource = (Resolve-Path -LiteralPath $CtlExe).Path
$traySource = if ($TrayExe) { (Resolve-Path -LiteralPath $TrayExe).Path } else { $null }
if ($BuildDeploy -and -not $WorkspaceImage) { throw '-BuildDeploy requires -WorkspaceImage' }
if ($WorkspaceImage -and -not $Docker) { throw '-WorkspaceImage requires -Docker' }

New-Item -ItemType Directory -Force -Path $DataDir, $InstallDir | Out-Null
$agent = Join-Path $InstallDir 'cofleet-agent.exe'
$ctl = Join-Path $InstallDir 'cofleetctl.exe'
if ($agentSource -ne $agent) { Copy-Item -Force -LiteralPath $agentSource -Destination $agent }
if ($ctlSource -ne $ctl) { Copy-Item -Force -LiteralPath $ctlSource -Destination $ctl }
if ($traySource) {
    $tray = Join-Path $InstallDir 'cofleet-tray.exe'
    if ($traySource -ne $tray) { Copy-Item -Force -LiteralPath $traySource -Destination $tray }
}
$config = Join-Path $DataDir 'agent.toml'
if (-not (Test-Path -LiteralPath $config)) {
    if ($Auth -eq 'development' -and -not $env:COFLEET_TOKEN) {
        throw 'Set COFLEET_TOKEN in this elevated PowerShell before installing a development-mode Agent.'
    }
    & $ctl --data-dir $DataDir init --manager $Manager --name $Name --auth $Auth
    if ($LASTEXITCODE -ne 0) { throw 'Agent initialization failed' }
}

$arguments = @('--service', '--data-dir', ('"{0}"' -f $DataDir))
if ($Docker) { $arguments += '--docker' }
if ($DockerObserve) { $arguments += '--docker-observe' }
if ($WorkspaceImage) { $arguments += @('--workspace-image', $WorkspaceImage) }
if ($BuildDeploy) { $arguments += '--build-deploy' }
$binaryPath = '"{0}" {1}' -f $agent, ($arguments -join ' ')

if (Get-Service -Name CoFleetAgent -ErrorAction SilentlyContinue) {
    throw 'CoFleetAgent is already installed. Uninstall it before changing the service command.'
}
if ($PSCmdlet.ShouldProcess('CoFleetAgent', 'Install and start Windows service')) {
    New-Service -Name CoFleetAgent -DisplayName 'CoFleet Agent' -Description 'CoFleet execution node agent' -BinaryPathName $binaryPath -StartupType Automatic | Out-Null
    & sc.exe failure CoFleetAgent reset= 86400 actions= restart/5000/restart/15000/restart/60000 | Out-Null
    Start-Service -Name CoFleetAgent
    Get-Service -Name CoFleetAgent
}
