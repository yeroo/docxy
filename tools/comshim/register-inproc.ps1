<#
.SYNOPSIS
  Register the xlcomshim DLL as an in-process COM server (InprocServer32) under
  HKCU. In-proc means COM loads the DLL into the CLIENT's process and calls our
  vtable directly -- NO marshalling and NO type library needed. This is the path
  that works on a machine with no Office at all.

.DESCRIPTION
  Writes, per-user only (HKCU\Software\Classes):
    CLSID\{shim}      InprocServer32 = <dll>, ThreadingModel = Apartment
    CLSID\{00024500}  (Excel's real CLSID) same, so `new Excel.Application()`
                      binds in-proc when no real Office owns the HKLM key.
    Excel.Application / .16  ProgID -> {shim}
  ASCII-only so Windows PowerShell 5.1 on the VDI parses it.

  Reversible with unregister-inproc.ps1. If real Excel is installed its HKLM
  registration is untouched; note that for CLSCTX_ALL activation an in-proc
  server registered in HKCU is preferred over an HKLM LocalServer, so this
  effectively shadows Excel for this user -- unregister when done.
#>
[CmdletBinding()]
param(
    [string]$Dll,
    [switch]$Force
)
$ErrorActionPreference = 'Stop'
if (-not $Dll) { $Dll = Join-Path $PSScriptRoot '..\..\target\release\xlcomshim.dll' }
$Dll = (Resolve-Path $Dll).Path
if (-not (Test-Path $Dll)) { throw "DLL not found: $Dll" }

$shim  = '{7B3F9E20-4C1A-4E8B-A2D6-9F5C1E0B7A31}'
$excel = '{00024500-0000-0000-C000-000000000046}'
$classes = 'HKCU:\Software\Classes'

function Set-Inproc([string]$clsid) {
    $base = Join-Path $classes ("CLSID\" + $clsid)
    $ips  = Join-Path $base 'InprocServer32'
    if ((Test-Path $ips) -and -not $Force) {
        $cur = (Get-ItemProperty $ips -ErrorAction SilentlyContinue).'(default)'
        if ($cur -and $cur -notlike '*xlcomshim*') {
            throw "InprocServer32 for $clsid already set to '$cur'; use -Force to override."
        }
    }
    New-Item -Path $ips -Force | Out-Null
    New-ItemProperty -Path $ips -Name '(default)' -Value $Dll -PropertyType String -Force | Out-Null
    New-ItemProperty -Path $ips -Name 'ThreadingModel' -Value 'Apartment' -PropertyType String -Force | Out-Null
}

Set-Inproc $shim
Set-Inproc $excel

# ProgID -> shim CLSID (so CreateObject/new by ProgID reaches us in-proc).
foreach ($progid in @('Excel.Application', 'Excel.Application.16')) {
    $p = Join-Path $classes $progid
    New-Item -Path (Join-Path $p 'CLSID') -Force | Out-Null
    New-ItemProperty -Path (Join-Path $p 'CLSID') -Name '(default)' -Value $shim -PropertyType String -Force | Out-Null
}

Write-Host "Registered xlcomshim.dll as InprocServer32 (HKCU) -> $Dll"
Write-Host "Undo: tools\comshim\unregister-inproc.ps1"
