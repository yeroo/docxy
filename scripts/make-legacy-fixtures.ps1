# Regenerate corpus/legacy/*.xls, *.xlsb and *.ods from corpus/xlsx/*.xlsx with
# the installed Microsoft Excel (#603). These are the oracle for gridcore's
# legacy readers: the same workbooks as corpus/xlsx, written by Excel itself.
#
# Run from the repo root in Windows PowerShell:  powershell -File scripts/make-legacy-fixtures.ps1
#
# The ProgID Excel.Application may be registered to xlcomshim on a dev box, so
# this starts EXCEL.EXE itself and binds to its running class object with
# CLSCTX_LOCAL_SERVER. One Excel process per source file: a batch in one
# process stopped silently after a few files.
param([string]$Excel = "C:\Program Files\Microsoft Office\Root\Office16\EXCEL.EXE")
$ErrorActionPreference = 'Stop'
Add-Type -TypeDefinition @"
using System; using System.Runtime.InteropServices;
public static class RealCom {
  [DllImport("ole32.dll")] static extern int CoCreateInstance(ref Guid clsid, IntPtr outer, uint ctx, ref Guid iid, [MarshalAs(UnmanagedType.IUnknown)] out object o);
  public static object Local(string clsid) { Guid c = new Guid(clsid); Guid i = new Guid("00020400-0000-0000-C000-000000000046"); object o; int hr = CoCreateInstance(ref c, IntPtr.Zero, 4, ref i, out o); if (hr != 0) throw new Exception("hr=" + hr.ToString("X")); return o; }
}
"@
$out = Join-Path $PSScriptRoot "..\corpus\legacy"
New-Item -ItemType Directory -Force $out | Out-Null
foreach ($src in Get-ChildItem (Join-Path $PSScriptRoot "..\corpus\xlsx\*.xlsx")) {
  $p = Start-Process $Excel -ArgumentList "/automation","-Embedding" -PassThru
  Start-Sleep -Seconds 8
  $xl = [RealCom]::Local("00024500-0000-0000-C000-000000000046")
  $xl.DisplayAlerts = $false
  try {
    foreach ($f in @(@{e='xls';n=56}, @{e='xlsb';n=50}, @{e='ods';n=60})) {
      $wb = $xl.Workbooks.Open($src.FullName)
      $wb.SaveAs((Join-Path (Resolve-Path $out) "$($src.BaseName).$($f.e)"), $f.n)
      $wb.Close($false)
    }
  } finally {
    $xl.Quit(); [Runtime.InteropServices.Marshal]::ReleaseComObject($xl) | Out-Null
    Start-Sleep 2; if (!$p.HasExited) { $p.Kill() }
  }
}

# corpus/legacy/extra: workbooks built by Excel itself, so the .xlsx source is
# Excel's too. chart-embedded: a sheet with formulas and an embedded column
# chart, whose .xls carries the chart as a BOF..EOF substream inside the
# worksheet's.
$extra = Join-Path $PSScriptRoot "..\corpus\legacy\extra"
New-Item -ItemType Directory -Force $extra | Out-Null
$extra = (Resolve-Path $extra).Path
$p = Start-Process $Excel -ArgumentList "/automation","-Embedding" -PassThru
Start-Sleep -Seconds 8
$xl = [RealCom]::Local("00024500-0000-0000-C000-000000000046")
$xl.DisplayAlerts = $false
try {
  $wb = $xl.Workbooks.Add()
  $ws = $wb.Worksheets.Item(1)
  $ws.Name = "Data"
  $ws.Range("A1").Value2 = "Month"; $ws.Range("B1").Value2 = "Sales"; $ws.Range("C1").Value2 = "Cost"
  for ($i = 2; $i -le 7; $i++) { $ws.Cells.Item($i,1).Value2 = "M$($i-1)"; $ws.Cells.Item($i,2).Value2 = 100*$i + 7; $ws.Cells.Item($i,3).Formula = "=B$i*0.6" }
  $ws.Range("E1").Value2 = "Total"; $ws.Range("E2").Formula = "=SUM(B2:B7)"
  $co = $ws.ChartObjects().Add(300, 20, 360, 220)
  $co.Chart.ChartType = 51
  $co.Chart.SetSourceData($ws.Range("A1:C7"))
  $wb.SaveAs((Join-Path $extra "chart-embedded.xlsx"), 51)
  foreach ($f in @(@{e='xls';n=56}, @{e='xlsb';n=50}, @{e='ods';n=60})) { $wb.SaveAs((Join-Path $extra "chart-embedded.$($f.e)"), $f.n) }
  $wb.Close($false)
} finally {
  $xl.Quit(); [Runtime.InteropServices.Marshal]::ReleaseComObject($xl) | Out-Null
  Start-Sleep 2; if (!$p.HasExited) { $p.Kill() }
}
