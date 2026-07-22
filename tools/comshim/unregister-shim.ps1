<#
.SYNOPSIS
  Remove the per-user xlcomshim Excel.Application registration (HKCU only).
.DESCRIPTION
  Deletes the HKCU keys written by register-shim.ps1. The machine-wide (HKLM)
  Excel registration is never touched, so a real installed Excel keeps working.
  Only removes the Excel.Application ProgID mapping if it currently points at our
  shim CLSID (so it won't disturb some other tool's mapping).
#>
[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
$ShimClsid  = '{7B3F9E20-4C1A-4E8B-A2D6-9F5C1E0B7A31}'
$ExcelClsid = '{00024500-0000-0000-C000-000000000046}'
$Classes    = 'HKCU:\Software\Classes'

Remove-Item "$Classes\CLSID\$ShimClsid" -Recurse -Force -ErrorAction SilentlyContinue

# Remove the HKCU Excel-CLSID shadow only if it points at our shim (never touch
# a mapping we didn't create; HKLM's real Excel is never in play here).
$exLs = (Get-ItemProperty "$Classes\CLSID\$ExcelClsid\LocalServer32" -Name '(default)' -ErrorAction SilentlyContinue).'(default)'
if ($exLs -and $exLs -match 'xlcomshim') {
    Remove-Item "$Classes\CLSID\$ExcelClsid" -Recurse -Force -ErrorAction SilentlyContinue
}

if (Test-Path "$Classes\Excel.Application\CLSID") {
    $mapped = (Get-ItemProperty "$Classes\Excel.Application\CLSID" -Name '(default)' -ErrorAction SilentlyContinue).'(default)'
    if ($mapped -eq $ShimClsid) {
        Remove-Item "$Classes\Excel.Application" -Recurse -Force -ErrorAction SilentlyContinue
        Write-Host "Removed HKCU Excel.Application shim mapping."
    } else {
        Write-Host "HKCU Excel.Application maps to $mapped (not our shim) - left untouched."
    }
} else {
    Write-Host "No HKCU Excel.Application mapping present."
}
Write-Host "Installed (HKLM) Excel, if any, is unaffected."
