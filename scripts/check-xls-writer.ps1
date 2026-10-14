# Check that Microsoft Excel opens what gridcore's .xls writer (#874) writes,
# and computes the same values from it. For each .xls in corpus/legacy (and
# its extra/ and addin/ folders), and each .xlsx in corpus/xlsx (a Save As
# Excel 97-2003 Workbook from a workbook), and a synthetic .xlsx of what the
# corpus lacks (dynamic arrays, a text legacy array, colours), this:
#
#   1. reads it with gridcore and writes it as .xls (the xls_roundtrip
#      example, which also runs the writer's structural check);
#   2. opens the original and the rewrite in Excel, side by side, with
#      alerts off: an open that throws (a file Excel would have to repair)
#      fails the file;
#   3. compares the sheets, and which cells hold a formula;
#   4. recalculates both (CalculateFullRebuild) and compares every used
#      cell's value, so a token Excel evaluates differently from the
#      original (a wrong operand class) shows up. The cached values as
#      saved aren't compared: Excel recalculates a file it didn't write
#      when it opens it.
#
# $Allow lists the differences that come from the import, not the writer,
# each with its reason.
#
# Run from the repo root in Windows PowerShell, before merging a change to
# the writer (it needs Excel, so CI doesn't run it):
#
#   powershell -File scripts/check-xls-writer.ps1
#
# It prints one line per file and exits 1 when any fails. As in
# make-legacy-fixtures.ps1, the ProgID Excel.Application may be registered to
# xlcomshim on a dev box, so this starts EXCEL.EXE itself (one process per
# file) and binds to its running class object with CLSCTX_LOCAL_SERVER; every
# Excel it starts is quit, released and, if still running, killed.
param(
  [string]$Excel = "C:\Program Files\Microsoft Office\Root\Office16\EXCEL.EXE",
  [string]$Out = (Join-Path ([IO.Path]::GetTempPath()) "xls-writer-check")
)
$ErrorActionPreference = 'Stop'
Add-Type -TypeDefinition @"
using System; using System.Runtime.InteropServices;
public static class RealCom {
  [DllImport("ole32.dll")] static extern int CoCreateInstance(ref Guid clsid, IntPtr outer, uint ctx, ref Guid iid, [MarshalAs(UnmanagedType.IUnknown)] out object o);
  public static object Local(string clsid) { Guid c = new Guid(clsid); Guid i = new Guid("00020400-0000-0000-C000-000000000046"); object o; int hr = CoCreateInstance(ref c, IntPtr.Zero, 4, ref i, out o); if (hr != 0) throw new Exception("hr=" + hr.ToString("X")); return o; }
}
"@
$Allow = @{
  # Excel read the source's unquoted SUM(Q1:Q3!A1:A1) with Q1 as a cell
  # (corpus/legacy/README.md), and the original's tokens say so: recalculated,
  # they give #VALUE!. The import reads them as the 3-D sum the source means,
  # which the rewrite stores, so Excel computes the sums there.
  'calc-3d.xls' = @('Total!1,1', 'Total!2,1', 'Total!3,1', 'Total!4,1', 'Total!5,1', 'Total!6,1')
  # The .xlsx itself, read by Excel the same way.
  'calc-3d.xlsx' = @('Total!1,1', 'Total!2,1', 'Total!3,1', 'Total!4,1', 'Total!5,1', 'Total!6,1')
  # The import doesn't read another workbook's names (corpus/legacy/README.md),
  # so `SUM([1]!Prices)` and `[1]!Half*2` arrive as their values.
  'ext-name.xls' = @('Data!1,1', 'Data!2,1')
}
$root = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
New-Item -ItemType Directory -Force $Out | Out-Null
$Out = (Resolve-Path $Out).Path

Push-Location $root
try {
  cargo build --quiet -p gridcore --example xls_roundtrip
  if ($LASTEXITCODE -ne 0) { throw "building the xls_roundtrip example failed" }
} finally { Pop-Location }
$tool = Join-Path $root "target\debug\examples\xls_roundtrip.exe"

# Every used cell of $wb as "sheet!row,col" -> its value, or whether it
# holds a formula ($formulas).
function Get-Cells($wb, [bool]$formulas) {
  $cells = @{}
  foreach ($ws in $wb.Worksheets) {
    $used = $ws.UsedRange
    $r0 = $used.Row; $c0 = $used.Column
    for ($r = 0; $r -lt $used.Rows.Count; $r++) {
      for ($c = 0; $c -lt $used.Columns.Count; $c++) {
        $cell = $ws.Cells.Item($r0 + $r, $c0 + $c)
        $v = $cell.Value2
        # A dynamic array's spilled cells hold no formula of their own; the
        # .xls holds the same array as a legacy one, every cell a formula.
        $spilled = $false
        try { $spilled = [bool]$cell.HasSpill } catch {}
        $hasFormula = $cell.HasFormula -or $spilled
        if ($null -eq $v -and -not $hasFormula) { continue }
        if ($v -is [double]) { $v = $v.ToString("R") }
        $cells["$($ws.Name)!$($r0 + $r),$($c0 + $c)"] = if ($formulas) { "$hasFormula" } else { "$v" }
      }
    }
  }
  $cells
}

function Compare-Cells($a, $b, $allowed) {
  $diffs = @()
  foreach ($k in $a.Keys) {
    if ($b[$k] -ne $a[$k] -and $k -notin $allowed) { $diffs += "${k}: $($a[$k]) -> $($b[$k])" }
  }
  foreach ($k in $b.Keys) {
    if (-not $a.ContainsKey($k) -and $k -notin $allowed) { $diffs += "${k}: (none) -> $($b[$k])" }
  }
  $diffs
}

# What the corpus lacks, built by the example: dynamic arrays (a SEQUENCE
# spill, SORT, FILTER), a text-valued legacy array, a custom colour and a
# bordered cell.
$synthetic = Join-Path $Out "synthetic.xlsx"
& $tool --synthetic $synthetic
if ($LASTEXITCODE -ne 0) { throw "writing $synthetic failed" }
$files = @(Get-ChildItem (Join-Path $root "corpus\legacy\*.xls")) +
  @(Get-ChildItem (Join-Path $root "corpus\legacy\extra\*.xls")) +
  @(Get-ChildItem (Join-Path $root "corpus\legacy\addin\*.xls")) +
  @(Get-ChildItem (Join-Path $root "corpus\xlsx\*.xlsx")) +
  @(Get-Item $synthetic)
$failed = 0
foreach ($src in $files) {
  $dst = Join-Path $Out "$($src.BaseName)-$($src.Extension.TrimStart('.'))-rewritten.xls"
  Remove-Item $dst -ErrorAction SilentlyContinue
  $lost = & $tool $src.FullName $dst 2>&1
  if ($LASTEXITCODE -ne 0) {
    Write-Output "FAIL $($src.Name): the writer: $lost"
    $failed++
    continue
  }
  $p = Start-Process $Excel -ArgumentList "/automation", "-Embedding" -PassThru
  $xl = $null
  $problems = @()
  try {
    for ($i = 0; $i -lt 60 -and $null -eq $xl; $i++) {
      Start-Sleep -Milliseconds 500
      try { $xl = [RealCom]::Local("00024500-0000-0000-C000-000000000046") } catch {}
    }
    if ($null -eq $xl) { throw "Excel did not start" }
    $xl.DisplayAlerts = $false
    $xl.AskToUpdateLinks = $false
    # Open(FileName, UpdateLinks = never, ReadOnly); CorruptLoad stays
    # normal, so a file Excel would repair throws instead.
    $a = $xl.Workbooks.Open($src.FullName, 0, $true)
    try {
      $b = $xl.Workbooks.Open($dst, 0, $true)
    } catch {
      $problems += "Excel did not open the rewrite: $($_.Exception.Message)"
    }
    if ($problems.Count -eq 0) {
      if ($b.FileFormat -ne 56) { $problems += "opened as file format $($b.FileFormat), not 56 (xls)" }
      $names = { param($w) ($w.Worksheets | ForEach-Object { $_.Name }) -join ',' }
      if ((& $names $a) -ne (& $names $b)) { $problems += "sheets $(& $names $a) -> $(& $names $b)" }
      $allowed = @($Allow[$src.Name])
      $problems += @(Compare-Cells (Get-Cells $a $true) (Get-Cells $b $true) $allowed | ForEach-Object { "formula: $_" })
      $xl.CalculateFullRebuild()
      $problems += @(Compare-Cells (Get-Cells $a $false) (Get-Cells $b $false) $allowed | ForEach-Object { "value: $_" })
    }
    foreach ($w in @($xl.Workbooks)) { $w.Close($false) }
  } catch {
    $problems += "Excel: $($_.Exception.Message)"
  } finally {
    if ($null -ne $xl) {
      try { $xl.Quit() } catch {}
      [Runtime.InteropServices.Marshal]::ReleaseComObject($xl) | Out-Null
    }
    $xl = $null
    [GC]::Collect(); [GC]::WaitForPendingFinalizers()
    if (-not $p.WaitForExit(10000)) { $p.Kill() }
  }
  if ($problems.Count -eq 0) {
    $note = if ($lost) { " ($($lost -join '; '))" } else { "" }
    Write-Output "ok   $($src.Name)$note"
  } else {
    $failed++
    Write-Output "FAIL $($src.Name)"
    $problems | Select-Object -First 20 | ForEach-Object { Write-Output "       $_" }
  }
}
Write-Output "$($files.Count - $failed) of $($files.Count) files passed"
if ($failed -gt 0) { exit 1 }
