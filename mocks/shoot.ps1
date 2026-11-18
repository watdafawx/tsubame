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
# ---- U3: sidebar, dual pane, drag states (mock-sidebar-panes.html) and inline rename (mock-rename.html). Prefix u3-
foreach ($th in 'dark', 'light') {
  $shots += , @("u3-sidebar-$th",           'mock-sidebar-panes.html', "theme=$th&only=sidebar",           1280, 840)
  $shots += , @("u3-sidebar-menu-$th",      'mock-sidebar-panes.html', "theme=$th&only=sidebar-menu",      1280, 840)
  $shots += , @("u3-sidebar-reorder-$th",   'mock-sidebar-panes.html', "theme=$th&only=sidebar-reorder",   1280, 840)
  $shots += , @("u3-sidebar-collapsed-$th", 'mock-sidebar-panes.html', "theme=$th&only=sidebar-collapsed", 1280, 720)
  $shots += , @("u3-sidebar-resize-$th",    'mock-sidebar-panes.html', "theme=$th&only=sidebar-resize",    1280, 720)
  $shots += , @("u3-dual-v-$th",            'mock-sidebar-panes.html', "theme=$th&only=dual-v",            1280, 720)
  $shots += , @("u3-dual-h-$th",            'mock-sidebar-panes.html', "theme=$th&only=dual-h",            1280, 860)
  $shots += , @("u3-dual-drag-$th",         'mock-sidebar-panes.html', "theme=$th&only=dual-drag",         1280, 720)
  $shots += , @("u3-drag-folder-$th",       'mock-sidebar-panes.html', "theme=$th&only=drag-folder",       1280, 720)
  $shots += , @("u3-drag-bookmark-$th",     'mock-sidebar-panes.html', "theme=$th&only=drag-bookmark",     1280, 720)
  $shots += , @("u3-drag-pane-$th",         'mock-sidebar-panes.html', "theme=$th&only=drag-pane",         1280, 720)
  $shots += , @("u3-keys-$th",              'mock-sidebar-panes.html', "theme=$th&only=keys",              1130, 174)
  $shots += , @("u3-rename-$th",            'mock-rename.html',        "theme=$th&only=rename",            1280, 720)
  $shots += , @("u3-rename-states-$th",     'mock-rename.html',        "theme=$th&only=rename-states",     1000, 384)
  $shots += , @("u3-batch-$th",             'mock-rename.html',        "theme=$th&only=batch",             1280, 720)
  $shots += , @("u3-batch-conflict-$th",    'mock-rename.html',        "theme=$th&only=batch-conflict",    1280, 720)
}
# U3 density variants (comfortable 26, spacious 30)
$shots += , @('u3-sidebar-dark-d30',      'mock-sidebar-panes.html', 'theme=dark&density=30&only=sidebar',  1280, 840)
$shots += , @('u3-dual-v-dark-d26',       'mock-sidebar-panes.html', 'theme=dark&density=26&only=dual-v',   1280, 720)
$shots += , @('u3-dual-v-light-d30',      'mock-sidebar-panes.html', 'theme=light&density=30&only=dual-v',  1280, 720)
$shots += , @('u3-drag-folder-dark-d26',  'mock-sidebar-panes.html', 'theme=dark&density=26&only=drag-folder', 1280, 720)
$shots += , @('u3-rename-dark-d30',       'mock-rename.html',        'theme=dark&density=30&only=rename',   1280, 720)
$shots += , @('u3-rename-light-d26',      'mock-rename.html',        'theme=light&density=26&only=rename',  1280, 720)
# ---- end U3
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
