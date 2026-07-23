<#
.SYNOPSIS
  Remove the per-user wordcomshim Word.Application registration (HKCU only).
.DESCRIPTION
  Deletes the HKCU keys written by register-word.ps1. HKLM (installed Word) is
  never touched; the ProgID mapping and the Word-CLSID shadow are removed only if
  they currently point at our shim.
#>
[CmdletBinding()]
param()
$ErrorActionPreference = 'Stop'
$ShimClsid = '{9C2F4A10-7D33-4B6E-B1A4-2E7C8D5F0A92}'
$WordClsid = '{000209FF-0000-0000-C000-000000000046}'
$Classes   = 'HKCU:\Software\Classes'

Remove-Item "$Classes\CLSID\$ShimClsid" -Recurse -Force -ErrorAction SilentlyContinue

# Unregister our per-user type library (no-op if never registered).
$mk  = Join-Path $PSScriptRoot '..\..\target\release\mkwordtypelib.exe'
$tlb = Join-Path $PSScriptRoot 'docxy-word.tlb'
if ((Test-Path -LiteralPath $mk) -and (Test-Path -LiteralPath $tlb)) {
    try { & $mk unregister $tlb | Out-Null } catch { }
}

$wLs = (Get-ItemProperty "$Classes\CLSID\$WordClsid\LocalServer32" -Name '(default)' -ErrorAction SilentlyContinue).'(default)'
if ($wLs -and $wLs -match 'wordcomshim') {
    Remove-Item "$Classes\CLSID\$WordClsid" -Recurse -Force -ErrorAction SilentlyContinue
}

if (Test-Path "$Classes\Word.Application\CLSID") {
    $mapped = (Get-ItemProperty "$Classes\Word.Application\CLSID" -Name '(default)' -ErrorAction SilentlyContinue).'(default)'
    if ($mapped -eq $ShimClsid) {
        Remove-Item "$Classes\Word.Application" -Recurse -Force -ErrorAction SilentlyContinue
        Write-Host "Removed HKCU Word.Application shim mapping."
    } else {
        Write-Host "HKCU Word.Application maps to $mapped (not our shim) - left untouched."
    }
} else {
    Write-Host "No HKCU Word.Application mapping present."
}
Write-Host "Installed (HKLM) Word, if any, is unaffected."
