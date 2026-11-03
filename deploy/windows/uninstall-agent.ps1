#Requires -RunAsAdministrator
[CmdletBinding(SupportsShouldProcess)]
param()

$service = Get-Service -Name CoFleetAgent -ErrorAction SilentlyContinue
if (-not $service) { Write-Output 'CoFleetAgent is not installed.'; return }
if ($PSCmdlet.ShouldProcess('CoFleetAgent', 'Stop and remove Windows service')) {
    if ($service.Status -ne 'Stopped') {
        Stop-Service -Name CoFleetAgent -Force
        $service.WaitForStatus('Stopped', [TimeSpan]::FromSeconds(30))
    }
    & sc.exe delete CoFleetAgent | Out-Null
    Write-Output 'CoFleetAgent service removed. Agent data was retained.'
}
