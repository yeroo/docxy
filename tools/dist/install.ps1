<#
.SYNOPSIS
  Install the Docxy Office COM shims (Excel + Word) so apps that automate Office
  over COM keep working on a machine with no Microsoft Office installed.

.DESCRIPTION
  Registers, per-user (HKCU\Software\Classes only), for each shim:
    * BOTH activation paths -- InprocServer32 (the .dll, loaded into the client's
      process; no marshalling, no type library) AND LocalServer32 (the .exe,
      out-of-process) -- on our own shim CLSID AND on Office's real CLSID, so
      `CreateObject("Excel.Application")` and `new Excel.Application()` alike reach
      the shim. COM prefers in-process when both are present.
    * the ProgIDs (Excel.Application[.16], Word.Application[.16]).
    * the type library (needed only for early-bound OUT-of-process marshalling).

  Everything is taken from this folder. Fully reversible (uninstall.ps1). HKLM and
  an installed Office are never touched. ASCII-only so Windows PowerShell 5.1 (the
  usual VDI shell) parses it.

.PARAMETER Force
  Overwrite an existing HKCU mapping that points somewhere other than our shim.
#>
[CmdletBinding()]
param([switch]$Force)
$ErrorActionPreference = 'Stop'
$here = $PSScriptRoot
$Classes = 'HKCU:\Software\Classes'

function Set-Default($path, $value) {
    New-Item -Path $path -Force | Out-Null
    Set-ItemProperty -Path $path -Name '(default)' -Value $value
}
function Set-Inproc($clsid, $dll) {
    $ips = "$Classes\CLSID\$clsid\InprocServer32"
    New-Item -Path $ips -Force | Out-Null
    Set-ItemProperty -Path $ips -Name '(default)' -Value $dll
    Set-ItemProperty -Path $ips -Name 'ThreadingModel' -Value 'Apartment'
}

function Register-Shim($friendly, $progids, $shimClsid, $officeClsid, $exe, $dll, $tlb, $mk) {
    foreach ($f in @($exe, $dll, $tlb, $mk)) {
        if (-not (Test-Path -LiteralPath $f)) { throw "missing package file: $f" }
    }
    # Guard: don't stomp a different existing ProgID mapping unless -Force.
    $existing = (Get-ItemProperty "$Classes\$($progids[0])\CLSID" -Name '(default)' -ErrorAction SilentlyContinue).'(default)'
    if ($existing -and $existing -ne $shimClsid -and -not $Force) {
        throw "$($progids[0]) already maps to $existing in HKCU. Re-run with -Force (HKLM/installed Office untouched)."
    }

    $ls = ('"{0}" /automation' -f $exe)
    # Our shim coclass: both activation paths + ProgID back-link.
    Set-Default "$Classes\CLSID\$shimClsid" "$friendly automation server"
    Set-Default "$Classes\CLSID\$shimClsid\LocalServer32" $ls
    Set-Inproc  $shimClsid $dll
    Set-Default "$Classes\CLSID\$shimClsid\ProgID" $progids[0]
    # Office's real coclass, shadowed so `new X.Application()` (activates by the
    # fixed CLSID, not the ProgID) also reaches the shim.
    Set-Default "$Classes\CLSID\$officeClsid\LocalServer32" $ls
    Set-Inproc  $officeClsid $dll
    # ProgID -> our shim CLSID.
    foreach ($p in $progids) {
        Set-Default "$Classes\$p" $friendly
        Set-Default "$Classes\$p\CLSID" $shimClsid
    }
    # Type library (early-bound out-of-process marshalling).
    & $mk register $tlb | Out-Null
    Write-Host ("  {0}: in-process + out-of-process + type library" -f $friendly)
}

Write-Host "Installing Docxy Office COM shims (per-user, HKCU)..."
Register-Shim "Docxy Excel" @("Excel.Application", "Excel.Application.16") `
    '{7B3F9E20-4C1A-4E8B-A2D6-9F5C1E0B7A31}' '{00024500-0000-0000-C000-000000000046}' `
    "$here\xlcomshim.exe" "$here\xlcomshim.dll" "$here\docxy-excel.tlb" "$here\mktypelib.exe"
Register-Shim "Docxy Word" @("Word.Application", "Word.Application.16") `
    '{9C2F4A10-7D33-4B6E-B1A4-2E7C8D5F0A92}' '{000209FF-0000-0000-C000-000000000046}' `
    "$here\wordcomshim.exe" "$here\wordcomshim.dll" "$here\docxy-word.tlb" "$here\mkwordtypelib.exe"

Write-Host ""
Write-Host "Installed. Verify:  .\selftest.ps1"
Write-Host "Uninstall:          .\uninstall.ps1"
Write-Host "Call log (per app): %TEMP%\xlcomshim.log , %TEMP%\wordcomshim.log"
