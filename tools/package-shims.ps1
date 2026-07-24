<#
.SYNOPSIS
  Build the Excel + Word COM shims in release and assemble a self-contained,
  deployable folder (default: dist\office-shims\) ready to copy to a no-Office
  target and install with the bundled install.ps1.

.DESCRIPTION
  Produces:  <exe> <dll> for each shim, the mktypelib helpers, the committed
  .tlb type libraries, and the install / uninstall / selftest / README from
  tools\dist\. Nothing here needs Office; the .tlb files are shipped artifacts
  (they can only be *generated* on a machine with Office -- see tools\dist\README.md).

.PARAMETER OutDir
  Destination folder. Default: <repo>\dist\office-shims

.PARAMETER Zip
  Also produce <OutDir>.zip.
#>
[CmdletBinding()]
param(
    [string]$OutDir,
    [switch]$Zip
)
$ErrorActionPreference = 'Stop'
$repo = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
if (-not $OutDir) { $OutDir = Join-Path $repo 'dist\office-shims' }
$dist = Join-Path $repo 'tools\dist'
$rel = Join-Path $repo 'target\release'

Write-Host "Building shims (release)..."
& cargo build --release -p xlcomshim -p wordcomshim
if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }

# Binaries the package must contain (built artifacts).
$bins = @(
    'xlcomshim.exe', 'xlcomshim.dll', 'mktypelib.exe',
    'wordcomshim.exe', 'wordcomshim.dll', 'mkwordtypelib.exe'
)
# Committed shipped artifacts (type libraries) — authored on an Office machine.
$tlbs = @(
    (Join-Path $repo 'tools\comshim\docxy-excel.tlb'),
    (Join-Path $repo 'tools\wordshim\docxy-word.tlb')
)
# Deployment scripts + guide.
$scripts = @('install.ps1', 'uninstall.ps1', 'selftest.ps1', 'README.md')

if (Test-Path $OutDir) { Remove-Item $OutDir -Recurse -Force }
New-Item -ItemType Directory -Path $OutDir | Out-Null

$missing = @()
foreach ($b in $bins) {
    $src = Join-Path $rel $b
    if (Test-Path $src) { Copy-Item $src $OutDir } else { $missing += $src }
}
foreach ($t in $tlbs) {
    if (Test-Path $t) { Copy-Item $t $OutDir } else { $missing += $t }
}
foreach ($s in $scripts) {
    $src = Join-Path $dist $s
    if (Test-Path $src) { Copy-Item $src $OutDir } else { $missing += $src }
}
if ($missing.Count) { throw "packaging incomplete, missing:`n  $($missing -join "`n  ")" }

$count = (Get-ChildItem $OutDir -File).Count
Write-Host ("Assembled {0} files -> {1}" -f $count, $OutDir)

if ($Zip) {
    $zipPath = "$OutDir.zip"
    if (Test-Path $zipPath) { Remove-Item $zipPath -Force }
    Compress-Archive -Path (Join-Path $OutDir '*') -DestinationPath $zipPath
    Write-Host ("Zipped -> {0}" -f $zipPath)
}

Write-Host ""
Write-Host "Deploy: copy the folder to the target, then:  .\install.ps1 ; .\selftest.ps1"
