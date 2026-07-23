<#
.SYNOPSIS
  Verify the installed Docxy Office COM shims on a machine with no Office: drive
  each shim over COM (late-bound, exactly as a host does) to create an .xlsx and a
  .docx, then validate the produced OOXML packages directly (no Office needed to
  open them). Prints PASS / FAIL per shim and a final summary.

.DESCRIPTION
  Late-bound `CreateObject` proves the whole chain: COM activates the shim
  (in-process, since InprocServer32 is preferred), the object graph runs, and a
  valid document is written. Early-bound (typed vtable) is exercised by the dev
  test suite; on the target the in-process path makes it work without a typelib.
#>
[CmdletBinding()]
param()
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.IO.Compression.FileSystem

$tmp = [System.IO.Path]::GetTempPath()
$xlsx = Join-Path $tmp "docxy-selftest.xlsx"
$docx = Join-Path $tmp "docxy-selftest.docx"
Remove-Item $xlsx, $docx -ErrorAction SilentlyContinue

# --- the two late-bound smoke scripts (cscript is the canonical IDispatch host) ---
$xlVbs = @"
Set app = CreateObject("Excel.Application")
app.Visible = False
Set wb = app.Workbooks.Add()
Set ws = wb.Worksheets(1)
ws.Range("A1").Value = "DocxyShimSelfTest"
ws.Cells(2, 1).Value = 20
ws.Cells(3, 1).Value = 22.5
ws.Range("A4").Formula = "=SUM(A2:A3)"
wb.SaveAs "$($xlsx -replace '\\','\\')", 51
wb.Close False
app.Quit
"@
$wdVbs = @"
Set app = CreateObject("Word.Application")
app.Visible = False
Set doc = app.Documents.Add()
app.Selection.TypeText "DocxyShimSelfTest"
app.Selection.TypeParagraph
app.Selection.TypeText "Second line."
doc.SaveAs2 "$($docx -replace '\\','\\')"
doc.Close
app.Quit
"@

function Run-Vbs($script, $label) {
    $f = Join-Path $tmp ("docxy-selftest-" + $label + ".vbs")
    Set-Content -LiteralPath $f -Value $script -Encoding ASCII
    & cscript.exe '//nologo' $f 2>&1 | Out-Null
    Remove-Item $f -ErrorAction SilentlyContinue
}

# Validate an OOXML package: it's a real zip, contains `mustHave`, and its text
# content includes `needle`.
function Test-Ooxml($path, $mustHave, $needle) {
    if (-not (Test-Path $path)) { return "no file produced" }
    if ((Get-Item $path).Length -lt 200) { return "file too small" }
    try {
        $zip = [System.IO.Compression.ZipFile]::OpenRead($path)
        try {
            $names = $zip.Entries.Name + ($zip.Entries | ForEach-Object { $_.FullName })
            if (-not ($zip.Entries.FullName -contains $mustHave)) { return "missing part $mustHave" }
            $found = $false
            foreach ($e in $zip.Entries) {
                if ($e.FullName -match '\.xml$') {
                    $sr = New-Object System.IO.StreamReader($e.Open())
                    if ($sr.ReadToEnd() -match [regex]::Escape($needle)) { $found = $true }
                    $sr.Close()
                }
            }
            if (-not $found) { return "text '$needle' not found in package" }
        } finally { $zip.Dispose() }
    } catch { return "not a valid zip: $($_.Exception.Message)" }
    return "ok"
}

$pass = $true
Write-Host "== Excel shim =="
try { Run-Vbs $xlVbs "excel" } catch { Write-Host "  COM activation FAILED (run install.ps1 first?): $($_.Exception.Message)"; $pass = $false }
$r = Test-Ooxml $xlsx "xl/workbook.xml" "DocxyShimSelfTest"
if ($r -eq "ok") { Write-Host ("  PASS: created {0} ({1} bytes), valid .xlsx with content" -f $xlsx, (Get-Item $xlsx -ErrorAction SilentlyContinue).Length) }
else { Write-Host "  FAIL: $r"; $pass = $false }

Write-Host "== Word shim =="
try { Run-Vbs $wdVbs "word" } catch { Write-Host "  COM activation FAILED (run install.ps1 first?): $($_.Exception.Message)"; $pass = $false }
$r = Test-Ooxml $docx "word/document.xml" "DocxyShimSelfTest"
if ($r -eq "ok") { Write-Host ("  PASS: created {0} ({1} bytes), valid .docx with content" -f $docx, (Get-Item $docx -ErrorAction SilentlyContinue).Length) }
else { Write-Host "  FAIL: $r"; $pass = $false }

Write-Host ""
if ($pass) {
    Write-Host "SELF-TEST PASSED: both shims create valid Office documents over COM with no Office installed."
    exit 0
} else {
    Write-Host "SELF-TEST FAILED. See %TEMP%\xlcomshim.log / %TEMP%\wordcomshim.log for the dispatch trace."
    exit 1
}
