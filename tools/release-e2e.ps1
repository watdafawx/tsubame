<#
.SYNOPSIS
  End-to-end rehearsal of the updater, headless, no network, nothing GUI.

.DESCRIPTION
  1. builds the updater CLI + the fake-GitHub test server (debug build, feature `test-server`)
  2. builds a real release (tools\release.ps1, DEV test key) of two stand-in versions
  3. serves the 0.2.0 release on http://127.0.0.1 and runs `tsubame-update check/download/apply/rollback`
     against it, then repeats with a tampered zip, a tampered manifest and a wrong signature

  The "app" is a stand-in file (never executed), so this runs anywhere, also on CI.
  Everything lives under -Work (default dist\e2e, gitignored). Exit code 0 = all checks passed.

  The test CLI trusts the public DEV key and accepts http://127.0.0.1. Both only exist in builds with
  the `test-server` feature, which refuses to compile in release mode.
#>
[CmdletBinding()]
param(
    [string]$Work,
    [string]$Cargo = $(if ($env:TSUBAME_CARGO) { $env:TSUBAME_CARGO } else { 'cargo' }),
    [switch]$Keep
)
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root
if (-not $Work) { $Work = Join-Path $root 'dist\e2e' }
$Work = [IO.Path]::GetFullPath($Work)
if (Test-Path $Work) { Remove-Item -Recurse -Force $Work }
$null = New-Item -ItemType Directory -Force $Work

$script:failed = 0
function Check([string]$name, [bool]$ok, [string]$detail = '') {
    if ($ok) { Write-Host "PASS  $name" -ForegroundColor Green } else { Write-Host "FAIL  $name  $detail" -ForegroundColor Red; $script:failed++ }
}
function Invoke-Cargo([string[]]$CargoArgs) {
    if ($Cargo -like '*.ps1') { & pwsh -NoProfile -File $Cargo @CargoArgs } else { & $Cargo @CargoArgs }
    if ($LASTEXITCODE -ne 0) { throw "cargo $($CargoArgs -join ' ') failed" }
}
function Sha([string]$p) { (Get-FileHash -Algorithm SHA256 -LiteralPath $p).Hash.ToLowerInvariant() }

# ---- 1. build the test binaries -------------------------------------------------------
Write-Host '== build test CLI + test server (debug, feature test-server)'
Invoke-Cargo @('build', '-p', 'tsubame-update', '--features', 'test-server', '--bin', 'tsubame-update', '--bin', 'tsubame-update-testserver')
$targetRoot = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $root 'target' }
$cli = Join-Path $Work 'tsubame-update.exe'; $srv = Join-Path $Work 'tsubame-update-testserver.exe'
Copy-Item (Join-Path $targetRoot 'debug\tsubame-update.exe') $cli
Copy-Item (Join-Path $targetRoot 'debug\tsubame-update-testserver.exe') $srv

# ---- 2. two stand-in releases built by the real release script -------------------------
function New-StandIn([string]$ver) {
    $d = Join-Path $Work "exes-$ver"; $null = New-Item -ItemType Directory -Force $d
    $bytes = [Text.Encoding]::ASCII.GetBytes("TSUBAME-STANDIN-EXE version=$ver`n") + (1..4096 | ForEach-Object { [byte](($_ * 7 + $ver.Length) % 251) })
    [IO.File]::WriteAllBytes((Join-Path $d 'tsubame.exe'), $bytes)
    return $d
}
$rel = @{}
foreach ($v in '0.1.0', '0.2.0') {
    Write-Host "== release script, version $v"
    $exes = New-StandIn $v
    & pwsh -NoProfile -File (Join-Path $PSScriptRoot 'release.ps1') -Version $v -DevKey -SkipBuild -ExeDir $exes -SkipInstaller -OutDir (Join-Path $Work "release-$v") -Cargo $Cargo
    if ($LASTEXITCODE -ne 0) { throw "release.ps1 failed for $v" }
    $rel[$v] = Join-Path $Work "release-$v"
}
Check 'release script made zip, manifest, signature, SHA256SUMS' ((@('tsubame-0.2.0-win-x64.zip', 'tsubame-update.json', 'tsubame-update.json.sig', 'SHA256SUMS') | Where-Object { -not (Test-Path (Join-Path $rel['0.2.0'] $_)) }).Count -eq 0)
# SHA256SUMS agrees with the files
$sumsOk = $true
foreach ($line in Get-Content (Join-Path $rel['0.2.0'] 'SHA256SUMS')) { $h, $f = $line -split '\s+', 2; if ((Sha (Join-Path $rel['0.2.0'] $f)) -ne $h) { $sumsOk = $false } }
Check 'SHA256SUMS matches the files' $sumsOk

# ---- helpers: server + one scenario sandbox ----------------------------------------------
$script:serverProc = $null
function Start-Server([string]$dir) {
    Stop-Server
    $log = Join-Path $Work 'server.out'; if (Test-Path $log) { Remove-Item $log }
    $script:serverProc = Start-Process -FilePath $srv -ArgumentList @('--dir', "`"$dir`"", '--port', '0') -WindowStyle Hidden -PassThru -RedirectStandardOutput $log
    for ($i = 0; $i -lt 100; $i++) {
        if ((Test-Path $log) -and (Get-Content $log -ErrorAction SilentlyContinue | Select-String 'listening (http://\S+)')) { break }
        Start-Sleep -Milliseconds 100
    }
    $m = Get-Content $log | Select-String 'listening (http://\S+)'
    if (-not $m) { throw 'test server did not start' }
    return $m.Matches[0].Groups[1].Value
}
function Stop-Server { if ($script:serverProc -and -not $script:serverProc.HasExited) { Stop-Process -Id $script:serverProc.Id -Force } $script:serverProc = $null }
function New-Sandbox([string]$name, [string]$installedVersion) {
    $s = Join-Path $Work "sandbox-$name"
    $null = New-Item -ItemType Directory -Force (Join-Path $s 'app'), (Join-Path $s 'data')
    Copy-Item (Join-Path (Join-Path $Work "exes-$installedVersion") 'tsubame.exe') (Join-Path $s 'app\tsubame.exe')
    return $s
}
function Run-Cli([string]$sandbox, [string]$base, [string[]]$cmd) {
    $a = @($cmd) + @('--install-dir', (Join-Path $sandbox 'app'), '--data-dir', (Join-Path $sandbox 'data'), '--api-base', $base, '--current-version', '0.1.0')
    $out = & $cli @a 2>&1 | Out-String
    $code = $LASTEXITCODE
    Write-Host "  > tsubame-update $($cmd -join ' ')  [exit $code]"
    ($out.TrimEnd() -split "`r?`n") | ForEach-Object { Write-Host "      $_" }
    return @{ Code = $code; Out = $out }
}

try {
    $old = Join-Path (Join-Path $Work 'exes-0.1.0') 'tsubame.exe'; $new = Join-Path (Join-Path $Work 'exes-0.2.0') 'tsubame.exe'
    $oldHash = Sha $old; $newHash = Sha $new

    # ---- 3a. happy path ------------------------------------------------------------------
    Write-Host "`n== A. check -> download -> apply (0.1.0 -> 0.2.0)"
    $base = Start-Server $rel['0.2.0']
    $sb = New-Sandbox 'happy' '0.1.0'; $exe = Join-Path $sb 'app\tsubame.exe'
    Check 'installed exe starts as 0.1.0' ((Sha $exe) -eq $oldHash)
    $r = Run-Cli $sb $base @('check', '--force'); Check 'check: exit 3, update available 0.1.0 -> 0.2.0' ($r.Code -eq 3 -and $r.Out -match '0\.1\.0 -> 0\.2\.0')
    $r = Run-Cli $sb $base @('download'); Check 'download: exit 0, staged under data\update\staging' ($r.Code -eq 0 -and $r.Out -match 'staged 0\.2\.0')
    Check 'staging is inside the data dir (not %TEMP%)' ($r.Out -match [regex]::Escape((Join-Path $sb 'data\update\staging')))
    Check 'installed exe still untouched after download' ((Sha $exe) -eq $oldHash)
    $r = Run-Cli $sb $base @('apply'); Check 'apply: exit 0' ($r.Code -eq 0 -and $r.Out -match 'applied 0\.2\.0 \(was 0\.1\.0\)')
    Check 'VERSION CHANGED: tsubame.exe is now the 0.2.0 file' ((Sha $exe) -eq $newHash)
    Check 'previous version kept as tsubame.exe.old' ((Test-Path "$exe.old") -and ((Sha "$exe.old") -eq $oldHash))
    $r = Run-Cli $sb $base @('status'); Check 'status: running 0.2.0, previous 0.1.0' ($r.Out -match 'running:\s+0\.2\.0' -and $r.Out -match 'previous: 0\.1\.0')
    # note: Run-Cli passes --current-version 0.1.0; state records 0.2.0 -> the explicit flag wins, so ask without it:
    $out = & $cli status --install-dir (Join-Path $sb 'app') --data-dir (Join-Path $sb 'data') --api-base $base 2>&1 | Out-String
    Check 'status without --current-version reads the recorded 0.2.0' ($out -match 'running:\s+0\.2\.0')
    $out = & $cli check --force --install-dir (Join-Path $sb 'app') --data-dir (Join-Path $sb 'data') --api-base $base 2>&1 | Out-String
    Check 'check again: up to date (exit 0)' ($LASTEXITCODE -eq 0 -and $out -match 'up to date \(0\.2\.0\)')

    # ---- 3b. rollback -----------------------------------------------------------------------
    Write-Host "`n== B. one-click rollback"
    $out = & $cli rollback --install-dir (Join-Path $sb 'app') --data-dir (Join-Path $sb 'data') --api-base $base 2>&1 | Out-String
    Write-Host "      $($out.Trim())"
    Check 'rollback: exit 0' ($LASTEXITCODE -eq 0 -and $out -match 'rolled back 0\.2\.0 -> 0\.1\.0')
    Check 'ROLLBACK RESTORED the 0.1.0 file' ((Sha $exe) -eq $oldHash)
    $out = & $cli check --force --install-dir (Join-Path $sb 'app') --data-dir (Join-Path $sb 'data') --api-base $base 2>&1 | Out-String
    Check 'rolled-back version is not offered again' ($LASTEXITCODE -eq 0 -and $out -match 'up to date \(0\.1\.0\)')
    $out = & $cli rollback --install-dir (Join-Path $sb 'app') --data-dir (Join-Path $sb 'data') --api-base $base 2>&1 | Out-String
    Check 'second rollback refused (only one step back)' ($LASTEXITCODE -eq 1)

    # ---- 3c. tampered zip --------------------------------------------------------------------
    Write-Host "`n== C. tampered zip (one byte flipped after signing)"
    $bad = Join-Path $Work 'release-tampered-zip'; Copy-Item $rel['0.2.0'] $bad -Recurse
    $zp = Join-Path $bad 'tsubame-0.2.0-win-x64.zip'; $zb = [IO.File]::ReadAllBytes($zp); $zb[[int]($zb.Length / 2)] = $zb[[int]($zb.Length / 2)] -bxor 0x55; [IO.File]::WriteAllBytes($zp, $zb)
    $base = Start-Server $bad
    $sb2 = New-Sandbox 'tamper-zip' '0.1.0'
    $r = Run-Cli $sb2 $base @('check', '--force'); Check 'check passes (manifest is genuine)' ($r.Code -eq 3)
    $r = Run-Cli $sb2 $base @('download'); Check 'download REFUSED: hash mismatch' ($r.Code -eq 1 -and $r.Out -match '(?i)hash')
    Check 'nothing staged, exe unchanged' (-not (Test-Path (Join-Path $sb2 'data\update\staging\0.2.0')) -and ((Sha (Join-Path $sb2 'app\tsubame.exe')) -eq $oldHash))
    $r = Run-Cli $sb2 $base @('apply'); Check 'apply has nothing to apply' ($r.Code -eq 1)

    # ---- 3d. tampered manifest -----------------------------------------------------------------
    Write-Host "`n== D. manifest edited after signing"
    $bad = Join-Path $Work 'release-tampered-manifest'; Copy-Item $rel['0.2.0'] $bad -Recurse
    $mp = Join-Path $bad 'tsubame-update.json'
    [IO.File]::WriteAllText($mp, ([IO.File]::ReadAllText($mp) -replace '"min_supported_version": "0.0.1"', '"min_supported_version": "0.0.2"'), (New-Object Text.UTF8Encoding($false)))
    $base = Start-Server $bad
    $sb3 = New-Sandbox 'tamper-manifest' '0.1.0'
    $r = Run-Cli $sb3 $base @('check', '--force'); Check 'check REFUSED: bad signature' ($r.Code -eq 1 -and $r.Out -match '(?i)signature')

    # ---- 3e. wrong signature --------------------------------------------------------------------
    Write-Host "`n== E. manifest signed with a different key"
    $tool = Join-Path $targetRoot 'release\tsubame-release.exe'
    $otherKey = Join-Path $Work 'other-key.txt'
    & $tool keygen --out $otherKey | Out-Null
    $bad = Join-Path $Work 'release-wrong-key'; Copy-Item $rel['0.2.0'] $bad -Recurse
    & $tool sign --manifest (Join-Path $bad 'tsubame-update.json') --out (Join-Path $bad 'tsubame-update.json.sig') --key-file $otherKey --allow-unpinned | Out-Null
    $base = Start-Server $bad
    $sb4 = New-Sandbox 'wrong-key' '0.1.0'
    $r = Run-Cli $sb4 $base @('check', '--force'); Check 'check REFUSED: signature not from a pinned key' ($r.Code -eq 1 -and $r.Out -match '(?i)signature')
    $r = Run-Cli $sb4 $base @('download'); Check 'download refused too' ($r.Code -eq 1)
    Check 'exe unchanged' ((Sha (Join-Path $sb4 'app\tsubame.exe')) -eq $oldHash)

    # ---- 3f. release tool refuses an unpinned key -------------------------------------------------
    Write-Host "`n== F. release tool refuses to sign with an unpinned key"
    $scratch = Join-Path $Work 'sign-test'; $null = New-Item -ItemType Directory $scratch
    Copy-Item (Join-Path $rel['0.2.0'] 'tsubame-update.json') $scratch
    $out = & $tool sign --manifest (Join-Path $scratch 'tsubame-update.json') --out (Join-Path $scratch 'x.sig') --key-file $otherKey 2>&1 | Out-String
    Check 'sign with unpinned key: refused (exit 1, no signature written)' ($LASTEXITCODE -eq 1 -and -not (Test-Path (Join-Path $scratch 'x.sig')) -and $out -match 'not in RELEASE_PUBLIC_KEYS_HEX')
    $seed = (Get-Content $otherKey -Raw).Trim()
    Check 'sign/keygen output never contains the private seed' (-not ($out -like "*$seed*"))
}
finally {
    Stop-Server
    if (-not $Keep) { Get-ChildItem $Work -Directory | Where-Object Name -like 'sandbox-*' | Remove-Item -Recurse -Force -ErrorAction SilentlyContinue }
}

Write-Host ''
if ($script:failed -eq 0) { Write-Host 'e2e: ALL CHECKS PASSED' -ForegroundColor Green; exit 0 }
Write-Host "e2e: $($script:failed) CHECK(S) FAILED" -ForegroundColor Red
exit 1
