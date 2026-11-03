[CmdletBinding(SupportsShouldProcess)]
param()

$startup = [Environment]::GetFolderPath('Startup')
foreach ($name in @('CoFleet Agent.lnk', 'CoFleet Tray.lnk')) {
    $path = Join-Path $startup $name
    if ((Test-Path -LiteralPath $path) -and $PSCmdlet.ShouldProcess($path, 'Remove current-user startup shortcut')) {
        Remove-Item -LiteralPath $path -Force
        Write-Output $path
    }
}
