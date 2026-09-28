<#
.SYNOPSIS
  Repeatable behaviour baseline of the docxy COM shims (wordcomshim, xlcomshim).

.DESCRIPTION
  For each shim x activation path (oop = LocalServer32 exe, inproc = InprocServer32
  dll) x client, registers ONLY that path (HKCU, via the repo's own register
  scripts), runs the client headless, and records exit code, stdout/stderr, the
  shim log, produced files (+ per-part hashes of the OOXML zip), the shim-process
  pid timeline and how long the exe takes to exit after the client is gone. Also
  a two-client test (oop) and a pywin32 GetTypeInfo / makepy probe.

  SAFETY
  * Refuses to start if WINWORD.EXE / EXCEL.EXE is running; aborts (and restores)
    if either appears, or if any visible window shows up owned by a client or shim.
  * Snapshots every HKCU key the register scripts (and RegisterTypeLibForUser)
    can touch to <OutDir>\reg-before, and in a finally block restores exactly that
    state (delete what did not exist, re-import what did), re-dumps to
    <OutDir>\reg-after and writes <OutDir>\reg-diff.txt (must be empty).
  * Only kills processes it started (clients) or shim exes running from -BinDir.
  * Out-of-proc (LocalServer32) runs only with -IncludeOop. Since the shims are
    built for the Windows GUI subsystem (#530) a COM launch opens no window; before
    that, a console-subsystem exe opened a visible terminal window for the server's
    lifetime. The window guard still aborts the run if any window appears.
  * Backs up and restores the user's real %TEMP%\wordcomshim.log / xlcomshim.log.

  Needs PowerShell 7 (pwsh). See README.md next to this script. Example:
    pwsh -NoProfile -File stage-bins.ps1 -Dest .\bin-main          # after a release build
    pwsh -NoProfile -File run.ps1 -BinDir .\bin-main -IncludeOop
    pwsh -NoProfile -File stage-bins.ps1 -Dest .\bin-upgrade       # after building the change
    pwsh -NoProfile -File run.ps1 -BinDir .\bin-upgrade -IncludeOop
    pwsh -NoProfile -File compare.ps1 .\run-bin-main .\run-bin-upgrade
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$BinDir,
    [string]$OutDir,
    [string]$Repo = (Resolve-Path (Join-Path $PSScriptRoot '..\..\..')).Path,
    [string]$Only = '.',            # regex over case ids, e.g. 'word-oop'
    [int]$ClientTimeout = 120,
    [int]$ExitWait = 30,
    [switch]$IncludeOop,            # also run the out-of-proc (LocalServer32) path; the window guard stays armed
    [string]$DiagScript             # optional: run this (-BinDir, -Kind) instead of the clients, per shim/path
)
$ErrorActionPreference = 'Stop'
$BinDir = (Resolve-Path -LiteralPath $BinDir).Path
if (-not $OutDir) { $OutDir = Join-Path $PSScriptRoot ('run-' + (Split-Path $BinDir -Leaf)) }
New-Item -ItemType Directory -Force $OutDir | Out-Null
$OutDir = (Resolve-Path -LiteralPath $OutDir).Path
$Lib = Join-Path $PSScriptRoot 'lib'
$RealTemp = [IO.Path]::GetTempPath().TrimEnd('\')
Add-Type -AssemblyName System.IO.Compression.FileSystem

# ---------------------------------------------------------------- constants
$Classes = 'HKCU:\Software\Classes'
$Shims = [ordered]@{
    word  = @{ ProgIds = @('Word.Application', 'Word.Application.16'); Shim = '{9C2F4A10-7D33-4B6E-B1A4-2E7C8D5F0A92}'
               Office = '{000209FF-0000-0000-C000-000000000046}'; Libid = '{9C2F4A11-7D33-4B6E-B1A4-2E7C8D5F0A92}'
               Exe = 'wordcomshim.exe'; Dll = 'wordcomshim.dll'; Mk = 'mkwordtypelib.exe'; Ext = 'docx'
               Tlb = "$Repo\tools\wordshim\docxy-word.tlb"; Tools = "$Repo\tools\wordshim"; Tests = "$Repo\tools\wordshim-tests"
               Reg = 'register-word.ps1'; Unreg = 'unregister-word.ps1'; RegIn = 'register-word-inproc.ps1'; UnregIn = 'unregister-word-inproc.ps1'
               Log = 'wordcomshim.log'; Cs = 'csharp\WordInteropTest.exe'; Tag = 'wd' }
    excel = @{ ProgIds = @('Excel.Application', 'Excel.Application.16'); Shim = '{7B3F9E20-4C1A-4E8B-A2D6-9F5C1E0B7A31}'
               Office = '{00024500-0000-0000-C000-000000000046}'; Libid = '{7B3F9E21-4C1A-4E8B-A2D6-9F5C1E0B7A31}'
               Exe = 'xlcomshim.exe'; Dll = 'xlcomshim.dll'; Mk = 'mktypelib.exe'; Ext = 'xlsx'
               Tlb = "$Repo\tools\comshim\docxy-excel.tlb"; Tools = "$Repo\tools\comshim"; Tests = "$Repo\tools\comshim-tests"
               Reg = 'register-shim.ps1'; Unreg = 'unregister-shim.ps1'; RegIn = 'register-inproc.ps1'; UnregIn = 'unregister-inproc.ps1'
               Log = 'xlcomshim.log'; Cs = 'csharp\ExcelInteropTest.exe'; Tag = 'xl' }
}
$LogNames = @('wordcomshim.log', 'xlcomshim.log', 'comshim.log')
$OurLibids = @($Shims.Values | ForEach-Object { $_.Libid })
$TargetKeys = @()
foreach ($s in $Shims.Values) {
    $TargetKeys += $s.ProgIds
    foreach ($c in @($s.Shim, $s.Office)) { $TargetKeys += "CLSID\$c"; $TargetKeys += "WOW6432Node\CLSID\$c" }
    $TargetKeys += "TypeLib\$($s.Libid)"; $TargetKeys += "WOW6432Node\TypeLib\$($s.Libid)"
    $TargetKeys += "AppID\$($s.Shim)"; $TargetKeys += "AppID\$($s.Exe)"
}
# Whole trees dumped (not restored wholesale) purely for the before/after diff.
$DumpTrees = @('Interface', 'WOW6432Node\Interface', 'TypeLib', 'AppID') + $TargetKeys

$Python = (Get-Command python -ErrorAction SilentlyContinue)?.Source
$Rscript = (Get-Command Rscript -ErrorAction SilentlyContinue)?.Source
if (-not $Rscript) { $Rscript = Get-ChildItem 'C:\Program Files\R\*\bin\Rscript.exe' -ErrorAction SilentlyContinue | Sort-Object FullName | Select-Object -Last 1 -ExpandProperty FullName }
$Pwsh = (Get-Command pwsh -ErrorAction SilentlyContinue)?.Source
$WinPs = "$env:SystemRoot\System32\WindowsPowerShell\v1.0\powershell.exe"
$Cscript = "$env:SystemRoot\System32\cscript.exe"

Add-Type -TypeDefinition @'
using System; using System.Text; using System.Collections.Generic; using System.Runtime.InteropServices;
public static class BaselineWin {
    delegate bool EnumProc(IntPtr h, IntPtr l);
    [DllImport("user32.dll")] static extern bool EnumWindows(EnumProc cb, IntPtr l);
    [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
    [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr h);
    [DllImport("user32.dll", CharSet=CharSet.Unicode)] static extern int GetWindowText(IntPtr h, StringBuilder s, int n);
    [DllImport("user32.dll", CharSet=CharSet.Unicode)] static extern int GetClassName(IntPtr h, StringBuilder s, int n);
    [StructLayout(LayoutKind.Sequential)] struct RECT { public int L, T, R, B; }
    [DllImport("user32.dll")] static extern bool GetWindowRect(IntPtr h, out RECT r);
    // Every visible top-level window: "hwnd|pid|class|L,T,R,B|title". Zero-size
    // windows (e.g. conhost's PseudoConsoleWindow) are reported with area 0.
    public static string[] Visible() {
        var r = new List<string>();
        EnumWindows((h, l) => { if (IsWindowVisible(h)) { uint pid; GetWindowThreadProcessId(h, out pid);
            var sb = new StringBuilder(256); GetWindowText(h, sb, 256); var cn = new StringBuilder(256); GetClassName(h, cn, 256);
            RECT rc; GetWindowRect(h, out rc);
            r.Add(h.ToInt64() + "|" + pid + "|" + cn + "|" + rc.L + "," + rc.T + "," + rc.R + "," + rc.B + "|" + sb); }
            return true; }, IntPtr.Zero);
        return r.ToArray();
    }
}
'@

# ---------------------------------------------------------------- helpers
function Get-OfficeProcs { @(Get-Process WINWORD, EXCEL -ErrorAction SilentlyContinue) }
function Get-ShimProcs {
    @(Get-Process wordcomshim, xlcomshim -ErrorAction SilentlyContinue | Where-Object {
        try { $_.Path -and $_.Path.StartsWith($BinDir, 'OrdinalIgnoreCase') } catch { $false } })
}
function Get-OurIfaceKeys {
    $r = @()
    foreach ($root in @('Interface', 'WOW6432Node\Interface')) {
        $p = "$Classes\$root"
        if (-not (Test-Path -LiteralPath $p)) { continue }
        foreach ($k in Get-ChildItem -LiteralPath $p -ErrorAction SilentlyContinue) {
            $tl = (Get-ItemProperty -LiteralPath "$($k.PSPath)\TypeLib" -ErrorAction SilentlyContinue).'(default)'
            if ($tl -and ($OurLibids -contains $tl.ToUpper())) { $r += "$root\$($k.PSChildName)" }
        }
    }
    $r
}
function Dump-Registry([string]$file) {
    $lines = foreach ($t in ($DumpTrees | Sort-Object -Unique)) {
        $p = "$Classes\$t"
        if (-not (Test-Path -LiteralPath $p)) { "<absent> $t"; continue }
        $items = @(Get-Item -LiteralPath $p) + @(Get-ChildItem -LiteralPath $p -Recurse -ErrorAction SilentlyContinue)
        foreach ($it in ($items | Sort-Object Name)) {
            "[$($it.Name)]"
            foreach ($v in ($it.GetValueNames() | Sort-Object)) {
                "  '$v' $($it.GetValueKind($v)) = $(@($it.GetValue($v, $null, 'DoNotExpandEnvironmentNames')) -join '|')"
            }
        }
    }
    $lines | Set-Content -LiteralPath $file -Encoding utf8
}
function Clear-ShimRegistry {
    foreach ($k in (@($TargetKeys) + @(Get-OurIfaceKeys))) {
        $p = "$Classes\$k"
        if (Test-Path -LiteralPath $p) { Remove-Item -LiteralPath $p -Recurse -Force }
    }
}
function Save-RegBefore([string]$dir) {
    New-Item -ItemType Directory -Force $dir | Out-Null
    $state = [ordered]@{ existed = @(); absent = @(); files = @() }
    $i = 0
    foreach ($k in (@($TargetKeys) + @(Get-OurIfaceKeys))) {
        $p = "$Classes\$k"
        if (Test-Path -LiteralPath $p) {
            $f = Join-Path $dir ('{0:D3}_{1}.reg' -f $i++, ($k -replace '[\\{}]', '_'))
            & reg.exe export "HKCU\Software\Classes\$k" $f /y | Out-Null
            if ($LASTEXITCODE -ne 0) { throw "reg export failed for $k" }
            $state.existed += $k; $state.files += (Split-Path $f -Leaf)
        } else { $state.absent += $k }
    }
    $state | ConvertTo-Json -Depth 4 | Set-Content (Join-Path $dir 'state.json')
    Dump-Registry (Join-Path $dir 'dump.txt')
}
function Restore-Reg([string]$dir) {
    Clear-ShimRegistry
    $state = Get-Content (Join-Path $dir 'state.json') -Raw | ConvertFrom-Json
    foreach ($f in $state.files) {
        & reg.exe import (Join-Path $dir $f) 2>&1 | Out-Null
        if ($LASTEXITCODE -ne 0) { throw "reg import failed for $f" }
    }
}
function Get-Fingerprint([string]$path) {
    $fi = Get-Item -LiteralPath $path
    $o = [ordered]@{ name = $fi.Name; size = $fi.Length; sha256 = (Get-FileHash -LiteralPath $path).Hash }
    if ($fi.Extension -in '.docx', '.xlsx') {
        $sha = [Security.Cryptography.SHA256]::Create()
        $z = [IO.Compression.ZipFile]::OpenRead($path)
        try {
            $parts = foreach ($e in ($z.Entries | Sort-Object FullName)) {
                $ms = New-Object IO.MemoryStream; $s = $e.Open(); $s.CopyTo($ms); $s.Close()
                $bytes = $ms.ToArray()
                $h = [BitConverter]::ToString($sha.ComputeHash($bytes)).Replace('-', '').Substring(0, 16)
                $norm = $h
                if ($e.FullName -match '\.(xml|rels)$') {
                    $txt = [Text.Encoding]::UTF8.GetString($bytes)
                    $t2 = $txt -replace '<dcterms:(created|modified)[^>]*>[^<]*</dcterms:\1>', '<dcterms:$1/>'
                    $norm = [BitConverter]::ToString($sha.ComputeHash([Text.Encoding]::UTF8.GetBytes($t2))).Replace('-', '').Substring(0, 16)
                }
                [ordered]@{ part = $e.FullName; len = $bytes.Length; sha = $h; normSha = $norm }
            }
        } finally { $z.Dispose() }
        $o.parts = @($parts)
        $xml = @($parts | Where-Object { $_.part -match '\.(xml|rels)$' })
        $o.xmlDigest = [BitConverter]::ToString($sha.ComputeHash([Text.Encoding]::UTF8.GetBytes((($xml | ForEach-Object { "$($_.part):$($_.sha)" }) -join "`n")))).Replace('-', '').Substring(0, 16)
        $o.xmlDigestNorm = [BitConverter]::ToString($sha.ComputeHash([Text.Encoding]::UTF8.GetBytes((($xml | ForEach-Object { "$($_.part):$($_.normSha)" }) -join "`n")))).Replace('-', '').Substring(0, 16)
    }
    $o
}

$script:Abort = $null
$script:WinBaseline = @{}
function Reset-WindowBaseline { $script:WinBaseline = @{}; foreach ($w in [BaselineWin]::Visible()) { $script:WinBaseline[$w.Split('|')[0]] = 1 } }
function Assert-Safe([int[]]$pids) {
    $off = @(Get-OfficeProcs)
    if ($off.Count) { $script:Abort = "Office process appeared: $($off | ForEach-Object { "$($_.Name)#$($_.Id)" })"; throw $script:Abort }
    $ours = @{}; foreach ($p in @($pids | Where-Object { $_ })) { $ours["$p"] = 1 }
    $bad = foreach ($w in [BaselineWin]::Visible()) {
        $f = $w.Split('|'); $r = @($f[3].Split(',') | ForEach-Object { [int]$_ })
        $area = [math]::Max(0, $r[2] - $r[0]) * [math]::Max(0, $r[3] - $r[1])
        if ($area -le 0) { continue }                       # hidden/zero-size (PseudoConsoleWindow etc.)
        if ($ours.ContainsKey($f[1]) -or -not $script:WinBaseline.ContainsKey($f[0])) {
            "$w [$((Get-Process -Id ([int]$f[1]) -ErrorAction SilentlyContinue)?.Path)]"
        }
    }
    if (@($bad).Count) { $script:Abort = "new on-screen window(s) during case: $(@($bad) -join '; ')"; throw $script:Abort }
}
# Timeline tracker for shim exes (running from $BinDir).
function New-Tracker { @{ t0 = [DateTime]::UtcNow; seen = @{}; events = [Collections.Generic.List[string]]::new() } }
function Update-Tracker($tr) {
    $now = ([DateTime]::UtcNow - $tr.t0).TotalSeconds
    $cur = @{}; foreach ($p in Get-ShimProcs) { $cur[$p.Id] = $p.Name }
    foreach ($id in $cur.Keys) { if (-not $tr.seen.ContainsKey($id)) { $tr.seen[$id] = @{ name = $cur[$id]; start = $now; exit = $null }; $tr.events.Add(('{0:F2}s start {1}#{2}' -f $now, $cur[$id], $id)) } }
    foreach ($id in @($tr.seen.Keys)) { if ($null -eq $tr.seen[$id].exit -and -not $cur.ContainsKey($id)) { $tr.seen[$id].exit = $now; $tr.events.Add(('{0:F2}s exit  {1}#{2}' -f $now, $tr.seen[$id].name, $id)) } }
    @($cur.Keys)
}

function Start-Client([string]$file, [string]$argline, [string]$tmp) {
    $psi = [Diagnostics.ProcessStartInfo]::new($file, $argline)
    $psi.UseShellExecute = $false; $psi.CreateNoWindow = $true
    $psi.RedirectStandardOutput = $true; $psi.RedirectStandardError = $true
    $psi.Environment['TEMP'] = $tmp; $psi.Environment['TMP'] = $tmp
    $psi.WorkingDirectory = $tmp
    $p = [Diagnostics.Process]::Start($psi)
    @{ proc = $p; out = $p.StandardOutput.ReadToEndAsync(); err = $p.StandardError.ReadToEndAsync() }
}
function Wait-Client($c, $tr, [int]$timeout) {
    $sw = [Diagnostics.Stopwatch]::StartNew()
    while (-not $c.proc.HasExited) {
        $shimPids = Update-Tracker $tr
        try { Assert-Safe (@($c.proc.Id) + $shimPids) } catch { try { $c.proc.Kill($true) } catch {}; throw }
        if ($sw.Elapsed.TotalSeconds -gt $timeout) { try { $c.proc.Kill($true) } catch {}; $c.timedOut = $true; break }
        Start-Sleep -Milliseconds 200
    }
    $c.proc.WaitForExit()
    Update-Tracker $tr | Out-Null
    @{ exit = $c.proc.ExitCode; stdout = $c.out.Result; stderr = $c.err.Result; timedOut = [bool]$c['timedOut']; seconds = [math]::Round($sw.Elapsed.TotalSeconds, 2) }
}
function Wait-ShimExit($tr) {
    # After the last client is gone: how long until every shim exe we saw exits?
    $t = [DateTime]::UtcNow; $alive = @()
    while ((([DateTime]::UtcNow - $t).TotalSeconds -lt $ExitWait)) {
        $alive = @(Update-Tracker $tr)
        Assert-Safe $alive
        if (-not $alive.Count) { break }
        Start-Sleep -Milliseconds 200
    }
    $res = @{ exitedWithin = $null; killed = @() }
    if ($alive.Count) {
        foreach ($id in $alive) { try { Stop-Process -Id $id -Force; $res.killed += $id } catch {} }
        $tr.events.Add(('{0:F2}s KILLED leftover shim pid(s) {1} after {2}s' -f ([DateTime]::UtcNow - $tr.t0).TotalSeconds, ($alive -join ','), $ExitWait))
    } else { $res.exitedWithin = [math]::Round(([DateTime]::UtcNow - $t).TotalSeconds, 2) }
    $res
}

function Collect-Case($case, [string]$dir, [string]$tmp, $shim) {
    # The exe logs to %TEMP%\<shim>.log of the COM-launched server (the user's real
    # TEMP); the in-proc dll logs to the CLIENT's %TEMP% (as comshim.log on main).
    $logs = [ordered]@{}
    foreach ($ln in $LogNames) {
        foreach ($src in @(@{ k = 'realTemp'; p = Join-Path $RealTemp $ln }, @{ k = 'clientTemp'; p = Join-Path $tmp $ln })) {
            if (Test-Path -LiteralPath $src.p) {
                Copy-Item -LiteralPath $src.p (Join-Path $dir "$($src.k)-$ln")
                $logs["$($src.k)/$ln"] = @(Get-Content -LiteralPath $src.p).Count
            }
        }
    }
    $case.logLines = $logs
    $case.files = @(Get-ChildItem -LiteralPath $tmp -File -ErrorAction SilentlyContinue | Where-Object { $_.Name -notlike '*.log' -and $_.Name -notmatch '^(ready|go)$' } |
        Sort-Object Name | ForEach-Object { Get-Fingerprint $_.FullName })
}
function Clear-Logs($tmp) {
    foreach ($ln in $LogNames) {
        Remove-Item -LiteralPath (Join-Path $RealTemp $ln) -Force -ErrorAction SilentlyContinue
        if ($tmp) { Remove-Item -LiteralPath (Join-Path $tmp $ln) -Force -ErrorAction SilentlyContinue }
    }
}

function Invoke-Case([string]$shimKey, [string]$path, [string]$client, [string]$file, [string]$argline, [scriptblock]$judge) {
    $id = "$shimKey-$path-$client"
    if ($id -notmatch $Only) { return }
    $shim = $Shims[$shimKey]
    $dir = Join-Path $OutDir "cases\$id"; $tmp = Join-Path $dir 'tmp'
    Remove-Item -LiteralPath $dir -Recurse -Force -ErrorAction SilentlyContinue
    New-Item -ItemType Directory -Force $tmp | Out-Null
    $case = [ordered]@{ id = $id; shim = $shimKey; path = $path; client = $client; status = $null; note = '' }
    if (-not $file -or -not (Test-Path -LiteralPath $file)) {
        $case.status = 'unavailable'; $case.note = "runtime/client not found: $file"; $script:Results += $case; Write-Host "  $id : unavailable"; return
    }
    foreach ($p in Get-ShimProcs) { Stop-Process -Id $p.Id -Force }   # leftovers from OUR bin dir only
    Clear-Logs $tmp
    $argline = $argline.Replace('{TMP}', $tmp).Replace('{DIR}', $dir)
    $case.command = "$file $argline"
    Reset-WindowBaseline
    $tr = New-Tracker
    Write-Host "  $id ..." -NoNewline
    $c = Start-Client $file $argline $tmp
    $r = Wait-Client $c $tr $ClientTimeout
    $case.exit = $r.exit; $case.timedOut = $r.timedOut; $case.clientSeconds = $r.seconds
    Set-Content (Join-Path $dir 'stdout.txt') $r.stdout; Set-Content (Join-Path $dir 'stderr.txt') $r.stderr
    $case.stdout = $r.stdout.Trim(); $case.stderr = $r.stderr.Trim()
    $case.shimExit = if ($path -eq 'oop') { Wait-ShimExit $tr } else { $null }
    $case.shimPids = @($tr.seen.Keys); $case.timeline = @($tr.events)
    Collect-Case $case $dir $tmp $shim
    $case.status = & $judge $case $tmp
    Write-Host (" {0} (exit {1}, {2}s{3})" -f $case.status, $case.exit, $case.clientSeconds, $(if ($case.shimExit) { ", shim exit " + $(if ($null -ne $case.shimExit.exitedWithin) { "$($case.shimExit.exitedWithin)s" } else { 'NO (killed)' }) } else { '' }))
    $script:Results += $case
}

# Judges ------------------------------------------------------------------
function Test-Ooxml([string]$f) { (Test-Path -LiteralPath $f) -and ((Get-Item -LiteralPath $f).Length -gt 0) }
$JudgeVbs = { param($c, $tmp) if ($c.timedOut) { 'fail' } elseif ($c.exit -eq 0 -and -not $c.stderr -and $c.stdout -match '(OK ->|wrote )' -and @($c.files).Count -ge 1) { 'pass' } else { 'fail' } }
$JudgeConf = { param($c, $tmp) if ($c.timedOut) { 'fail' } elseif ($c.stdout -match '^SKIP') { 'unavailable' } elseif ($c.exit -eq 0 -and $c.stdout -match 'CONFORMANCE: PASS') { 'pass' } else { 'fail' } }
$JudgeCs = { param($c, $tmp) if ($c.timedOut) { 'fail' } elseif ($c.exit -eq 0 -and $c.stdout -match 'RESULT: OK' -and @($c.files).Count -ge 1) { 'pass' } else { 'fail' } }
$JudgeProbe = { param($c, $tmp)
    $j = Join-Path (Split-Path $tmp) 'typeinfo.json'
    if (-not (Test-Path $j)) { return 'fail' }
    $d = Get-Content $j -Raw | ConvertFrom-Json
    $c.probe = $d.PSObject.Properties['summary'] ? $d.summary : $null
    if ($d.PSObject.Properties['unavailable']) { return 'unavailable' }
    if ($d.dispatch_error) { $c.note = "dispatch: $($d.dispatch_error)" }
    foreach ($k in 'ensure', 'explicit') { $e = $d.PSObject.Properties[$k]?.Value; if ($e -and -not $e.ok) { $c.note += " ${k}: $($e.error)" } }
    $sm = $d.summary
    if (-not $d.dispatch_error -and $sm.objects_with_typeinfo -eq $sm.objects_total -and $sm.ensure_used_makepy -and $sm.explicit_ok) { 'pass' } else { 'partial' }
}

function Invoke-Clients([string]$shimKey, [string]$path) {
    $s = $Shims[$shimKey]
    $vbs = if ($shimKey -eq 'word') { 'word-smoke', 'word-graceful', 'word-lists', 'word-styles', 'word-tables', 'word-breaks' } else { 'excel-smoke', 'graceful-smoke' }
    foreach ($v in $vbs) { Invoke-Case $shimKey $path "vbs-$v" $Cscript "//nologo `"$($s.Tests)\$v.vbs`" `"{TMP}\$v.$($s.Ext)`"" $JudgeVbs }
    Invoke-Case $shimKey $path 'python' $Python "`"$($s.Tests)\python\pywin32_conformance.py`"" $JudgeConf
    Invoke-Case $shimKey $path 'ps51' $WinPs "-NoProfile -NonInteractive -ExecutionPolicy Bypass -File `"$($s.Tests)\powershell\com_conformance.ps1`"" $JudgeConf
    Invoke-Case $shimKey $path 'pwsh7' $Pwsh "-NoProfile -NonInteractive -File `"$($s.Tests)\powershell\com_conformance.ps1`"" $JudgeConf
    Invoke-Case $shimKey $path 'r' $Rscript "--vanilla `"$($s.Tests)\r\rdcom_conformance.R`"" $JudgeConf
    $mode = if ($path -eq 'oop') { 'castshim' } else { 'castinproc' }
    Invoke-Case $shimKey $path "csharp-$mode" "$($s.Tests)\$($s.Cs)" "$mode `"{TMP}\cs-$mode.$($s.Ext)`"" $JudgeCs
    Invoke-Case $shimKey $path 'pywin32-typeinfo' $Python "`"$Lib\typeinfo_probe.py`" $shimKey `"{DIR}\typeinfo.json`" `"{TMP}\probe.$($s.Ext)`"" $JudgeProbe
}

function Invoke-SecondClient([string]$shimKey) {
    $id = "$shimKey-oop-second-client"
    if ($id -notmatch $Only) { return }
    $s = $Shims[$shimKey]
    $dir = Join-Path $OutDir "cases\$id"; $tmp = Join-Path $dir 'tmp'
    Remove-Item -LiteralPath $dir -Recurse -Force -ErrorAction SilentlyContinue
    New-Item -ItemType Directory -Force $tmp | Out-Null
    $case = [ordered]@{ id = $id; shim = $shimKey; path = 'oop'; client = 'second-client'; status = $null; note = '' }
    if (-not $Python) { $case.status = 'unavailable'; $script:Results += $case; return }
    foreach ($p in Get-ShimProcs) { Stop-Process -Id $p.Id -Force }
    Clear-Logs $tmp
    Write-Host "  $id ..." -NoNewline
    Reset-WindowBaseline
    $tr = New-Tracker
    $ready = Join-Path $tmp 'ready'; $go = Join-Path $tmp 'go'
    $h = Start-Client $Python "`"$Lib\holder.py`" hold $shimKey `"$tmp\holder.$($s.Ext)`" `"$ready`" `"$go`"" $tmp
    $sw = [Diagnostics.Stopwatch]::StartNew()
    while (-not (Test-Path $ready) -and -not $h.proc.HasExited -and $sw.Elapsed.TotalSeconds -lt 60) {
        $pp = Update-Tracker $tr; Assert-Safe (@($h.proc.Id) + $pp); Start-Sleep -Milliseconds 200
    }
    $case.holderReady = Test-Path $ready
    $pidsAtReady = @(Update-Tracker $tr)
    $case.pidsWhileHolderOnly = $pidsAtReady
    $c2 = Start-Client $Python "`"$Lib\holder.py`" second $shimKey `"$tmp\second.$($s.Ext)`"" $tmp
    $pidsDuring = [Collections.Generic.HashSet[int]]::new()
    while (-not $c2.proc.HasExited) { foreach ($x in (Update-Tracker $tr)) { [void]$pidsDuring.Add($x) }; Assert-Safe (@($h.proc.Id, $c2.proc.Id) + @($pidsDuring)); Start-Sleep -Milliseconds 100 }
    $r2 = Wait-Client $c2 $tr $ClientTimeout
    $case.pidsWhileBoth = @($pidsDuring)
    $case.pidsAfterSecondExit = @(Update-Tracker $tr)
    $case.holderAliveAfterSecond = -not $h.proc.HasExited
    Set-Content $go 'go'
    $r1 = Wait-Client $h $tr $ClientTimeout
    $case.shimExit = Wait-ShimExit $tr
    $case.client1 = @{ exit = $r1.exit; stdout = $r1.stdout.Trim(); stderr = $r1.stderr.Trim() }
    $case.client2 = @{ exit = $r2.exit; stdout = $r2.stdout.Trim(); stderr = $r2.stderr.Trim() }
    Set-Content (Join-Path $dir 'client1.txt') ($r1.stdout + "`n--stderr--`n" + $r1.stderr)
    Set-Content (Join-Path $dir 'client2.txt') ($r2.stdout + "`n--stderr--`n" + $r2.stderr)
    $case.shimPids = @($tr.seen.Keys); $case.timeline = @($tr.events)
    $case.sameServer = ($pidsAtReady.Count -eq 1) -and ($pidsDuring.Count -eq 1) -and ($pidsAtReady[0] -in $pidsDuring)
    Collect-Case $case $dir $tmp $s
    $case.status = if ($r1.exit -eq 0 -and $r2.exit -eq 0 -and @($case.files).Count -eq 2) { 'pass' } else { 'fail' }
    Write-Host (" {0} (sameServer={1}, c1={2}, c2={3}, shim exit {4})" -f $case.status, $case.sameServer, $r1.exit, $r2.exit, $(if ($null -ne $case.shimExit.exitedWithin) { "$($case.shimExit.exitedWithin)s" } else { 'NO' }))
    $script:Results += $case
}

function Assert-Registered([string]$shimKey, [string]$path) {
    $s = $Shims[$shimKey]
    foreach ($pid_ in $s.ProgIds[0]) {
        $m = (Get-ItemProperty -LiteralPath "Registry::HKEY_CLASSES_ROOT\$pid_\CLSID" -ErrorAction SilentlyContinue).'(default)'
        if ($m -ne $s.Shim) { throw "SAFETY: HKCR $pid_ -> '$m', not the shim; refusing to run clients" }
    }
    foreach ($cl in @($s.Shim, $s.Office)) {
        $base = "Registry::HKEY_CURRENT_USER\Software\Classes\CLSID\$cl"
        if ($path -eq 'oop') {
            $ls = (Get-ItemProperty -LiteralPath "$base\LocalServer32" -ErrorAction SilentlyContinue).'(default)'
            if ($ls -notlike "*$BinDir*") { throw "SAFETY: $cl LocalServer32 = '$ls'" }
        } else {
            $ip = (Get-ItemProperty -LiteralPath "$base\InprocServer32" -ErrorAction SilentlyContinue).'(default)'
            if ($ip -notlike "*$BinDir*") { throw "SAFETY: $cl InprocServer32 = '$ip'" }
        }
    }
}

# ---------------------------------------------------------------- main
$script:Results = @()
$meta = [ordered]@{
    started = (Get-Date -Format o); binDir = $BinDir; repo = $Repo
    repoHead = (git -C $Repo log -1 --format='%H %s' 2>$null); repoBranch = (git -C $Repo branch --show-current 2>$null)
    buildInfo = (Test-Path "$BinDir\BUILDINFO.txt") ? @(Get-Content "$BinDir\BUILDINFO.txt") : @()
    binaries = @(Get-ChildItem $BinDir -File | ForEach-Object { "$($_.Name) $($_.Length) $((Get-FileHash $_.FullName).Hash.Substring(0,16))" })
    tools = @{ python = $Python; rscript = $Rscript; pwsh = $Pwsh; winps = $WinPs }
    includeOop = [bool]$IncludeOop
}
if (@(Get-OfficeProcs).Count) { throw "WINWORD/EXCEL is running - refusing to start (nothing touched)." }
if (@(Get-ShimProcs).Count) { throw "Shim exes from $BinDir already running - refusing to start." }

# Preserve the user's own shim logs.
$logBackup = Join-Path $OutDir 'prior-logs'; New-Item -ItemType Directory -Force $logBackup | Out-Null
foreach ($ln in $LogNames) { $lf = Join-Path $RealTemp $ln; if (Test-Path $lf) { Copy-Item $lf $logBackup -Force } }

$regBefore = Join-Path $OutDir 'reg-before'
Remove-Item -LiteralPath $regBefore -Recurse -Force -ErrorAction SilentlyContinue
Save-RegBefore $regBefore
Write-Host "registry snapshot -> $regBefore"
$meta.regExisted = @((Get-Content "$regBefore\state.json" -Raw | ConvertFrom-Json).existed)

try {
    foreach ($shimKey in $Shims.Keys) {
        $s = $Shims[$shimKey]
        foreach ($path in @(if ($IncludeOop) { 'oop' }) + 'inproc') {
            Write-Host "== $shimKey / $path =="
            Clear-ShimRegistry
            $regOut = if ($path -eq 'oop') {
                & "$($s.Tools)\$($s.Reg)" -Exe "$BinDir\$($s.Exe)" -Force 6>&1 2>&1 | Out-String
            } else {
                (& "$($s.Tools)\$($s.RegIn)" -Dll "$BinDir\$($s.Dll)" -Force 6>&1 2>&1 | Out-String) +
                # in-proc needs no typelib for dispatch, but per-object GetTypeInfo uses
                # LoadRegTypeLib, so register it as the shipped installer does.
                (& "$BinDir\$($s.Mk)" register $s.Tlb 2>&1 | Out-String)
            }
            Set-Content (Join-Path $OutDir "register-$shimKey-$path.txt") $regOut
            Dump-Registry (Join-Path $OutDir "reg-registered-$shimKey-$path.txt")
            Assert-Registered $shimKey $path
            try {
                if ($DiagScript) {
                    if ("$shimKey-$path" -match $Only) { & $DiagScript -BinDir $BinDir -Kind $shimKey 2>&1 | Tee-Object -FilePath (Join-Path $OutDir "diag-$shimKey-$path.txt") | Out-Host }
                } else {
                    Invoke-Clients $shimKey $path
                    if ($path -eq 'oop') { Invoke-SecondClient $shimKey }
                }
            } finally {
                foreach ($p in Get-ShimProcs) { Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue }
                $u = if ($path -eq 'oop') { & "$($s.Tools)\$($s.Unreg)" 6>&1 2>&1 | Out-String } else { & "$($s.Tools)\$($s.UnregIn)" 6>&1 2>&1 | Out-String }
                Set-Content (Join-Path $OutDir "unregister-$shimKey-$path.txt") $u
                Clear-ShimRegistry
            }
        }
    }
} catch {
    $meta.error = "$_"
    Write-Host "RUN ABORTED: $_" -ForegroundColor Red
} finally {
    foreach ($p in Get-ShimProcs) { Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue }
    Restore-Reg $regBefore
    $regAfter = Join-Path $OutDir 'reg-after'; New-Item -ItemType Directory -Force $regAfter | Out-Null
    Dump-Registry (Join-Path $regAfter 'dump.txt')
    $diff = Compare-Object (Get-Content "$regBefore\dump.txt") (Get-Content "$regAfter\dump.txt") | ForEach-Object { "$($_.SideIndicator) $($_.InputObject)" }
    Set-Content (Join-Path $OutDir 'reg-diff.txt') $diff
    $meta.regDiffLines = @($diff).Count
    foreach ($ln in $LogNames) {
        $lf = Join-Path $RealTemp $ln; Remove-Item $lf -Force -ErrorAction SilentlyContinue
        $b = Join-Path $logBackup $ln; if (Test-Path $b) { Copy-Item $b $lf -Force }   # Copy-Item keeps LastWriteTime
    }
    $meta.abort = $script:Abort
    $meta.finished = (Get-Date -Format o)
    [ordered]@{ meta = $meta; cases = $script:Results } | ConvertTo-Json -Depth 8 | Set-Content (Join-Path $OutDir 'results.json')
    Write-Host ("registry restored; before/after diff lines: {0}" -f @($diff).Count)
}

# ---------------------------------------------------------------- results.md
$md = @('# COM shim baseline', '', "- binaries: ``$BinDir``", "- repo HEAD at run: $($meta.repoHead) ($($meta.repoBranch))")
$md += $meta.buildInfo | Select-Object -First 5 | ForEach-Object { "- build: $_" }
$md += "- run: $($meta.started) .. $($meta.finished)"
if (-not $meta.includeOop) { $md += "- out-of-proc (LocalServer32) path NOT run (no -IncludeOop): COM-launching the console-subsystem shim exe opens a visible Windows Terminal window on this host" }
if ($meta.Contains('error')) { $md += "- **RUN ERROR:** $($meta.error)" }
$md += "- registry before/after diff lines: **$($meta.regDiffLines)** (see reg-diff.txt)", ''
$md += '| case | status | exit | client s | shim exit after client | shim pids | files (xmlDigestNorm) | note |', '|---|---|---|---|---|---|---|---|'
foreach ($c in $script:Results) {
    $se = if ($c.Contains('shimExit') -and $c.shimExit) { if ($null -ne $c.shimExit.exitedWithin) { "$($c.shimExit.exitedWithin)s" } else { 'NO (killed)' } } else { 'n/a' }
    $files = if ($c.Contains('files')) { (@($c.files) | ForEach-Object { "$($_.name) $($_.size)B $($_['xmlDigestNorm'])" }) -join '<br>' } else { '' }
    $ex = if ($c.Contains('exit')) { $c.exit } elseif ($c.Contains('client1')) { "$($c.client1.exit)/$($c.client2.exit)" } else { '' }
    $cs = if ($c.Contains('clientSeconds')) { $c.clientSeconds } else { '' }
    $pids = if ($c.Contains('shimPids')) { @($c.shimPids).Count } else { '' }
    $note = $c.note
    if ($c.client -eq 'second-client') { $note = "sameServer=$($c.sameServer); pids holder-only=[$($c.pidsWhileHolderOnly -join ',')] both=[$($c.pidsWhileBoth -join ',')] afterC2=[$($c.pidsAfterSecondExit -join ',')]; holderAliveAfterC2=$($c.holderAliveAfterSecond)" }
    if ($c.Contains('probe') -and $c.probe) { $p_ = $c.probe; $note = "GetTypeInfo $($p_.objects_with_typeinfo)/$($p_.objects_total) (missing: $(@($p_.missing_typeinfo) -join ',')); CDispatch.CLSID: $($p_.cdispatch_CLSID_attr); EnsureDispatch ok=$($p_.ensure_ok) usedMakepy=$($p_.ensure_used_makepy); explicit EnsureModule+Dispatch ok=$($p_.explicit_ok) genClasses=$(@($p_.explicit_gen_classes).Count) saved=$($p_.explicit_saved); " + $note }
    $md += "| $($c.id) | $($c.status) | $ex | $cs | $se | $pids | $files | $($note -replace '\|', '/' -replace "`r?`n", ' ') |"
}
$md += '', 'Per-case stdout/stderr/logs/timeline: `cases/<id>/`, full data: `results.json`.'
$md | Set-Content (Join-Path $OutDir 'results.md')
Write-Host "wrote $OutDir\results.md"
