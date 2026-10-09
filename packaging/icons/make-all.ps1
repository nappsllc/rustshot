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
