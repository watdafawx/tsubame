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
# ---- U4: command palette, keybinds, settings (mock-palette.html, mock-settings.html) ----
foreach ($th in 'dark', 'light') {
  $shots += , @("u4-palette-default-$th",  'mock-palette.html', "theme=$th&only=default", 1280, 600)
  $shots += , @("u4-palette-fuzzy-$th",    'mock-palette.html', "theme=$th&only=fuzzy",   1280, 560)
  $shots += , @("u4-palette-path-$th",     'mock-palette.html', "theme=$th&only=path",    1280, 560)
  $shots += , @("u4-palette-filter-$th",   'mock-palette.html', "theme=$th&only=filter",  1280, 560)
  $shots += , @("u4-palette-tabs-$th",     'mock-palette.html', "theme=$th&only=tabs",    1280, 520)
  $shots += , @("u4-palette-fresh-$th",    'mock-palette.html', "theme=$th&only=fresh",   1280, 480)
  $shots += , @("u4-palette-none-$th",     'mock-palette.html', "theme=$th&only=none",    1280, 440)
  $shots += , @("u4-settings-general-$th",           'mock-settings.html', "theme=$th&only=general",           1280, 760)
  $shots += , @("u4-settings-appearance-$th",        'mock-settings.html', "theme=$th&only=appearance",        1280, 1440)
  $shots += , @("u4-settings-context-$th",           'mock-settings.html', "theme=$th&only=context",           1280, 860)
  $shots += , @("u4-settings-context-add-$th",       'mock-settings.html', "theme=$th&only=context-add",       1280, 780)
  $shots += , @("u4-settings-keys-$th",              'mock-settings.html', "theme=$th&only=keys",              1280, 900)
  $shots += , @("u4-settings-keys-record-$th",       'mock-settings.html', "theme=$th&only=keys-record",       1280, 820)
  $shots += , @("u4-settings-updates-$th",           'mock-settings.html', "theme=$th&only=updates",           1280, 760)
  $shots += , @("u4-settings-updates-available-$th", 'mock-settings.html', "theme=$th&only=updates-available", 1280, 760)
  $shots += , @("u4-settings-default-$th",           'mock-settings.html', "theme=$th&only=default",           1280, 940)
  $shots += , @("u4-settings-advanced-$th",          'mock-settings.html', "theme=$th&only=advanced",          1280, 900)
  $shots += , @("u4-settings-states-$th",            'mock-settings.html', "theme=$th&only=states",            1280, 1240)
}
# ---- end U4 ----
$pattern = if ($args.Count -gt 0) { $args[0] } else { "*" }
$shots = @($shots | Where-Object { $_[0] -like $pattern })

foreach ($s in $shots) {
  $url = ([Uri](Join-Path $here $s[1])).AbsoluteUri + '?' + $s[2]
  $png = Join-Path $out ($s[0] + '.png')
  & $browser --headless=new --disable-gpu --hide-scrollbars --force-device-scale-factor=1 `
    "--window-size=$($s[3]),$($s[4])" "--screenshot=$png" $url 2>$null | Out-Null
  Write-Host ("{0,-22} {1}" -f $s[0], (Get-Item $png).Length)
}
