<#
.SYNOPSIS
Regenerate corpus/word-import/: one document Word saves as .docx, .rtf, filtered
Web Page (.htm) and .pdf, the fixtures of docxcore's importers (#633).

.DESCRIPTION
Needs Microsoft Word (COM). The .docx Word writes is the oracle the tests
compare the other three against, so all four must come from one run. Tests
never need Word: the files are committed.

The document has headings, bold/italic/underline runs, accented Latin,
Cyrillic and a euro sign, a tab, a manual line break, a bulleted and a
numbered list, and a 2x3 table. The author fields say docxy; the
Web Page is written as windows-1252, Word's default for Western text, so the
importer's code-page path and its &#NNNN; entities are both exercised.
#>
param(
    [string]$Out = (Join-Path $PSScriptRoot '..\word-import'),
    # Show Word while it works, to see a dialog that stops a save.
    [switch]$Visible
)
$ErrorActionPreference = 'Stop'

# Word stamps the signed-in Office account's display name into every file it
# saves (docProps Author / Last Modified By, the RTF \operator, the .doc's
# summary information and saved-by list), whatever Application.UserName says.
# Keep that out of the corpus: after Word quits, overwrite each name Word
# reported with "docxy" padded to the same length, so every offset and length
# in the binary formats stays valid, and rewrite the .docx metadata parts.
function Remove-UserNames([string]$dir, [string[]]$names) {
    Add-Type -AssemblyName System.IO.Compression, System.IO.Compression.FileSystem
    $latin = [Text.Encoding]::GetEncoding(1252)
    foreach ($name in $names | Where-Object { $_ -and $_.Length -ge 2 } | Sort-Object -Unique) {
        $mask = ('docxy' + (' ' * $name.Length)).Substring(0, $name.Length)
        foreach ($f in Get-ChildItem $dir -File) {
            if ($f.Extension -eq '.docx') {
                $zip = [IO.Compression.ZipFile]::Open($f.FullName, 'Update')
                try {
                    foreach ($e in @($zip.Entries)) {
                        if ($e.FullName -notmatch '\.(xml|rels)$') { continue }
                        $r = New-Object IO.StreamReader($e.Open()); $text = $r.ReadToEnd(); $r.Close()
                        if (-not $text.Contains($name)) { continue }
                        $text = $text.Replace($name, 'docxy')
                        $e.Delete()
                        $w = New-Object IO.StreamWriter($zip.CreateEntry($e.FullName).Open(), (New-Object Text.UTF8Encoding($false)))
                        $w.Write($text); $w.Close()
                    }
                } finally { $zip.Dispose() }
                continue
            }
            $bytes = [IO.File]::ReadAllBytes($f.FullName)
            $hex = [BitConverter]::ToString($bytes)
            $changed = $false
            foreach ($enc in $latin, [Text.Encoding]::Unicode, [Text.Encoding]::BigEndianUnicode) {
                $from = [BitConverter]::ToString($enc.GetBytes($name))
                if ($hex.Contains($from)) { $hex = $hex.Replace($from, [BitConverter]::ToString($enc.GetBytes($mask))); $changed = $true }
            }
            if ($changed) {
                [IO.File]::WriteAllBytes($f.FullName, [byte[]]($hex.Split('-') | ForEach-Object { [Convert]::ToByte($_, 16) }))
            }
        }
    }
}

$Out = [IO.Path]::GetFullPath($Out)
New-Item -ItemType Directory -Force $Out | Out-Null

# WdBuiltinStyle / WdBreakType / WdSaveFormat / WdExportFormat values.
$Normal = -1; $Heading1 = -2; $Heading2 = -3
$LineBreak = 6
$FormatDocx = 16; $FormatRtf = 6; $FormatFilteredHtml = 10; $ExportPdf = 17

# Real Word, never a stand-in: docxy's own wordcomshim may be registered per
# user for Word.Application (and Word's CLSID), and it ignores the save format,
# so it would write the .docx three times and no PDF. Unregister the shim
# (HKCU\Software\Classes: Word.Application, Word.Application.16 and
# CLSID\{000209FF-0000-0000-C000-000000000046}) for the run, or this script
# refuses below when the server it got is not WINWORD.EXE.
# The script ends with Quit, so it must never be handed a Word someone is
# using: a running Word may answer the connection below.
if (Get-Process WINWORD -ErrorAction SilentlyContinue) {
    throw 'Microsoft Word is running; close it first (this script starts and quits its own Word)'
}
# A COM launch starts Word hidden (/Automation -Embedding): no window appears.
$word = New-Object -ComObject Word.Application
$path = $word.Path
if (-not ($path -is [string]) -or -not (Test-Path (Join-Path $path 'WINWORD.EXE'))) {
    try { $word.Quit(0) } catch {}
    throw "Word.Application is not Microsoft Word (Path: $path): unregister docxy's wordcomshim for the run"
}
Write-Host "Word $($word.Build) at $($word.Path)"
$word.Visible = [bool]$Visible
$word.DisplayAlerts = 0
$names = @($word.UserName)
try {
    Write-Host 'building the document'
    $doc = $word.Documents.Add()
    $sel = $word.Selection

    function Style($id) { $sel.Style = $doc.Styles.Item([int]$id) }
    function Para($text) { $sel.TypeText($text); $sel.TypeParagraph() }
    function Styled($id, $text) { Style $id; Para $text }

    Styled $Heading1 'Import fixture'
    Styled $Heading2 'Formatting'
    Style $Normal
    $sel.TypeText('Plain text with ')
    $sel.Font.Bold = 1; $sel.TypeText('bold'); $sel.Font.Bold = 0
    $sel.TypeText(', ')
    $sel.Font.Italic = 1; $sel.TypeText('italic'); $sel.Font.Italic = 0
    $sel.TypeText(' and ')
    $sel.Font.Underline = 1; $sel.TypeText('underlined'); $sel.Font.Underline = 0
    Para ' words.'
    Para ("Caf" + [char]0xE9 + " na" + [char]0xEF + "ve r" + [char]0xE9 + "sum" + [char]0xE9 +
        " and " + [string]::new([char[]](0x41F, 0x440, 0x438, 0x432, 0x435, 0x442)) +
        " cost 20 " + [char]0x20AC + ".")
    Para "Name`tValue"
    $sel.TypeText('First line')
    $sel.InsertBreak($LineBreak)
    Para 'second line'

    Styled $Heading2 'Lists'
    Style $Normal
    $sel.Range.ListFormat.ApplyBulletDefault()
    Para 'Apples'
    Para 'Bananas'
    Para 'Cherries'
    $sel.Range.ListFormat.RemoveNumbers()
    $sel.Range.ListFormat.ApplyNumberDefault()
    Para 'First step'
    Para 'Second step'
    $sel.Range.ListFormat.RemoveNumbers()

    Styled $Heading2 'Table'
    Style $Normal
    # Typed as tab-separated rows, then converted to a table.
    $start = $sel.Start
    Para "North`tSouth`tEast"
    $sel.TypeText("10`t20`t30")
    $rows = $doc.Range($start, $sel.End)
    [void]$rows.ConvertToTable(1, 2, 3)  # wdSeparateByTabs
    $sel.EndKey(6) | Out-Null  # wdStory: after the table
    $sel.TypeText('The end.')

    # No author in any of the four files. (RemovePersonalInformation would
    # do it, but its save-time warning is a modal that hangs a hidden Word.)
    $props = $doc.BuiltInDocumentProperties
    foreach ($name in 'Author', 'Last Author', 'Company', 'Manager') {
        $prop = [System.__ComObject].InvokeMember('Item', 'GetProperty', $null, $props, @($name))
        [void][System.__ComObject].InvokeMember('Value', 'SetProperty', $null, $prop, @('docxy'))
    }
    $doc.WebOptions.Encoding = 1252
    Write-Host 'saving source.docx'
    $doc.SaveAs2((Join-Path $Out 'source.docx'), $FormatDocx)
    foreach ($name in 'Author', 'Last Author') {
        $prop = [System.__ComObject].InvokeMember('Item', 'GetProperty', $null, $props, @($name))
        $names += [string][System.__ComObject].InvokeMember('Value', 'GetProperty', $null, $prop, $null)
    }
    # Export the PDF before the RTF and Web Page saves switch the document's format and view.
    Write-Host 'exporting source.pdf'
    $doc.ExportAsFixedFormat((Join-Path $Out 'source.pdf'), $ExportPdf)
    Write-Host 'saving source.rtf'
    $doc.SaveAs2((Join-Path $Out 'source.rtf'), $FormatRtf)
    Write-Host 'saving source.htm'
    $doc.SaveAs2((Join-Path $Out 'source.htm'), $FormatFilteredHtml)
    $doc.Close(0)
}
finally {
    $word.Quit(0)
    [void][Runtime.InteropServices.Marshal]::ReleaseComObject($word)
}
Remove-UserNames $Out ($names | Where-Object { $_ -ne 'docxy' })
Get-ChildItem $Out | Select-Object Name, Length
