<#
.SYNOPSIS
  Build a Tsubame release: portable zip, Inno Setup installer, SHA256SUMS, signed update manifest.

.DESCRIPTION
  Output folder (default dist\<version>\), the files a GitHub Release carries:
    tsubame-<ver>-win-x64.zip      portable build (tsubame.exe, tsubame-handler.exe, [tsubame-plughost.exe], portable.txt, LICENSE, README.txt)
    tsubame-<ver>-x64-setup.exe    Inno Setup installer (only when iscc.exe is found)
    tsubame-update.json            update manifest (version, assets, sizes, sha256, notes URL, min_supported_version)
    tsubame-update.json.sig        ed25519 signature of the manifest
    SHA256SUMS                     sha256 of every file above

  Signing key (the manifest signature the app verifies against the key pinned in keys.rs):
    -KeyFile <path>      file holding the 64-hex-char seed, or
    env UPDATE_SIGNING_KEY (override the variable name with -KeyEnv), or
    -DevKey              the public DEV test key: the output is trusted ONLY by test builds
  The private key is passed to tsubame-release by file path / variable NAME. This script never
  reads it into a PowerShell variable, never prints it, never writes it anywhere.

  Optional Authenticode (once a certificate exists), applied to every exe in the package
  (tsubame.exe, tsubame-handler.exe, tsubame-plughost.exe) and to the installer and its uninstaller:
    -SignThumbprint <sha1>  (or env SIGN_CERT_THUMBPRINT)            certificate in the user/machine store
    env ARTIFACT_SIGNING_DLIB + ARTIFACT_SIGNING_METADATA            Azure Artifact Signing (signtool /dlib)
  Needs signtool.exe from the Windows SDK. Not exercised in the repo yet (no certificate exists).

.EXAMPLE
  pwsh tools\release.ps1 -KeyFile $env:USERPROFILE\.tsubame\update-signing.key

.EXAMPLE
  # CI: the key comes from the UPDATE_SIGNING_KEY repository secret (env var)
  pwsh tools\release.ps1

  Channel (docs\updater.md "Channels"):
    -Channel stable      (default) version = Cargo.toml's plain x.y.z, tag vX.Y.Z, manifest tsubame-update.json
    -Channel nightly     test build: version X.Y.(Z+1)-nightly.<run> (-NightlyRun N, computed by
                         `tsubame-release nightly-version`, the one tested implementation of the rule),
                         tag `nightly`, manifest tsubame-update-nightly.json (+ .sig), signed channel "nightly"
  The build embeds the version, channel and commit in the exe (env TSUBAME_VERSION, TSUBAME_CHANNEL,
  TSUBAME_COMMIT, read by the app at compile time); Settings > Updates shows them.

.EXAMPLE
  # local rehearsal, no real key, existing exes, nothing pinned required
  pwsh tools\release.ps1 -DevKey -SkipBuild -ExeDir C:\somewhere\exes -OutDir C:\somewhere\out

.EXAMPLE
  # what the nightly workflow runs (key from env UPDATE_SIGNING_KEY)
  pwsh tools\release.ps1 -Channel nightly -NightlyRun 42 -Commit <sha>
#>
[CmdletBinding()]
param(
    # Release version. Default: [workspace.package] version of the root Cargo.toml.
    [string]$Version,
    # Bump the version in the root Cargo.toml first (patch|minor|major). Mutually exclusive with -Version.
    [ValidateSet('', 'patch', 'minor', 'major')][string]$Bump = '',
    [string]$OutDir,
    [string]$KeyFile,
    [string]$KeyEnv = 'UPDATE_SIGNING_KEY',
    [switch]$DevKey,
    # Sign even if the key's public half is not pinned in keys.rs (the app would reject such a release).
    [switch]$AllowUnpinned,
    [switch]$SkipBuild,
    # Folder with ready-made tsubame.exe, tsubame-handler.exe [+ tsubame-plughost.exe] (implies -SkipBuild).
    [string]$ExeDir,
    [switch]$WithPlughost,
    [string]$MinSupported = '0.0.1',
    [string]$Repo = 'watdafawx/tsubame',
    # cargo command. Default: env TSUBAME_CARGO, else "cargo". A .ps1 path is run with pwsh -File
    # (a wrapper script around cargo).
    [string]$Cargo = $(if ($env:TSUBAME_CARGO) { $env:TSUBAME_CARGO } else { 'cargo' }),
    [string]$SignThumbprint = $env:SIGN_CERT_THUMBPRINT,
    [string]$TimestampUrl = 'http://timestamp.digicert.com',
    [switch]$SkipInstaller,
    [ValidateSet('stable', 'nightly')][string]$Channel = 'stable',
    # nightly only: the CI run number (github.run_number of the nightly workflow)
    [int]$NightlyRun = 0,
    # commit the build is made from (shown in Settings > Updates). Default: env GITHUB_SHA, else git HEAD.
    [string]$Commit
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

function Invoke-Cargo {
    param([string[]]$CargoArgs)
    if ($Cargo -like '*.ps1') { & pwsh -NoProfile -File $Cargo @CargoArgs } else { & $Cargo @CargoArgs }
    if ($LASTEXITCODE -ne 0) { throw "cargo $($CargoArgs -join ' ') failed ($LASTEXITCODE)" }
}

# ---- version ------------------------------------------------------------------
$manifestToml = Join-Path $root 'Cargo.toml'
$toml = Get-Content -Raw $manifestToml
if ($toml -notmatch '(?ms)^\[workspace\.package\]\s*(?:[^\[]*?)^version\s*=\s*"(?<v>[^"]+)"') { throw 'cannot find [workspace.package] version in Cargo.toml' }
$current = $Matches['v']
if ($Bump -and $Version) { throw 'use -Bump or -Version, not both' }
if ($Bump) {
    $p = $current.Split('.')
    if ($p.Count -ne 3) { throw "cannot bump '$current'" }
    switch ($Bump) {
        'major' { $Version = "$([int]$p[0] + 1).0.0" }
        'minor' { $Version = "$($p[0]).$([int]$p[1] + 1).0" }
        'patch' { $Version = "$($p[0]).$($p[1]).$([int]$p[2] + 1)" }
    }
    $new = [regex]::Replace($toml, '(?ms)(^\[workspace\.package\]\s*(?:[^\[]*?)^version\s*=\s*")[^"]+(")', "`${1}$Version`${2}", 1)
    [IO.File]::WriteAllText($manifestToml, $new, (New-Object Text.UTF8Encoding($false)))
    Write-Host "version bumped $current -> $Version (Cargo.toml edited; commit it)"
}
elseif (-not $Version -and $Channel -eq 'stable') { $Version = $current }
if ($Channel -eq 'nightly' -and $Bump) { throw '-Bump is for stable releases; a nightly never edits Cargo.toml' }

# the signing tool (maintainer tool, never shipped); also computes the nightly version
$targetRoot = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $root 'target' }
Invoke-Cargo @('build', '--release', '-p', 'tsubame-update', '--features', 'release-tool', '--bin', 'tsubame-release')
$tool = Join-Path $targetRoot 'release\tsubame-release.exe'
if (-not (Test-Path $tool)) { throw "signing tool not found at $tool" }

if ($Channel -eq 'nightly') {
    if (-not $Version) {
        if ($NightlyRun -le 0) { throw '-Channel nightly needs -NightlyRun <n> (the CI run number) or an explicit -Version x.y.z-nightly.n' }
        $Version = (& $tool nightly-version --base $current --run $NightlyRun | Out-String).Trim()
        if ($LASTEXITCODE -ne 0) { throw "cannot compute the nightly version from $current" }
    }
    if ($Version -notmatch '^\d+\.\d+\.\d+-nightly\.[1-9]\d*$') { throw "nightly version '$Version' must look like x.y.z-nightly.n" }
    $tag = 'nightly'
    $manifestFile = 'tsubame-update-nightly.json'
    # Inno Setup / exe resource need 4 numbers: x.y.z.<run>
    $numeric = ($Version -replace '-nightly\.', '.')
}
else {
    if ($Version -notmatch '^\d+\.\d+\.\d+$') { throw "version '$Version' must be plain x.y.z (stable releases; nightlies use -Channel nightly)" }
    if (-not $ExeDir -and -not $SkipBuild -and $Version -ne $current -and -not $Bump) {
        throw "-Version $Version differs from Cargo.toml ($current); the build would still say $current. Use -Bump, edit Cargo.toml, or pass -ExeDir."
    }
    $tag = "v$Version"
    $manifestFile = 'tsubame-update.json'
    $numeric = "$Version.0"
}
$sigFile = "$manifestFile.sig"
if (-not $Commit) {
    if ($env:GITHUB_SHA) { $Commit = $env:GITHUB_SHA }
    else { $Commit = (& git -C $root rev-parse HEAD 2>$null | Out-String).Trim() }
}
if ($Commit -notmatch '^[0-9a-fA-F]{7,40}$') { $Commit = '' }
if (-not $OutDir) { $OutDir = Join-Path $root "dist\$Version" }
Write-Host "release $Version ($Channel, tag $tag, commit $(if ($Commit) { $Commit.Substring(0, [Math]::Min(12, $Commit.Length)) } else { 'unknown' })) -> $OutDir"

# ---- build ---------------------------------------------------------------------
if ($ExeDir) { $SkipBuild = $true; $exeSrc = (Resolve-Path $ExeDir).Path } else { $exeSrc = Join-Path $targetRoot 'release' }
if (-not $SkipBuild) {
    # The app reads these at compile time (src\buildinfo.rs): the updater compares against
    # TSUBAME_VERSION, so a nightly exe knows it is x.y.z-nightly.n without touching Cargo.toml.
    $env:TSUBAME_VERSION = $Version
    $env:TSUBAME_CHANNEL = $Channel
    $env:TSUBAME_COMMIT = $Commit
    try {
        $pk = @('-p', 'tsubame', '-p', 'tsubame-handler'); if ($WithPlughost) { $pk += @('-p', 'tsubame-plughost') }
        Invoke-Cargo (@('build', '--release') + $pk)
    }
    finally { Remove-Item Env:TSUBAME_VERSION, Env:TSUBAME_CHANNEL, Env:TSUBAME_COMMIT -ErrorAction SilentlyContinue }
}

$exes = @('tsubame.exe'); if ($WithPlughost -or (Test-Path (Join-Path $exeSrc 'tsubame-plughost.exe'))) { $exes += 'tsubame-plughost.exe' }
# The fail-safe folder-handler shim (docs\default-file-manager.md) ships next to tsubame.exe: it is
# signed, hashed in the manifest and swapped by the updater together with the app. A rehearsal
# (-DevKey) may leave it out; a real release may not, or the Settings button stays disabled.
if (Test-Path (Join-Path $exeSrc 'tsubame-handler.exe')) { $exes += 'tsubame-handler.exe' }
elseif (-not $DevKey) { throw "missing tsubame-handler.exe in $exeSrc (a real release must ship the fail-safe shim; -DevKey rehearsals may skip it)" }
foreach ($e in $exes) { if (-not (Test-Path (Join-Path $exeSrc $e))) { throw "missing $e in $exeSrc" } }
# The plugin host must stay runtime-only (security review PH-20): a host over this size almost
# certainly has Cranelift linked in, usually because a build of test targets (or a workspace-wide
# build) overwrote target\release\tsubame-plughost.exe. Checked on the file that is packaged, so
# -SkipBuild and -ExeDir are covered too.
$plughostBudget = 1572864   # 1.5 MiB; a runtime-only host is about 1.0 MiB
if ($exes -contains 'tsubame-plughost.exe') {
    $plughostSize = (Get-Item (Join-Path $exeSrc 'tsubame-plughost.exe')).Length
    if ($plughostSize -gt $plughostBudget) {
        throw ("tsubame-plughost.exe in $exeSrc is $plughostSize bytes, over the $plughostBudget byte budget. " +
            "It probably contains the Cranelift compiler (built together with test targets). Rebuild it alone: cargo build --release -p tsubame-plughost")
    }
    Write-Host ("tsubame-plughost.exe {0:N0} bytes (budget {1:N0})" -f $plughostSize, $plughostBudget)
}

# ---- staging area ----------------------------------------------------------------
$null = New-Item -ItemType Directory -Force $OutDir
$work = Join-Path $OutDir '_work'
if (Test-Path $work) { Remove-Item -Recurse -Force $work }
$null = New-Item -ItemType Directory -Force $work
foreach ($e in $exes) { Copy-Item (Join-Path $exeSrc $e) (Join-Path $work $e) }
if (Test-Path (Join-Path $root 'LICENSE')) { Copy-Item (Join-Path $root 'LICENSE') $work }
# The zip ships README.txt; the repo keeps it as README.md.
$readme = @('README.txt', 'README.md') | Where-Object { Test-Path (Join-Path $root $_) } | Select-Object -First 1
if ($readme) { Copy-Item (Join-Path $root $readme) (Join-Path $work 'README.txt') }

# ---- Authenticode (optional) -----------------------------------------------------
function Find-SignTool {
    $c = Get-Command signtool.exe -ErrorAction SilentlyContinue
    if ($c) { return $c.Source }
    $kits = Join-Path ${env:ProgramFiles(x86)} 'Windows Kits\10\bin'
    if (Test-Path $kits) {
        $f = Get-ChildItem $kits -Recurse -Filter signtool.exe -ErrorAction SilentlyContinue | Where-Object FullName -like '*\x64\*' | Sort-Object FullName -Descending | Select-Object -First 1
        if ($f) { return $f.FullName }
    }
    return $null
}
$signMode = $null
if ($SignThumbprint) { $signMode = 'thumbprint' }
elseif ($env:ARTIFACT_SIGNING_DLIB -and $env:ARTIFACT_SIGNING_METADATA) { $signMode = 'artifact-signing' }
$signtool = $null
if ($signMode) {
    $signtool = Find-SignTool
    if (-not $signtool) { throw 'Authenticode signing is configured but signtool.exe was not found (install the Windows SDK)' }
}
function Get-SignArgs {
    switch ($signMode) {
        'thumbprint' { return @('sign', '/fd', 'sha256', '/tr', $TimestampUrl, '/td', 'sha256', '/sha1', $SignThumbprint) }
        'artifact-signing' { return @('sign', '/fd', 'sha256', '/tr', 'http://timestamp.acs.microsoft.com', '/td', 'sha256', '/dlib', $env:ARTIFACT_SIGNING_DLIB, '/dmdf', $env:ARTIFACT_SIGNING_METADATA) }
    }
}
function Invoke-AuthenticodeSign([string]$path) {
    if (-not $signMode) { return }
    & $signtool @(Get-SignArgs) $path | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "signtool failed on $path" }
    Write-Host "authenticode: signed $(Split-Path -Leaf $path)"
}
if ($signMode) { foreach ($e in $exes) { Invoke-AuthenticodeSign (Join-Path $work $e) } } else { Write-Host 'authenticode: not configured (ed25519-signed manifest only)' }

# ---- portable zip ----------------------------------------------------------------
$zipName = "tsubame-$Version-win-x64.zip"
$zipPath = Join-Path $OutDir $zipName
if (Test-Path $zipPath) { Remove-Item -Force $zipPath }
[IO.File]::WriteAllText((Join-Path $work 'portable.txt'),
    "Tsubame portable mode.`r`nWhile this file sits next to tsubame.exe, settings and update state are kept in the 'data' folder here instead of %LOCALAPPDATA%\Tsubame.`r`nDelete this file to switch to the normal per-user location.`r`n",
    (New-Object Text.UTF8Encoding($false)))
Add-Type -AssemblyName System.IO.Compression, System.IO.Compression.FileSystem
$zip = [IO.Compression.ZipFile]::Open($zipPath, 'Create')
try {
    foreach ($f in (Get-ChildItem $work -File | Sort-Object Name)) {
        $null = [IO.Compression.ZipFileExtensions]::CreateEntryFromFile($zip, $f.FullName, $f.Name, [IO.Compression.CompressionLevel]::Optimal)
    }
}
finally { $zip.Dispose() }

# ---- installer -------------------------------------------------------------------
$setupName = "tsubame-$Version-x64-setup.exe"
$setupPath = Join-Path $OutDir $setupName
if (Test-Path $setupPath) { Remove-Item -Force $setupPath }
$iscc = $null
if (-not $SkipInstaller) {
    foreach ($cand in @($env:ISCC, (Get-Command iscc.exe -ErrorAction SilentlyContinue).Source,
            (Join-Path ${env:ProgramFiles(x86)} 'Inno Setup 6\ISCC.exe'), (Join-Path $env:ProgramFiles 'Inno Setup 6\ISCC.exe'),
            (Join-Path $env:LOCALAPPDATA 'Programs\Inno Setup 6\ISCC.exe'))) {
        if ($cand -and (Test-Path $cand)) { $iscc = $cand; break }
    }
}
if ($iscc) {
    $isArgs = @("/DAppVersion=$Version", "/DNumericVersion=$numeric","/DSourceDir=$work", "/DOutDir=$OutDir", '/Qp')
    if ($signMode) {
        $st = '"' + $signtool + '" ' + ((Get-SignArgs | ForEach-Object { if ($_ -match '\s') { '"' + $_ + '"' } else { $_ } }) -join ' ') + ' $f'
        $isArgs += @('/DSignTool', "/Stsubame=$st")
    }
    & $iscc @isArgs (Join-Path $root 'installer\tsubame.iss')
    if ($LASTEXITCODE -ne 0) { throw "iscc failed ($LASTEXITCODE)" }
    if (-not (Test-Path $setupPath)) { throw "iscc did not produce $setupPath" }
}
else { Write-Warning 'iscc.exe (Inno Setup 6) not found: skipping the installer. Set env ISCC or install Inno Setup.' }

# ---- manifest ----------------------------------------------------------------------
function Get-Sha256([string]$p) { (Get-FileHash -Algorithm SHA256 -LiteralPath $p).Hash.ToLowerInvariant() }
$contents = @()
foreach ($e in $exes) {
    $p = Join-Path $work $e
    $contents += [ordered]@{ name = $e; size = [int64](Get-Item $p).Length; sha256 = (Get-Sha256 $p) }
}
$assets = @([ordered]@{ kind = 'portable'; arch = 'x64'; file = $zipName; size = [int64](Get-Item $zipPath).Length; sha256 = (Get-Sha256 $zipPath); contents = $contents })
if (Test-Path $setupPath) {
    $assets += [ordered]@{ kind = 'setup'; arch = 'x64'; file = $setupName; size = [int64](Get-Item $setupPath).Length; sha256 = (Get-Sha256 $setupPath) }
}
$manifest = [ordered]@{
    schema                = 1
    version               = $Version
    channel               = $Channel
    released              = (Get-Date).ToUniversalTime().ToString('yyyy-MM-ddTHH:mm:ssZ')
    notes_url             = "https://github.com/$Repo/releases/tag/$tag"
    min_supported_version = $MinSupported
    assets                = $assets
}
$mPath = Join-Path $OutDir $manifestFile
$sPath = Join-Path $OutDir $sigFile
[IO.File]::WriteAllText($mPath, ($manifest | ConvertTo-Json -Depth 6), (New-Object Text.UTF8Encoding($false)))

# ---- sign the manifest -------------------------------------------------------------
$signArgs = @('sign', '--manifest', $mPath, '--out', $sPath)
if ($DevKey) { $signArgs += '--dev-test-key' }
elseif ($KeyFile) { $signArgs += @('--key-file', $KeyFile) }
elseif ([Environment]::GetEnvironmentVariable($KeyEnv)) { $signArgs += @('--key-env', $KeyEnv) }
else { throw "no signing key: pass -KeyFile, set env $KeyEnv, or use -DevKey (rehearsal only)" }
if ($AllowUnpinned) { $signArgs += '--allow-unpinned' }
& $tool @signArgs
if ($LASTEXITCODE -ne 0) { throw 'manifest signing failed' }
$verifyArgs = @('verify', '--manifest', $mPath, '--sig', $sPath); if ($DevKey) { $verifyArgs += '--dev-test-key' }
if (-not $AllowUnpinned -or $DevKey) {
    & $tool @verifyArgs
    if ($LASTEXITCODE -ne 0) { throw 'self-verification of the signed manifest failed' }
}

# ---- SHA256SUMS ----------------------------------------------------------------------
$sumFiles = @($zipName, $manifestFile, $sigFile); if (Test-Path $setupPath) { $sumFiles = @($zipName, $setupName, $manifestFile, $sigFile) }
$sums = ($sumFiles | ForEach-Object { '{0}  {1}' -f (Get-Sha256 (Join-Path $OutDir $_)), $_ }) -join "`n"
[IO.File]::WriteAllText((Join-Path $OutDir 'SHA256SUMS'), $sums + "`n", (New-Object Text.UTF8Encoding($false)))

Remove-Item -Recurse -Force $work

# ---- report --------------------------------------------------------------------------
Write-Host ''
Write-Host "Release $Version ($Channel, tag $tag) files in ${OutDir}:"
Get-ChildItem $OutDir -File | Sort-Object Name | ForEach-Object { '  {0,-40} {1,12:N0} bytes  {2}' -f $_.Name, $_.Length, (Get-Sha256 $_.FullName).Substring(0, 16) }
if ($DevKey) { Write-Warning 'signed with the DEV TEST key: release builds of the app will reject this. Rehearsal only; never upload it.' }
Write-Host ''
Write-Host "Upload exactly these files to the GitHub Release for tag $tag (the CI workflow does it for you)."
