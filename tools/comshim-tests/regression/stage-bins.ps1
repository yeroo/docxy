<#
.SYNOPSIS
  Copy the current release build of the COM shims into a folder that run.ps1 can
  test, with a BUILDINFO.txt saying exactly what was built.

.DESCRIPTION
  Run after `cargo build --release -p wordcomshim -p xlcomshim` (which also builds
  the in-proc DLLs and the typelib tools). Staging copies them out of target\ so a
  baseline taken on one commit is not overwritten by the next build.

    pwsh -NoProfile -File stage-bins.ps1 -Dest .\bin-main
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$Dest,
    [string]$Repo = (Resolve-Path (Join-Path $PSScriptRoot '..\..\..')).Path
)
$ErrorActionPreference = 'Stop'
New-Item -ItemType Directory -Force $Dest | Out-Null
$Dest = (Resolve-Path -LiteralPath $Dest).Path
$rel = Join-Path $Repo 'target\release'
$files = 'wordcomshim.exe', 'wordcomshim.dll', 'xlcomshim.exe', 'xlcomshim.dll', 'mktypelib.exe', 'mkwordtypelib.exe'
foreach ($f in $files) { Copy-Item -LiteralPath (Join-Path $rel $f) -Destination $Dest -Force }
Copy-Item (Join-Path $Repo 'tools\comshim\docxy-excel.tlb'), (Join-Path $Repo 'tools\wordshim\docxy-word.tlb') -Destination $Dest -Force

# Every windows-rs version in the lock: the shims use one, ratatui-image may pull another.
$lock = Get-Content -Raw (Join-Path $Repo 'Cargo.lock')
$windows = [regex]::Matches($lock, '(?m)^name = "(windows(?:-core|-implement)?)"\r?\nversion = "([^"]+)"') |
    ForEach-Object { "$($_.Groups[1].Value) $($_.Groups[2].Value)" } | Sort-Object -Unique

$info = @(
    "git: $(git -C $Repo log -1 --format='%H %ci %s')"
    "branch: $(git -C $Repo rev-parse --abbrev-ref HEAD)"
    "dirty: $((git -C $Repo status --porcelain) -join ' ')"
    "windows-rs in Cargo.lock: $($windows -join ', ')"
    "rustc: $(rustc --version)"
    "staged: $(Get-Date -Format o)"
)
foreach ($f in Get-ChildItem -LiteralPath $Dest -File -Include *.exe, *.dll, *.tlb -Name) {
    $p = Join-Path $Dest $f
    $info += "{0}  {1}  sha256={2}" -f $f, (Get-Item $p).Length, (Get-FileHash -Algorithm SHA256 $p).Hash.ToLower()
}
$info | Set-Content -Encoding utf8 (Join-Path $Dest 'BUILDINFO.txt')
$info
