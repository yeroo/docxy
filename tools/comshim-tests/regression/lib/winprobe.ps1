# One-off diagnostic: does an out-of-proc shim launch create any on-screen window
# anywhere (not just in the shim's own pid)? Diffs all visible top-level windows
# before/while the shim runs and lists the shim's console-host process.
param([string]$BinDir, [string]$Kind = 'word')
$ErrorActionPreference = 'Stop'
Add-Type -TypeDefinition @'
using System; using System.Text; using System.Collections.Generic; using System.Runtime.InteropServices;
public static class WP {
    delegate bool EnumProc(IntPtr h, IntPtr l);
    [DllImport("user32.dll")] static extern bool EnumWindows(EnumProc cb, IntPtr l);
    [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
    [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr h);
    [DllImport("user32.dll", CharSet=CharSet.Unicode)] static extern int GetWindowText(IntPtr h, StringBuilder s, int n);
    [DllImport("user32.dll", CharSet=CharSet.Unicode)] static extern int GetClassName(IntPtr h, StringBuilder s, int n);
    [StructLayout(LayoutKind.Sequential)] struct RECT { public int L, T, R, B; }
    [DllImport("user32.dll")] static extern bool GetWindowRect(IntPtr h, out RECT r);
    public static string[] All() {
        var r = new List<string>();
        EnumWindows((h, l) => { if (IsWindowVisible(h)) { uint pid; GetWindowThreadProcessId(h, out pid);
            var sb = new StringBuilder(256); GetWindowText(h, sb, 256); var cn = new StringBuilder(256); GetClassName(h, cn, 256);
            RECT rc; GetWindowRect(h, out rc); r.Add(h.ToInt64() + " pid=" + pid + " class=" + cn + " rect=" + rc.L + "," + rc.T + "," + rc.R + "," + rc.B + " title=" + sb); }
            return true; }, IntPtr.Zero);
        return r.ToArray();
    }
}
'@
$before = [WP]::All()
$py = (Get-Command python).Source
$tmp = Join-Path $env:TEMP ("winprobe-" + [guid]::NewGuid()); New-Item -ItemType Directory $tmp | Out-Null
$ready = "$tmp\ready"; $go = "$tmp\go"
$psi = [Diagnostics.ProcessStartInfo]::new($py, "`"$PSScriptRoot\holder.py`" hold $Kind `"$tmp\h.docx`" `"$ready`" `"$go`"")
$psi.UseShellExecute = $false; $psi.CreateNoWindow = $true; $psi.RedirectStandardOutput = $true
$p = [Diagnostics.Process]::Start($psi)
$sw = [Diagnostics.Stopwatch]::StartNew()
while (-not (Test-Path $ready) -and $sw.Elapsed.TotalSeconds -lt 30) { Start-Sleep -Milliseconds 100 }
Start-Sleep -Milliseconds 700
$during = [WP]::All()
$shim = Get-Process wordcomshim, xlcomshim -ErrorAction SilentlyContinue | Where-Object { $_.Path -like "$BinDir*" }
"shim: " + ($shim | ForEach-Object { "$($_.Id) $($_.Path)" })
foreach ($s in $shim) {
    Get-CimInstance Win32_Process -Filter "ParentProcessId=$($s.Id)" | ForEach-Object { "  child of shim: $($_.ProcessId) $($_.Name) $($_.CommandLine)" }
    $pp = (Get-CimInstance Win32_Process -Filter "ProcessId=$($s.Id)").ParentProcessId
    "  shim parent: $pp " + (Get-CimInstance Win32_Process -Filter "ProcessId=$pp").Name
}
"new visible windows while shim ran:"
Compare-Object $before $during | Where-Object SideIndicator -eq '=>' | ForEach-Object { "  " + $_.InputObject }
Set-Content $go go
$p.WaitForExit(30000) | Out-Null
"holder: " + $p.StandardOutput.ReadToEnd()
Remove-Item $tmp -Recurse -Force
