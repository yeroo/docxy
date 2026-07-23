<#
.SYNOPSIS
  PowerShell COM conformance test for the Excel shim (xlcomshim).

.DESCRIPTION
  `New-Object -ComObject Excel.Application` drives the shim through the CLR's
  reflection-based IDispatch late binding — an implementation independent of
  VBScript (scripting engine) and pywin32 (C). Runs under BOTH Windows PowerShell
  5.1 (desktop CLR) and PowerShell 7+ (.NET Core COM interop). Builds a workbook,
  saves it, and validates the produced .xlsx directly (no Excel needed to open it).

  Run (shim installed + typelib registered):  powershell -File com_conformance.ps1
  Exit 0 = PASS.
#>
[CmdletBinding()]
param()
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.IO.Compression.FileSystem

function Test-Ooxml($path, $part, $needle) {
    if (-not (Test-Path $path)) { return "no file produced" }
    $z = [System.IO.Compression.ZipFile]::OpenRead($path)
    try {
        $names = $z.Entries.FullName
        if ($names -notcontains $part) { return "missing $part" }
        # gridcore never writes docProps/app.xml; real Excel always does.
        if ($names -contains 'docProps/app.xml') { return "docProps/app.xml present -> real Excel served, not the shim" }
        $found = $false
        foreach ($e in $z.Entries) {
            if ($e.FullName -match '\.xml$') {
                $sr = New-Object System.IO.StreamReader($e.Open())
                if ($sr.ReadToEnd() -match [regex]::Escape($needle)) { $found = $true }
                $sr.Close()
            }
        }
        if (-not $found) { return "text '$needle' not found" }
    } finally { $z.Dispose() }
    return "ok"
}

$xlsx = Join-Path $env:TEMP "posh-xl-conformance.xlsx"
Remove-Item $xlsx -ErrorAction SilentlyContinue

$xl = New-Object -ComObject Excel.Application
try {
    $xl.Visible = $false
    $wb = $xl.Workbooks.Add()
    $ws = $wb.Worksheets.Item(1)
    $ws.Range("A1").Value2 = "PoshComConformance"
    $ws.Cells.Item(2, 1).Value2 = 10
    $ws.Cells.Item(3, 1).Value2 = 32
    $ws.Range("A4").Formula = "=SUM(A2:A3)"
    $ws.Range("A1").Font.Bold = $true
    $wb.SaveAs($xlsx, 51)
    $wb.Close($false)
    $xl.Quit()
} finally {
    [void][System.Runtime.InteropServices.Marshal]::ReleaseComObject($xl)
}

$host_ = "PowerShell $($PSVersionTable.PSVersion)"
$r = Test-Ooxml $xlsx "xl/workbook.xml" "PoshComConformance"
if ($r -eq "ok") {
    Write-Host ("  PASS [{0}]: created {1} ({2} bytes)" -f $host_, $xlsx, (Get-Item $xlsx).Length)
    Write-Host "POWERSHELL EXCEL CONFORMANCE: PASS"
    exit 0
} else {
    Write-Host "  FAIL [$host_]: $r"
    Write-Host "POWERSHELL EXCEL CONFORMANCE: FAIL"
    exit 1
}
