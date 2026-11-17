<#
.SYNOPSIS
  Compile installer\tsubame.iss and prove install + uninstall are clean. RUN ON RIG, NOT ON FOX
  (the installer is a GUI program, even when silent; it never runs on the maintainer's PC).

.DESCRIPTION
  Expects  <Work>\src\installer\tsubame.iss  and  <Work>\src\exes\{tsubame.exe,tsubame-plughost.exe}
  (stand-in files are fine). Builds the installer with iscc, then:
    1. silent install, default type    -> only tsubame.exe (+ uninstaller); HKCU Uninstall entry; Start menu shortcut
    2. diff of Run keys, shell/file-association keys, scheduled tasks, services before/after: must be empty
    3. fake updater leftovers (*.old, *.new) are removed by the uninstaller; user data is NOT touched
    4. silent uninstall                -> files, registry entry and shortcuts all gone
    5. repeat with the plugin-host component + desktop shortcut
  Everything installs under <Work>\install*. Exit code 0 = all checks passed.
#>
[CmdletBinding()]
param(
    [string]$Work = 'G:\rig-test\tsubame-d2d',
    [string]$Version = '0.2.0',
    [switch]$Cleanup
)
$ErrorActionPreference = 'Stop'
$AppKey = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\{B3F1A7C4-5D2E-4C8A-9F0B-7A6E1D3C2B48}_is1'
$script:failed = 0
function Check([string]$name, [bool]$ok, [string]$detail = '') {
    if ($ok) { Write-Host "PASS  $name" } else { Write-Host "FAIL  $name  $detail"; $script:failed++ }
}

$iscc = @($env:ISCC, "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe", "$env:ProgramFiles\Inno Setup 6\ISCC.exe", "$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe") |
    Where-Object { $_ -and (Test-Path $_) } | Select-Object -First 1
if (-not $iscc) { throw 'ISCC.exe not found' }
$out = Join-Path $Work 'out'; $null = New-Item -ItemType Directory -Force $out
Write-Host "== compile with $iscc"
& $iscc "/DAppVersion=$Version" "/DNumericVersion=$Version.0" "/DSourceDir=$Work\src\exes" "/DOutDir=$out" /Q "$Work\src\installer\tsubame.iss"
if ($LASTEXITCODE -ne 0) { throw "iscc failed ($LASTEXITCODE)" }
$setup = Join-Path $out "tsubame-$Version-x64-setup.exe"
Check 'installer built' (Test-Path $setup)
Write-Host ("   {0}  {1:N0} bytes" -f $setup, (Get-Item $setup).Length)

function Snap {
    $run = foreach ($p in 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run', 'HKLM:\Software\Microsoft\Windows\CurrentVersion\Run') {
        $k = Get-Item $p -ErrorAction SilentlyContinue; if ($k) { $k.GetValueNames() | ForEach-Object { "$p|$_" } }
    }
    [ordered]@{
        Run      = @($run | Sort-Object)
        Classes  = @(Get-ChildItem 'HKCU:\Software\Classes' -ErrorAction SilentlyContinue | ForEach-Object PSChildName | Sort-Object)
        Tasks    = @(Get-ScheduledTask -ErrorAction SilentlyContinue | ForEach-Object { $_.TaskPath + $_.TaskName } | Sort-Object)
        Services = @(Get-Service | ForEach-Object Name | Sort-Object)
        Uninst   = @(Get-ChildItem 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall' -ErrorAction SilentlyContinue | ForEach-Object PSChildName | Sort-Object)
        StartMenu = @(Get-ChildItem ([Environment]::GetFolderPath('Programs')) -Recurse -ErrorAction SilentlyContinue | ForEach-Object FullName | Sort-Object)
        Desktop  = @(Get-ChildItem ([Environment]::GetFolderPath('Desktop')) -ErrorAction SilentlyContinue | ForEach-Object FullName | Sort-Object)
    }
}
function Get-SnapDiff([hashtable]$a, [hashtable]$b, [string[]]$keys) {
    foreach ($k in $keys) { $d = Compare-Object @($a[$k]) @($b[$k]) -ErrorAction SilentlyContinue; if ($d) { "$k : $(($d | ForEach-Object { $_.SideIndicator + ' ' + $_.InputObject }) -join '; ')" } }
}
function Wait-Gone([string]$dir) { for ($i = 0; $i -lt 120; $i++) { if (-not (Test-Path $dir)) { return $true }; Start-Sleep -Milliseconds 500 }; return $false }
function Install([string]$dir, [string[]]$extra) {
    $p = Start-Process -FilePath $setup -ArgumentList (@('/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART', '/SP-', "/DIR=`"$dir`"", "/LOG=`"$Work\install.log`"") + $extra) -Wait -PassThru
    return $p.ExitCode
}
function Uninstall([string]$dir) {
    $u = Join-Path $dir 'unins000.exe'
    $p = Start-Process -FilePath $u -ArgumentList @('/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART') -Wait -PassThru
    $gone = Wait-Gone $dir
    return @{ Code = $p.ExitCode; Gone = $gone }
}

$dataDir = Join-Path $env:LOCALAPPDATA 'Tsubame'
$dataPreexisted = Test-Path $dataDir
$sentinel = Join-Path $dataDir 'update\state.json'
$sentinelMade = $false
if (-not (Test-Path $sentinel)) { $null = New-Item -ItemType Directory -Force (Split-Path $sentinel); '{"sentinel":"installer-test user data"}' | Set-Content $sentinel; $sentinelMade = $true }

$before = Snap
try {
    # ---------------- round 1: default install ----------------
    Write-Host "`n== 1. silent install, default components"
    $dir = Join-Path $Work 'install'
    $code = Install $dir @()
    Check "installer exit code 0" ($code -eq 0) "got $code"
    Get-ChildItem $dir | ForEach-Object { Write-Host ("   {0,-24} {1,10:N0} bytes" -f $_.Name, $_.Length) }
    Check 'tsubame.exe installed' (Test-Path "$dir\tsubame.exe")
    Check 'plugin host NOT installed by default' (-not (Test-Path "$dir\tsubame-plughost.exe"))
    Check 'portable.txt NOT installed' (-not (Test-Path "$dir\portable.txt"))
    Check 'uninstaller present (unins000.exe)' (Test-Path "$dir\unins000.exe")
    $k = Get-ItemProperty $AppKey -ErrorAction SilentlyContinue
    Check 'HKCU Uninstall entry exists' ($null -ne $k)
    if ($k) {
        Write-Host ("   DisplayName={0}  DisplayVersion={1}  Publisher={2}" -f $k.DisplayName, $k.DisplayVersion, $k.Publisher)
        Write-Host ("   InstallLocation={0}" -f $k.InstallLocation)
        Write-Host ("   UninstallString={0}" -f $k.UninstallString)
        Check 'entry has name, version, location, uninstall string' ($k.DisplayName -eq 'Tsubame' -and $k.DisplayVersion -eq $Version -and $k.InstallLocation -like "$dir*" -and $k.UninstallString -like '*unins000.exe*')
    }
    Check 'no HKLM Uninstall entry (per-user)' (-not (Test-Path 'HKLM:\Software\Microsoft\Windows\CurrentVersion\Uninstall\{B3F1A7C4-5D2E-4C8A-9F0B-7A6E1D3C2B48}_is1'))
    $lnk = Join-Path ([Environment]::GetFolderPath('Programs')) 'Tsubame.lnk'
    Check 'Start menu shortcut created' (Test-Path $lnk)
    if (Test-Path $lnk) { $t = (New-Object -ComObject WScript.Shell).CreateShortcut($lnk).TargetPath; Check 'shortcut points at the installed exe' ($t -eq "$dir\tsubame.exe") $t }
    Check 'no desktop shortcut by default' (-not (Test-Path (Join-Path ([Environment]::GetFolderPath('Desktop')) 'Tsubame.lnk')))
    $after = Snap
    $d = Get-SnapDiff $before $after @('Run', 'Classes', 'Tasks', 'Services')
    Check 'NO startup entry, shell/file-association key, scheduled task or service added' (-not $d) ($d -join ' | ')
    $d2 = Get-SnapDiff $before $after @('Uninst', 'StartMenu')
    Write-Host "   registry/shortcut changes made by install:"; $d2 | ForEach-Object { Write-Host "     $_" }

    # the updater's leftovers next to the exe go with the uninstaller; user data does not
    Set-Content "$dir\tsubame.exe.old" 'old'; Set-Content "$dir\tsubame.exe.new" 'new'; Set-Content "$dir\tsubame.exe.bad" 'bad'
    Write-Host "`n== 2. silent uninstall"
    $r = Uninstall $dir
    Check 'uninstaller exit code 0' ($r.Code -eq 0) "got $($r.Code)"
    Check 'install folder fully removed (incl. *.old/*.new/*.bad)' $r.Gone
    Check 'HKCU Uninstall entry removed' (-not (Test-Path $AppKey))
    Check 'Start menu shortcut removed' (-not (Test-Path $lnk))
    Check 'user data (%LOCALAPPDATA%\Tsubame) untouched' (Test-Path $sentinel)
    $end = Snap
    $d = Get-SnapDiff $before $end @('Run', 'Classes', 'Tasks', 'Services', 'Uninst', 'StartMenu', 'Desktop')
    Check 'registry, tasks, services, shortcuts identical to before the install' (-not $d) ($d -join ' | ')

    # ---------------- round 2: optional component + desktop shortcut ----------------
    Write-Host "`n== 3. silent install with plugin host + desktop shortcut"
    $dir2 = Join-Path $Work 'install2'
    $code = Install $dir2 @('/COMPONENTS="main,plughost"', '/TASKS="desktopicon"')
    Check 'installer exit code 0' ($code -eq 0) "got $code"
    Check 'tsubame.exe and tsubame-plughost.exe installed' ((Test-Path "$dir2\tsubame.exe") -and (Test-Path "$dir2\tsubame-plughost.exe"))
    $dl = Join-Path ([Environment]::GetFolderPath('Desktop')) 'Tsubame.lnk'
    Check 'desktop shortcut created on request' (Test-Path $dl)
    $r = Uninstall $dir2
    Check 'uninstall: folder gone' $r.Gone
    Check 'desktop shortcut removed' (-not (Test-Path $dl))
    $end = Snap
    $d = Get-SnapDiff $before $end @('Run', 'Classes', 'Tasks', 'Services', 'Uninst', 'StartMenu', 'Desktop')
    Check 'everything identical to before' (-not $d) ($d -join ' | ')
}
finally {
    if ($sentinelMade) { Remove-Item $sentinel -Force -ErrorAction SilentlyContinue; if (-not $dataPreexisted) { Remove-Item (Split-Path $sentinel) -Recurse -Force -ErrorAction SilentlyContinue; Remove-Item $dataDir -Force -ErrorAction SilentlyContinue } }
    foreach ($d in "$Work\install", "$Work\install2") { if (Test-Path $d) { Remove-Item -Recurse -Force $d -ErrorAction SilentlyContinue } }
    if ($Cleanup) { Remove-Item -Recurse -Force $Work -ErrorAction SilentlyContinue }
}
if ($script:failed -eq 0) { Write-Host "`ninstaller test: ALL CHECKS PASSED"; exit 0 }
Write-Host "`ninstaller test: $($script:failed) CHECK(S) FAILED"; exit 1
