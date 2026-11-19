<#
.SYNOPSIS
  Counts the wake-ups of an idle process, per thread.

.DESCRIPTION
  Reads every thread's context-switch count, CPU time and cycle count from
  NtQuerySystemInformation(SystemProcessInformation) at the start and end of
  each window and prints the difference. A thread that was not switched in
  during a window did not wake up. Each woken thread is named by its thread
  description (Rust sets it from `thread::Builder::name`) and by the module +
  offset of its Win32 start address, so driver and OS threads are told apart
  from Tsubame's own.

  Run it in the same session as the app (rig: the rig-windows PowerShell tool,
  not plain ssh) so OpenThread works. Costs nothing inside the measured process.

.EXAMPLE
  pwsh -NoProfile -File idle-wakes.ps1 -PathLike 'G:\rig-test\tsubame\*' -Windows 3
#>
[CmdletBinding()]
param(
    [int]$ProcessId = 0,
    [string]$PathLike = '',
    [int]$Settle = 3,
    [int]$Seconds = 10,
    [int]$Windows = 3,
    # list every thread, also those that stayed asleep
    [switch]$All
)
$ErrorActionPreference = 'Stop'

if (-not ('IdleWakes.Nt' -as [type])) {
Add-Type -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
namespace IdleWakes {
public class Th { public int Tid; public uint Cs; public long Kernel; public long User; public ulong Cycles; public int State; public int Wait; }
public static class Nt {
    [DllImport("ntdll.dll")] static extern int NtQuerySystemInformation(int cls, IntPtr buf, int len, out int ret);
    [DllImport("ntdll.dll")] static extern int NtQueryInformationThread(IntPtr h, int cls, out IntPtr info, int len, IntPtr ret);
    [DllImport("kernel32.dll")] static extern IntPtr OpenThread(uint access, bool inherit, int tid);
    [DllImport("kernel32.dll")] static extern bool CloseHandle(IntPtr h);
    [DllImport("kernel32.dll")] static extern bool QueryThreadCycleTime(IntPtr h, out ulong cycles);
    [DllImport("kernel32.dll")] static extern int GetThreadDescription(IntPtr h, out IntPtr desc);
    [DllImport("kernel32.dll")] static extern IntPtr LocalFree(IntPtr p);

    // SystemProcessInformation (5): SYSTEM_PROCESS_INFORMATION (0x100 bytes on x64)
    // followed by NumberOfThreads SYSTEM_THREAD_INFORMATION (0x50 bytes each).
    public static Dictionary<int, Th> Snap(int pid) {
        int len = 1 << 20;
        while (true) {
            IntPtr buf = Marshal.AllocHGlobal(len);
            try {
                int ret;
                int st = NtQuerySystemInformation(5, buf, len, out ret);
                if (st == unchecked((int)0xC0000004)) { len = Math.Max(len * 2, ret + 65536); continue; }
                if (st != 0) throw new Exception("NtQuerySystemInformation 0x" + st.ToString("x"));
                var map = new Dictionary<int, Th>();
                long off = 0;
                while (true) {
                    IntPtr p = new IntPtr(buf.ToInt64() + off);
                    int next = Marshal.ReadInt32(p, 0);
                    int n = Marshal.ReadInt32(p, 4);
                    long id = Marshal.ReadIntPtr(p, 0x50).ToInt64();
                    if (id == pid) {
                        for (int i = 0; i < n; i++) {
                            IntPtr t = new IntPtr(p.ToInt64() + 0x100 + i * 0x50);
                            var th = new Th();
                            th.Kernel = Marshal.ReadInt64(t, 0);
                            th.User = Marshal.ReadInt64(t, 8);
                            th.Tid = (int)Marshal.ReadIntPtr(t, 0x30).ToInt64();
                            th.Cs = (uint)Marshal.ReadInt32(t, 0x40);
                            th.State = Marshal.ReadInt32(t, 0x44);
                            th.Wait = Marshal.ReadInt32(t, 0x48);
                            IntPtr h = OpenThread(0x0800 /* QUERY_LIMITED */, false, th.Tid);
                            if (h != IntPtr.Zero) { ulong c; if (QueryThreadCycleTime(h, out c)) th.Cycles = c; CloseHandle(h); }
                            map[th.Tid] = th;
                        }
                        return map;
                    }
                    if (next == 0) return map;
                    off += next;
                }
            } finally { Marshal.FreeHGlobal(buf); }
        }
    }
    public static long Start(int tid) {
        IntPtr h = OpenThread(0x0040 /* QUERY_INFORMATION */, false, tid);
        if (h == IntPtr.Zero) return 0;
        IntPtr a; int st = NtQueryInformationThread(h, 9 /* Win32StartAddress */, out a, IntPtr.Size, IntPtr.Zero);
        CloseHandle(h);
        return st == 0 ? a.ToInt64() : 0;
    }
    public static string Name(int tid) {
        IntPtr h = OpenThread(0x0800, false, tid);
        if (h == IntPtr.Zero) return "";
        IntPtr d; string s = "";
        if (GetThreadDescription(h, out d) >= 0 && d != IntPtr.Zero) { s = Marshal.PtrToStringUni(d); LocalFree(d); }
        CloseHandle(h);
        return s;
    }
}
}
'@
}

if ($ProcessId -eq 0) {
    $p = Get-Process -EA SilentlyContinue | Where-Object { $PathLike -and $_.Path -like $PathLike } | Select-Object -First 1
    if (-not $p) { throw "no process matches -PathLike '$PathLike'" }
    $ProcessId = $p.Id
}
$proc = Get-Process -Id $ProcessId
$mods = @($proc.Modules | ForEach-Object { [pscustomobject]@{ Name = $_.ModuleName; Base = $_.BaseAddress.ToInt64(); End = $_.BaseAddress.ToInt64() + $_.ModuleMemorySize } })
$exeName = [IO.Path]::GetFileName($proc.Path)
function Where-Addr([long]$a) {
    if ($a -eq 0) { return '?' }
    $m = $mods | Where-Object { $a -ge $_.Base -and $a -lt $_.End } | Select-Object -First 1
    if ($m) { return '{0}+0x{1:x}' -f $m.Name, ($a - $m.Base) }
    return '0x{0:x}' -f $a
}
$info = @{}
function Describe([int]$tid) {
    if (-not $info.ContainsKey($tid)) {
        $info[$tid] = [pscustomobject]@{ Name = [IdleWakes.Nt]::Name($tid); Start = Where-Addr ([IdleWakes.Nt]::Start($tid)) }
    }
    $info[$tid]
}

"idle-wakes: pid $ProcessId ($($proc.Path)), settle $Settle s, $Windows x $Seconds s"
Start-Sleep -Seconds $Settle
$totals = @()
for ($w = 1; $w -le $Windows; $w++) {
    $a = [IdleWakes.Nt]::Snap($ProcessId)
    $proc.Refresh(); $cpu0 = $proc.TotalProcessorTime.TotalMilliseconds
    Start-Sleep -Seconds $Seconds
    $b = [IdleWakes.Nt]::Snap($ProcessId)
    $proc.Refresh(); $cpu1 = $proc.TotalProcessorTime.TotalMilliseconds
    $rows = foreach ($tid in $b.Keys) {
        $t1 = $b[$tid]; $t0 = $a[$tid]
        $cs = if ($t0) { [long]$t1.Cs - $t0.Cs } else { [long]$t1.Cs }
        $ms = if ($t0) { (($t1.Kernel - $t0.Kernel) + ($t1.User - $t0.User)) / 10000.0 } else { ($t1.Kernel + $t1.User) / 10000.0 }
        $mc = if ($t0) { ($t1.Cycles - $t0.Cycles) / 1e6 } else { $t1.Cycles / 1e6 }
        if ($cs -gt 0 -or $All) {
            $d = Describe $tid
            [pscustomobject]@{ Tid = $tid; Wakes = $cs; CpuMs = [math]::Round($ms, 1); MCycles = [math]::Round($mc, 2); Name = $d.Name; Start = $d.Start; New = (-not $t0) }
        }
    }
    $gone = @($a.Keys | Where-Object { -not $b.ContainsKey($_) }).Count
    $sum = ($rows | Measure-Object Wakes -Sum).Sum
    if (-not $sum) { $sum = 0 }
    $mine = ($rows | Where-Object { $_.Start -like "$exeName*" } | Measure-Object Wakes -Sum).Sum
    if (-not $mine) { $mine = 0 }
    "window $w : wakes=$sum (exe-started threads $mine) cpu_ms=$([math]::Round($cpu1 - $cpu0, 1)) threads=$($b.Count) new=$(@($rows | Where-Object New).Count) gone=$gone"
    $rows | Sort-Object Wakes -Descending | Format-Table -AutoSize Tid, Wakes, CpuMs, MCycles, Name, Start, New | Out-String -Width 200 | ForEach-Object { $_.TrimEnd() }
    $totals += $sum
}
"idle-wakes: wakes per window = $($totals -join ', ')"
