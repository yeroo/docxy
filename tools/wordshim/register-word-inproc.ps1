<#
.SYNOPSIS
  Register the wordcomshim DLL as an in-process COM server (InprocServer32, HKCU).
  In-proc means COM loads the DLL into the CLIENT's process and calls our vtable
  directly -- no marshalling, no type library. Reversible with
  unregister-word-inproc.ps1. ASCII-only for Windows PowerShell 5.1.
#>
[CmdletBinding()]
param(
    [string]$Dll,
    [switch]$Force
)
$ErrorActionPreference = 'Stop'
if (-not $Dll) { $Dll = Join-Path $PSScriptRoot '..\..\target\release\wordcomshim.dll' }
$Dll = (Resolve-Path $Dll).Path
if (-not (Test-Path $Dll)) { throw "DLL not found: $Dll" }

$shim = '{9C2F4A10-7D33-4B6E-B1A4-2E7C8D5F0A92}'
$word = '{000209FF-0000-0000-C000-000000000046}'
$classes = 'HKCU:\Software\Classes'

function Set-Inproc([string]$clsid) {
    $ips = Join-Path $classes ("CLSID\" + $clsid + "\InprocServer32")
    if ((Test-Path $ips) -and -not $Force) {
        $cur = (Get-ItemProperty $ips -ErrorAction SilentlyContinue).'(default)'
        if ($cur -and $cur -notlike '*wordcomshim*') {
            throw "InprocServer32 for $clsid already set to '$cur'; use -Force."
        }
    }
    New-Item -Path $ips -Force | Out-Null
    New-ItemProperty -Path $ips -Name '(default)' -Value $Dll -PropertyType String -Force | Out-Null
    New-ItemProperty -Path $ips -Name 'ThreadingModel' -Value 'Apartment' -PropertyType String -Force | Out-Null
}

Set-Inproc $shim
Set-Inproc $word
foreach ($progid in @('Word.Application', 'Word.Application.16')) {
    $p = Join-Path $classes ($progid + '\CLSID')
    New-Item -Path $p -Force | Out-Null
    New-ItemProperty -Path $p -Name '(default)' -Value $shim -PropertyType String -Force | Out-Null
}
Write-Host "Registered wordcomshim.dll as InprocServer32 (HKCU) -> $Dll"
Write-Host "Undo: tools\wordshim\unregister-word-inproc.ps1"
