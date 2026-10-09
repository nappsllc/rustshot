# Regenerates every committed icon (Windows only: uses System.Drawing).
# Run: powershell -File packaging\icons\make-all.ps1
$ErrorActionPreference = 'Stop'
$d = $PSScriptRoot
$mk = Join-Path $d 'make-icon.ps1'
foreach ($s in 16, 32, 48, 64, 128, 256, 512, 1024) {
    & $mk -Size $s -Out (Join-Path $d "rustshot-$s.png")
}
$m = Join-Path $d 'msix'
New-Item -ItemType Directory -Force $m | Out-Null
& $mk -Size 44 -Out (Join-Path $m 'Square44x44Logo.png')
foreach ($s in 16, 24, 32, 48, 256) {
    & $mk -Size $s -Out (Join-Path $m "Square44x44Logo.targetsize-${s}_altform-unplated.png")
}
& $mk -Size 150 -Out (Join-Path $m 'Square150x150Logo.png')
& $mk -Size 150 -CanvasW 310 -Out (Join-Path $m 'Wide310x150Logo.png')
& $mk -Size 50 -Out (Join-Path $m 'StoreLogo.png')

# rustshot.ico: PNG-compressed ICO (ICONDIR + ICONDIRENTRYs + PNG data).
$tmp = Join-Path ([IO.Path]::GetTempPath()) ("rustshot-ico-" + [guid]::NewGuid())
New-Item -ItemType Directory -Force $tmp | Out-Null
try {
    $sizes = 16, 20, 24, 32, 40, 48, 64, 256
    $pngs = @()
    foreach ($s in $sizes) {
        $p = Join-Path $tmp "$s.png"
        & $mk -Size $s -Out $p
        $pngs += , [IO.File]::ReadAllBytes($p)
    }
    $ms = New-Object IO.MemoryStream
    $bw = New-Object IO.BinaryWriter($ms)
    $bw.Write([uint16]0); $bw.Write([uint16]1); $bw.Write([uint16]$sizes.Count)
    $offset = 6 + 16 * $sizes.Count
    for ($i = 0; $i -lt $sizes.Count; $i++) {
        $dim = if ($sizes[$i] -ge 256) { 0 } else { $sizes[$i] }
        $bw.Write([byte]$dim); $bw.Write([byte]$dim); $bw.Write([byte]0); $bw.Write([byte]0)
        $bw.Write([uint16]1); $bw.Write([uint16]32)
        $bw.Write([uint32]$pngs[$i].Length); $bw.Write([uint32]$offset)
        $offset += $pngs[$i].Length
    }
    foreach ($b in $pngs) { $bw.Write($b) }
    $bw.Flush()
    [IO.File]::WriteAllBytes((Join-Path $d 'rustshot.ico'), $ms.ToArray())
} finally {
    Remove-Item -Recurse -Force $tmp
}
