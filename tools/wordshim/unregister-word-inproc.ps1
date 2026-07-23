<#
.SYNOPSIS
  Remove the HKCU in-process (InprocServer32) registration written by
  register-word-inproc.ps1, only where it points at wordcomshim.
#>
[CmdletBinding()]
param()
$ErrorActionPreference = 'Stop'
$shim = '{9C2F4A10-7D33-4B6E-B1A4-2E7C8D5F0A92}'
$word = '{000209FF-0000-0000-C000-000000000046}'
$classes = 'HKCU:\Software\Classes'

function Remove-Inproc([string]$clsid) {
    $ips = Join-Path $classes ("CLSID\" + $clsid + "\InprocServer32")
    if (Test-Path $ips) {
        $cur = (Get-ItemProperty $ips -ErrorAction SilentlyContinue).'(default)'
        if ($cur -like '*wordcomshim*') {
            Remove-Item (Join-Path $classes ("CLSID\" + $clsid)) -Recurse -Force
        }
    }
}
Remove-Inproc $shim
Remove-Inproc $word
foreach ($progid in @('Word.Application', 'Word.Application.16')) {
    $p = Join-Path $classes $progid
    if (Test-Path $p) {
        $cur = (Get-ItemProperty (Join-Path $p 'CLSID') -ErrorAction SilentlyContinue).'(default)'
        if ($cur -eq $shim) { Remove-Item $p -Recurse -Force }
    }
}
Write-Host "Removed HKCU in-proc Word shim registration (installed Word unaffected)."
