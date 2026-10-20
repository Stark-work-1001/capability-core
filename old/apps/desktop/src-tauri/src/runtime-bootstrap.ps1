$ErrorActionPreference='Stop'
[Console]::OutputEncoding=[System.Text.UTF8Encoding]::new($false)
$root=$env:CORE_BOOTSTRAP_ROOT
$source=Join-Path $root 'runtime\webview2'
$manifestPath=Join-Path $root 'runtime\webview2-manifest.json'
$manifestHash=(Get-FileHash -LiteralPath $manifestPath -Algorithm SHA256).Hash.ToLowerInvariant()
if ($manifestHash -ne $env:CORE_BOOTSTRAP_MANIFEST) { throw 'Runtime manifest changed' }
$manifest=Get-Content -LiteralPath $manifestPath -Raw -Encoding UTF8 | ConvertFrom-Json
$bases=@()
if ($env:CORE_RUNTIME_CACHE_DIR) {$bases+= $env:CORE_RUNTIME_CACHE_DIR}
else {
    if ($env:LOCALAPPDATA) {$bases+= (Join-Path $env:LOCALAPPDATA 'CapabilityCore\runtime-cache')}
    if ($env:PROGRAMDATA) {$bases+= (Join-Path $env:PROGRAMDATA 'CapabilityCore\runtime-cache')}
}
$failures=@()
foreach ($base in $bases) {
    try {
        $cache=[IO.Path]::GetFullPath((Join-Path $base $manifestHash))
        [IO.Directory]::CreateDirectory($cache) | Out-Null
        foreach ($entry in $manifest.files.PSObject.Properties) {
            $name=$entry.Name
            if ([IO.Path]::IsPathRooted($name) -or $name.Contains(':') -or $name.Contains('\') -or ($name.Split('/') | Where-Object {$_ -eq '..' -or $_ -eq '.' -or $_ -eq ''})) {throw 'Invalid runtime file path'}
            $inputFile=Join-Path $source $name
            $outputFile=[IO.Path]::GetFullPath((Join-Path $cache $name))
            if (-not $outputFile.StartsWith($cache+'\',[StringComparison]::OrdinalIgnoreCase)) {throw 'Runtime path escaped cache'}
            if ((Get-Item -LiteralPath $inputFile).Attributes -band [IO.FileAttributes]::ReparsePoint) {throw 'Runtime source must not be a link'}
            $expected=$entry.Value
            if ((Get-FileHash -LiteralPath $inputFile -Algorithm SHA256).Hash.ToLowerInvariant() -ne $expected) {throw ('Bundled runtime checksum failed: '+$name)}
            [IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($outputFile)) | Out-Null
            if (-not (Test-Path -LiteralPath $outputFile)) {
                $stream=[IO.File]::Open($outputFile,[IO.FileMode]::CreateNew,[IO.FileAccess]::Write,[IO.FileShare]::Read)
                try {$bytes=[IO.File]::ReadAllBytes($inputFile);$stream.Write($bytes,0,$bytes.Length);$stream.Flush($true)} finally {$stream.Dispose()}
            }
            if ((Get-Item -LiteralPath $outputFile).Attributes -band [IO.FileAttributes]::ReparsePoint) {throw 'Runtime cache must not be a link'}
            if ((Get-FileHash -LiteralPath $outputFile -Algorithm SHA256).Hash.ToLowerInvariant() -ne $expected) {throw ('Cached runtime checksum failed: '+$name)}
        }
        [IO.File]::WriteAllText((Join-Path $cache '.complete'),$manifestHash,[Text.UTF8Encoding]::new($false))
        [Console]::WriteLine($cache)
        exit 0
    } catch {$failures+= $_.Exception.Message}
}
throw ('Offline runtime deployment failed: '+($failures -join '; '))
