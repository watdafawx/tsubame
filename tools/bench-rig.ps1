<#
.SYNOPSIS
  Run the Tsubame app benchmark suite and the full leak gate on rig, and append
  the numbers to docs/bench-log.md.

.DESCRIPTION
  This is the harness behind docs/bench.md. It never runs the GUI on FOX: it
  builds and ships with push-to-rig.ps1, launches on rig through a scheduled
  task in the interactive session, reads the app's own reports back over the
  \\Rig\g share, and finally hands them to `tsubame-bench app`, which owns the
  parsing, the budget thresholds and the bench-log markdown (so that part is
  unit-tested instead of living in PowerShell).

  What it measures, all on the release build of the app in a real window:
    * cold start to first painted frame and the 5 s scroll run at 4K
      (`--bench --dir <100k>`, 10 launches: first paint, paint p95, private
      bytes before/after the scroll, exe size)
    * open the 100,000-entry folder: first rows and complete+sorted
      (`--timing --timing-dirs <100k> --reps 10`, warm)
    * idle CPU over 10 s (a plain launch, process CPU time sampled twice)
    * the full leak gate in one process (`--leak-gate`: 10,000 navigations and
      1,000 tab open/close with real icons, counters every 1,000 ops)
    * optionally a cold-cache open (-ColdRuns, default 3), the open-probe.ps1
      cold method: `<bench> purge` from the elevated ssh session (gsudo -n as
      fallback), wait for the disks to settle, then one process that warms up
      on small folders and times only its navigation into the still cold 100k
      folder; if the purge is not permitted the log says "not measured"

  Fixtures live under G:\rig-bench\tsubame-b1 (100k folder + the gate's
  200-folder tree; well under the 10 GB cap) and are deleted at the end unless
  -KeepFixtures. Raw reports are copied into G:\rig-test\<slot>\bench-<stamp>\.

.EXAMPLE
  pwsh X:\Dev\tools\bench-rig.ps1 -Project X:\Dev\tsubame

.EXAMPLE
  # 2-minute smoke test of the harness itself: small counts, no log entry
  pwsh X:\Dev\tools\bench-rig.ps1 -Project <worktree> -Quick -KeepFixtures

.NOTES
  Needs: ssh rig (see the rig skill), rig's G: with 20 GB free, push-to-rig.ps1.
  The leak gate takes minutes; the script waits for the app to exit and reports
  its exit code (0 pass, 1 fail) from the launch wrapper.
#>
[CmdletBinding()]
param(
  [string] $Project = '',
  [string] $BenchRoot = 'G:\rig-bench\tsubame-b1',
  [string] $GateFixture = '',
  [int]    $Runs = 10,
  [int]    $GateNavs = 10000,
  [int]    $GateTabs = 1000,
  [int]    $GateRuns = 3,
  [ValidateSet('none', 'standby', 'working')] [string] $ColdMode = 'working',
  [int]    $ColdRuns = 3,
  [int]    $Slot = 0,
  [switch] $SkipBuild,
  [switch] $SkipGen,
  [switch] $SkipLeakGate,
  [switch] $LeakGateOnly,
  [switch] $KeepFixtures,
  [switch] $Quick,
  [switch] $NoLog,
  [string] $LogPath = 'docs\bench-log.md',
  # experiments (docs/memory.md): env vars for every app launch ('NAME=value'),
  # cargo features for the rig build (one comma list, e.g. 'memdiag'), and a
  # mode that runs only the cold-start + scroll launches (no log entry)
  [string[]] $AppEnv = @(),
  [string] $Features = '',
  [string] $BenchArgs = '',
  [switch] $ScrollOnly
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

if (-not $Project) { $Project = Split-Path $PSScriptRoot -Parent }
$Project = (Resolve-Path $Project).Path
if (-not $LogPath.Contains(':') -and -not $LogPath.StartsWith('\\')) { $LogPath = Join-Path $Project $LogPath }
if ($Quick) {
  if ($Runs -eq 10) { $Runs = 2 }
  if ($GateNavs -eq 10000) { $GateNavs = 300 }
  if ($GateTabs -eq 1000) { $GateTabs = 30 }
  $GateRuns = 1
  if (-not $PSBoundParameters.ContainsKey('NoLog')) { $NoLog = $true }
  $ColdMode = 'none'
  Write-Host "bench-rig: QUICK mode: $Runs cold-start runs, $GateNavs navigations, $GateTabs tabs, no log entry"
}
$pushToRig = 'X:\Dev\tools\push-to-rig.ps1'
$cargoCcd1 = 'X:\Dev\tools\cargo-ccd1.ps1'
foreach ($p in $pushToRig, $cargoCcd1) { if (-not (Test-Path $p)) { throw "missing $p" } }

# ---- rig helpers ------------------------------------------------------------

function Q([string] $s) { "'" + ($s -replace "'", "''") + "'" }

# Runs a PowerShell command on rig over ssh. Mirrors push-to-rig.ps1: stdin is
# closed at once (-n), no tty (-T), ssh's post-quantum warning is filtered, and
# a dead link cannot block forever.
function Invoke-Rig([string] $ps, [int] $TimeoutSec = 300) {
  $psi = [Diagnostics.ProcessStartInfo]::new('ssh')
  foreach ($a in '-n', '-T', '-o', 'BatchMode=yes', '-o', 'ConnectTimeout=20', '-o', 'ServerAliveInterval=20', '-o', 'ServerAliveCountMax=15', 'rig', $ps.Trim()) { $psi.ArgumentList.Add($a) }
  $psi.UseShellExecute = $false
  $psi.CreateNoWindow = $true
  $psi.RedirectStandardInput = $true
  $psi.RedirectStandardOutput = $true
  $psi.RedirectStandardError = $true
  $psi.StandardOutputEncoding = [Text.Encoding]::UTF8
  $psi.StandardErrorEncoding = [Text.Encoding]::UTF8
  $p = [Diagnostics.Process]::Start($psi)
  $p.StandardInput.Close()
  $noise = 'post-quantum|store now, decrypt later|upgraded\. See'
  $out = [Collections.Generic.List[string]]::new()
  $err = [Collections.Generic.List[string]]::new()
  $tOut = $p.StandardOutput.ReadLineAsync()
  $tErr = $p.StandardError.ReadLineAsync()
  $sw = [Diagnostics.Stopwatch]::StartNew()
  try {
    while ($tOut -or $tErr) {
      [void][Threading.Tasks.Task]::WaitAny([Threading.Tasks.Task[]]@(@($tOut, $tErr) | Where-Object { $_ }), 250)
      if ($tOut -and $tOut.IsCompleted) {
        $l = $tOut.Result
        if ($null -eq $l) { $tOut = $null } else { $out.Add($l); $tOut = $p.StandardOutput.ReadLineAsync() }
      }
      if ($tErr -and $tErr.IsCompleted) {
        $l = $tErr.Result
        if ($null -eq $l) { $tErr = $null } else {
          if ("$l" -notmatch $noise) { $err.Add($l); if ($err.Count -gt 6) { $err.RemoveAt(0) } }
          $tErr = $p.StandardError.ReadLineAsync()
        }
      }
      if ($sw.Elapsed.TotalSeconds -gt $TimeoutSec) { throw "ssh rig timed out after ${TimeoutSec}s: $($ps.Substring(0, [Math]::Min(80, $ps.Length)))..." }
    }
    $p.WaitForExit()
    if ($p.ExitCode -ne 0) { throw "ssh rig failed ($($p.ExitCode))$(if ($err.Count) { ': ' + ($err -join ' | ') })" }
  } finally {
    if (-not $p.HasExited) { try { $p.Kill($true) } catch {} }
    $p.Dispose()
  }
  $out.ToArray()
}

# G:\x -> \\Rig\g\x (the run dir is shareable; ssh is not needed for file IO)
function To-Share([string] $path) {
  if ($path -match '^([A-Za-z]):(.*)$') { return "\\Rig\$($Matches[1].ToLower())$($Matches[2])" }
  return $path
}

function Get-RigLoad {
  $js = Invoke-Rig @'
$java = @(Get-Process java, javaw -EA SilentlyContinue)
$cpu = $null
try { $cpu = [math]::Round((Get-Counter '\Processor(_Total)\% Processor Time' -EA Stop).CounterSamples[0].CookedValue, 0) } catch {}
$docker = if (Get-Command docker -EA SilentlyContinue) { (docker ps -q 2>$null | Measure-Object).Count } else { 0 }
$up = [int]((Get-Date) - (Get-CimInstance Win32_OperatingSystem).LastBootUpTime).TotalHours
[pscustomobject]@{
  java = $java.Count
  javaCpuMin = [int](($java | Measure-Object -Property CPU -Sum).Sum)
  cpuPct = if ($null -eq $cpu) { -1 } else { $cpu }
  docker = $docker
  uptimeH = $up
} | ConvertTo-Json -Compress
'@
  $o = $js | ConvertFrom-Json
  $parts = @("$($o.java) Minecraft java process(es)", "$($o.docker) docker container(s)")
  if ($o.cpuPct -ge 0) { $parts += "CPU $($o.cpuPct)% busy" }
  if ($o.java -gt 0) { $parts += 'servers loaded' }
  "$($parts -join ', '); rig up $($o.uptimeH) h"
}

# ---- app launch on rig ------------------------------------------------------

# Starts the app in rig's interactive session. -Wait waits until it exits (or
# until -Report appears, for runs that write a report and close); the wrapper
# cmd waits too, so the app's exit code lands in <run dir>\_exit.txt.
function Start-RigApp([string] $exe, [string] $argLine, [switch] $Wait, [int] $TimeoutSec = 300, [string] $Report = '') {
  if (-not $script:RunDir) { throw 'no run dir yet' }
  # A previous report must not make a delayed scheduled launch look complete.
  if ($Report -and (Test-Path -LiteralPath (Join-Path $script:RunShare $Report))) {
    Remove-Item -LiteralPath (Join-Path $script:RunShare $Report) -ErrorAction Stop
  }
  $cmd = @('@echo off', "cd /d `"$script:RunDir`"")
  # `pwsh -File` passes an array as one comma-joined string: split it again
  foreach ($e in @($AppEnv | ForEach-Object { $_ -split ',' } | Where-Object { $_ })) { if ($e -match '^[A-Za-z_][A-Za-z0-9_]*=') { $cmd += "set `"$e`"" } else { throw "-AppEnv wants NAME=value, got '$e'" } }
  if ($Wait) { $cmd += "`"$script:RunDir\$exe`" $argLine" }
  else { $cmd += "start `"`" `"$script:RunDir\$exe`" $argLine" }
  $cmd += 'echo %ERRORLEVEL%> _exit.txt'
  Set-Content -Path "$script:RunShare\_bench.cmd" -Encoding ascii -Value $cmd
  Invoke-Rig "schtasks /Create /TN '$script:TaskName' /TR '$($script:RunDir)\_bench.cmd' /SC ONCE /ST 00:00 /IT /RL LIMITED /F | Out-Null; schtasks /Run /TN '$script:TaskName' | Out-Null" | Out-Null
  if (-not $Wait) {
    Start-Sleep -Milliseconds 1200
    $running = Invoke-Rig "`$p = Get-Process -EA SilentlyContinue | Where-Object { `$_.Path -like $(Q "$script:RunDir\*") }; if (`$p) { 'yes' } else { 'no' }"
    if ($running -ne 'yes') { Write-Warning "$exe was not running 1.2 s after the launch (a very fast run, or it failed to start)" }
    Write-Host "bench-rig: $exe launched on rig"
    return
  }
  # Wait for the app to appear and then exit. A run that closes in under a
  # second is normal (--timing), so the report file also counts as "done".
  $reportRemote = if ($Report) { "$script:RunDir\$Report" } else { '' }
  $remote = @"
`$pat = $(Q "$script:RunDir\*")
`$report = $(Q $reportRemote)
`$deadline = (Get-Date).AddSeconds($TimeoutSec)
`$seen = `$false
while ((Get-Date) -lt `$deadline) {
  `$p = Get-Process -EA SilentlyContinue | Where-Object { `$_.Path -like `$pat }
  if (`$p) { `$seen = `$true }
  elseif (`$seen) { 'gone'; exit 0 }
  elseif (`$report -and (Test-Path `$report)) { 'gone'; exit 0 }
  elseif (((Get-Date) - `$start) -gt [TimeSpan]::FromSeconds(20) -and -not `$seen) { 'never started'; exit 2 }
  Start-Sleep -Milliseconds 400
}
'still running'; exit 1
"@
  $remote = "`$start = Get-Date`n" + $remote
  $r = Invoke-Rig $remote ($TimeoutSec + 120)
  if ($r -contains 'never started') { throw "$exe never started on rig (task $script:TaskName, dir $script:RunDir)" }
  if ($r -notcontains 'gone') { throw "the app did not exit within ${TimeoutSec}s" }
  Start-Sleep -Milliseconds 600
}

function Stop-RigApp {
  if (-not $script:RunDir) { return }
  Invoke-Rig "Get-Process -EA SilentlyContinue | Where-Object { `$_.Path -like $(Q "$script:RunDir\*") } | Stop-Process -Force; Start-Sleep -Milliseconds 300" | Out-Null
}

# Moves one report out of the run dir into the run's report dir, or throws.
function Save-Report([string] $name, [string] $as) {
  try { Invoke-Rig "Move-Item -Force $(Q "$script:RunDir\$name") $(Q "$script:ReportDir\$as")" | Out-Null }
  catch { throw "the app did not leave $name behind: $($_.Exception.Message)" }
  Write-Host "bench-rig: report $as"
}

# ---- main ------------------------------------------------------------------

$slotNum = 0
$RunDir = ''
$RunShare = ''
$script:ReportDir = ''
$script:ReportShare = ''
$script:SlotName = ''
$script:TaskName = ''
$fixturesMade = $false
$started = Get-Date
try {
  Write-Host "bench-rig: project $Project"
  $commit = (& git -C $Project rev-parse --short HEAD).Trim()
  $branch = (& git -C $Project rev-parse --abbrev-ref HEAD).Trim()
  $dirty = if ((& git -C $Project status --porcelain)) { 1 } else { 0 }
  Write-Host "bench-rig: commit $commit on $branch$(if ($dirty) { ' (dirty worktree)' })"
  $stamp = (Get-Date).ToString('yyyy-MM-ddTHH:mm:ss')

  $hostName = Invoke-Rig "hostname" | Select-Object -First 1
  if ($hostName -ne 'RIG') { throw "ssh rig reached '$hostName', not RIG" }
  $freeGB = [double](Invoke-Rig "[math]::Round((Get-Volume G).SizeRemaining / 1GB, 1)" | Select-Object -First 1)
  if ($freeGB -lt 20) { throw "rig's G: has only $freeGB GB free (bench fixtures need a few hundred MB, but G: also holds the Minecraft servers)" }
  $rigLoad = Get-RigLoad
  Write-Host "bench-rig: rig load: $rigLoad"

  # 1. build and ship the app on rig, then its bench tool in the same target
  #    dir (push-to-rig's -CargoArgs cannot carry several values through
  #    `pwsh -File`, and the harness needs tsubame-bench.exe on rig too)
  if (-not $SkipBuild) {
    $pushArgs = @('-Project', $Project, '-BuildOnRig', '-Exe', 'tsubame', '-Profile', 'release')
    if ($Slot) { $pushArgs += @('-Slot', "$Slot") }
    if ($Features) { $pushArgs += @('-CargoArgs', "--features=$Features") }
    $log = & pwsh -NoProfile -File $pushToRig @pushArgs 2>&1
    $log | ForEach-Object { Write-Host $_ }
    $line = $log | Where-Object { "$_" -match '^push-to-rig: slot (\d) -> rig:(.+)$' } | Select-Object -Last 1
    if (-not $line -or "$line" -notmatch '^push-to-rig: slot (\d) -> rig:(.+)$') { throw "push-to-rig did not report its slot; output above" }
    $slotNum = [int]$Matches[1]
    $RunDir = $Matches[2].Trim()
    $slotName = Split-Path $RunDir -Leaf
    Write-Host "bench-rig: building tsubame-bench in rig:G:\rig-build\$slotName (release)"
    $b = Invoke-Rig "(Get-Process -Id `$PID).PriorityClass = 'BelowNormal'; Set-Location $(Q "G:\rig-build\$slotName"); cargo build --release -j 8 -p tsubame-bench 2>&1; exit `$LASTEXITCODE" 1200
    $b | Select-Object -Last 6 | ForEach-Object { Write-Host "  $_" }
    Invoke-Rig "Copy-Item -Force $(Q "G:\rig-build\$slotName\target\release\tsubame-bench.exe") $(Q $RunDir)" | Out-Null
  } else {
    if (-not $Slot) { throw '-SkipBuild needs -Slot N (the slot to launch from)' }
    $slotNum = $Slot
    $base = if ($Project -match '\\\.t3\\worktrees\\([^\\]+)\\') { $Matches[1] } elseif ($Project -match '^(.*?)\\\.claude\\worktrees\\') { Split-Path $Matches[1] -Leaf } else { Split-Path $Project -Leaf }
    $slotName = if ($slotNum -eq 1) { $base } else { "$base-s$slotNum" }
    $RunDir = "G:\rig-test\$slotName"
  }
  $script:SlotName = Split-Path $RunDir -Leaf
  $RunShare = To-Share $RunDir
  $reportName = "bench-$(($stamp -replace '[:\s]', '-'))"
  $script:ReportDir = "$RunDir\$reportName"
  $script:ReportShare = "$RunShare\$reportName"
  $script:TaskName = "tsubame-bench-$script:SlotName"
  Write-Host "bench-rig: slot $slotNum, run dir $RunDir, reports $script:ReportDir"
  Invoke-Rig "New-Item -ItemType Directory -Force $(Q $script:ReportDir) | Out-Null" | Out-Null

  # The exact binary behind every number below: size and SHA-256 of the shipped
  # exe go into meta.txt, and from there into the bench-log section header. A
  # stale exe on a warm rig slot once produced a plausible but wrong size row,
  # so a section now names the file it measured.
  $appExe = Join-Path $RunShare 'tsubame.exe'
  $appExeItem = Get-Item -LiteralPath $appExe -ErrorAction SilentlyContinue
  if (-not $appExeItem) { throw "no tsubame.exe in $RunShare (the build/copy step did not put one there)" }
  $exeBytes = $appExeItem.Length
  $exeSha = (Get-FileHash -LiteralPath $appExe -Algorithm SHA256).Hash.ToLowerInvariant()
  Write-Host "bench-rig: tsubame.exe $('{0:N0}' -f $exeBytes) B sha256 $($exeSha.Substring(0, 16))"

  # 2. fixtures (under G:\rig-bench\tsubame-b1; deleted at the end unless -KeepFixtures)
  $benchExe = "$RunDir\tsubame-bench.exe"
  if (-not $SkipGen) {
    $fixturesMade = $true
    Write-Host "bench-rig: generating fixtures under $BenchRoot"
    foreach ($a in @("gen --root $(Q $BenchRoot) --sizes 100k", "gate-fixture --root $(Q $BenchRoot)")) {
      $out = Invoke-Rig "& $(Q $benchExe) $a; exit `$LASTEXITCODE" 1200
      $out | ForEach-Object { Write-Host "  $_" }
      if (-not ($out -match 'done under|already complete|folders under')) { throw "fixture generation failed (see output above)" }
    }
  }
  $fixture100k = "$BenchRoot\100k"
  $gateFixture = if ($GateFixture) { $GateFixture } else { "$BenchRoot\gate" }

  # 3. cold start + scroll + memory: -Runs launches of --bench over the 100k folder
  $coldText = 'not measured'
  if (-not $LeakGateOnly) {
    for ($i = 1; $i -le $Runs; $i++) {
      $sw = [Diagnostics.Stopwatch]::StartNew()
      Start-RigApp 'tsubame.exe' "--bench --dir `"$fixture100k`" $BenchArgs" -Wait -TimeoutSec 240 -Report 's1-bench.txt'
      Save-Report 's1-bench.txt' ('cold-start-{0:d2}.txt' -f $i)
      Write-Host ("bench-rig: cold-start run {0}/{1} took {2:N0} s" -f $i, $Runs, $sw.Elapsed.TotalSeconds)
      $rep = Get-Content -Path ("$script:ReportShare\cold-start-{0:d2}.txt" -f $i) -EA SilentlyContinue
      $rep | Where-Object { $_ -match '^(first_paint_ms|paint_all|private_bytes_before|private_bytes_end|icon cache|gfx caches)' } | ForEach-Object { Write-Host "  $_" }
    }
  }
  if ($ScrollOnly) {
    Write-Host "bench-rig: -ScrollOnly: reports in $script:ReportDir (\\Rig share: $script:ReportShare)"
    $NoLog = $true
    $SkipLeakGate = $true
  }
  if (-not $LeakGateOnly -and -not $ScrollOnly) {

    # 4. open the 100k folder: first rows and complete+sorted, warm
    Start-RigApp 'tsubame.exe' "--timing --timing-dirs `"$fixture100k`" --reps $Runs" -Wait -TimeoutSec 300 -Report 'u2-timing.txt'
    Save-Report 'u2-timing.txt' 'timing.txt'

    # 5. idle CPU over 10 s on a plain launch (two windows: the first absorbs
    #    whatever the launch still had queued, the second is the measured one)
    Start-RigApp 'tsubame.exe' "--dir `"$fixture100k`""
    $idle = Invoke-Rig @"
`$pat = $(Q "$RunDir\*")
`$p = Get-Process -EA SilentlyContinue | Where-Object { `$_.Path -like `$pat } | Select-Object -First 1
if (-not `$p) { 'noapp'; exit 1 }
Start-Sleep -Seconds 3
`$p.Refresh(); `$t0 = `$p.TotalProcessorTime.TotalMilliseconds
`$th0 = @(`$p.Threads).Count
Start-Sleep -Seconds 10
`$p.Refresh(); `$t1 = `$p.TotalProcessorTime.TotalMilliseconds
`$cs = -1
try { `$cs = [math]::Round(((Get-Counter '\Thread(*)\Context Switches/sec' -EA Stop).CounterSamples | Where-Object { `$_.InstanceName -like 'tsubame*' } | Measure-Object CookedValue -Sum).Sum, 1) } catch {}
Start-Sleep -Seconds 10
`$p.Refresh(); `$t2 = `$p.TotalProcessorTime.TotalMilliseconds
`$th1 = @(`$p.Threads).Count
"idle.seconds=10"
"idle.cpu_ms_first={0}" -f [math]::Round(`$t1 - `$t0, 1)
"idle.cpu_ms={0}" -f [math]::Round(`$t2 - `$t1, 1)
"idle.threads={0}" -f `$th1
"idle.threads_warm={0}" -f `$th0
"idle.context_switches_per_s={0}" -f `$cs
"idle.pid={0}" -f `$p.Id
"@
    Stop-RigApp
    $idle | Where-Object { $_ } | Set-Content -Path "$script:ReportShare\idle.txt" -Encoding utf8
    $idle | ForEach-Object { Write-Host "  $_" }
    $cpuLine = $idle | Where-Object { $_ -like 'idle.cpu_ms=*' }
    Write-Host "bench-rig: idle $cpuLine over 10 s (second window)"

    # 6. optional cold-cache open, the open-probe.ps1 "cold" method: purge from
    #    outside the app, let rig's disks settle, then one process per run that
    #    warms itself up on small folders before it opens the still cold 100k
    #    folder; only that last navigation is the measurement (a purge also
    #    empties the app's own pages, so a cold *process* would time the app)
    if ($ColdMode -ne 'none') {
      $tiny = "$gateFixture\tiny\0001"
      $warmup = "$gateFixture\small\0001"
      $done = 0
      for ($c = 1; $c -le $ColdRuns; $c++) {
        # the ssh session on rig is elevated already; gsudo -n only when it is
        # not (its credential cache fails now and then): retry a few times
        $purged = $false
        for ($try = 1; $try -le 4 -and -not $purged; $try++) {
          $p = Invoke-Rig "`$o = & $(Q $benchExe) purge --mode $ColdMode 2>&1; if (-not ((`$o -join ' ') -match 'purge: \w+ ok')) { `$o = gsudo -n -w $(Q $benchExe) purge --mode $ColdMode 2>&1 }; `$o" 300
          $purged = ($p -join "`n") -match 'purge: \w+ ok'
          if (-not $purged) { $p | ForEach-Object { Write-Host "  $_" } }
        }
        if (-not $purged) { break }
        # a working-set purge makes every process on rig re-fault its pages
        $s = Invoke-Rig "`$t0 = Get-Date; do { `$d = (Get-Counter '\PhysicalDisk(_Total)\% Disk Time' -SampleInterval 1 -MaxSamples 1).CounterSamples[0].CookedValue } while (`$d -gt 15 -and ((Get-Date) - `$t0).TotalSeconds -lt 20); [math]::Round(((Get-Date) - `$t0).TotalSeconds, 1)" 120
        Start-RigApp 'tsubame.exe' "--timing --timing-dirs `"$tiny;<idle>;$warmup;$tiny;$fixture100k`" --reps 1" -Wait -TimeoutSec 300 -Report 'u2-timing.txt'
        Save-Report 'u2-timing.txt' ('cold-timing-{0:d2}.txt' -f $c)
        $last = Get-Content -Path ("$script:ReportShare\cold-timing-{0:d2}.txt" -f $c) -EA SilentlyContinue | Where-Object { $_ -match 'first_frame' } | Select-Object -Last 1
        Write-Host ("bench-rig: cold run {0}/{1} (disks settled after {2} s): {3}" -f $c, $ColdRuns, ($s -join ''), $last)
        $done++
      }
      $coldText = if ($done -gt 0) {
        "measured ($done runs: purge $ColdMode from outside, app warmed up on small folders, only the cold 100k navigation timed)"
      } else {
        "not measured (purge $ColdMode needs elevation on rig: the ssh session and gsudo -n were refused)"
      }
      Write-Host "bench-rig: cold cache: $coldText"
    }
  }

  # 7. the full leak gate, in one process, in a real window (repeated: the
  #    verdict is the worst run, the spread is part of the result)
  if (-not $SkipLeakGate) {
    $timeout = if ($Quick) { 900 } else { 1800 }
    for ($g = 1; $g -le $GateRuns; $g++) {
      $sw = [Diagnostics.Stopwatch]::StartNew()
      Start-RigApp 'tsubame.exe' "--leak-gate --gate-fixture `"$gateFixture`" --gate-navs $GateNavs --gate-tabs $GateTabs" -Wait -TimeoutSec $timeout -Report 'leak-gate.txt'
      Save-Report 'leak-gate.txt' ('leak-gate-{0:d2}.txt' -f $g)
      $exit = try { (Invoke-Rig "Get-Content $(Q "$RunDir\_exit.txt") -EA SilentlyContinue" | Select-Object -First 1) } catch { '' }
      $gateText = Get-Content -Path "$script:ReportShare\leak-gate-$('{0:d2}' -f $g).txt" -EA SilentlyContinue
      $gateLine = $gateText | Where-Object { $_ -like 'gate=*' }
      $priv = $gateText | Where-Object { $_ -like 'delta.private_pct=*' }
      Write-Host "bench-rig: leak gate run $g/$GateRuns took $([int]$sw.Elapsed.TotalSeconds) s: $gateLine $priv (launcher exit '$exit')"
    }
  }

  # 8. hand the raw reports to tsubame-bench app (parsing, thresholds, markdown)
  $meta = @(
    "stamp=$stamp"
    "commit=$commit"
    "branch=$branch"
    "dirty=$dirty"
    "machine=rig (Ryzen 9 5900X, RTX 3080, 4K at 200%)"
    "build=release (lto, codegen-units=1, strip, panic=abort)"
    "exe_bytes=$exeBytes"
    "exe_sha256=$exeSha"
    "mode=$(if ($Quick) { 'quick' } else { 'full' })"
    "runs=$Runs"
    "cache=warm filesystem cache (no purge before these runs)"
    "cold=$coldText"
    "dpi=192"
    "rig_load=$rigLoad"
    "fixture_100k=$fixture100k"
    "gate_fixture=$gateFixture"
    "gate_navs=$GateNavs"
    "gate_tabs=$GateTabs"
    "gate_runs=$GateRuns"
    "slot=$slotNum"
    "reports=$script:ReportDir"
    "notes=launched through a scheduled task in rig's interactive session; every number is the app's own report from that process"
  )
  $meta | Set-Content -Path "$script:ReportShare\meta.txt" -Encoding utf8

  if (-not $Quick) {
    $haveFull = (Test-Path "$script:ReportShare\idle.txt") -and (Test-Path "$script:ReportShare\timing.txt")
    if (-not $haveFull) {
      Write-Host "bench-rig: this run has no timing/idle reports (-LeakGateOnly), so no bench-log section"
    } else {
      Write-Host 'bench-rig: building tsubame-bench on FOX for the log assembly'
      $build = & pwsh -NoProfile -File $cargoCcd1 build -p tsubame-bench 2>&1
      $build | Select-Object -Last 5 | ForEach-Object { Write-Host $_ }
      if ($LASTEXITCODE -ne 0) { throw 'tsubame-bench build failed' }
      $localBench = Join-Path $Project 'target\debug\tsubame-bench.exe'
      if (-not (Test-Path $localBench)) { throw "no $localBench" }
      $appArgs = @('app', '--reports', $script:ReportShare, '--commit', $commit)
      if (-not $NoLog) { $appArgs += @('--append', $LogPath) }
      Write-Host "bench-rig: $localBench $($appArgs -join ' ')"
      & $localBench @appArgs | ForEach-Object { Write-Host $_ }
      if ($LASTEXITCODE -ne 0) { throw 'tsubame-bench app failed' }
      if (-not $NoLog) { Write-Host "bench-rig: appended the section to $LogPath" }
    }
  } else {
    $localBench = Join-Path $Project 'target\debug\tsubame-bench.exe'
    if (Test-Path $localBench) { & $localBench app --reports $script:ReportShare --commit $commit | ForEach-Object { Write-Host $_ } }
    else { Write-Host 'bench-rig: no local tsubame-bench build, skipping the log section (run without -Quick)' }
  }
  Write-Host ("bench-rig: done in {0:N0} s; raw reports in {1}" -f ((Get-Date) - $started).TotalSeconds, $script:ReportDir)
} finally {
  # never leave a process, a task or the fixtures behind
  try { Stop-RigApp } catch { Write-Warning "stop: $($_.Exception.Message)" }
  if ($script:TaskName) {
    try { Invoke-Rig "schtasks /Delete /TN '$script:TaskName' /F 2>`$null | Out-Null" | Out-Null } catch {}
    try { Remove-Item -Force "$RunShare\_bench.cmd" -EA SilentlyContinue } catch {}
  }
  if ($fixturesMade -and -not $KeepFixtures) {
    try { Invoke-Rig "Remove-Item -Recurse -Force -EA SilentlyContinue $(Q $BenchRoot); 'removed'" 600 | Out-Null; Write-Host "bench-rig: deleted fixtures $BenchRoot" } catch { Write-Warning "could not delete $BenchRoot : $($_.Exception.Message)" }
  } elseif ($fixturesMade) {
    Write-Host "bench-rig: fixtures kept in $BenchRoot (-KeepFixtures)"
  }
  if ($slotNum) {
    # release the slot (kills whatever still runs from it and updates its owner record)
    try { & pwsh -NoProfile -File $pushToRig -Project $Project -Stop -Slot $slotNum | Out-Null } catch { Write-Warning "push-to-rig -Stop -Slot $slotNum failed: $($_.Exception.Message)" }
  }
}
