<#
.SYNOPSIS
  Repeated open-a-big-folder probe on rig: cold (purge before every launch)
  or warm, several builds interleaved so they see the same rig load.

.DESCRIPTION
  Each build is a folder on rig holding tsubame.exe (and tsubame-bench.exe for
  the purge), e.g. one staged per bisect point. Per run and per build:

    cold: tsubame-bench purge --mode working (the ssh session on rig is
          elevated; gsudo -n as fallback), wait until rig's disks settle (the
          purge starts a paging storm), then
          tsubame --timing --timing-dirs "<tiny>;<idle>;<warmup>;<tiny>;<dir>" --reps 1
          (the tiny folder takes the process start, <idle> waits 3 s for the
          startup tail (GPU paint path) to end, <warmup> (1k files, a full
          screen of rows) faults the paint path's pages back in after the
          purge; the measured row is the navigation into the still cold
          <dir>, timed from the navigation event). -StartupNav measures the
          old way, "<tiny>;<dir>", which overlaps the startup tail.
    navcold: warm app, cold folder: an elevated launch with
          --timing-dirs "<tiny>;<purge>;<tiny>;<dir>"; the app purges itself
          after startup (working: its own pages too, an artifact), re-warms on the tiny folder and
          then opens <dir>. This is the navigation the budget means.
    warm: tsubame --timing --timing-dirs "<tiny>;<idle>;<dir>" --reps <WarmReps>
          (repeats of <dir> are measured; <idle> keeps them out of the
          startup tail, which otherwise slows the first few)

  Builds rotate inside every run (A B C, A B C, ...), so a load change on rig
  hits all of them. Reports land in <Out>\<label>-<run>.txt on FOX and the
  script prints median / p95 / min / max per metric and build.

.EXAMPLE
  pwsh -NoProfile -File tools\open-probe.ps1 -Builds 'base=G:\rig-bench\tsubame-perf2\bin\63c27b7','main=G:\rig-bench\tsubame-perf2\bin\e230e30' -Runs 6 -Out $env:TEMP\probe
#>
[CmdletBinding()]
param(
  [Parameter(Mandatory)] [string[]] $Builds,          # label=rigdir
  [string] $Dir = 'G:\rig-bench\tsubame-perf2\100k',
  [string] $Tiny = 'G:\rig-bench\tsubame-perf2\tiny',
  [string] $Warmup = 'G:\rig-bench\tsubame-perf2\1k',  # cold: a screenful of rows that warms the paint path
  [int]    $Runs = 6,
  [ValidateSet('cold', 'navcold', 'warm')] [string] $Mode = 'cold',
  [int]    $WarmReps = 10,
  [string] $PurgeMode = 'working',                     # the only mode that makes NTFS metadata cold on Win11
  [string] $ExtraArgs = '',                             # more app args, e.g. sort/filter probes
  [string] $Report = 'u2-timing.txt',
  [int]    $SettleDiskPct = 15,                         # cold: after the purge, wait until rig's disks are this idle
  [int]    $SettleMaxSec = 20,
  [switch] $StartupNav,
  [switch] $Ops,                                        # warm: also time sort switches + filter keystrokes (<ops>) on <dir>                                 # cold: navigate right after startup (no <idle>)
  [Parameter(Mandatory)] [string] $Out
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
New-Item -ItemType Directory -Force $Out | Out-Null

function Q([string] $s) { "'" + ($s -replace "'", "''") + "'" }
function Invoke-Rig([string] $ps, [int] $TimeoutSec = 300) {
  $psi = [Diagnostics.ProcessStartInfo]::new('ssh')
  foreach ($a in '-n', '-T', '-o', 'BatchMode=yes', '-o', 'ConnectTimeout=20', 'rig', $ps.Trim()) { $psi.ArgumentList.Add($a) }
  $psi.UseShellExecute = $false; $psi.CreateNoWindow = $true
  $psi.RedirectStandardOutput = $true; $psi.RedirectStandardError = $true
  $p = [Diagnostics.Process]::Start($psi)
  $tOut = $p.StandardOutput.ReadToEndAsync(); $tErr = $p.StandardError.ReadToEndAsync()
  if (-not $p.WaitForExit($TimeoutSec * 1000)) { try { $p.Kill($true) } catch {}; throw "ssh rig timed out" }
  $p.WaitForExit()
  $o = $tOut.Result -split "`r?`n" | Where-Object { $_ -ne '' }
  if ($p.ExitCode -ne 0) { throw "ssh rig failed ($($p.ExitCode)): $(($tErr.Result -split "`n" | Where-Object { $_ -notmatch 'quantum|decrypt later|upgraded' }) -join ' | ')" }
  $o
}
function To-Share([string] $path) { if ($path -match '^([A-Za-z]):(.*)$') { "\\Rig\$($Matches[1].ToLower())$($Matches[2])" } else { $path } }

$list = foreach ($b in $Builds) {
  foreach ($one in ($b -split ',')) {
    if ($one -notmatch '^([^=]+)=(.+)$') { throw "-Builds wants label=rigdir, got '$one'" }
    [pscustomobject]@{ Label = $Matches[1]; Dir = $Matches[2].TrimEnd('\') }
  }
}

# One launch: scheduled task in rig's interactive session, wait for the report.
function Invoke-Launch($b, [string] $argLine, [switch] $Elevated) {
  $rl = if ($Elevated) { 'HIGHEST' } else { 'LIMITED' }
  $task = "tsubame-probe-$($b.Label)"
  $share = To-Share $b.Dir
  Set-Content -Path "$share\_probe.cmd" -Encoding ascii -Value @('@echo off', "cd /d `"$($b.Dir)`"", "`"$($b.Dir)\tsubame.exe`" $argLine", 'echo %ERRORLEVEL%> _exit.txt')
  $remote = @"
Remove-Item -LiteralPath $(Q "$($b.Dir)\$Report") -EA SilentlyContinue
schtasks /Create /TN '$task' /TR '$($b.Dir)\_probe.cmd' /SC ONCE /ST 00:00 /IT /RL $rl /F | Out-Null
schtasks /Run /TN '$task' | Out-Null
`$rep = $(Q "$($b.Dir)\$Report")
`$pat = $(Q "$($b.Dir)\*")
`$t0 = Get-Date
while (((Get-Date) - `$t0).TotalSeconds -lt 180) {
  if (Test-Path `$rep) { Start-Sleep -Milliseconds 300; break }
  Start-Sleep -Milliseconds 200
}
Get-Process -EA SilentlyContinue | Where-Object { `$_.Path -like `$pat -and `$_.ProcessName -eq 'tsubame' } | ForEach-Object { `$_.WaitForExit(15000) | Out-Null; if (-not `$_.HasExited) { `$_ | Stop-Process -Force } }
schtasks /Delete /TN '$task' /F 2>`$null | Out-Null
if (Test-Path `$rep) { 'ok'; Get-Content `$rep } else { 'noreport' }
"@
  $r = Invoke-Rig $remote 260
  # read back over ssh: the SMB client caches "file not found" for seconds
  $at = [array]::IndexOf([string[]]$r, 'ok')
  if ($at -lt 0) { throw "no $Report from $($b.Label)" }
  $r | Select-Object -Skip ($at + 1)
}

$keys = 'first_batch', 'first_frame', 'done', 'final_frame', 'worker', 'overhead', 'store_cpu', 'final_sort', 'watch_start'
$rows = [Collections.Generic.List[object]]::new()
$opRows = [Collections.Generic.List[object]]::new()
for ($i = 1; $i -le $Runs; $i++) {
  foreach ($b in $list) {
    if ($Mode -eq 'cold') {
      # the ssh session on rig is elevated already; gsudo -n only when it is not
      # (gsudo's credential cache fails now and then): retry a few times
      for ($try = 1; ; $try++) {
        $exe = Q "$($b.Dir)\tsubame-bench.exe"
        $p = Invoke-Rig "`$o = & $exe purge --mode $PurgeMode 2>&1; if (-not ((`$o -join ' ') -match 'purge: \w+ ok')) { `$o = gsudo -n -w $exe purge --mode $PurgeMode 2>&1 }; `$o; 'exit=' + `$LASTEXITCODE" 300
        if (($p -join "`n") -match 'purge: \w+ ok') { break }
        if ($try -ge 4) { throw "purge failed: $($p -join ' | ')" }
        Start-Sleep -Seconds 3
      }
      # a working-set purge makes every process on rig re-fault its pages (a
      # paging storm, disks 100%+ busy for seconds): let it pass, else the run
      # measures the storm. The folder's metadata stays cold meanwhile.
      $d = if ($PurgeMode -ne 'working') { 0 } else { Invoke-Rig "`$t0 = Get-Date; do { `$d = (Get-Counter '\PhysicalDisk(_Total)\% Disk Time' -SampleInterval 1 -MaxSamples 1).CounterSamples[0].CookedValue } while (`$d -gt $SettleDiskPct -and ((Get-Date) - `$t0).TotalSeconds -lt $SettleMaxSec); [math]::Round(((Get-Date) - `$t0).TotalSeconds, 1)" 120 }
      Write-Host "  settled after $d s"
      $dirs = if ($StartupNav) { "$Tiny;$Dir" } else { "$Tiny;<idle>;$Warmup;$Tiny;$Dir" }
      $text = Invoke-Launch $b "--timing --timing-dirs `"$dirs`" --reps 1 $ExtraArgs"
    } elseif ($Mode -eq 'navcold') {
      # warm app, cold folder: the app purges (elevated) after its start, lets
      # the storm pass, re-warms itself on the tiny folder, then opens <dir>
      $text = Invoke-Launch $b "--timing --timing-dirs `"$Tiny;<purge>;$Tiny;$Dir`" --reps 1 $ExtraArgs" -Elevated
    } else {
      # tiny + <idle> first: the repeats must not land in the startup tail
      $dirs = if ($Ops) { "$Tiny;<idle>;$Dir;<ops>" } else { "$Tiny;<idle>;$Dir" }
      $text = Invoke-Launch $b "--timing --timing-dirs `"$dirs`" --reps $WarmReps $ExtraArgs"
    }
    $text | Set-Content -Path (Join-Path $Out ('{0}-{1:d2}.txt' -f $b.Label, $i))
    # ops: "ops: entries N  sort  size 1.23 ..." / "ops: filter "abc"  +1 0.50 (123) ..."
    foreach ($l in $text) {
      if ($l -match '^ops: entries') {
        foreach ($m in [regex]::Matches($l, '  (\w+) ([\d.]+)')) { $opRows.Add([pscustomobject]@{ label = $b.Label; op = "sort_$($m.Groups[1].Value)"; ms = [double]$m.Groups[2].Value }) }
      } elseif ($l -match '^ops: filter') {
        foreach ($m in [regex]::Matches($l, '  ([+-]\d+) ([\d.]+) \(')) { $opRows.Add([pscustomobject]@{ label = $b.Label; op = "filter$($m.Groups[1].Value)"; ms = [double]$m.Groups[2].Value }) }
      }
    }
    # cold: the row after `startup`; warm: every `repeat` row (and `first` separately)
    foreach ($l in $text) {
      if ($l -notmatch '^(startup|first|repeat \d+)\s+entries\s+(\d+)') { continue }
      $kind = $Matches[1]
      if ($kind -eq 'startup') { continue }
      if ($Mode -ne 'warm' -and $kind -ne 'first') { continue }
      if (-not $l.TrimEnd().EndsWith($Dir)) { continue }
      $row = [ordered]@{ label = $b.Label; run = $i; kind = ($kind -replace ' \d+', ''); entries = [int]$Matches[2] }
      foreach ($k in $keys) { if ($k -eq 'overhead') { continue }; $row[$k] = if ($l -match "$k\s+([\d.]+)") { [double]$Matches[1] } else { $null } }
      # app time on top of the disk: complete+sorted on screen minus the worker's listing time
      $row['overhead'] = if ($null -ne $row['final_frame'] -and $null -ne $row['worker']) { $row['final_frame'] - $row['worker'] } else { $null }
      $rows.Add([pscustomobject]$row)
    }
    $last = $rows | Where-Object { $_.label -eq $b.Label -and $_.run -eq $i } | Select-Object -Last 1
    if ($last) { Write-Host ("run {0} {1,-10} first_frame {2,8} final {3,8} worker {4,8} store {5,8}" -f $i, $b.Label, $last.first_frame, $last.final_frame, $last.worker, $last.store_cpu) }
  }
}
$rows | Export-Csv -Path (Join-Path $Out 'rows.csv')

function Stat([double[]] $v) {
  $s = @($v | Sort-Object)
  $n = $s.Count
  if (-not $n) { return '-' }
  $med = if ($n % 2) { $s[($n - 1) / 2] } else { ($s[$n / 2 - 1] + $s[$n / 2]) / 2 }
  $p95 = $s[[math]::Min($n - 1, [math]::Ceiling(0.95 * $n) - 1)]
  '{0:N1} / {1:N1} [{2:N1},{3:N1}]' -f $med, $p95, $s[0], $s[-1]
}
Write-Host "`nmedian / p95 [min,max] ms, mode $Mode, $Runs runs"
foreach ($kind in ($rows | Select-Object -ExpandProperty kind -Unique)) {
  foreach ($b in $list) {
    $r = @($rows | Where-Object { $_.label -eq $b.Label -and $_.kind -eq $kind })
    if (-not $r.Count) { continue }
    $line = "{0,-10} {1,-6} n={2,-3}" -f $b.Label, $kind, $r.Count
    foreach ($k in $keys) {
      $v = @($r | Where-Object { $null -ne $_.$k } | ForEach-Object { $_.$k })
      if ($v.Count) { $line += "  $k $(Stat $v)" }
    }
    Write-Host $line
  }
}
if ($opRows.Count) {
  Write-Host "`nops (UI work, no paint): median / p95 [min,max] ms"
  foreach ($b in $list) {
    foreach ($op in ($opRows | Where-Object label -eq $b.Label | Select-Object -ExpandProperty op -Unique)) {
      $v = @($opRows | Where-Object { $_.label -eq $b.Label -and $_.op -eq $op } | ForEach-Object ms)
      Write-Host ("{0,-10} {1,-16} {2}" -f $b.Label, $op, (Stat $v))
    }
  }
}
