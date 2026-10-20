<#
.SYNOPSIS
  Assemble the standalone, self-contained "capability-core" delivery directory (idempotent).

.DESCRIPTION
  1) Build the release exe (cargo build --release)
  2) Copy the exe to the standalone root
  3) robocopy /MIR vendor -> standalone\vendor
  4) Ensure standalone\data exists
  5) Write SHA256SUMS.txt
  6) Print a result summary
  Any failure exits with a non-zero code.

.NOTES
  Writes only into the standalone directory. Does not modify kernel/**, apps/desktop/src/**,
  data/registry.db or vendor/** (vendor is a read-only copy source).

  NOTE: ASCII-only on purpose. Windows PowerShell 5.1 parses a .ps1 without a BOM as ANSI,
  which corrupts non-ASCII literals on non-English locales. Keep this file ASCII.
#>
[CmdletBinding()]
param(
    [switch]$ReviewedStandalone,
    [switch]$SkipBuild,
    [switch]$SkipVendor,
    [string]$RepoRoot = 'C:\work\capability-core',
    [string]$StandaloneRoot = 'C:\work\capability-core-standalone'
)

$ErrorActionPreference = 'Stop'
if (-not $ReviewedStandalone) { throw 'Standalone promotion requires explicit user review. Use scripts/build-candidate.ps1 for development candidates.' }

Set-StrictMode -Version Latest

# ---------- constants ----------
$EXPECTED_VENDOR_FILES = 16000
$EXPECTED_VENDOR_BYTES = 686131545

$ManifestPath = Join-Path $RepoRoot 'apps\desktop\src-tauri\Cargo.toml'
$SrcExe       = Join-Path $RepoRoot 'apps\desktop\src-tauri\target\release\assets-desktop.exe'
$SrcVendor    = Join-Path $RepoRoot 'vendor'
$DstExe       = Join-Path $StandaloneRoot 'assets-desktop.exe'
$DstVendor    = Join-Path $StandaloneRoot 'vendor'
$DstData      = Join-Path $StandaloneRoot 'data'
$SumsPath     = Join-Path $StandaloneRoot 'SHA256SUMS.txt'

function Write-Step([string]$Text) {
    Write-Host ''
    Write-Host ('==== ' + $Text + ' ====') -ForegroundColor Cyan
}
function Write-Ok([string]$Text) { Write-Host ('  [OK]   ' + $Text) -ForegroundColor Green }
function Write-Warn2([string]$Text) { Write-Host ('  [WARN] ' + $Text) -ForegroundColor Yellow }
function Fail([string]$Text, [int]$Code = 1) {
    Write-Host ''
    Write-Host ('  [FAIL] ' + $Text) -ForegroundColor Red
    Write-Host ''
    exit $Code
}
# Native commands do not throw under $ErrorActionPreference = 'Stop'; capture the exit code.
function Invoke-Native([string]$FilePath, [string[]]$Arguments) {
    Write-Host ('  > ' + $FilePath + ' ' + ($Arguments -join ' ')) -ForegroundColor DarkGray
    & $FilePath @Arguments
    return $LASTEXITCODE
}

Write-Host 'capability-core - standalone assembly script' -ForegroundColor White
Write-Host ('Repo  : ' + $RepoRoot)
Write-Host ('Target: ' + $StandaloneRoot)

# ---------- path assertions ----------
if (-not (Test-Path -LiteralPath $StandaloneRoot)) {
    New-Item -ItemType Directory -Path $StandaloneRoot -Force | Out-Null
    Write-Ok ('created target directory ' + $StandaloneRoot)
}
if (-not (Test-Path -LiteralPath $RepoRoot -PathType Container)) { Fail ('RepoRoot does not exist: ' + $RepoRoot) 2 }
$saFull = (Resolve-Path -LiteralPath $StandaloneRoot).Path.TrimEnd('\')
$repoFull = (Resolve-Path -LiteralPath $RepoRoot).Path.TrimEnd('\')
if ($saFull -eq $repoFull) { Fail 'StandaloneRoot must not equal RepoRoot (refusing to overwrite the repo)' 2 }
if ($saFull.Length -le 3) { Fail ('Invalid StandaloneRoot: ' + $saFull) 2 }
if (-not (Test-Path -LiteralPath $SrcVendor -PathType Container)) { Fail ('vendor source missing: ' + $SrcVendor) 2 }
if (-not (Test-Path -LiteralPath $ManifestPath)) { Fail ('Cargo.toml missing: ' + $ManifestPath) 2 }
Write-Ok ('path assertions passed, standalone root: ' + $saFull)

# ---------- (a) build ----------
if ($SkipBuild) {
    Write-Step '(a) build exe - SKIPPED (-SkipBuild)'
    Write-Warn2 'skipping build, reusing the existing exe'
} else {
    Write-Step '(a) build exe: cargo build --release'
    Write-Host '  If another build is running, cargo waits for the lock. Do not interrupt.' -ForegroundColor DarkGray
    $cargoExit = Invoke-Native 'cargo' @('build', '--release', '--manifest-path', $ManifestPath)
    if ($cargoExit -ne 0) { Fail ('cargo build failed, exit code ' + $cargoExit) $cargoExit }
    Write-Ok 'cargo build succeeded'
}
if (-not (Test-Path -LiteralPath $SrcExe)) { Fail ('build artifact exe missing: ' + $SrcExe) 3 }

# ---------- (b) copy exe ----------
Write-Step '(b) copy exe to standalone root'
$srcExeItem = Get-Item -LiteralPath $SrcExe
Copy-Item -LiteralPath $SrcExe -Destination $DstExe -Force
$dstExeItem = Get-Item -LiteralPath $DstExe
if ($dstExeItem.Length -ne $srcExeItem.Length) {
    Fail ('exe size mismatch after copy: src ' + $srcExeItem.Length + ' / dst ' + $dstExeItem.Length) 4
}
Write-Ok ('exe copied: ' + $dstExeItem.Length + ' bytes')

# ---------- (c) sync vendor ----------
if ($SkipVendor) {
    Write-Step '(c) sync vendor - SKIPPED (-SkipVendor)'
    Write-Warn2 'skipping vendor sync'
} else {
    Write-Step '(c) sync vendor: robocopy /MIR'
    # /MIR deletes extra files in the destination, so assert the destination first.
    if ($DstVendor -notmatch '\\capability-core-standalone\\vendor$') {
        Fail ('/MIR destination failed its assertion, refusing to run: ' + $DstVendor) 5
    }
    $dstVendorParent = (Split-Path -Parent $DstVendor).TrimEnd('\')
    if ($dstVendorParent -ne $saFull) {
        Fail ('/MIR destination parent is not the standalone root, refusing to run: ' + $dstVendorParent) 5
    }
    if ($SrcVendor.TrimEnd('\') -eq $DstVendor.TrimEnd('\')) { Fail '/MIR source equals destination, refusing to run' 5 }
    if (-not (Test-Path -LiteralPath $DstVendor)) {
        New-Item -ItemType Directory -Path $DstVendor -Force | Out-Null
    }
    Write-Ok ('/MIR assertions passed: ' + $SrcVendor + '  ->  ' + $DstVendor)

    # robocopy exit codes 0-7 mean success (some files copied), >= 8 means failure.
    $roboExit = Invoke-Native 'robocopy' @($SrcVendor, $DstVendor, '/MIR', '/COPY:DAT', '/R:1', '/W:1', '/NFL', '/NDL', '/NJH', '/NJS')
    Write-Host ('  robocopy exit code: ' + $roboExit)
    if ($roboExit -ge 8) { Fail ('robocopy failed, exit code ' + $roboExit) 6 }
    Write-Ok ('robocopy finished (exit code ' + $roboExit + ', 0-7 is success)')
}

# ---------- (d) data directory ----------
Write-Step '(d) ensure data directory exists'
if (-not (Test-Path -LiteralPath $DstData -PathType Container)) {
    New-Item -ItemType Directory -Path $DstData -Force | Out-Null
    Write-Ok ('created ' + $DstData)
} else {
    Write-Ok ('already exists ' + $DstData + ' (existing data preserved, not cleared)')
}

# ---------- measure vendor ----------
Write-Step 'measure vendor files and bytes'
$vendorFiles = @(Get-ChildItem -LiteralPath $DstVendor -Recurse -File -Force -ErrorAction SilentlyContinue)
$vendorStat = $vendorFiles | Measure-Object -Property Length -Sum
$vendorCount = [int64]$vendorStat.Count
$vendorBytes = 0
if ($null -ne $vendorStat.Sum) { $vendorBytes = [int64]$vendorStat.Sum }
Write-Host ('  vendor files: ' + $vendorCount + ' (expected ' + $EXPECTED_VENDOR_FILES + ')')
Write-Host ('  vendor bytes: ' + $vendorBytes + ' (expected ' + $EXPECTED_VENDOR_BYTES + ')')
$vendorVerified = ($vendorCount -eq $EXPECTED_VENDOR_FILES) -and ($vendorBytes -eq $EXPECTED_VENDOR_BYTES)
if ($vendorVerified) {
    Write-Ok 'vendor check passed: file count and byte total match exactly'
} else {
    Write-Warn2 'vendor differs from expected values (the source vendor may have changed); not fatal, please review'
}

# ---------- (e) SHA256SUMS.txt ----------
Write-Step '(e) write SHA256SUMS.txt'
$exeHash = (Get-FileHash -LiteralPath $DstExe -Algorithm SHA256).Hash
$nl = [Environment]::NewLine
$lines = New-Object System.Collections.Generic.List[string]
$lines.Add('# capability-core standalone integrity manifest')
$lines.Add('# generated: ' + (Get-Date -Format 'yyyy-MM-dd HH:mm:ss zzz'))
$lines.Add('# format: <sha256>  <relative path>')
$lines.Add('')
$lines.Add($exeHash.ToLower() + '  assets-desktop.exe')
$lines.Add('')
$lines.Add('# vendor summary (per-file hashing is too large; file count and total bytes are recorded)')
$lines.Add('# vendor_files  ' + $vendorCount)
$lines.Add('# vendor_bytes  ' + $vendorBytes)
$lines.Add('# vendor_subdirs mcp, plugin, skill')
if ($vendorVerified) {
    $lines.Add('# vendor_expected  16000 files / 686131545 bytes  (MATCH)')
} else {
    $lines.Add('# vendor_expected  16000 files / 686131545 bytes  (MISMATCH - see WARN above)')
}
$lines.Add('')
$lines.Add('# verify from the standalone root:')
$lines.Add('#   certutil -hashfile assets-desktop.exe SHA256')
$lines.Add('# or PowerShell: (Get-FileHash .\assets-desktop.exe -Algorithm SHA256).Hash')
$tmpSums = $SumsPath + '.tmp'
Set-Content -LiteralPath $tmpSums -Value ($lines -join $nl) -Encoding UTF8
Move-Item -LiteralPath $tmpSums -Destination $SumsPath -Force
Write-Ok ('wrote ' + $SumsPath)

# ---------- (f) summary ----------
Write-Step '(f) assembly result'
$exeItem = Get-Item -LiteralPath $DstExe
Write-Host ('  exe path   : ' + $exeItem.FullName)
Write-Host ('  exe size   : ' + $exeItem.Length + ' bytes')
Write-Host ('  exe mtime  : ' + $exeItem.LastWriteTime.ToString('yyyy-MM-dd HH:mm:ss'))
Write-Host ('  exe sha256 : ' + $exeHash)
Write-Host ('  vendor     : ' + $vendorCount + ' files / ' + $vendorBytes + ' bytes')
if (Test-Path -LiteralPath $DstData) { Write-Host '  data dir   : present' } else { Write-Host '  data dir   : MISSING' }
Write-Host ('  SHA256SUMS : ' + $SumsPath)
Write-Host ''
Write-Host '  standalone root contents:' -ForegroundColor White
Get-ChildItem -LiteralPath $saFull -Force | Sort-Object Name | ForEach-Object {
    if ($_.PSIsContainer) {
        Write-Host ('    <DIR>        ' + $_.Name)
    } else {
        Write-Host ('    ' + ('{0,12}' -f $_.Length) + ' ' + $_.Name)
    }
}

$missing = @()
foreach ($required in @('assets-desktop.exe', 'vendor', 'data', 'SHA256SUMS.txt')) {
    if (-not (Test-Path -LiteralPath (Join-Path $saFull $required))) { $missing += $required }
}
if ($missing.Count -gt 0) { Fail ('assembly missing required items: ' + ($missing -join ', ')) 7 }

Write-Host ''
Write-Host 'Assembly complete.' -ForegroundColor Green
if (-not $vendorVerified) {
    Write-Host 'Note: vendor counts differ from expectations, see WARN above.' -ForegroundColor Yellow
}
exit 0
