<#
.SYNOPSIS
  Run the Office-shim conformance matrix: drive both shims (Excel + Word) from every
  independent COM automation client available on this machine, and print a pass/fail
  matrix. Each client is a distinct IDispatch implementation, so agreement across
  them is strong evidence the shims behave like real Office.

.DESCRIPTION
  Clients exercised (auto-detected; missing ones are marked SKIP):
    * VBScript / cscript        — the scripting engine (via the bundled selftest)
    * pywin32 Dispatch          — Python, C-based; late-bound + re-introspection
    * pywin32 EnsureDispatch    — Python, early-bound via makepy over our typelib
    * PowerShell 5.1 COM        — desktop CLR reflection-based late binding
    * PowerShell 7 (pwsh) COM   — .NET Core COM interop

  The shims must be installed and the typelibs registered first (dist\install.ps1).
  Each per-client test validates the produced OOXML directly (no Office needed) and
  checks it carries the gridcore/docxcore signature (no docProps/app.xml) — proving
  the SHIM served, not a real Office that happens to be present.

.PARAMETER Root
  Repo root (defaults to the parent of this script's tools\ dir).
#>
[CmdletBinding()]
param([string]$Root)
$ErrorActionPreference = 'Continue'
if (-not $Root) { $Root = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path }
$tools = Join-Path $Root 'tools'

$results = [ordered]@{}
function Record($client, $app, $ok) { $results["$client | $app"] = $ok }

function Have($exe) { return [bool](Get-Command $exe -ErrorAction SilentlyContinue) }
function HavePyWin32 {
    if (-not (Have 'python')) { return $false }
    & python -c "import win32com.client" 2>$null
    return ($LASTEXITCODE -eq 0)
}

# --- VBScript (via the deployment selftest, which drives both shims) ---
$selftest = Join-Path $Root 'dist\office-shims\selftest.ps1'
if (-not (Test-Path $selftest)) { $selftest = Join-Path $tools 'dist\selftest.ps1' }
if (Test-Path $selftest) {
    $out = & powershell -NoProfile -ExecutionPolicy Bypass -File $selftest 2>&1
    $ok = ($out -join "`n") -match 'SELF-TEST PASSED'
    Record 'VBScript (cscript)' 'Excel+Word' $ok
} else {
    Record 'VBScript (cscript)' 'Excel+Word' $null
}

# --- pywin32 (Excel does Dispatch + EnsureDispatch; Word does Dispatch) ---
if (HavePyWin32) {
    & python (Join-Path $tools 'comshim-tests\python\pywin32_conformance.py')  | Out-Null
    Record 'pywin32' 'Excel' ($LASTEXITCODE -eq 0)
    & python (Join-Path $tools 'wordshim-tests\python\pywin32_conformance.py') | Out-Null
    Record 'pywin32' 'Word' ($LASTEXITCODE -eq 0)
} else {
    Record 'pywin32' 'Excel' $null
    Record 'pywin32' 'Word' $null
}

# --- PowerShell COM, under whichever hosts exist ---
$xlPosh = Join-Path $tools 'comshim-tests\powershell\com_conformance.ps1'
$wdPosh = Join-Path $tools 'wordshim-tests\powershell\com_conformance.ps1'
foreach ($sh in @('powershell', 'pwsh')) {
    if (-not (Have $sh)) {
        Record "$sh COM" 'Excel' $null; Record "$sh COM" 'Word' $null; continue
    }
    & $sh -NoProfile -ExecutionPolicy Bypass -File $xlPosh | Out-Null
    Record "$sh COM" 'Excel' ($LASTEXITCODE -eq 0)
    & $sh -NoProfile -ExecutionPolicy Bypass -File $wdPosh | Out-Null
    Record "$sh COM" 'Word' ($LASTEXITCODE -eq 0)
}

# --- report ---
Write-Host ""
Write-Host "Office-shim conformance matrix"
Write-Host "------------------------------"
$anyFail = $false
foreach ($k in $results.Keys) {
    $v = $results[$k]
    $tag = if ($null -eq $v) { 'SKIP (client not installed)' } elseif ($v) { 'PASS' } else { $anyFail = $true; 'FAIL' }
    Write-Host ("  {0,-24} {1}" -f $k, $tag)
}
Write-Host ""
if ($anyFail) { Write-Host "CONFORMANCE: FAIL (see above)"; exit 1 }
else { Write-Host "CONFORMANCE: PASS (all available clients drive both shims)"; exit 0 }
