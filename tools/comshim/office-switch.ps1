<#
.SYNOPSIS
  Switch this user's Excel.Application COM server between Microsoft Office and the
  xlcomshim shim, or show which is active.

.DESCRIPTION
  A convenience wrapper over register-shim.ps1 / unregister-shim.ps1. All changes
  are per-user (HKCU\Software\Classes) and reversible; the machine-wide (HKLM)
  Excel registration is never touched, so real Excel is always one 'office'
  switch away.

    office-switch.ps1 status     show the effective Excel.Application server
    office-switch.ps1 shim       route Excel.Application -> xlcomshim
    office-switch.ps1 office     route Excel.Application -> Microsoft Office

.PARAMETER Action  status | shim | office   (default: status)
.PARAMETER Exe     Path to xlcomshim.exe (for 'shim').
#>
[CmdletBinding()]
param(
    [ValidateSet('status', 'shim', 'office')]
    [string]$Action = 'status',
    [string]$Exe = "$PSScriptRoot\..\..\target\release\xlcomshim.exe"
)
$ErrorActionPreference = 'Stop'
$ExcelClsid = '{00024500-0000-0000-C000-000000000046}'

function Get-Default($path) {
    (Get-ItemProperty $path -Name '(default)' -ErrorAction SilentlyContinue).'(default)'
}

function Show-Status {
    # HKCU shadows HKLM for COM lookups, so resolve HKCU first.
    $progHkcu = Get-Default "HKCU:\Software\Classes\Excel.Application\CLSID"
    $clsid = if ($progHkcu) { $progHkcu } else { Get-Default "Registry::HKEY_CLASSES_ROOT\Excel.Application\CLSID" }
    $srvHkcu = Get-Default "HKCU:\Software\Classes\CLSID\$clsid\LocalServer32"
    $srv = if ($srvHkcu) { $srvHkcu } else { Get-Default "Registry::HKEY_CLASSES_ROOT\CLSID\$clsid\LocalServer32" }
    $earlyShadow = Get-Default "HKCU:\Software\Classes\CLSID\$ExcelClsid\LocalServer32"

    $active = if ($srv -match 'xlcomshim') { 'xlcomshim (Docxy)' }
              elseif ($srv -match 'EXCEL.EXE') { 'Microsoft Excel' }
              else { 'unknown / not registered' }

    Write-Host ""
    Write-Host "  Excel.Application is served by:  $active"
    Write-Host "    ProgID -> CLSID   : $clsid"
    Write-Host "    LocalServer32     : $srv"
    Write-Host ("    early-bound (HKCU {0}) : {1}" -f $ExcelClsid, ($(if ($earlyShadow) { $earlyShadow } else { '(none -> real Excel)' })))
    Write-Host ""
}

switch ($Action) {
    'status' { Show-Status }
    'shim' {
        & "$PSScriptRoot\register-shim.ps1" -Exe $Exe -Force
        Show-Status
    }
    'office' {
        & "$PSScriptRoot\unregister-shim.ps1"
        Show-Status
    }
}
