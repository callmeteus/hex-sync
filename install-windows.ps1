# hex-sync online installer for Windows.
# Usage:
#   powershell -ExecutionPolicy Bypass -Command "iwr https://raw.githubusercontent.com/callmeteus/hex-sync/master/install-windows.ps1 -OutFile install-windows.ps1; ./install-windows.ps1"
$ErrorActionPreference = 'Stop'

$Repo = 'callmeteus/hex-sync'
$Asset = 'hex-sync-windows-x86_64.exe'
$InstallDir = if ($env:HEX_SYNC_INSTALL_DIR) { $env:HEX_SYNC_INSTALL_DIR } else { Join-Path $env:LOCALAPPDATA 'hex-sync\bin' }
$Target = Join-Path $InstallDir 'hex-sync.exe'

if ($env:HEX_SYNC_VERSION) {
    $Url = "https://github.com/$Repo/releases/download/$($env:HEX_SYNC_VERSION)/$Asset"
} else {
    $Url = "https://github.com/$Repo/releases/latest/download/$Asset"
}

$temp = Join-Path ([System.IO.Path]::GetTempPath()) ("hex-sync-" + [guid]::NewGuid().ToString('N') + '.exe')
try {
    Write-Host "Downloading hex-sync from $Url"
    Invoke-WebRequest -Uri $Url -OutFile $temp -UseBasicParsing

    if (-not (Test-Path $temp) -or (Get-Item $temp).Length -eq 0) {
        throw 'Download produced an empty file.'
    }

    New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
    Move-Item -Force -Path $temp -Destination $Target
    $temp = $null
} finally {
    if ($temp -and (Test-Path $temp)) {
        Remove-Item -Force $temp
    }
}

# Add the install directory to the user PATH when it is not there yet.
$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
$entries = if ($userPath) { $userPath -split ';' } else { @() }
if ($entries -notcontains $InstallDir) {
    $newPath = if ($userPath) { "$userPath;$InstallDir" } else { $InstallDir }
    [Environment]::SetEnvironmentVariable('Path', $newPath, 'User')
    Write-Host "Added $InstallDir to your user PATH. Open a new terminal to use hex-sync."
}

Write-Host "Installed: $Target"
