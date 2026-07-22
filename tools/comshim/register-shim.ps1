<#
.SYNOPSIS
  Register xlcomshim as the per-user Excel.Application COM server (HKCU only).

.DESCRIPTION
  Points the Excel.Application ProgID at our own shim CLSID under
  HKCU\Software\Classes, so a COM client that has no Microsoft Excel resolves
  Excel.Application to xlcomshim.exe. Per-user and fully reversible: nothing is
  written to HKLM, and a distinct shim CLSID is used (never Microsoft's Excel
  CLSID {00024500-...}).

  GUARD: if Excel.Application is already mapped in HKCU to a different CLSID, the
  script refuses unless -Force. It never touches the machine-wide (HKLM) Excel
  registration, so an installed Excel keeps working for other users and elevated
  processes.

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
$ShimClsid  = '{7B3F9E20-4C1A-4E8B-A2D6-9F5C1E0B7A31}'   # our own coclass
$ExcelClsid = '{00024500-0000-0000-C000-000000000046}'   # Excel's real coclass
$Classes    = 'HKCU:\Software\Classes'

if (-not (Test-Path -LiteralPath $Exe)) {
    throw "xlcomshim.exe not found at $Exe. Build it first: cargo build --release -p xlcomshim"
}
$Exe = (Resolve-Path -LiteralPath $Exe).Path

# Guard: do not stomp a different existing mapping.
$existing = $null
if (Test-Path "$Classes\Excel.Application\CLSID") {
    $existing = (Get-ItemProperty "$Classes\Excel.Application\CLSID" -Name '(default)' -ErrorAction SilentlyContinue).'(default)'
}
if ($existing -and $existing -ne $ShimClsid -and -not $Force) {
    throw "HKCU already maps Excel.Application to $existing. Re-run with -Force to replace it (only affects your user; HKLM/installed Excel is untouched)."
}

# NB: HKLM (machine-wide) Excel is deliberately NOT read or changed. For this
# user, HKCU\Software\Classes shadows HKLM for COM lookups.
function Set-Default($path, $value) {
    New-Item -Path $path -Force | Out-Null
    Set-ItemProperty -Path $path -Name '(default)' -Value $value
}

# Late-bound path: the Excel.Application ProgID -> our shim CLSID.
Set-Default "$Classes\Excel.Application"               "Docxy Spreadsheet Application"
Set-Default "$Classes\Excel.Application\CLSID"         $ShimClsid
Set-Default "$Classes\CLSID\$ShimClsid"               "Docxy spreadsheet automation server"
Set-Default "$Classes\CLSID\$ShimClsid\LocalServer32" ('"{0}" /automation' -f $Exe)
Set-Default "$Classes\CLSID\$ShimClsid\ProgID"        "Excel.Application"

# Early-bound path: shadow Excel's REAL coclass CLSID in HKCU so a .NET client
# that does `new Excel.Application()` (activates by the fixed CLSID, not the
# ProgID) also reaches the shim. HKLM's Excel is untouched; deleting this HKCU
# key restores it for this user.
$g = ("HKCU already maps the Excel CLSID to {0}. Re-run with -Force." -f `
    (Get-ItemProperty "$Classes\CLSID\$ExcelClsid\LocalServer32" -Name '(default)' -ErrorAction SilentlyContinue).'(default)')
$exLs = (Get-ItemProperty "$Classes\CLSID\$ExcelClsid\LocalServer32" -Name '(default)' -ErrorAction SilentlyContinue).'(default)'
if ($exLs -and $exLs -notmatch 'xlcomshim' -and -not $Force) { throw $g }
Set-Default "$Classes\CLSID\$ExcelClsid\LocalServer32" ('"{0}" /automation' -f $Exe)

Write-Host "Registered Excel.Application (ProgID + coclass) -> $Exe (HKCU, per-user)."
Write-Host 'Test:  $x = New-Object -ComObject Excel.Application; $x.Version; $x.Quit()'
Write-Host 'Undo:  tools\comshim\unregister-shim.ps1'
