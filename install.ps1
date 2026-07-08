# dcode-ai Windows installer.
#
# Usage (PowerShell):
#   irm https://raw.githubusercontent.com/Dhanuzh/dcode-ai/main/install.ps1 | iex
#
# Pin a version:
#   $env:DCODE_AI_VERSION = "v0.0.31"
#   irm https://raw.githubusercontent.com/Dhanuzh/dcode-ai/main/install.ps1 | iex
#
# Custom install dir:
#   $env:DCODE_AI_INSTALL_DIR = "D:\tools\dcode-ai"

$ErrorActionPreference = "Stop"

$Repo = "Dhanuzh/dcode-ai"
$Binary = "dcode-ai"
$Target = "x86_64-pc-windows-msvc"

function Info($msg)  { Write-Host "=> $msg" -ForegroundColor Blue }
function Fail($msg)  { Write-Host "error: $msg" -ForegroundColor Red; exit 1 }

if ([System.Environment]::OSVersion.Platform -ne "Win32NT") {
    Fail "This installer is for Windows. Use install.sh on Linux/macOS."
}
if (-not [Environment]::Is64BitOperatingSystem) {
    Fail "Only 64-bit Windows (x86_64) builds are published."
}

# ── Resolve version ─────────────────────────────────────────────────────────
$Version = $env:DCODE_AI_VERSION
if (-not $Version) {
    Info "Looking up the latest release…"
    try {
        $release = Invoke-RestMethod -Uri "https://api.github.com/repos/$Repo/releases/latest" -Headers @{ "User-Agent" = "dcode-ai-installer" }
        $Version = $release.tag_name
    } catch {
        Fail "Could not query the latest release: $_"
    }
}
Info "Installing $Binary $Version"

# ── Download ────────────────────────────────────────────────────────────────
$zipName = "$Binary-$Target.zip"
$baseUrl = "https://github.com/$Repo/releases/download/$Version"
$tmp = Join-Path ([System.IO.Path]::GetTempPath()) "dcode-ai-install-$PID"
New-Item -ItemType Directory -Force -Path $tmp | Out-Null
$zipPath = Join-Path $tmp $zipName

Info "Downloading $zipName…"
Invoke-WebRequest -Uri "$baseUrl/$zipName" -OutFile $zipPath -Headers @{ "User-Agent" = "dcode-ai-installer" }

# ── Verify checksum when published ──────────────────────────────────────────
try {
    $shaFile = Join-Path $tmp "$zipName.sha256"
    Invoke-WebRequest -Uri "$baseUrl/$zipName.sha256" -OutFile $shaFile -Headers @{ "User-Agent" = "dcode-ai-installer" }
    $expected = ((Get-Content $shaFile -Raw).Trim() -split "\s+")[0].ToLower()
    $actual = (Get-FileHash $zipPath -Algorithm SHA256).Hash.ToLower()
    if ($expected -ne $actual) {
        Fail "Checksum mismatch: expected $expected, got $actual"
    }
    Info "Checksum verified"
} catch [System.Net.WebException], [Microsoft.PowerShell.Commands.HttpResponseException] {
    Write-Host "warning: no checksum file published for $Version; skipping verification" -ForegroundColor Yellow
}

# ── Install ─────────────────────────────────────────────────────────────────
$installDir = $env:DCODE_AI_INSTALL_DIR
if (-not $installDir) {
    $installDir = Join-Path $env:LOCALAPPDATA "Programs\dcode-ai"
}
New-Item -ItemType Directory -Force -Path $installDir | Out-Null

# A running dcode-ai locks its exe; Windows still allows renaming it, so
# move the old binary aside instead of failing the upgrade.
$exe = Join-Path $installDir "$Binary.exe"
# Sweep stale renamed binaries from earlier upgrades (ignore still-locked ones).
Get-ChildItem -Path $installDir -Filter "$Binary.exe.old-*" -ErrorAction SilentlyContinue |
    ForEach-Object { Remove-Item $_.FullName -Force -ErrorAction SilentlyContinue }
if (Test-Path $exe) {
    # Unique name per upgrade: an .old from a previous upgrade may itself
    # still be locked by a running instance.
    $old = "$exe.old-$([System.Diagnostics.Process]::GetCurrentProcess().Id)-$(Get-Random)"
    try {
        Move-Item $exe $old -Force
    } catch {
        Fail "cannot replace $exe — close running dcode-ai instances and retry"
    }
}

Expand-Archive -Path $zipPath -DestinationPath $installDir -Force
Remove-Item -Recurse -Force $tmp
if (-not (Test-Path $exe)) {
    Fail "archive did not contain $Binary.exe"
}

# ── PATH (user scope) ───────────────────────────────────────────────────────
$userPath = [Environment]::GetEnvironmentVariable("Path", "User")
if (($userPath -split ";") -notcontains $installDir) {
    [Environment]::SetEnvironmentVariable("Path", "$userPath;$installDir", "User")
    $env:Path = "$env:Path;$installDir"
    Info "Added $installDir to your user PATH (restart existing terminals to pick it up)"
}

Info "Installed: $exe"
& $exe --version
Write-Host ""
Write-Host "Run 'dcode-ai' in a project directory to get started." -ForegroundColor Green
