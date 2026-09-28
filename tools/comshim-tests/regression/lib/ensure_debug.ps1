# One-off diagnostic (via run.ps1 -DiagScript): why does gencache.EnsureDispatch
# hand back a plain CDispatch? Runs headless with a throwaway TEMP (gen_py sandbox).
param([string]$BinDir, [string]$Kind = 'excel')
$tmp = Join-Path $env:TEMP ("ensdbg-" + [guid]::NewGuid()); New-Item -ItemType Directory $tmp | Out-Null
$prog = if ($Kind -eq 'word') { 'Word.Application' } else { 'Excel.Application' }
$code = @"
import os, win32com, pythoncom
import win32com.client as wc
from win32com.client import gencache
print('gen_path', win32com.__gen_path__)
d = wc.Dispatch('$prog')
print('dispatch type', type(d), 'CLSID attr', d.__dict__.get('CLSID'))
ti = d._oleobj_.GetTypeInfo(); a = ti.GetTypeAttr()
tlb, idx = ti.GetContainingTypeLib(); la = tlb.GetLibAttr()
print('typeattr iid', a[0], 'typekind', a.typekind, 'lib', la)
try:
    mod = gencache.EnsureModule(la[0], la[1], la[3], la[4], bForDemand=1)
    print('EnsureModule ->', mod)
except Exception as e:
    print('EnsureModule raised', type(e).__name__, e)
e = gencache.EnsureDispatch('$prog')
print('EnsureDispatch type', type(e))
print('gen dir', os.listdir(win32com.__gen_path__))
d.Quit(); e.Quit()
"@
$psi = [Diagnostics.ProcessStartInfo]::new((Get-Command python).Source, "-c `"exec(open(r'$tmp\d.py').read())`"")
Set-Content "$tmp\d.py" $code
$psi.UseShellExecute = $false; $psi.CreateNoWindow = $true; $psi.RedirectStandardOutput = $true; $psi.RedirectStandardError = $true
$psi.Environment['TEMP'] = $tmp; $psi.Environment['TMP'] = $tmp
$p = [Diagnostics.Process]::Start($psi); $o = $p.StandardOutput.ReadToEndAsync(); $er = $p.StandardError.ReadToEndAsync()
$p.WaitForExit(60000) | Out-Null
$o.Result; $er.Result
Remove-Item $tmp -Recurse -Force
