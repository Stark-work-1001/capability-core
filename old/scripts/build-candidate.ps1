# Produces a new review candidate only. Never promotes the daily standalone.
[CmdletBinding()]
param([switch]$SkipBuild)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$repo = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..')).Path
$out = Join-Path $repo ('artifacts\candidates\' + (Get-Date -Format 'yyyyMMdd-HHmmss'))
if (Test-Path -LiteralPath $out) { throw 'Candidate path already exists' }
$cache = Join-Path $repo 'artifacts\runtime-cache'
$pythonZip = Join-Path $cache 'python-3.12.10-embed-amd64.zip'
$webview = Join-Path $cache 'webview-extracted\Microsoft.WebView2.FixedVersionRuntime.155.0.4283.62.x64'
if (-not (Test-Path -LiteralPath (Join-Path $webview 'msedgewebview2.exe'))) { throw 'Extract the official Fixed Version runtime into runtime-cache first' }
if ((Get-FileHash -LiteralPath $pythonZip -Algorithm SHA256).Hash -ne '156c7eea90d58cd7e91a23f28a0056616b13e9f4cf4901b7b99b837b7848c6da') { throw 'Python runtime checksum mismatch' }
if (-not $SkipBuild) {
    Push-Location (Join-Path $repo 'apps\desktop')
    try { & npm.cmd run build:vite; if ($LASTEXITCODE -ne 0) {throw 'Frontend build failed'} } finally {Pop-Location}
    & cargo build --release --manifest-path (Join-Path $repo 'kernel\Cargo.toml') -p ac-mcp --bin assets-serve
    if ($LASTEXITCODE -ne 0) {throw 'Service build failed'}
    & cargo build --release --features standalone --manifest-path (Join-Path $repo 'apps\desktop\src-tauri\Cargo.toml')
    if ($LASTEXITCODE -ne 0) {throw 'Desktop build failed'}
}
New-Item -ItemType Directory -Path $out | Out-Null
New-Item -ItemType Directory -Path (Join-Path $out 'data'),(Join-Path $out 'runtime'),(Join-Path $out 'docs\planning'),(Join-Path $out 'docs\testing'),(Join-Path $out 'docs\usage') | Out-Null
Copy-Item -LiteralPath (Join-Path $repo 'apps\desktop\src-tauri\target\release\assets-desktop.exe') -Destination $out
Copy-Item -LiteralPath (Join-Path $repo 'kernel\target\release\assets-serve.exe') -Destination $out
Expand-Archive -LiteralPath $pythonZip -DestinationPath (Join-Path $out 'runtime\python')
Copy-Item -LiteralPath $webview -Destination (Join-Path $out 'runtime\webview2') -Recurse
& (Join-Path $out 'runtime\python\python.exe') -I (Join-Path $PSScriptRoot 'runtime-manifest.py') (Join-Path $out 'runtime\webview2')
if ($LASTEXITCODE -ne 0) {throw 'Runtime manifest generation failed'}
& robocopy (Join-Path $repo 'vendor') (Join-Path $out 'vendor') /E /XJ /XF *.sock /R:1 /W:1 /NFL /NDL /NJH /NJS /NP
if ($LASTEXITCODE -ge 8) {throw 'Vendor copy failed'}
if (Test-Path -LiteralPath (Join-Path $repo 'data\asset-research.json')) { Copy-Item -LiteralPath (Join-Path $repo 'data\asset-research.json') -Destination (Join-Path $out 'data') }
Copy-Item -LiteralPath (Join-Path $repo 'docs\configuration\model-slots.example.json') -Destination (Join-Path $out 'data\llm-providers.json')
$candidate = @{edition='standalone-review-candidate';created_at=(Get-Date -Format o);port=18297;approved_for_promotion=$false;python='3.12.10';webview2='155.0.4283.62';data='data/registry.db';production_directory_modified=$false}
$candidate | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $out 'CANDIDATE.json') -Encoding UTF8
Copy-Item -LiteralPath (Join-Path $repo 'docs\planning\2026-10-18-upgrade.md') -Destination (Join-Path $out 'docs\planning')
Copy-Item -LiteralPath (Join-Path $repo 'docs\testing\2026-10-19-upgrade-acceptance.md') -Destination (Join-Path $out 'docs\testing')
Copy-Item -LiteralPath (Join-Path $repo 'docs\usage\2026-10-19-candidate-guide.md') -Destination (Join-Path $out 'docs\usage')
Copy-Item -LiteralPath (Join-Path $repo 'artifacts\asset-research\summary.json') -Destination (Join-Path $out 'docs\asset-research-summary.json')
@'
Capability Core review candidate
Start assets-desktop.exe. Runtime dependencies are bundled in runtime/.
First launch verifies and copies the bundled WebView2 into LOCALAPPDATA/CapabilityCore/runtime-cache.
This offline cache contains runtime binaries only. Databases remain in this package's data/.
This candidate uses port 18297 and its own data/ database.
The daily standalone directory is not changed. Do not copy this over it before review.
Cloud models need your provider configuration; no keys are distributed.
Imported plugins may require their own external programs. Check dependencies in Repair.
The Fixed Version WebView2 runtime must be updated with future candidate builds.
'@ | Set-Content -LiteralPath (Join-Path $out 'README.txt') -Encoding UTF8
& (Join-Path $out 'runtime\python\python.exe') -I (Join-Path $PSScriptRoot 'hash-candidate.py') $out
if ($LASTEXITCODE -ne 0) {throw 'Candidate checksum generation failed'}
$out | Set-Content -LiteralPath (Join-Path $repo 'artifacts\latest-candidate.txt') -Encoding UTF8
Write-Host ('CANDIDATE=' + $out)
