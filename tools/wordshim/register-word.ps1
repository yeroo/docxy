<#
.SYNOPSIS
  Register wordcomshim as the per-user Word.Application COM server (HKCU only).

.DESCRIPTION
  Points the Word.Application ProgID at our own shim CLSID under
  HKCU\Software\Classes, and shadows Word's real coclass CLSID {000209FF-...} so a
  client that does `new Word.Application()` (activates by the fixed CLSID) also
  reaches the shim. Per-user and fully reversible: nothing is written to HKLM, a
  distinct shim CLSID is used, and it refuses to overwrite a different existing
  mapping unless -Force. ASCII-only so Windows PowerShell 5.1 parses it.
#>
[CmdletBinding()]
param(
    [string]$Exe = "$PSScriptRoot\..\..\target\release\wordcomshim.exe",
    [switch]$Force
)
$ErrorActionPreference = 'Stop'
$ShimClsid = '{9C2F4A10-7D33-4B6E-B1A4-2E7C8D5F0A92}'   # our own coclass
$WordClsid = '{000209FF-0000-0000-C000-000000000046}'   # Word's real coclass
$Classes   = 'HKCU:\Software\Classes'

if (-not (Test-Path -LiteralPath $Exe)) {
    throw "wordcomshim.exe not found at $Exe. Build it: cargo build --release -p wordcomshim"
}
$Exe = (Resolve-Path -LiteralPath $Exe).Path

$existing = $null
if (Test-Path "$Classes\Word.Application\CLSID") {
    $existing = (Get-ItemProperty "$Classes\Word.Application\CLSID" -Name '(default)' -ErrorAction SilentlyContinue).'(default)'
}
if ($existing -and $existing -ne $ShimClsid -and -not $Force) {
    throw "HKCU already maps Word.Application to $existing. Re-run with -Force (HKLM/installed Word untouched)."
}

function Set-Default($path, $value) {
    New-Item -Path $path -Force | Out-Null
    Set-ItemProperty -Path $path -Name '(default)' -Value $value
}

# Late-bound path: the Word.Application ProgID -> our shim CLSID.
Set-Default "$Classes\Word.Application"               "Docxy Word Application"
Set-Default "$Classes\Word.Application\CLSID"         $ShimClsid
Set-Default "$Classes\CLSID\$ShimClsid"               "Docxy word automation server"
Set-Default "$Classes\CLSID\$ShimClsid\LocalServer32" ('"{0}" /automation' -f $Exe)
Set-Default "$Classes\CLSID\$ShimClsid\ProgID"        "Word.Application"

# Early-bound path: shadow Word's REAL coclass CLSID in HKCU.
$wLs = (Get-ItemProperty "$Classes\CLSID\$WordClsid\LocalServer32" -Name '(default)' -ErrorAction SilentlyContinue).'(default)'
if ($wLs -and $wLs -notmatch 'wordcomshim' -and -not $Force) {
    throw "HKCU already maps the Word CLSID to $wLs. Re-run with -Force."
}
Set-Default "$Classes\CLSID\$WordClsid\LocalServer32" ('"{0}" /automation' -f $Exe)

Write-Host "Registered Word.Application (ProgID + coclass) -> $Exe (HKCU, per-user)."
Write-Host 'Test:  $w = New-Object -ComObject Word.Application; $w.Version; $w.Quit()'
Write-Host 'Undo:  tools\wordshim\unregister-word.ps1'
