<#
.SYNOPSIS
  Build the C# interop test and run it against BOTH real Excel and the shim,
  for both early-bound (PIA/vtable) and late-bound (IDispatch) binding.

.DESCRIPTION
  This is the pre-VDI confidence gate. Petrel is a .NET app; the early-bound run
  against the shim tells us whether our IDispatch server is enough or whether we
  must ship a type library (P2). We can iterate here until BOTH bindings pass
  against the shim -- before the single VDI attempt.

  ASCII-only (PowerShell 5.1 safe). Restores real Excel at the end.
#>
[CmdletBinding()]
param(
    [string]$Exe
)
$ErrorActionPreference = 'Stop'
if (-not $Exe) { $Exe = Join-Path $PSScriptRoot '..\..\..\target\release\xlcomshim.exe' }
$Exe = (Resolve-Path -LiteralPath $Exe).Path
Write-Host "shim exe: $Exe"
$csc = 'C:\Windows\Microsoft.NET\Framework64\v4.0.30319\csc.exe'
$pia = (Get-ChildItem 'C:\Windows\assembly\GAC_MSIL\Microsoft.Office.Interop.Excel' -Recurse -Filter 'Microsoft.Office.Interop.Excel.dll' -ErrorAction SilentlyContinue | Select-Object -First 1).FullName
$csharpDll = 'C:\Windows\Microsoft.NET\Framework64\v4.0.30319\Microsoft.CSharp.dll'
$app = "$PSScriptRoot\ExcelInteropTest.exe"
$src = "$PSScriptRoot\ExcelInteropTest.cs"
$switch = "$PSScriptRoot\..\..\comshim\office-switch.ps1"

if (-not $pia) { throw 'Microsoft.Office.Interop.Excel PIA not found in the GAC.' }
Write-Host "PIA: $pia"

Write-Host "== compiling ExcelInteropTest.cs =="
& $csc /nologo /platform:x64 "/reference:$pia" "/reference:$csharpDll" "/out:$app" $src
if ($LASTEXITCODE -ne 0) { throw 'csc failed' }

function Run($mode, $tag) {
    $out = Join-Path $env:TEMP ("csinterop-{0}-{1}.xlsx" -f $tag, $mode)
    Remove-Item $out -ErrorAction SilentlyContinue
    Write-Host ("--- {0} / {1}-bound ---" -f $tag, $mode)
    & $app $mode $out 2>&1 | ForEach-Object { Write-Host ("    " + $_) }
    return $out
}

try {
    Write-Host "`n########## BASELINE: real Microsoft Excel ##########"
    & $switch office | Out-Null
    Run 'early' 'office' | Out-Null
    Run 'late'  'office' | Out-Null

    Write-Host "`n########## SHIM: xlcomshim ##########"
    Remove-Item "$env:TEMP\xlcomshim.log" -ErrorAction SilentlyContinue
    & $switch shim -Exe $Exe | Out-Null
    # Decisive P2 gate: activate the shim by its OWN clsid and cast to the PIA's
    # typed Excel.Application interface (no real-Excel conflict).
    Run 'castshim' 'shim' | Out-Null
    # Simulate the VDI (no Excel present): kill any lingering EXCEL.EXE, else an
    # already-running Excel serves the {00024500} class object and early-bound
    # activation never consults our registry shadow.
    function Kill-Excel {
        for ($i = 0; $i -lt 8; $i++) {
            $p = Get-Process EXCEL -ErrorAction SilentlyContinue
            if (-not $p) { return $true }
            $p | Stop-Process -Force -ErrorAction SilentlyContinue
            Start-Sleep -Milliseconds 400
        }
        return $null -eq (Get-Process EXCEL -ErrorAction SilentlyContinue)
    }
    Write-Host ("  killed EXCEL.EXE (none running now: {0})" -f (Kill-Excel))
    # THE decisive early-bound test: forces LocalServer (our shim), then casts to
    # the PIA's typed interface. This is what a no-Office VDI does for early-bound.
    Run 'earlyls' 'shim' | Out-Null
    Write-Host ("  killed EXCEL.EXE again (none running now: {0})" -f (Kill-Excel))
    Run 'early' 'shim' | Out-Null
    Write-Host ("  killed EXCEL.EXE again (none running now: {0})" -f (Kill-Excel))
    $lOut = Run 'late'  'shim'
    Write-Host "`n--- shim dispatch log ---"
    if (Test-Path "$env:TEMP\xlcomshim.log") {
        Get-Content "$env:TEMP\xlcomshim.log" | ForEach-Object { Write-Host ("    " + $_) }
    } else { Write-Host "    (no log written)" }
    Write-Host "`nfiles: early=$([bool](Test-Path $eOut))  late=$([bool](Test-Path $lOut))"
}
finally {
    & $switch office | Out-Null
    Write-Host "restored: Excel.Application -> Microsoft Office"
}
