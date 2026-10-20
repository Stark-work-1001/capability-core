# Run only after the user has reviewed this exact candidate.
[CmdletBinding()]
param([switch]$ReviewedStandalone,[Parameter(Mandatory=$true)][string]$CandidateRoot,[string]$StandaloneRoot='C:\work\capability-core-standalone')
$ErrorActionPreference='Stop'
Set-StrictMode -Version Latest
if (-not $ReviewedStandalone) {throw 'Explicit user review is required before standalone promotion'}
$candidate=(Resolve-Path -LiteralPath $CandidateRoot).Path.TrimEnd('\')
$target=[IO.Path]::GetFullPath($StandaloneRoot).TrimEnd('\')
$repo=(Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..')).Path.TrimEnd('\')
if ($candidate -eq $target -or $target -eq $repo -or $target.Length -le 3) {throw 'Invalid promotion paths'}
if (-not (Test-Path -LiteralPath (Join-Path $candidate 'CANDIDATE.json'))) {throw 'Candidate marker missing'}
if (Test-Path -LiteralPath (Join-Path $target 'CANDIDATE.json')) {throw 'The daily-use target must not be a review candidate'}
foreach ($line in Get-Content -LiteralPath (Join-Path $candidate 'SHA256SUMS.txt') -Encoding UTF8) {
    if ($line -notmatch '^([a-f0-9]{64})  (.+)$') {throw 'Invalid checksum manifest'}
    $expected=$Matches[1];$relative=$Matches[2]
    # User-configured data is mutable and is never copied to the daily database.
    if ($relative.StartsWith('data\') -or $relative.StartsWith('data/')) {continue}
    $file=[IO.Path]::GetFullPath((Join-Path $candidate $relative))
    if (-not $file.StartsWith($candidate+'\',[StringComparison]::OrdinalIgnoreCase)) {throw 'Checksum path escaped candidate'}
    if ((Get-FileHash -LiteralPath $file -Algorithm SHA256).Hash.ToLowerInvariant() -ne $expected) {throw ('Checksum mismatch: '+$relative)}
}
$running=Get-Process -ErrorAction SilentlyContinue | Where-Object {$_.Path -eq (Join-Path $target 'assets-desktop.exe')}
if ($running) {throw 'Close the daily-use standalone before promotion'}
[IO.Directory]::CreateDirectory($target) | Out-Null
$tag=Get-Date -Format 'yyyyMMdd-HHmmss'
$stage=Join-Path $target ('.upgrade-stage-'+$tag)
$backup=Join-Path $target ('.upgrade-backup-'+$tag)
foreach ($path in @($stage,$backup)) {
    if (-not [IO.Path]::GetFullPath($path).StartsWith($target+'\',[StringComparison]::OrdinalIgnoreCase)) {throw 'Upgrade path escaped target'}
    if (Test-Path -LiteralPath $path) {throw 'Upgrade path already exists'}
    [IO.Directory]::CreateDirectory($path) | Out-Null
}
foreach ($name in @('assets-desktop.exe','assets-serve.exe','vendor','runtime','docs','README.txt')) {
    $src=Join-Path $candidate $name
    if (Test-Path -LiteralPath $src) {Copy-Item -LiteralPath $src -Destination (Join-Path $stage $name) -Recurse}
}
$movedOld=@();$published=@()
try {
    foreach ($item in Get-ChildItem -LiteralPath $stage) {
        $destination=Join-Path $target $item.Name
        if (Test-Path -LiteralPath $destination) {Move-Item -LiteralPath $destination -Destination (Join-Path $backup $item.Name);$movedOld+=$item.Name}
        Move-Item -LiteralPath $item.FullName -Destination $destination
        $published+=$item.Name
    }
    # Metadata is public, versioned research. Database and model credentials stay local.
    [IO.Directory]::CreateDirectory((Join-Path $target 'data')) | Out-Null
    Copy-Item -LiteralPath (Join-Path $candidate 'data\asset-research.json') -Destination (Join-Path $target 'data\asset-research.json') -Force
    if (-not (Test-Path -LiteralPath (Join-Path $target 'data\llm-providers.json'))) {Copy-Item -LiteralPath (Join-Path $candidate 'data\llm-providers.json') -Destination (Join-Path $target 'data\llm-providers.json')}
    @{candidate=$candidate;promoted_at=(Get-Date -Format o);backup=$backup;database_preserved=$true;port=18198} | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $target 'LAST-UPGRADE.json') -Encoding UTF8
} catch {
    # All paths are verified children of the explicitly named standalone target.
    foreach ($name in $published) {Move-Item -LiteralPath (Join-Path $target $name) -Destination (Join-Path $stage $name)}
    foreach ($name in $movedOld) {Move-Item -LiteralPath (Join-Path $backup $name) -Destination (Join-Path $target $name)}
    throw
}
Write-Host ('Standalone promoted. Original program files retained in '+$backup)
