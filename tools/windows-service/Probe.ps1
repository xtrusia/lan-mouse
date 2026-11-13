param([Parameter(Mandatory=$true)][string]$SourcePath, [Parameter(Mandatory=$true)][string]$ExpectedSha256)
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
$binary = 'C:\Program Files\Lan Mouse UAC\lan-mouse.exe'
$backup = 'C:\Program Files\Lan Mouse UAC\lan-mouse.before-probe.exe'
if ((Get-Service lan-mouse).Status -ne 'Stopped') { throw 'Stop candidate service first.' }
if (Test-Path $backup) { throw 'Probe backup already exists.' }
if ((Get-FileHash $SourcePath -Algorithm SHA256).Hash -ne $ExpectedSha256) { throw 'Probe hash mismatch.' }
$originalHash = (Get-FileHash $binary -Algorithm SHA256).Hash
Copy-Item $binary $backup
& icacls.exe $backup /setowner '*S-1-5-32-544' | Out-Null
if ($LASTEXITCODE) { throw 'Cannot protect backup owner.' }
try {
    Copy-Item $SourcePath $binary -Force
    Start-Service lan-mouse
    Start-Sleep 4
    Get-Content 'C:\ProgramData\lan-mouse\desktop-probe.log'
} finally {
    Stop-Service lan-mouse
    Copy-Item $backup $binary -Force
    if ((Get-FileHash $binary -Algorithm SHA256).Hash -ne $originalHash) { throw 'Restore hash mismatch.' }
}
