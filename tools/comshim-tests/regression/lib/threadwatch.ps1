param([string]$BinDir)
$end=(Get-Date).AddSeconds(90)
while((Get-Date) -lt $end){
  $p=Get-Process wordcomshim,xlcomshim -EA SilentlyContinue | ? { $_.Path -like "$BinDir*" }
  if($p){ Start-Sleep 4; $p.Refresh(); "pid $($p.Id) threads=$($p.Threads.Count) cpu=$($p.TotalProcessorTime)"; $p.Threads | % { "  tid $($_.Id) state=$($_.ThreadState) wait=$(try{$_.WaitReason}catch{'-'}) start=0x{0:X}" -f $_.StartAddress.ToInt64() }; break }
  Start-Sleep -Milliseconds 100 }
