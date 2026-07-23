<#
.SYNOPSIS
  PowerShell COM conformance test for the Word shim (wordcomshim).

.DESCRIPTION
  `New-Object -ComObject Word.Application` drives the shim through the CLR's
  reflection-based IDispatch late binding — independent of VBScript and pywin32,
  and runs under both Windows PowerShell 5.1 and PowerShell 7+. Builds a document
  with formatting and validates the produced .docx directly (no Word needed).

  Run (shim installed + typelib registered):  powershell -File com_conformance.ps1
  Exit 0 = PASS.
#>
[CmdletBinding()]
param()
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.IO.Compression.FileSystem

$docx = Join-Path $env:TEMP "posh-wd-conformance.docx"
Remove-Item $docx -ErrorAction SilentlyContinue

$wd = New-Object -ComObject Word.Application
try {
    $wd.Visible = $false
    $doc = $wd.Documents.Add()
    $sel = $wd.Selection
    $sel.Font.Bold = $true
    $sel.TypeText("PoshWordTitle")
    $sel.TypeParagraph()
    $sel.Font.Bold = $false
    $sel.Font.Size = 14
    $sel.TypeText("PoshWordBody")
    $sel.TypeParagraph()
    $sel.ParagraphFormat.Alignment = 1   # center (set before the paragraph mark)
    $sel.TypeText("PoshWordCentered")
    $sel.TypeParagraph()
    $doc.SaveAs2($docx)
    $doc.Close()
    $wd.Quit()
} finally {
    [void][System.Runtime.InteropServices.Marshal]::ReleaseComObject($wd)
}

$host_ = "PowerShell $($PSVersionTable.PSVersion)"
$fail = @()
if (-not (Test-Path $docx)) { $fail += "no file produced" }
else {
    $z = [System.IO.Compression.ZipFile]::OpenRead($docx)
    try {
        $names = $z.Entries.FullName
        if ($names -notcontains 'word/document.xml') { $fail += "missing word/document.xml" }
        if ($names -contains 'docProps/app.xml') { $fail += "docProps/app.xml present -> real Word served" }
        $sr = New-Object System.IO.StreamReader(($z.Entries | Where-Object { $_.FullName -eq 'word/document.xml' }).Open())
        $x = $sr.ReadToEnd(); $sr.Close()
        foreach ($c in @(
                @('PoshWordTitle', 'PoshWordTitle'),
                @('PoshWordBody', 'PoshWordBody'),
                @('PoshWordCentered', 'PoshWordCentered'),
                @('bold run', '<w:b'),
                @('font size', '<w:sz'),
                @('center align', 'w:val="center"'))) {
            if ($x -notmatch [regex]::Escape($c[1])) { $fail += "missing $($c[0])" }
        }
    } finally { $z.Dispose() }
}

if ($fail.Count -eq 0) {
    Write-Host ("  PASS [{0}]: created {1} ({2} bytes)" -f $host_, $docx, (Get-Item $docx).Length)
    Write-Host "POWERSHELL WORD CONFORMANCE: PASS"
    exit 0
} else {
    Write-Host ("  FAIL [{0}]: {1}" -f $host_, ($fail -join ', '))
    Write-Host "POWERSHELL WORD CONFORMANCE: FAIL"
    exit 1
}
