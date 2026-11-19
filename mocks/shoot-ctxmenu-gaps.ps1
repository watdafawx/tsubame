<#
  Renders mocks/mock-ctxmenu-gaps.html to mocks/shots/ctxgap-<id>-light|dark.png with headless Chrome (no window).
  The page height is read from the page itself (body[data-h]), so every shot is cropped to its content.
  Usage: pwsh -NoProfile -File mocks/shoot-ctxmenu-gaps.ps1 [id-pattern]
#>
$ErrorActionPreference = 'Stop'
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$out = Join-Path $here 'shots'
New-Item -ItemType Directory -Force -Path $out | Out-Null
$browser = @('C:\Program Files\Google\Chrome\Application\chrome.exe', 'C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe') |
  Where-Object { Test-Path $_ } | Select-Object -First 1
if (-not $browser) { throw 'no Chrome or Edge found' }
$tmp = Join-Path ([IO.Path]::GetTempPath()) 'ctxgap-shoot'
New-Item -ItemType Directory -Force -Path $tmp | Out-Null
$pattern = if ($args.Count -gt 0) { $args[0] } else { '*' }
$width = 1160
foreach ($id in 'rowmenu', 'refresh', 'pin', 'groupname', 'nss', 'kinds') {
  if ($id -notlike $pattern) { continue }
  foreach ($th in 'light', 'dark') {
    $url = ([Uri](Join-Path $here 'mock-ctxmenu-gaps.html')).AbsoluteUri + "?only=$id&theme=$th"
    $dom = Join-Path $tmp 'dom.txt'
    Start-Process $browser -ArgumentList '--headless=new', '--disable-gpu', "--user-data-dir=$tmp\prof", "--window-size=$width,300", '--virtual-time-budget=3000', '--dump-dom', "`"$url`"" `
      -RedirectStandardOutput $dom -RedirectStandardError "$tmp\err.txt" -Wait -WindowStyle Hidden | Out-Null
    $h = [regex]::Match((Get-Content $dom -Raw), 'data-h="(\d+)"').Groups[1].Value
    if (-not $h) { throw "no height for $id" }
    $png = Join-Path $out "ctxgap-$id-$th.png"
    Start-Process $browser -ArgumentList '--headless=new', '--disable-gpu', '--hide-scrollbars', '--force-device-scale-factor=1', "--user-data-dir=$tmp\prof", "--window-size=$width,$([int]$h + 8)", '--virtual-time-budget=3000', "--screenshot=`"$png`"", "`"$url`"" `
      -RedirectStandardError "$tmp\err.txt" -Wait -WindowStyle Hidden | Out-Null
    Write-Host ('{0,-10} {1,-6} h={2} {3}' -f $id, $th, $h, (Get-Item $png).Length)
  }
}
Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
