<#
.SYNOPSIS
  Remove the HKCU in-process (InprocServer32) registration written by
  register-inproc.ps1, but only if it points at xlcomshim.
#>
[CmdletBinding()]
param()
$ErrorActionPreference = 'Stop'
$shim  = '{7B3F9E20-4C1A-4E8B-A2D6-9F5C1E0B7A31}'
$excel = '{00024500-0000-0000-C000-000000000046}'
$classes = 'HKCU:\Software\Classes'

function Remove-Inproc([string]$clsid) {
    $ips = Join-Path $classes ("CLSID\" + $clsid + "\InprocServer32")
    if (Test-Path $ips) {
        $cur = (Get-ItemProperty $ips -ErrorAction SilentlyContinue).'(default)'
        if ($cur -like '*xlcomshim*') {
            Remove-Item (Join-Path $classes ("CLSID\" + $clsid)) -Recurse -Force
        }
    }
}
Remove-Inproc $shim
Remove-Inproc $excel
foreach ($progid in @('Excel.Application', 'Excel.Application.16')) {
    $p = Join-Path $classes $progid
    if (Test-Path $p) {
        $cur = (Get-ItemProperty (Join-Path $p 'CLSID') -ErrorAction SilentlyContinue).'(default)'
        if ($cur -eq $shim) { Remove-Item $p -Recurse -Force }
    }
}
Write-Host "Removed HKCU in-proc shim registration (installed Excel, if any, is unaffected)."
