<#
.SYNOPSIS
  End-to-end verification of the xlcomshim Excel shim (P1).

.DESCRIPTION
  1. Registers the shim (HKCU) and runs excel-smoke.vbs against it -> the shim
     creates an .xlsx via gridcore.
  2. Unregisters the shim.
  3. If real Excel is installed, opens the shim's file with it (the oracle) and
     checks the values + the live formula recompute -- i.e. proves the shim
     produced a genuine, Excel-openable workbook with no repair.

  Safe on a machine that also has Excel: registration is per-user and reverted.
  ASCII-only so it runs under Windows PowerShell 5.1 (the VDI) and PowerShell 7.
#>
[CmdletBinding()]
param(
    [string]$Exe,
    [string]$Out = "$env:TEMP\xlcomshim-smoke.xlsx"
)
$ErrorActionPreference = 'Stop'
if (-not $Exe) { $Exe = Join-Path $PSScriptRoot '..\..\target\release\xlcomshim.exe' }
$tools = "$PSScriptRoot\..\comshim"
Remove-Item $Out -ErrorAction SilentlyContinue

& "$tools\register-shim.ps1" -Exe $Exe -Force | Out-Null
try {
    Write-Host "== create via shim =="
    cscript.exe '//nologo' "$PSScriptRoot\excel-smoke.vbs" $Out 2>&1 | ForEach-Object { Write-Host $_ }
} finally {
    & "$tools\unregister-shim.ps1" | Out-Null
}
if (-not (Test-Path $Out)) { throw "shim did not produce $Out" }
Write-Host ("shim wrote {0} bytes" -f (Get-Item $Out).Length)

# Oracle check (only if real Excel is present).
$hasExcel = $null -ne (Get-ItemProperty "Registry::HKEY_CLASSES_ROOT\Excel.Application\CLSID" -ErrorAction SilentlyContinue)
if (-not $hasExcel) { Write-Host "no real Excel installed; skipping oracle check"; return }

Write-Host "== open the shim's file in REAL Excel =="
$rx = New-Object -ComObject Excel.Application
$rx.Visible = $false; $rx.DisplayAlerts = $false
try {
    $wb = $rx.Workbooks.Open($Out)
    $ws = $wb.Worksheets.Item(1)
    $b4 = $ws.Range("B4").Value2
    Write-Host ("sheet={0}  A1={1}  B2={2}  B3={3}  B4={4}  formula={5}" -f `
        $ws.Name, $ws.Range("A1").Value2, $ws.Cells(2,2).Value2, $ws.Cells(3,2).Value2, $b4, $ws.Range("B4").Formula)
    $wb.Close($false)
    if ([math]::Abs([double]$b4 - 42.5) -lt 1e-9) { Write-Host "PASS: real Excel opened + recomputed the shim's file." }
    else { throw "B4 expected 42.5, got $b4" }
} finally {
    $rx.Quit()
    [Runtime.InteropServices.Marshal]::ReleaseComObject($rx) | Out-Null
}
