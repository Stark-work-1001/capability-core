# capability-core release builder: build devel + standalone, stage both dirs, zip both.
#
# Why two editions:
# - devel      = development/testing (user tests changes here; promoted to standalone after review)
# - standalone = production (portable, what the user actually runs)
# They MUST coexist, so: different ports (devel 18197 / standalone 18198),
# different icons (icons/devel vs icons/standalone), different titles,
# and each keeps its own portable data dir (data/ + vendor/ next to the exe).
#
# Usage:
#   powershell -NoProfile -ExecutionPolicy Bypass -File scripts\build-release.ps1
#   powershell -NoProfile -ExecutionPolicy Bypass -File scripts\build-release.ps1 -SkipFull
#
# NOTE: this machine has PowerShell 5.1 only (no pwsh), execution policy Restricted,
#       and PS 5.1 parses BOM-less UTF-8 .ps1 as ANSI -> keep this file PURE ASCII.

# Parameters must precede executable statements.
param(
    [switch]$ReviewedStandalone,
    [switch]$SkipFull,
    [switch]$SkipDevel,
    [switch]$SkipStandalone,
    [string]$RepoRoot = 'C:\work\capability-core',
    [string]$StandaloneRoot = 'C:\work\capability-core-standalone',
    [string]$ReleaseRoot = 'C:\work\capability-core-release'
)

$ErrorActionPreference = 'Stop'
if (-not $ReviewedStandalone) { throw 'Standalone promotion requires explicit user review. Use scripts/build-candidate.ps1 for development candidates.' }



function Step($msg) { Write-Host ("== " + $msg) }
function Fail($msg) { Write-Host ("!! " + $msg); exit 1 }

if (-not (Test-Path -LiteralPath $RepoRoot)) { Fail ("repo not found: " + $RepoRoot) }
if (-not (Test-Path -LiteralPath (Join-Path $RepoRoot 'kernel'))) { Fail ("not an capability-core repo: " + $RepoRoot) }

$tauriManifest = Join-Path $RepoRoot 'apps\desktop\src-tauri\Cargo.toml'
$desktopDir = Join-Path $RepoRoot 'apps\desktop'
$exePath = Join-Path $RepoRoot 'apps\desktop\src-tauri\target\release\assets-desktop.exe'

# ---------------------------------------------------------------- 1. frontend
Step 'build frontend dist'
Push-Location $desktopDir
try {
    & node node_modules/typescript/bin/tsc --noEmit
    if ($LASTEXITCODE -ne 0) { Fail ('tsc failed: ' + $LASTEXITCODE) }
    & node node_modules/vite/bin/vite.js build
    if ($LASTEXITCODE -ne 0) { Fail ('vite build failed: ' + $LASTEXITCODE) }
} finally { Pop-Location }

# ---------------------------------------------------------------- 2. devel build
if (-not $SkipDevel) {
    Step 'build DEVEL exe'
    Push-Location $RepoRoot
    try {
        & cargo build --release --manifest-path $tauriManifest
        if ($LASTEXITCODE -ne 0) { Fail ('cargo build (devel) failed: ' + $LASTEXITCODE) }
    } finally { Pop-Location }
    $develExe = Join-Path $ReleaseRoot 'devel\assets-desktop-devel.exe'
    New-Item -ItemType Directory -Path (Split-Path $develExe) -Force | Out-Null
    Copy-Item -LiteralPath $exePath -Destination $develExe -Force
    Step ('devel exe staged: ' + $develExe)
}

# ---------------------------------------------------------------- 3. standalone build
if (-not $SkipStandalone) {
    Step 'build STANDALONE exe (--features standalone)'
    Push-Location $RepoRoot
    try {
        & cargo build --release --features standalone --manifest-path $tauriManifest
        if ($LASTEXITCODE -ne 0) { Fail ('cargo build (standalone) failed: ' + $LASTEXITCODE) }
    } finally { Pop-Location }

    Step 'assemble standalone dir'
    if (-not (Test-Path -LiteralPath $StandaloneRoot)) { New-Item -ItemType Directory -Path $StandaloneRoot -Force | Out-Null }
    Copy-Item -LiteralPath $exePath -Destination (Join-Path $StandaloneRoot 'assets-desktop.exe') -Force
    if (-not (Test-Path -LiteralPath (Join-Path $StandaloneRoot 'data'))) {
        New-Item -ItemType Directory -Path (Join-Path $StandaloneRoot 'data') -Force | Out-Null
    }
    # vendor: mirror source, but guard the destination path first
    $srcVendor = Join-Path $RepoRoot 'vendor'
    $dstVendor = Join-Path $StandaloneRoot 'vendor'
    if (-not $dstVendor.EndsWith('standalone\vendor')) { Fail ('refusing to mirror into unexpected path: ' + $dstVendor) }
    Step 'mirror vendor'
    robocopy $srcVendor $dstVendor /MIR /NFL /NDL /NJH /NJS /R:1 /W:1 | Out-Null
    $rc = $LASTEXITCODE
    if ($rc -ge 8) { Fail ('robocopy failed: ' + $rc) }
    & powershell -NoProfile -ExecutionPolicy Bypass -Command ("(Get-FileHash -LiteralPath '" + (Join-Path $StandaloneRoot 'assets-desktop.exe') + "' -Algorithm SHA256).Hash.ToLower() + '  assets-desktop.exe'") | Set-Content -LiteralPath (Join-Path $StandaloneRoot 'SHA256SUMS.txt') -Encoding ASCII
    $v = Get-ChildItem -LiteralPath $dstVendor -Recurse -File -Force | Measure-Object Length -Sum
    Step ('standalone ready: exe + vendor(' + $v.Count + ' files / ' + $v.Sum + ' bytes)')
}

# ---------------------------------------------------------------- 4. release zips
Step 'build release zips'
New-Item -ItemType Directory -Path $ReleaseRoot -Force | Out-Null
Add-Type -AssemblyName System.IO.Compression.FileSystem

$stamp = Get-Date -Format 'yyyyMMdd-HHmm'
$stage = Join-Path $ReleaseRoot '_stage'
if (Test-Path -LiteralPath $stage) { Remove-Item -LiteralPath $stage -Recurse -Force }
New-Item -ItemType Directory -Path $stage -Force | Out-Null
Copy-Item -LiteralPath (Join-Path $StandaloneRoot 'assets-desktop.exe') $stage -Force
# Copy the two Chinese-named doc files by extension filter, never by literal name:
# PS 5.1 parses BOM-less UTF-8 as ANSI, so a Chinese literal here would corrupt the script.
Get-ChildItem -LiteralPath $StandaloneRoot -File -Filter '*.md' -ErrorAction SilentlyContinue |
    Copy-Item -Destination $stage -Force -ErrorAction SilentlyContinue
Get-ChildItem -LiteralPath $StandaloneRoot -File -Filter '*.cmd' -ErrorAction SilentlyContinue |
    Copy-Item -Destination $stage -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Path (Join-Path $stage 'data') -Force | Out-Null
Copy-Item -LiteralPath (Join-Path $StandaloneRoot 'data\registry.db') (Join-Path $stage 'data\registry.db') -Force -ErrorAction SilentlyContinue

$lite = Join-Path $ReleaseRoot ('assets-lite-' + $stamp + '.zip')
[IO.Compression.ZipFile]::CreateFromDirectory($stage, $lite, [IO.Compression.CompressionLevel]::Optimal, $false)
Step ('lite zip: ' + (Get-Item $lite).Length + ' bytes')

if (-not $SkipFull) {
    Step 'stage vendor for FULL zip (this takes a while)'
    $stageVendor = Join-Path $stage 'vendor'
    robocopy (Join-Path $StandaloneRoot 'vendor') $stageVendor /E /NFL /NDL /NJH /NJS /R:1 /W:1 | Out-Null
    if ($LASTEXITCODE -ge 8) { Fail ('robocopy (full stage) failed: ' + $LASTEXITCODE) }
    # runtime leftovers (unix sockets / stale locks) must not enter the archive
    Get-ChildItem -LiteralPath $stageVendor -Recurse -File -Force -Filter '*.sock' -ErrorAction SilentlyContinue | Remove-Item -Force -ErrorAction SilentlyContinue
    $full = Join-Path $ReleaseRoot ('assets-full-' + $stamp + '.zip')
    [IO.Compression.ZipFile]::CreateFromDirectory($stage, $full, [IO.Compression.CompressionLevel]::Optimal, $false)
    Step ('full zip: ' + (Get-Item $full).Length + ' bytes')
    Remove-Item -LiteralPath $stageVendor -Recurse -Force
}

Remove-Item -LiteralPath $stage -Recurse -Force

# ---------------------------------------------------------------- 5. delivery manifest
$exeHash = (Get-FileHash -LiteralPath (Join-Path $StandaloneRoot 'assets-desktop.exe') -Algorithm SHA256).Hash
$manifest = [ordered]@{
    product    = 'capability-core'
    built_at   = (Get-Date -Format 'yyyy-MM-dd HH:mm:ss')
    devel_port = 18197
    standalone_port = 18198
    standalone = [ordered]@{
        exe_sha256 = $exeHash
        exe_bytes  = (Get-Item (Join-Path $StandaloneRoot 'assets-desktop.exe')).Length
    }
    zips = @(Get-ChildItem -LiteralPath $ReleaseRoot -Filter 'assets-*.zip' | ForEach-Object {
        [ordered]@{ name = $_.Name; bytes = $_.Length; sha256 = (Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash }
    })
}
($manifest | ConvertTo-Json -Depth 6) | Set-Content -LiteralPath (Join-Path $ReleaseRoot 'BUILD_DELIVERY.json') -Encoding UTF8

Step 'DONE'
Get-ChildItem -LiteralPath $ReleaseRoot -Force | Select-Object Length, Name | Format-Table -AutoSize
