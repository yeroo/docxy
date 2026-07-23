<#
.SYNOPSIS
  Remove the Docxy Office COM shims installed by install.ps1 (HKCU only). HKLM and
  any installed Office are never touched; ProgID / Office-CLSID mappings are
  removed only where they currently point at our shims.
#>
[CmdletBinding()]
param()
$ErrorActionPreference = 'Stop'
$here = $PSScriptRoot
$Classes = 'HKCU:\Software\Classes'

function Unregister-Shim($friendly, $progids, $shimClsid, $officeClsid, $tlb, $mk) {
    # Our shim coclass (always ours) -> remove.
    Remove-Item "$Classes\CLSID\$shimClsid" -Recurse -Force -ErrorAction SilentlyContinue
    # Office CLSID shadow -> remove only if it points at our shim's dll/exe.
    $ips = (Get-ItemProperty "$Classes\CLSID\$officeClsid\InprocServer32" -Name '(default)' -ErrorAction SilentlyContinue).'(default)'
    $ls = (Get-ItemProperty "$Classes\CLSID\$officeClsid\LocalServer32" -Name '(default)' -ErrorAction SilentlyContinue).'(default)'
    if (($ips -like '*comshim*') -or ($ls -like '*comshim*')) {
        Remove-Item "$Classes\CLSID\$officeClsid" -Recurse -Force -ErrorAction SilentlyContinue
    }
    # ProgIDs -> remove only if mapped to our shim.
    foreach ($p in $progids) {
        $m = (Get-ItemProperty "$Classes\$p\CLSID" -Name '(default)' -ErrorAction SilentlyContinue).'(default)'
        if ($m -eq $shimClsid) { Remove-Item "$Classes\$p" -Recurse -Force -ErrorAction SilentlyContinue }
    }
    # Type library.
    if ((Test-Path $mk) -and (Test-Path $tlb)) { try { & $mk unregister $tlb | Out-Null } catch {} }
    Write-Host ("  {0}: removed" -f $friendly)
}

Write-Host "Uninstalling Docxy Office COM shims..."
Unregister-Shim "Docxy Excel" @("Excel.Application", "Excel.Application.16") `
    '{7B3F9E20-4C1A-4E8B-A2D6-9F5C1E0B7A31}' '{00024500-0000-0000-C000-000000000046}' `
    "$here\docxy-excel.tlb" "$here\mktypelib.exe"
Unregister-Shim "Docxy Word" @("Word.Application", "Word.Application.16") `
    '{9C2F4A10-7D33-4B6E-B1A4-2E7C8D5F0A92}' '{000209FF-0000-0000-C000-000000000046}' `
    "$here\docxy-word.tlb" "$here\mkwordtypelib.exe"

Write-Host "Uninstalled. Installed (HKLM) Office, if any, is unaffected."
