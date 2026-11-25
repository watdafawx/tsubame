<#
Builds a plugin from this folder and installs it into a cache folder: the
`.cwasm` Tsubame loads and the install record that pins it.

  pwsh plugins\build-plugin.ps1 plugins\sample-file-kind
  pwsh plugins\build-plugin.ps1 plugins\sample-csv-summary -Out C:\temp\plugin-cache

Steps:
  1. cargo build --release --target wasm32-wasip2 in the plugin folder
     (needs: rustup target add wasm32-wasip2)
  2. copies the component named by `component` in plugin.toml next to plugin.toml
  3. runs tsubame-plugc, which checks plugin.toml and writes
     <Out>\<id>\<version>\component.cwasm and the install record
     <Out>\<id>\<version>\install.toml (hashes and the approved capabilities)

A version is installed once: to rebuild it, delete <Out>\<id>\<version> first
(-Force does that). -Data is the plugin's `$data` folder, if it asks for one.

tsubame-plugc is looked for in -Plugc, then target\release\tsubame-plugc.exe
and target\debug\tsubame-plugc.exe of the repo; if neither exists it is built
(`cargo build --release -p tsubame-plugc`).

-Cargo is the cargo command to use (default `cargo`).
#>
param(
    [Parameter(Mandatory, Position = 0)][string]$Plugin,
    [string]$Out,
    [string]$Data,
    [switch]$Force,
    [string]$Plugc,
    [string]$Cargo = 'cargo'
)
$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent $PSScriptRoot
$dir = (Resolve-Path $Plugin).Path
$manifest = Join-Path $dir 'plugin.toml'
if (-not (Test-Path $manifest)) { throw "no plugin.toml in $dir" }
if (-not $Out) { $Out = Join-Path $repo 'target\plugins' }

# the component file name the manifest asks for
$component = $null
foreach ($line in Get-Content $manifest) {
    if ($line -match '^\s*component\s*=\s*"([^"]+)"') { $component = $Matches[1]; break }
}
if (-not $component) { throw 'plugin.toml has no component = "..." line' }

function Invoke-Native([string[]]$cmd) {
    & $cmd[0] @($cmd[1..($cmd.Length - 1)])
    if ($LASTEXITCODE -ne 0) { throw "failed: $($cmd -join ' ')" }
}

# 1. build the component
Push-Location $dir
try {
    Invoke-Native @($Cargo, 'build', '--release', '--target', 'wasm32-wasip2')
    $meta = & $Cargo metadata --format-version 1 --no-deps | ConvertFrom-Json
    $targetDir = $meta.target_directory
} finally { Pop-Location }
$built = Join-Path $targetDir "wasm32-wasip2\release\$component"
if (-not (Test-Path $built)) { throw "the build did not produce $built (does `component` in plugin.toml match the crate's library name?)" }

# 2. put it next to the manifest
Copy-Item $built (Join-Path $dir $component) -Force
Write-Host ("component  {0} ({1:N0} bytes)" -f $component, (Get-Item (Join-Path $dir $component)).Length)

# 3. compile it
if (-not $Plugc) {
    foreach ($p in 'target\release\tsubame-plugc.exe', 'target\debug\tsubame-plugc.exe') {
        $c = Join-Path $repo $p
        if (Test-Path $c) { $Plugc = $c; break }
    }
}
if (-not $Plugc) {
    Push-Location $repo
    try { Invoke-Native @($Cargo, 'build', '--release', '-p', 'tsubame-plugc') } finally { Pop-Location }
    $Plugc = Join-Path $repo 'target\release\tsubame-plugc.exe'
}
$id = $null; $version = $null
foreach ($line in Get-Content $manifest) {
    if (-not $id -and $line -match '^\s*id\s*=\s*"([^"]+)"') { $id = $Matches[1] }
    if (-not $version -and $line -match '^\s*version\s*=\s*"([^"]+)"') { $version = $Matches[1] }
}
if ($Force -and $id -and $version) {
    $installed = Join-Path $Out (Join-Path $id $version)
    if (Test-Path $installed) { Remove-Item -Recurse -Force $installed }
}
$plugcArgs = @($Plugc, $dir, $Out)
if ($Data) { $plugcArgs += $Data }
$out = & $plugcArgs[0] @($plugcArgs[1..($plugcArgs.Length - 1)])
if ($LASTEXITCODE -ne 0) { throw "failed: $($plugcArgs -join ' ')" }
$out | ForEach-Object { Write-Host $_ }
$file = $out | Where-Object { $_ -match '^file\s+(.+)$' } | ForEach-Object { $Matches[1].Trim() } | Select-Object -First 1
if ($file) { Write-Host ("cwasm size {0:N0} bytes" -f (Get-Item $file).Length) }
