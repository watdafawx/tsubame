<#
.SYNOPSIS
  Builds res\tsubame.ico from the two SVGs in res\.

.DESCRIPTION
  Frame plan (same cut as mocks\logo-final.html):
    16, 24 px      res\tsubame-small.svg   fat wings, shallow fork, no red dot
    32, 48, 64, 256 res\tsubame.svg        master cut

  Each frame is rasterised by ImageMagick at an exact 8x the target size (4x for
  256) and box-downscaled, so every pixel is supersampled by an integer area
  average; a wide-window filter only smears the mark at 16 px. Frames below 256
  are stored as 32-bit BGRA DIBs with the AND mask filled from alpha (every
  Windows loader takes those); 256 is PNG, as in blink. The ICO container is
  written here rather than by ImageMagick so the frame formats are exactly what
  we choose.

  ImageMagick is a build-time tool only: nothing in the app depends on it, and
  the committed .ico is the input to the build.

.EXAMPLE
  pwsh -NoProfile -File tools\make-icon.ps1
  pwsh -NoProfile -File tools\make-icon.ps1 -PreviewDir C:\Temp\tsubame-icon
  pwsh -NoProfile -File tools\make-icon.ps1 -VerifyExe target\release\tsubame.exe
#>
[CmdletBinding()]
param(
    # Output .ico path; defaults to res\tsubame.ico next to this repo.
    [string]$Out,
    # If set, also writes <size>.png of every frame there for eyeballing.
    [string]$PreviewDir,
    # If set, only verifies the icon resources embedded in this exe.
    [string]$VerifyExe
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$repo = Split-Path -Parent $PSScriptRoot
if (-not $Out) { $Out = Join-Path $repo 'res\tsubame.ico' }

# ---------------------------------------------------------------- verification

Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;

public static class TsuRes {
    public const uint LOAD_LIBRARY_AS_DATAFILE = 0x00000002;
    public const uint LOAD_LIBRARY_AS_IMAGE_RESOURCE = 0x00000020;
    public const int RT_ICON = 3;
    public const int RT_GROUP_ICON = 14;
    public const int RT_MANIFEST = 24;

    [DllImport("kernel32", CharSet = CharSet.Unicode, SetLastError = true)]
    public static extern IntPtr LoadLibraryExW(string file, IntPtr hFile, uint flags);

    [DllImport("kernel32", SetLastError = true)]
    public static extern IntPtr FindResourceW(IntPtr hModule, IntPtr name, IntPtr type);

    [DllImport("kernel32", SetLastError = true)]
    public static extern IntPtr LoadResource(IntPtr hModule, IntPtr hResInfo);

    [DllImport("kernel32", SetLastError = true)]
    public static extern uint SizeofResource(IntPtr hModule, IntPtr hResInfo);

    [DllImport("kernel32", SetLastError = true)]
    public static extern bool FreeLibrary(IntPtr hModule);

    public static byte[] Bytes(IntPtr hModule, IntPtr hResInfo) {
        uint size = SizeofResource(hModule, hResInfo);
        IntPtr data = LoadResource(hModule, hResInfo);
        if (size == 0 || data == IntPtr.Zero) { return new byte[0]; }
        byte[] buf = new byte[size];
        Marshal.Copy(data, buf, 0, (int)size);
        return buf;
    }
}
'@

if ($VerifyExe) {
    $exe = (Resolve-Path -LiteralPath $VerifyExe).ProviderPath
    $h = [TsuRes]::LoadLibraryExW($exe, [IntPtr]::Zero,
        [TsuRes]::LOAD_LIBRARY_AS_DATAFILE -bor [TsuRes]::LOAD_LIBRARY_AS_IMAGE_RESOURCE)
    if ($h -eq [IntPtr]::Zero) { throw "LoadLibraryEx failed for $exe" }
    try {
        $grp = [TsuRes]::FindResourceW($h, [IntPtr]1, [IntPtr][TsuRes]::RT_GROUP_ICON)
        if ($grp -eq [IntPtr]::Zero) { throw "$exe has no RT_GROUP_ICON id 1" }
        $g = [TsuRes]::Bytes($h, $grp)
        $count = [BitConverter]::ToUInt16($g, 4)
        "RT_GROUP_ICON 1: $count frames"
        for ($i = 0; $i -lt $count; $i++) {
            $o = 6 + 14 * $i
            $w = $g[$o]; $ht = $g[$o + 1]
            if ($w -eq 0) { $w = 256 }
            if ($ht -eq 0) { $ht = 256 }
            $bits = [BitConverter]::ToUInt16($g, $o + 6)
            $bytes = [BitConverter]::ToUInt32($g, $o + 8)
            $id = [BitConverter]::ToUInt16($g, $o + 12)
            $ok = [TsuRes]::FindResourceW($h, [IntPtr][int]$id, [IntPtr][TsuRes]::RT_ICON) -ne [IntPtr]::Zero
            "  {0,3}x{1,-3} {2} bpp {3,6} bytes  RT_ICON {4} {5}" -f $w, $ht, $bits, $bytes, $id,
                $(if ($ok) { 'ok' } else { 'MISSING' })
            if (-not $ok) { throw "RT_ICON $id is missing" }
        }
        $mf = [TsuRes]::FindResourceW($h, [IntPtr]1, [IntPtr][TsuRes]::RT_MANIFEST)
        "RT_MANIFEST 1: " + $(if ($mf -ne [IntPtr]::Zero) { 'present' } else { 'absent' })
    }
    finally {
        [void][TsuRes]::FreeLibrary($h)
    }
    return
}

# ------------------------------------------------------------------ rendering

$magick = Get-Command magick -ErrorAction SilentlyContinue
if (-not $magick) {
    throw 'ImageMagick 7 (magick.exe) is required to rasterise the SVGs and is not on PATH.'
}

$plan = @(
    @{ Size = 16; Svg = 'res\tsubame-small.svg' },
    @{ Size = 24; Svg = 'res\tsubame-small.svg' },
    @{ Size = 32; Svg = 'res\tsubame.svg' },
    @{ Size = 48; Svg = 'res\tsubame.svg' },
    @{ Size = 64; Svg = 'res\tsubame.svg' },
    @{ Size = 256; Svg = 'res\tsubame.svg' }
)

$tmp = Join-Path ([IO.Path]::GetTempPath()) ('tsubame-icon-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $tmp | Out-Null
try {
    $frames = @()
    foreach ($f in $plan) {
        $n = $f.Size
        $svg = Join-Path $repo $f.Svg
        if (-not (Test-Path -LiteralPath $svg)) { throw "missing $svg" }
        $png = Join-Path $tmp "$n.png"
        # Rasterise at exactly $ss x the target (96 dpi * 100 units = density/0.96 px),
        # then box-downscale: an integer-factor box is a plain area average, which
        # stays crisp at 16 px where a wide-window filter only smears the mark.
        $ss = if ($n -ge 256) { 4 } else { 8 }
        $density = 0.96 * $ss * $n
        # -strip plus the chunk excludes keep the 256 px PNG byte-identical between
        # runs (ImageMagick otherwise stamps tIME), so rebuilding the .ico is a no-op diff.
        & $magick.Source -background none -density $density $svg -filter Box -resize "${n}x${n}" `
            -strip -define png:exclude-chunk=date,time -depth 8 "png32:$png"
        if ($LASTEXITCODE -ne 0) { throw "magick failed on $($f.Svg) at $n px" }
        $frames += @{ Size = $n; Png = $png }
        if ($PreviewDir) {
            New-Item -ItemType Directory -Force -Path $PreviewDir | Out-Null
            Copy-Item -LiteralPath $png -Destination (Join-Path $PreviewDir "tsubame-$n.png") -Force
        }
    }

    # 32-bit DIB (bottom-up BGRA + 1bpp AND mask) for everything a legacy loader
    # may touch; 256 stays PNG.
    function New-IconDib([string]$PngPath, [int]$N) {
        $bin = [IO.Path]::ChangeExtension($PngPath, '.bgra')
        & $magick.Source "png32:$PngPath" -depth 8 "bgra:$bin"
        if ($LASTEXITCODE -ne 0) { throw "magick failed converting $PngPath to bgra" }
        $bgra = [IO.File]::ReadAllBytes($bin)
        if ($bgra.Length -ne $N * $N * 4) { throw "expected $($N*$N*4) bytes of bgra, got $($bgra.Length)" }

        $stride = $N * 4
        $maskStride = [int]([math]::Floor(($N + 31) / 32) * 4)
        $mask = New-Object byte[] ($maskStride * $N)
        for ($y = 0; $y -lt $N; $y++) {
            $row = ($N - 1 - $y) * $maskStride
            for ($x = 0; $x -lt $N; $x++) {
                if ($bgra[($y * $stride) + ($x * 4) + 3] -lt 128) {
                    $mask[$row + ($x -shr 3)] = $mask[$row + ($x -shr 3)] -bor (0x80 -shr ($x -band 7))
                }
            }
        }

        $ms = New-Object System.IO.MemoryStream
        $w = New-Object System.IO.BinaryWriter($ms)
        # BITMAPINFOHEADER: 32 bpp, height doubled (XOR + AND masks)
        $w.Write([int]40); $w.Write([int]$N); $w.Write([int]($N * 2))
        $w.Write([int16]1); $w.Write([int16]32); $w.Write([int]0)
        $w.Write([int]($stride * $N))
        $w.Write([int]0); $w.Write([int]0); $w.Write([int]0); $w.Write([int]0)
        for ($y = $N - 1; $y -ge 0; $y--) { $w.Write($bgra, $y * $stride, $stride) }
        $w.Write($mask)
        $w.Flush()
        $w.Dispose()
        return , $ms.ToArray()
    }

    $images = @()
    foreach ($f in $frames) {
        $data = if ($f.Size -ge 256) { [IO.File]::ReadAllBytes($f.Png) } else { New-IconDib $f.Png $f.Size }
        $images += @{ Size = $f.Size; Data = $data }
    }

    # ICONDIR + ICONDIRENTRY per frame, then the images.
    $stream = New-Object System.IO.MemoryStream
    $w = New-Object System.IO.BinaryWriter($stream)
    $w.Write([int16]0); $w.Write([int16]1); $w.Write([int16]$images.Count)
    $offset = 6 + 16 * $images.Count
    foreach ($img in $images) {
        $dim = if ($img.Size -ge 256) { 0 } else { $img.Size }
        $w.Write([byte]$dim); $w.Write([byte]$dim); $w.Write([byte]0); $w.Write([byte]0)
        $w.Write([int16]1); $w.Write([int16]32)
        $w.Write([int]$img.Data.Length); $w.Write([int]$offset)
        $offset += $img.Data.Length
    }
    foreach ($img in $images) { $w.Write($img.Data, 0, $img.Data.Length) }
    $w.Flush()

    $bytes = $stream.ToArray()
    $w.Dispose()
    $dir = Split-Path -Parent $Out
    if ($dir) { New-Item -ItemType Directory -Force -Path $dir | Out-Null }
    [IO.File]::WriteAllBytes($Out, $bytes)

    $sizes = ($images | ForEach-Object { $_.Size }) -join ', '
    "$Out : $($bytes.Length) bytes, frames $sizes"
}
finally {
    Remove-Item -LiteralPath $tmp -Recurse -Force -ErrorAction SilentlyContinue
}
