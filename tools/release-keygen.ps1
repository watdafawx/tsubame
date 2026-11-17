<#
.SYNOPSIS
  Create the Tsubame update-signing keypair (ed25519) ONCE.

.DESCRIPTION
  Writes the private key (a 32-byte seed as 64 hex chars) to a file OUTSIDE the repository and
  prints only the PUBLIC key. Paste the public key into RELEASE_PUBLIC_KEYS_HEX in
  crates\tsubame-update\src\keys.rs, commit that, then add the private key as the GitHub
  repository secret UPDATE_SIGNING_KEY. See docs\release.md.

  Refuses to overwrite an existing key file, and refuses a path inside the repository.
  The private key is never printed.

.EXAMPLE
  pwsh tools\release-keygen.ps1
  pwsh tools\release-keygen.ps1 -Out D:\secure\tsubame-update.key
#>
[CmdletBinding()]
param(
    [string]$Out = (Join-Path $env:USERPROFILE '.tsubame\update-signing.key'),
    [string]$Cargo = $(if ($env:TSUBAME_CARGO) { $env:TSUBAME_CARGO } else { 'cargo' })
)
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

$outFull = [IO.Path]::GetFullPath($Out)
if ($outFull.StartsWith($root, [StringComparison]::OrdinalIgnoreCase)) { throw "refusing to write a private key inside the repository ($outFull)" }
if (Test-Path $outFull) { throw "$outFull already exists; refusing to overwrite a signing key. Pick another -Out or delete it on purpose." }
$null = New-Item -ItemType Directory -Force (Split-Path -Parent $outFull)

if ($Cargo -like '*.ps1') { & pwsh -NoProfile -File $Cargo build --release -p tsubame-update --features release-tool --bin tsubame-release } else { & $Cargo build --release -p tsubame-update --features release-tool --bin tsubame-release }
if ($LASTEXITCODE -ne 0) { throw 'building tsubame-release failed' }
$targetRoot = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $root 'target' }
& (Join-Path $targetRoot 'release\tsubame-release.exe') keygen --out $outFull
if ($LASTEXITCODE -ne 0) { throw 'keygen failed' }

# Owner-only access on the key file.
try {
    $acl = Get-Acl $outFull
    $acl.SetAccessRuleProtection($true, $false)
    $me = [Security.Principal.WindowsIdentity]::GetCurrent().Name
    $acl.AddAccessRule((New-Object Security.AccessControl.FileSystemAccessRule($me, 'FullControl', 'Allow')))
    Set-Acl $outFull $acl
}
catch { Write-Warning "could not restrict the key file ACL: $($_.Exception.Message)" }

Write-Host ''
Write-Host 'Next:'
Write-Host '  1. Paste the public key above into RELEASE_PUBLIC_KEYS_HEX in crates\tsubame-update\src\keys.rs and commit.'
Write-Host "  2. Back up $outFull somewhere safe (password manager / encrypted drive). Lose it = installed copies can never update again."
Write-Host "  3. gh secret set UPDATE_SIGNING_KEY < `"$outFull`"   (or paste the file contents in GitHub > Settings > Secrets > Actions)"
