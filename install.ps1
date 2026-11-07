$ErrorActionPreference = "Stop"

$Repo = "madLinux7/mad-super-bot"
$InstallDir = if ($env:INSTALL_DIR) { $env:INSTALL_DIR } else { Join-Path $env:LOCALAPPDATA "mad-super-bot" }
$InstallDir = [IO.Path]::GetFullPath($InstallDir)
$Binary = "mad-super-bot.exe"

if ([Environment]::OSVersion.Platform -ne [PlatformID]::Win32NT) {
    throw "This installer is for Windows. Use install.sh on Linux or macOS."
}
$Architecture = if ($env:PROCESSOR_ARCHITEW6432) { $env:PROCESSOR_ARCHITEW6432 } else { $env:PROCESSOR_ARCHITECTURE }
if ($Architecture -ne "AMD64") {
    throw "Unsupported architecture: $Architecture. This installer supports Windows x64."
}

$Url = "https://github.com/$Repo/releases/latest/download/$Binary"
$TempFile = Join-Path ([IO.Path]::GetTempPath()) ([IO.Path]::GetRandomFileName())
try {
    Write-Host "Downloading Mad Super Bot for Windows..."
    Invoke-WebRequest -Uri $Url -OutFile $TempFile -UseBasicParsing
    New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
    $OutFile = Join-Path $InstallDir $Binary
    Move-Item -LiteralPath $TempFile -Destination $OutFile -Force
} finally {
    if (Test-Path -LiteralPath $TempFile) {
        Remove-Item -LiteralPath $TempFile -Force
    }
}

$UserPath = [Environment]::GetEnvironmentVariable("Path", "User")
if (($UserPath -split ";") -notcontains $InstallDir) {
    [Environment]::SetEnvironmentVariable("Path", "$InstallDir;$UserPath", "User")
}
if (($env:Path -split ";") -notcontains $InstallDir) {
    $env:Path = "$InstallDir;$env:Path"
}

Write-Host "Mad Super Bot installed to $OutFile"
Write-Host "Run mad-super-bot --help to get started."
