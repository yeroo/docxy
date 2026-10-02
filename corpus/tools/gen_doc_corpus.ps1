# Build the Word 97-2003 (.doc) corpus for docxcore's legacy reader (#634).
#
# Each fixture is typed into a new document by Word itself (COM), saved as a
# Word 97-2003 Document (`<stem>.doc`), closed, opened again from that `.doc`
# and saved as a Word Document (`<stem>.docx`). The `.docx` is therefore
# Word's own reading of the binary file: the oracle `docxcore/tests/legacy_doc.rs`
# compares `import_doc(<stem>.doc)` against.
#
# Needs Microsoft Word on Windows. Run from the repo root:
#   pwsh -File corpus/tools/gen_doc_corpus.ps1
param([string]$Out = "corpus/legacy/word")

$ErrorActionPreference = "Stop"

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

$Out = (New-Item -ItemType Directory -Force $Out).FullName

# Word constants.
$wdFormatDocument97 = 0
$wdFormatXMLDocument = 12
$wdLineBreak = 6
$wdPageBreak = 7
$wdFieldDate = 31
$wdFieldPage = 33
$wdStyleNormal = -1
$wdStyleHeading = @{ 1 = -2; 2 = -3; 3 = -4 }
$wdAlign = @{ left = 0; center = 1; right = 2; justify = 3 }
$wdUnderlineSingle = 1
$wdColorRed = 255

function Assert-Magic([string]$path, [byte[]]$magic) {
    $head = [IO.File]::ReadAllBytes($path)[0..($magic.Length - 1)]
    if (Compare-Object $head $magic) {
        throw "$path does not start with the expected bytes: is Word.Application really Word?"
    }
}

function Para($sel, [string]$text) {
    $sel.TypeText($text)
    $sel.TypeParagraph()
}

# Type `text` with the font set by `apply`, then put the font back.
function Styled($sel, [string]$text, [scriptblock]$apply) {
    $sel.Font.Reset()
    & $apply $sel.Font
    $sel.TypeText($text)
    $sel.Font.Reset()
}

$recipes = [ordered]@{
    # ASCII only: Word stores the text as compressed (8-bit) pieces.
    "plain"          = {
        param($doc, $sel)
        Para $sel "The first paragraph of a plain document."
        Para $sel "A second paragraph, with some punctuation: commas, (parens) and 100%."
        $sel.TypeText("The last paragraph.")
    }
    # cp1252 characters outside ASCII, which a compressed piece stores through
    # MS-DOC's exception table (0x80-0x9F) or as Latin-1.
    "cp1252"         = {
        param($doc, $sel)
        Para $sel ("Caf" + [char]0xE9 + " " + [char]0x2014 + " " + [char]0x201C + "quoted" + [char]0x201D + " costs " + [char]0x20AC + "5" + [char]0x2026)
        $sel.TypeText("It" + [char]0x2019 + "s " + [char]0x2018 + "single" + [char]0x2019 + ", na" + [char]0xEF + "ve, " + [char]0x2122 + " and " + [char]0xA9 + ".")
    }
    # Text outside cp1252: Word stores it as UTF-16 pieces.
    "unicode"        = {
        param($doc, $sel)
        Para $sel ([string][char]0x041F + [char]0x0440 + [char]0x0438 + [char]0x0432 + [char]0x0435 + [char]0x0442 + ", " + [char]0x043C + [char]0x0438 + [char]0x0440 + "!")
        Para $sel ("Mixed ASCII and " + [char]0x4E2D + [char]0x6587 + " text.")
        $sel.TypeText("An emoji: " + [char]::ConvertFromUtf32(0x1F600) + " and Greek " + [char]0x03B1 + [char]0x03B2 + [char]0x03B3 + ".")
    }
    # Direct character formatting, one property per run.
    "formatting"     = {
        param($doc, $sel)
        $sel.TypeText("Plain, ")
        Styled $sel "bold" { param($f) $f.Bold = 1 }
        $sel.TypeText(", ")
        Styled $sel "italic" { param($f) $f.Italic = 1 }
        $sel.TypeText(", ")
        Styled $sel "underlined" { param($f) $f.Underline = $wdUnderlineSingle }
        $sel.TypeText(", ")
        Styled $sel "struck" { param($f) $f.StrikeThrough = 1 }
        $sel.TypeParagraph()
        $sel.TypeText("Sizes: ")
        Styled $sel "sixteen point" { param($f) $f.Size = 16 }
        $sel.TypeText(", ")
        Styled $sel "eight point" { param($f) $f.Size = 8 }
        $sel.TypeParagraph()
        $sel.TypeText("Fonts and colours: ")
        Styled $sel "Courier New" { param($f) $f.Name = "Courier New" }
        $sel.TypeText(", ")
        Styled $sel "red" { param($f) $f.Color = $wdColorRed }
        $sel.TypeText(", ")
        Styled $sel "bold italic" { param($f) $f.Bold = 1; $f.Italic = 1 }
        $sel.TypeText(".")
    }
    # Paragraph alignment and the built-in heading styles.
    "align-headings" = {
        param($doc, $sel)
        foreach ($level in 1, 2, 3) {
            $sel.Style = $doc.Styles.Item($wdStyleHeading[$level])
            Para $sel "Heading level $level"
            $sel.Style = $doc.Styles.Item($wdStyleNormal)
        }
        foreach ($a in "left", "center", "right", "justify") {
            $sel.ParagraphFormat.Alignment = $wdAlign[$a]
            Para $sel "This paragraph is aligned $a, and long enough to wrap onto a second line when justified across the page width."
        }
        $sel.ParagraphFormat.Alignment = $wdAlign.left
        $sel.TypeText("Back to the left.")
    }
    # A 3x3 table with a paragraph before and after it.
    "table"          = {
        param($doc, $sel)
        Para $sel "Before the table."
        $table = $doc.Tables.Add($sel.Range, 3, 3)
        foreach ($r in 1..3) {
            foreach ($c in 1..3) {
                $table.Cell($r, $c).Range.Text = "r${r}c${c}"
            }
        }
        $sel.EndKey(6) | Out-Null # wdStory
        $sel.TypeText("After the table.")
    }
    # Fields (their results stay), a line break, a tab and a page break.
    "fields-breaks"  = {
        param($doc, $sel)
        $sel.TypeText("Page ")
        $doc.Fields.Add($sel.Range, $wdFieldPage) | Out-Null
        $sel.EndKey(6) | Out-Null
        $sel.TypeText(" of the document.")
        $sel.TypeParagraph()
        $sel.TypeText("Line one")
        $sel.InsertBreak($wdLineBreak)
        $sel.TypeText("line two`tafter a tab")
        $sel.TypeParagraph()
        $sel.TypeText("Before the page break")
        $sel.InsertBreak($wdPageBreak)
        $sel.TypeText("On the second page; dated ")
        $doc.Fields.Add($sel.Range, $wdFieldDate, '\@ "yyyy"') | Out-Null
        $sel.EndKey(6) | Out-Null
        $sel.TypeText(".")
    }
}

# The script ends with Quit, so it must never be handed a Word someone is using.
if (Get-Process WINWORD -ErrorAction SilentlyContinue) {
    throw "Microsoft Word is running; close it first (this script starts and quits its own Word)"
}
# docxy's own wordcomshim may be registered per user over Word.Application
# (HKCU\Software\Classes: Word.Application, Word.Application.16 and
# CLSID\{000209FF-0000-0000-C000-000000000046}); unregister it for the run.
# A COM launch starts Word hidden (/Automation -Embedding): no window appears.
$word = New-Object -ComObject Word.Application
$path = $word.Path
if (-not ($path -is [string]) -or -not (Test-Path (Join-Path $path "WINWORD.EXE"))) {
    try { $word.Quit(0) } catch {}
    throw "Word.Application is not Microsoft Word (Path: $path): unregister docxy's wordcomshim for the run"
}
Write-Host "Word $($word.Build) at $path"
$word.Visible = $false
$word.DisplayAlerts = 0
$names = @($word.UserName)
try {
    foreach ($stem in $recipes.Keys) {
        $doc = $word.Documents.Add()
        & $recipes[$stem] $doc $word.Selection
        $docPath = Join-Path $Out "$stem.doc"
        $docxPath = Join-Path $Out "$stem.docx"
        $doc.SaveAs2($docPath, $wdFormatDocument97)
        $doc.Close(0)
        # The oracle is Word's reading of the binary file, not the document
        # it was typed into.
        $doc = $word.Documents.Open($docPath)
        $doc.SaveAs2($docxPath, $wdFormatXMLDocument)
        # DocumentProperties is late-bound only: PowerShell needs InvokeMember.
        $props = $doc.BuiltInDocumentProperties
        foreach ($name in 'Author', 'Last Author') {
            $prop = [System.__ComObject].InvokeMember('Item', 'GetProperty', $null, $props, @($name))
            $names += [string][System.__ComObject].InvokeMember('Value', 'GetProperty', $null, $prop, $null)
        }
        $doc.Close(0)
        # A COM server that is not Word (an Office shim registered over
        # Word.Application) may accept the format and write something else.
        Assert-Magic $docPath ([byte[]](0xD0, 0xCF, 0x11, 0xE0))
        Assert-Magic $docxPath ([byte[]](0x50, 0x4B))
        Write-Host "wrote $stem.doc, $stem.docx"
    }
}
finally {
    $word.Quit(0)
    [void][Runtime.InteropServices.Marshal]::ReleaseComObject($word)
}
Remove-UserNames $Out ($names | Where-Object { $_ -ne 'docxy' })
