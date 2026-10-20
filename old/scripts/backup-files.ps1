param([Parameter(Mandatory=$true,ValueFromRemainingArguments=$true)][string[]]$Paths)
$ErrorActionPreference = 'Stop'
# 根目录由脚本自身位置推出，不写死绝对路径 —— 项目目录改名后本脚本仍可用。
$root = Split-Path -Parent $PSScriptRoot
$bk   = Join-Path $root 'artifacts\backups'
New-Item -ItemType Directory -Force -Path $bk | Out-Null
$stamp = Get-Date -Format 'yyyyMMdd-HHmmss'
foreach ($p in $Paths) {
  $full = if ([System.IO.Path]::IsPathRooted($p)) { $p } else { Join-Path $root $p }
  if (-not (Test-Path -LiteralPath $full -PathType Leaf)) { Write-Output "SKIP (not a file): $full"; continue }
  $rel = $full.Substring($root.Length).TrimStart('\')
  $flat = $rel -replace '[\\/:*?"<>| ]','_'
  $dst  = Join-Path $bk ($flat + '.bak-' + $stamp)
  Copy-Item -LiteralPath $full -Destination $dst -Force
  $h1 = (Get-FileHash -Algorithm SHA256 -LiteralPath $full).Hash
  $h2 = (Get-FileHash -Algorithm SHA256 -LiteralPath $dst).Hash
  if ($h1 -ne $h2) { throw "SHA256 MISMATCH for $rel : $h1 vs $h2" }
  Write-Output ("OK {0}.bak-{1} sha256={2} bytes={3}" -f $flat, $stamp, $h1, (Get-Item -LiteralPath $full).Length)
}
