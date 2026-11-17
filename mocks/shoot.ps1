<#
  Renders the mocks to mocks/shots/*.png with headless Chrome/Edge (no window opens).
  Usage: pwsh -NoProfile -File mocks/shoot.ps1
#>
$ErrorActionPreference = 'Stop'
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$out = Join-Path $here 'shots'
New-Item -ItemType Directory -Force -Path $out | Out-Null

$browser = @(
  'C:\Program Files\Google\Chrome\Application\chrome.exe',
  'C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe'
) | Where-Object { Test-Path $_ } | Select-Object -First 1
if (-not $browser) { throw 'no Chrome or Edge found' }

# name, file, query, width, height
$shots = @(
  @('main-dark-multi',   'mock-main.html',   'theme=dark&sel=multi&only=1',   1280, 720),
  @('main-light-multi',  'mock-main.html',   'theme=light&sel=multi&only=1',  1280, 720),
  @('main-dark-single',  'mock-main.html',   'theme=dark&sel=single&only=1',  1280, 720),
  @('main-dark-zebra',   'mock-main.html',   'theme=dark&sel=multi&zebra=1&only=1', 1280, 720)
)
foreach ($th in 'dark', 'light') {
  $shots += , @("rows-$th",      'mock-states.html', "theme=$th&only=rows",      1000, 250)
  $shots += , @("path-edit-$th", 'mock-states.html', "theme=$th&only=path-edit", 1280, 420)
  $shots += , @("loading-$th",   'mock-states.html', "theme=$th&only=loading",   1280, 520)
  $shots += , @("empty-$th",     'mock-states.html', "theme=$th&only=empty",     1280, 520)
  $shots += , @("filter-$th",    'mock-states.html', "theme=$th&only=filter",    1280, 520)
  $shots += , @("menu-$th",      'mock-states.html', "theme=$th&only=menu",      1280, 640)
}
$pattern = if ($args.Count -gt 0) { $args[0] } else { "*" }
$shots = @($shots | Where-Object { $_[0] -like $pattern })

foreach ($s in $shots) {
  $url = ([Uri](Join-Path $here $s[1])).AbsoluteUri + '?' + $s[2]
  $png = Join-Path $out ($s[0] + '.png')
  & $browser --headless=new --disable-gpu --hide-scrollbars --force-device-scale-factor=1 `
    "--window-size=$($s[3]),$($s[4])" "--screenshot=$png" $url 2>$null | Out-Null
  Write-Host ("{0,-22} {1}" -f $s[0], (Get-Item $png).Length)
}
