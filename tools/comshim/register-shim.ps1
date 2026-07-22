<#
.SYNOPSIS
  Register xlcomshim as the per-user Excel.Application COM server (HKCU only).

.DESCRIPTION
  Points the Excel.Application ProgID at our own shim CLSID under
  HKCU\Software\Classes, so a COM client that has no Microsoft Excel resolves
  Excel.Application to xlcomshim.exe. Per-user and fully reversible: nothing is
  written to HKLM, and a distinct shim CLSID is used (never Microsoft's Excel
  CLSID {00024500-...}).

  GUARD: if Excel.Application is already mapped in HKCU to a different CLSID
  (e.g. you registered something else), the script refuses unless -Force. It
  never touches the machine-wide (HKLM) Excel registration, so an installed
  Excel keeps working for other users and elevated processes.

.PARAMETER Exe
  Path to xlcomshim.exe. Defaults to the release build in this repo.

.PARAMETER Force
  Overwrite an existing HKCU Excel.Application mapping.
#>
[CmdletBinding()]
param(
    [string]$Exe = "$PSScriptRoot\..\..\target\release\xlcomshim.exe",
    [switch]$Force
)

$ErrorActionPreference = 'Stop'
$ShimClsid = '{7B3F9E20-4C1A-4E8B-A2D6-9F5C1E0B7A31}'
$Classes   = 'HKCU:\Software\Classes'

$Exe = (Resolve-Path -LiteralPath $Exe -ErrorAction Stop).Path
if (-not (Test-Path -LiteralPath $Exe)) { throw "xlcomshim.exe not found at $Exe — build it first (cargo build --release -p xlcomshim)." }

# --- guard: don't stomp a different existing mapping ---------------------------
$existing = $null
if (Test-Path "$Classes\Excel.Application\CLSID") {
    $existing = (Get-ItemProperty "$Classes\Excel.Application\CLSID" -Name '(default)' -ErrorAction SilentlyContinue).'(default)'
}
if ($existing -and $existing -ne $ShimClsid -and -not $Force) {
    throw "HKCU already maps Excel.Application to $existing. Re-run with -Force to replace it (this only affects your user; HKLM/installed Excel is untouched)."
}

# NB: HKLM (machine-wide) Excel is deliberately NOT read or changed. On this
# user, HKCU\Software\Classes shadows HKLM for COM lookups.
function Set-Default($path, $value) {
    New-Item -Path $path -Force | Out-Null
    Set-ItemProperty -Path $path -Name '(default)' -Value $value
}

Set-Default "$Classes\Excel.Application"                      "Docxy Spreadsheet Application"
Set-Default "$Classes\Excel.Application\CLSID"                $ShimClsid
Set-Default "$Classes\CLSID\$ShimClsid"                       "Docxy spreadsheet automation server"
Set-Default "$Classes\CLSID\$ShimClsid\LocalServer32"        ('"{0}" /automation' -f $Exe)
Set-Default "$Classes\CLSID\$ShimClsid\ProgID"               "Excel.Application"

Write-Host "Registered Excel.Application -> $ShimClsid -> $Exe (HKCU, per-user)." -ForegroundColor Green
Write-Host "Test:  `$x = New-Object -ComObject Excel.Application; `$x.Version; `$x.Quit()" -ForegroundColor Cyan
Write-Host "Undo:  tools\comshim\unregister-shim.ps1" -ForegroundColor DarkGray
