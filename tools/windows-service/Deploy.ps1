param(
    [ValidateSet('Install', 'Update', 'Rollback')][string]$Action = 'Install',
    [string]$SourcePath,
    [string]$ExpectedSha256,
    [string]$PeerAddress = '192.168.0.6'
)
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
$serviceName = 'lan-mouse'
$installDir = 'C:\Program Files\Lan Mouse UAC'
$dataDir = 'C:\ProgramData\lan-mouse'
$binary = Join-Path $installDir 'lan-mouse.exe'
$taskName = 'LanMouse-Interactive'
$ruleName = 'LanMouse-UAC-Mac-UDP4242'
$oldBinary = Join-Path $env:LOCALAPPDATA 'Microsoft\WinGet\Packages\feschber.LanMouse_Microsoft.Winget.Source_8wekyb3d8bbwe\lan-mouse.exe'

function Restore-Interactive {
    $service = Get-Service $serviceName -ErrorAction SilentlyContinue
    if ($service) {
        Stop-Service $serviceName -ErrorAction Stop
        Set-Service $serviceName -StartupType Manual
    }
    Start-ScheduledTask -TaskName $taskName
    Write-Output 'Restored LanMouse-Interactive; clipboard task unchanged.'
}

if ($Action -eq 'Rollback') {
    Restore-Interactive
    return
}
if (-not $SourcePath -or $ExpectedSha256 -notmatch '^[a-fA-F0-9]{64}$') {
    throw 'SourcePath and ExpectedSha256 are required.'
}
if ((Get-FileHash -LiteralPath $SourcePath -Algorithm SHA256).Hash -ne $ExpectedSha256) {
    throw 'Candidate hash mismatch.'
}
if ($Action -eq 'Install') {
    if (Get-Service $serviceName -ErrorAction SilentlyContinue) {
        throw 'Service already exists; refusing to replace an unknown installation.'
    }
    Get-ScheduledTask -TaskName $taskName | Out-Null
    foreach ($dir in @($installDir, $dataDir)) {
        if (Test-Path -LiteralPath $dir) { throw "Destination already exists: $dir" }
    }
    $sourceConfig = Join-Path $env:LOCALAPPDATA 'lan-mouse\config.toml'
    $sourceCert = Join-Path $env:LOCALAPPDATA 'lan-mouse\lan-mouse.pem'
    foreach ($path in @($sourceConfig, $sourceCert, $oldBinary)) {
        if (-not (Test-Path -LiteralPath $path -PathType Leaf)) { throw "Missing: $path" }
    }
    foreach ($dir in @($installDir, $dataDir)) {
        New-Item -ItemType Directory -Path $dir | Out-Null
        & icacls.exe $dir /inheritance:r /grant:r '*S-1-5-18:(OI)(CI)(F)' '*S-1-5-32-544:(OI)(CI)(F)' | Out-Null
        if ($LASTEXITCODE) { throw "Cannot protect $dir" }
        & icacls.exe $dir /setowner '*S-1-5-32-544' | Out-Null
        if ($LASTEXITCODE) { throw "Cannot set owner on $dir" }
    }
    Copy-Item -LiteralPath $SourcePath -Destination $binary
    Copy-Item -LiteralPath $sourceConfig -Destination (Join-Path $dataDir 'config.toml')
    Copy-Item -LiteralPath $sourceCert -Destination (Join-Path $dataDir 'lan-mouse.pem')
    Export-ScheduledTask -TaskName $taskName | Set-Content -LiteralPath (Join-Path $dataDir 'interactive-task-before-service.xml') -Encoding Unicode
    foreach ($dir in @($installDir, $dataDir)) {
        & icacls.exe $dir /setowner '*S-1-5-32-544' /T | Out-Null
        if ($LASTEXITCODE) { throw "Cannot set file ownership in $dir" }
    }
    if ((Get-FileHash -LiteralPath $binary -Algorithm SHA256).Hash -ne $ExpectedSha256) {
        throw 'Installed binary hash mismatch.'
    }
    New-NetFirewallRule -Name $ruleName -DisplayName $ruleName -Direction Inbound -Action Allow -Protocol UDP -LocalPort 4242 -RemoteAddress $PeerAddress -Program $binary -Profile Any | Out-Null
    New-Service -Name $serviceName -DisplayName 'Lan Mouse UAC' -BinaryPathName ('"' + $binary + '" win-svc') -StartupType Automatic | Out-Null
} else {
    $service = Get-CimInstance Win32_Service -Filter "Name='lan-mouse'"
    if (-not $service -or $service.PathName -ne ('"' + $binary + '" win-svc') -or $service.StartName -ne 'LocalSystem') {
        throw 'Installed service does not match this deployment.'
    }
    if ((Get-Service $serviceName).Status -ne 'Stopped') { throw 'Stop candidate service before updating.' }
    Copy-Item -LiteralPath $SourcePath -Destination $binary -Force
    if ((Get-FileHash -LiteralPath $binary -Algorithm SHA256).Hash -ne $ExpectedSha256) {
        throw 'Updated binary hash mismatch.'
    }
    Set-Service $serviceName -StartupType Automatic
}

try {
    Stop-ScheduledTask -TaskName $taskName
    Get-CimInstance Win32_Process -Filter "name='lan-mouse.exe'" | Where-Object { $_.ExecutablePath -eq $oldBinary } | ForEach-Object { Stop-Process -Id $_.ProcessId -Force }
    Start-Service $serviceName
    Start-Sleep -Seconds 4
    $children = @(Get-CimInstance Win32_Process -Filter "name='lan-mouse.exe'" | Where-Object { $_.ExecutablePath -eq $binary -and $_.CommandLine -match 'service-daemon' })
    if ((Get-Service $serviceName).Status -ne 'Running' -or $children.Count -ne 1) {
        throw 'Service/session daemon did not start.'
    }
    $owner = Invoke-CimMethod -InputObject $children[0] -MethodName GetOwnerSid
    if ($owner.Sid -ne 'S-1-5-18' -or $children[0].SessionId -eq 0) {
        throw 'Session daemon has the wrong identity or session.'
    }
    if (Get-NetTCPConnection -State Listen -LocalPort 5252 -ErrorAction SilentlyContinue) {
        throw 'Unexpected frontend control listener.'
    }
    if (-not (Get-NetUDPEndpoint -LocalPort 4242 | Where-Object OwningProcess -eq $children[0].ProcessId)) {
        throw 'Session daemon did not bind input port.'
    }
    Write-Output ('Service installed; SYSTEM session daemon PID=' + $children[0].ProcessId)
} catch {
    Restore-Interactive
    throw
}
