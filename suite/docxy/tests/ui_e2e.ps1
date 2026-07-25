<#
.SYNOPSIS
  End-to-end UI integration tests for the docxy desktop app.

  gpui's headless test harness (TestAppContext) can't be used here: compiling
  this crate under `--test` with gpui test-support triggers an unbounded macro
  expansion (the enormous builder-chain render fn). So instead these tests drive
  the REAL built app through simulated keyboard/mouse input and assert on the
  document it saves — a genuine end-to-end check of the UI event path.

  Windows only (needs a desktop session). Run:
      pwsh suite/docxy/tests/ui_e2e.ps1
  Exits 0 if all scenarios pass, 1 otherwise.
#>
param(
    [string]$Exe = "$PSScriptRoot\..\..\target\debug\suite.exe"
)
$ErrorActionPreference = "Stop"
Add-Type -AssemblyName System.Drawing
Add-Type -AssemblyName System.IO.Compression.FileSystem
Add-Type @"
using System; using System.Runtime.InteropServices;
public class U {
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out R r);
  [DllImport("user32.dll")] public static extern bool SetCursorPos(int x,int y);
  [DllImport("user32.dll")] public static extern void mouse_event(uint f,uint a,uint b,int c,IntPtr d);
  [DllImport("user32.dll")] public static extern void keybd_event(byte v,byte s,uint f,IntPtr e);
  [StructLayout(LayoutKind.Sequential)] public struct R { public int L,T,Rt,B; }
}
"@

$LDOWN = 0x02; $LUP = 0x04
$work = Join-Path $env:TEMP "docxy-e2e"
New-Item -ItemType Directory -Force $work | Out-Null
$cfg = Join-Path $env:APPDATA "docxy"
$Utf8NoBom = New-Object System.Text.UTF8Encoding $false

# A minimal, entirely PLAIN .docx (one "typing area" paragraph, a section, and an
# empty relationships part) so any marker in the saved file can only come from the
# action under test — never a false positive from pre-existing sample content.
function Build-Base($path) {
    $ct = '<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>'
    $rels = '<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>'
    $doc = '<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t xml:space="preserve">typing area</w:t></w:r></w:p><w:sectPr><w:pgSz w:w="12240" w:h="15840"/><w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440" w:header="720" w:footer="720" w:gutter="0"/></w:sectPr></w:body></w:document>'
    $drels = '<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"></Relationships>'
    Remove-Item $path -Force -ErrorAction SilentlyContinue
    $zip = [System.IO.Compression.ZipFile]::Open($path, "Create")
    function Add-Entry($zip, $name, $text) {
        $e = $zip.CreateEntry($name)
        $sw = New-Object System.IO.StreamWriter($e.Open(), (New-Object System.Text.UTF8Encoding $false))
        $sw.Write($text); $sw.Dispose()
    }
    Add-Entry $zip "[Content_Types].xml" $ct
    Add-Entry $zip "_rels/.rels" $rels
    Add-Entry $zip "word/document.xml" $doc
    Add-Entry $zip "word/_rels/document.xml.rels" $drels
    $zip.Dispose()
}

function Seed($docx) {
    Remove-Item (Join-Path $cfg "hot") -Recurse -Force -ErrorAction SilentlyContinue
    New-Item -ItemType Directory -Force $cfg | Out-Null
    $p = $docx.Replace('\', '/')
    $json = '{ "tabs": [ { "kind": "Docx", "title": "e2e.docx", "path": "' + $p + '", "dirty": false } ], "active": 0, "theme": "Auto" }'
    # UTF-8 WITHOUT BOM — a BOM makes serde reject the JSON and the app silently
    # falls back to its built-in sample (the bug that made these tests pass falsely).
    [System.IO.File]::WriteAllText((Join-Path $cfg "session.json"), $json, $Utf8NoBom)
}

function Launch {
    $p = Start-Process -FilePath $Exe -PassThru
    Start-Sleep -Seconds 4
    $p.Refresh()
    [U]::SetForegroundWindow($p.MainWindowHandle) | Out-Null
    Start-Sleep -Milliseconds 500
    return $p
}
function Rect($p) { $r = New-Object U+R; [U]::GetWindowRect($p.MainWindowHandle, [ref]$r) | Out-Null; $r }
function Click($p, $x, $y) {
    [U]::SetForegroundWindow($p.MainWindowHandle) | Out-Null
    $r = Rect $p; [U]::SetCursorPos($r.L + $x, $r.T + $y) | Out-Null
    Start-Sleep -Milliseconds 130
    [U]::mouse_event($LDOWN, 0, 0, 0, [IntPtr]::Zero); Start-Sleep -Milliseconds 45
    [U]::mouse_event($LUP, 0, 0, 0, [IntPtr]::Zero); Start-Sleep -Milliseconds 160
}
function Key($p, [byte]$vk) {
    [U]::SetForegroundWindow($p.MainWindowHandle) | Out-Null
    [U]::keybd_event($vk, 0, 0, [IntPtr]::Zero); Start-Sleep -Milliseconds 40
    [U]::keybd_event($vk, 0, 2, [IntPtr]::Zero); Start-Sleep -Milliseconds 75
}
function Chord($p, [byte]$mod, [byte]$vk) {
    [U]::SetForegroundWindow($p.MainWindowHandle) | Out-Null
    [U]::keybd_event($mod, 0, 0, [IntPtr]::Zero); Start-Sleep -Milliseconds 30
    Key $p $vk
    [U]::keybd_event($mod, 0, 2, [IntPtr]::Zero); Start-Sleep -Milliseconds 120
}
function Txt($p, $s) { foreach ($c in $s.ToCharArray()) { if ($c -eq ' ') { Key $p 0x20 } else { Key $p ([byte][char]([string]$c).ToUpper()[0]) } } }
function SaveClose($p) { Chord $p 0x11 0x53; Start-Sleep -Milliseconds 700; Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue; Start-Sleep -Milliseconds 300 }

# Read one part out of the saved .docx (a zip).
function Part($docx, $name) {
    $zip = [System.IO.Compression.ZipFile]::OpenRead($docx)
    try {
        $e = $zip.Entries | Where-Object { $_.FullName -eq $name } | Select-Object -First 1
        if (-not $e) { return "" }
        $sr = New-Object System.IO.StreamReader($e.Open())
        try { return $sr.ReadToEnd() } finally { $sr.Dispose() }
    } finally { $zip.Dispose() }
}

# ---- scenarios: each returns a list of "PASS/FAIL: message" strings ----------

$VK = @{ Ctrl = 0x11; F10 = 0x79; Esc = 0x1B; Home = 0x24; Tab = 0x09;
    A = 0x41; B = 0x42; G = 0x47; H = 0x48; I = 0x49; M = 0x4D; N = 0x4E; S = 0x53; U = 0x55; W = 0x57; Y = 0x59 }

function Test-Tab($doc) {
    $p = Launch
    Click $p 200 343; Key $p $VK.Home
    Key $p $VK.Tab; Key $p $VK.Tab
    SaveClose $p
    $n = ([regex]::Matches((Part $doc "word/document.xml"), "<w:tab/>")).Count
    if ($n -ge 2) { "PASS: Tab key inserts <w:tab/> (found $n)" } else { "FAIL: expected >=2 <w:tab/>, got $n" }
}
function Test-Bold($doc) {
    $p = Launch
    Chord $p $VK.Ctrl $VK.A          # select all
    Chord $p $VK.Ctrl $VK.B          # bold
    SaveClose $p
    $xml = Part $doc "word/document.xml"
    if ($xml -match "<w:b/>") { "PASS: Ctrl+B writes <w:b/>" } else { "FAIL: no <w:b/> after Ctrl+B" }
}
function Test-Italic($doc) {
    $p = Launch
    Chord $p $VK.Ctrl $VK.A
    Chord $p $VK.Ctrl $VK.I
    SaveClose $p
    $xml = Part $doc "word/document.xml"
    if ($xml -match "<w:i/>") { "PASS: Ctrl+I writes <w:i/>" } else { "FAIL: no <w:i/> after Ctrl+I" }
}
function Test-Header($doc) {
    $p = Launch
    Click $p 300 400
    Key $p $VK.F10; Key $p $VK.N; Key $p $VK.H   # Insert -> Edit Header
    Txt $p "hdrmark"
    Key $p $VK.Esc
    SaveClose $p
    $h = Part $doc "word/header1.xml"
    if ($h -match "hdrmark") { "PASS: header created + text saved (header1.xml)" } else { "FAIL: header text not saved: '$h'" }
}
function Test-FirstPage($doc) {
    $p = Launch
    Click $p 300 400
    Key $p $VK.F10; Key $p $VK.N; Key $p $VK.H   # edit header (opens contextual bar)
    Click $p 347 180                              # "Different First Page" checkbox
    Key $p $VK.Esc
    SaveClose $p
    $xml = Part $doc "word/document.xml"
    if ($xml -match "<w:titlePg/>") { "PASS: Different First Page writes <w:titlePg/>" } else { "FAIL: no <w:titlePg/>" }
}
function Test-LineSpacing($doc) {
    $p = Launch
    Click $p 200 343
    Key $p $VK.F10; Key $p $VK.H; Key $p $VK.Y   # Home -> Line spacing (opens the menu)
    Click $p 207 179                              # click the "1.5" chip
    SaveClose $p
    $xml = Part $doc "word/document.xml"
    if ($xml -match 'w:line="360"') { "PASS: line spacing menu writes w:line=360 (1.5x)" } else { "FAIL: no w:line=360" }
}
function Test-PageNumber($doc) {
    $p = Launch
    Click $p 200 343
    Key $p $VK.F10; Key $p $VK.N; Key $p $VK.G   # Insert -> Page Number (PAGE field)
    SaveClose $p
    $xml = Part $doc "word/document.xml"
    if ($xml -match 'w:instr="PAGE"') { "PASS: Page Number inserts a PAGE field" } else { "FAIL: no PAGE field" }
}
function Test-NoSpacing($doc) {
    $p = Launch
    Click $p 200 343                              # caret in the paragraph
    Click $p 706 107                              # the "No Spacing" style in the gallery (Home is active)
    SaveClose $p
    $xml = Part $doc "word/document.xml"
    if ($xml -match 'w:line="240"') { "PASS: No Spacing sets single spacing (w:line=240)" } else { "FAIL: No Spacing not applied" }
}
function Test-Heading($doc) {
    $p = Launch
    Click $p 200 343
    Click $p 782 107                              # "Heading 1" in the Styles gallery
    SaveClose $p
    $xml = Part $doc "word/document.xml"
    if ($xml -match 'w:val="Heading1"') { "PASS: Heading 1 applies pStyle Heading1" } else { "FAIL: no Heading1 style" }
}
function Test-CenterAlign($doc) {
    $p = Launch
    Click $p 200 343
    Key $p $VK.F10; Key $p $VK.H; Key $p $VK.A   # Home -> Center
    SaveClose $p
    $xml = Part $doc "word/document.xml"
    if ($xml -match 'w:jc w:val="center"') { "PASS: Center writes jc=center" } else { "FAIL: no jc=center" }
}
function Test-Bullets($doc) {
    $p = Launch
    Click $p 200 343
    Key $p $VK.F10; Key $p $VK.H; Key $p $VK.U   # Home -> Bullets
    SaveClose $p
    $xml = Part $doc "word/document.xml"
    if ($xml -match '<w:numPr>') { "PASS: Bullets writes a numPr" } else { "FAIL: no numPr" }
}
function Test-Indent($doc) {
    $p = Launch
    Click $p 200 343
    Chord $p $VK.Ctrl $VK.M                        # Ctrl+M -> increase indent
    SaveClose $p
    $xml = Part $doc "word/document.xml"
    if ($xml -match '<w:ind ') { "PASS: Ctrl+M writes an indent" } else { "FAIL: no w:ind" }
}
function Test-Symbol($doc) {
    $p = Launch
    Click $p 200 343
    Key $p $VK.F10; Key $p $VK.N; Key $p $VK.S   # Insert -> Symbol (opens picker)
    Click $p 107 178                              # first chip: em dash
    SaveClose $p
    $xml = Part $doc "word/document.xml"
    $emdash = [string][char]0x2014
    if ($xml.Contains($emdash)) { "PASS: Insert Symbol inserts the em dash" } else { "FAIL: em dash not inserted" }
}

$scenarios = @(
    @{ n = "tab"; f = ${function:Test-Tab} },
    @{ n = "bold"; f = ${function:Test-Bold} },
    @{ n = "italic"; f = ${function:Test-Italic} },
    @{ n = "header"; f = ${function:Test-Header} },
    @{ n = "first-page"; f = ${function:Test-FirstPage} },
    @{ n = "heading"; f = ${function:Test-Heading} },
    @{ n = "center-align"; f = ${function:Test-CenterAlign} },
    @{ n = "bullets"; f = ${function:Test-Bullets} },
    @{ n = "indent"; f = ${function:Test-Indent} },
    @{ n = "line-spacing"; f = ${function:Test-LineSpacing} },
    @{ n = "page-number"; f = ${function:Test-PageNumber} },
    @{ n = "no-spacing"; f = ${function:Test-NoSpacing} },
    @{ n = "symbol"; f = ${function:Test-Symbol} }
)

$results = @()
foreach ($s in $scenarios) {
    $doc = Join-Path $work "$($s.n).docx"
    Build-Base $doc
    Seed $doc
    Write-Host "--- $($s.n) ---" -ForegroundColor Cyan
    try { $r = & $s.f $doc } catch { $r = "FAIL: exception $($_.Exception.Message)" }
    $color = if ($r -like "PASS*") { "Green" } else { "Red" }
    Write-Host "  $r" -ForegroundColor $color
    $results += [pscustomobject]@{ Scenario = $s.n; Result = $r }
}

Get-Process suite -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
$fail = @($results | Where-Object { $_.Result -notlike "PASS*" }).Count
Write-Host ""
Write-Host "==== $($results.Count - $fail)/$($results.Count) passed ====" -ForegroundColor $(if ($fail) { "Red" } else { "Green" })
if ($fail) { exit 1 } else { exit 0 }
