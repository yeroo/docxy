<#
.SYNOPSIS
  Compare two run.ps1 result folders (e.g. baseline on main vs the windows-crate
  upgrade branch). Prints one line per case that differs in: status, exit code,
  normalized stdout, produced files (name + normalized XML digest per part),
  shim-exit behaviour (exited / not, bucketed seconds), shim process count,
  second-client sameServer, pywin32 probe summary, and dispatch-log line count.
  Exit 0 when nothing differs.

  pwsh -NoProfile -File compare.ps1 <runA> <runB>
#>
param([Parameter(Mandatory)][string]$A, [Parameter(Mandatory)][string]$B)
$ErrorActionPreference = 'Stop'
function Load($d) { (Get-Content (Join-Path $d 'results.json') -Raw | ConvertFrom-Json) }
function Norm([string]$s) {
    # Strip run-specific paths/pids/timestamps from client output.
    ($s -replace '[A-Za-z]:\\[^\s"'']*\\cases\\[^\\/\s]+\\tmp[\\/]?', '<TMP>\' -replace 'STEP \d+\.\d+', 'STEP' -replace '\(\d+ bytes\)', '(N bytes)' -replace 'created \S+ \(', 'created <f> (' -replace '\[\d+\]', '[pid]').Trim()
}
function Sig($c) {
    $h = [ordered]@{}
    $h.status = $c.status
    if ($c.PSObject.Properties['exit']) { $h.exit = $c.exit }
    if ($c.PSObject.Properties['stdout']) { $h.stdout = Norm $c.stdout }
    if ($c.PSObject.Properties['files']) {
        $h.files = (@($c.files) | ForEach-Object {
            $parts = if ($_.PSObject.Properties['parts']) { (@($_.parts) | ForEach-Object { "$($_.part)=$($_.normSha)" }) -join ',' } else { $_.sha256 }
            "$($_.name)[$parts]" }) -join ' ; '
    }
    if ($c.PSObject.Properties['shimExit'] -and $c.shimExit) {
        $w = $c.shimExit.exitedWithin
        $h.shimExit = if ($null -eq $w) { 'did-not-exit' } elseif ($w -lt 1) { '<1s' } elseif ($w -lt 5) { '1-5s' } else { '>=5s' }
    }
    if ($c.PSObject.Properties['shimPids']) { $h.shimProcs = @($c.shimPids).Count }
    if ($c.PSObject.Properties['sameServer']) { $h.sameServer = $c.sameServer; $h.holderAliveAfterC2 = $c.holderAliveAfterSecond; $h.c1 = $c.client1.exit; $h.c2 = $c.client2.exit }
    if ($c.PSObject.Properties['probe'] -and $c.probe) { $h.probe = ($c.probe | ConvertTo-Json -Compress -Depth 4) }
    if ($c.PSObject.Properties['logLines'] -and $c.logLines) { $h.logLines = ($c.logLines | ConvertTo-Json -Compress) }
    $h
}
$ra = Load $A; $rb = Load $B
"A: $A  ($($ra.meta.buildInfo | Select-Object -First 1))"
"B: $B  ($($rb.meta.buildInfo | Select-Object -First 1))"
"reg diff lines: A=$($ra.meta.regDiffLines) B=$($rb.meta.regDiffLines); includeOop: A=$($ra.meta.includeOop) B=$($rb.meta.includeOop)"
$ids = @($ra.cases.id) + @($rb.cases.id) | Select-Object -Unique
$diffs = 0
foreach ($id in $ids) {
    $ca = $ra.cases | Where-Object id -eq $id; $cb = $rb.cases | Where-Object id -eq $id
    if (-not $ca -or -not $cb) { "{0}: only in {1}" -f $id, $(if ($ca) { 'A' } else { 'B' }); $diffs++; continue }
    $sa = Sig $ca; $sb = Sig $cb
    $bad = foreach ($k in (@($sa.Keys) + @($sb.Keys) | Select-Object -Unique)) { if ("$($sa[$k])" -ne "$($sb[$k])") { "  $k`n    A: $($sa[$k])`n    B: $($sb[$k])" } }
    if ($bad) { "$id DIFFERS:"; $bad; $diffs++ } else { "$id same ($($sa.status))" }
}
"`n$diffs case(s) differ"
exit ([int]($diffs -gt 0))
