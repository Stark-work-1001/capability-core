# Embed application manifest into capabilitycore-desktop.exe
#
# Why this exists
# ---------------
# Tauri 2's `wry` / `windows-sys` import `comctl32.dll`'s `TaskDialogIndirect`.
# On Windows 7+ that symbol lives in `comctl32.dll v6`, which is **activated by
# the presence of an application manifest** requesting
# `Microsoft.Windows.Common-Controls 6.0.0.0`.
#
# The bare `target\release\capabilitycore-desktop.exe` that `cargo build` produces
# does **not** carry that manifest. Running it triggers:
#   "Cannot find entry point TaskDialogIndirect in DLL comctl32.dll"
#
# The proper fix in production is the `cargo tauri build` artifact:
#   `target\release\bundle\nsis\CapabilityCore_*.exe` -- NSIS bundles the manifest
#   for us. This script is the **fast inner-loop** escape hatch so the dev
#   build also starts cleanly without doing a full NSIS run.
#
# Usage
# -----
#   powershell -ExecutionPolicy Bypass -File resources\embed-manifest.ps1
#
# Requires
# --------
#   mt.exe (Windows SDK) -- absolute path is resolved below.
#   The manifest file lives next to this script as `assets-desktop.exe.manifest`.
#
# Idempotent
# ----------
# Running twice does not stack the manifest. `mt.exe` overwrites the existing
# resource if it already has the same id.

$ErrorActionPreference = 'Stop'

$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$Exe       = Join-Path $ScriptDir '..\target\release/capabilitycore-desktop.exe' | Resolve-Path -ErrorAction SilentlyContinue
$Manifest  = Join-Path $ScriptDir 'assets-desktop.exe.manifest'

if (-not $Exe) {
  throw "Cannot find capabilitycore-desktop.exe -- run `cargo build --release` first."
}
if (-not (Test-Path -LiteralPath $Manifest)) {
  throw "Cannot find manifest: $Manifest"
}

# Locate mt.exe (Windows SDK)
$Mt = (Get-ChildItem -LiteralPath 'C:\Program Files (x86)\Windows Kits\10\bin' `
  -Recurse -Filter 'mt.exe' -ErrorAction SilentlyContinue |
  Where-Object { $_.FullName -match '\\x64\\mt\.exe$' } |
  Select-Object -First 1).FullName
if (-not $Mt) {
  throw "Cannot find mt.exe -- install Windows SDK."
}

$sizeBefore = (Get-Item -LiteralPath $Exe).Length
$bytes = [System.IO.File]::ReadAllBytes($Exe)
$hasManifest = $bytes | Select-String -Pattern '<assembly' -SimpleMatch -Quiet
Write-Output ("exe size before: {0:N0} bytes" -f $sizeBefore)
Write-Output ("already has manifest? {0}" -f $hasManifest)

# Embed. id 1 is the standard application-manifest resource slot.
& $Mt -manifest $Manifest -outputresource:"$Exe;#1"
if ($LASTEXITCODE -ne 0) { throw "mt.exe failed: $LASTEXITCODE" }

$sizeAfter = (Get-Item -LiteralPath $Exe).Length
Write-Output ("exe size after:  {0:N0} bytes  (delta {1:+#,##0;-#,##0;0})" -f $sizeAfter, ($sizeAfter - $sizeBefore))
Write-Output "OK. Try:"
Write-Output "  $Exe"
