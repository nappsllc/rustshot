# Generates packaging/icons/rustshot-256.png (app icon for deb/AppImage/icns).
# Rounded gradient tile + white capture-bracket glyph. Run: powershell -File make-icon.ps1
Add-Type -AssemblyName System.Drawing

$w = 256
$r = 56
$fmt = [System.Drawing.Imaging.PixelFormat]::Format32bppArgb
$bmp = New-Object -TypeName System.Drawing.Bitmap -ArgumentList @($w, $w, $fmt)
$g = [System.Drawing.Graphics]::FromImage($bmp)
$g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
$g.Clear([System.Drawing.Color]::Transparent)

$path = New-Object -TypeName System.Drawing.Drawing2D.GraphicsPath
$path.AddArc(0, 0, (2 * $r), (2 * $r), 180, 90)
$path.AddArc(($w - 2 * $r), 0, (2 * $r), (2 * $r), 270, 90)
$path.AddArc(($w - 2 * $r), ($w - 2 * $r), (2 * $r), (2 * $r), 0, 90)
$path.AddArc(0, ($w - 2 * $r), (2 * $r), (2 * $r), 90, 90)
$path.CloseFigure()

$c1 = [System.Drawing.Color]::FromArgb(255, 79, 142, 247)
$c2 = [System.Drawing.Color]::FromArgb(255, 37, 78, 196)
$p0 = New-Object -TypeName System.Drawing.Point -ArgumentList 0, 0
$p1 = New-Object -TypeName System.Drawing.Point -ArgumentList $w, $w
$brush = New-Object -TypeName System.Drawing.Drawing2D.LinearGradientBrush -ArgumentList @($p0, $p1, $c1, $c2)
$g.FillPath($brush, $path)

$pen = New-Object -TypeName System.Drawing.Pen -ArgumentList @([System.Drawing.Color]::White, 16.0)
$pen.StartCap = [System.Drawing.Drawing2D.LineCap]::Round
$pen.EndCap = [System.Drawing.Drawing2D.LineCap]::Round

$i = 66
$ip = $w - $i
$L = 52
$segments = @(
    @($i, $i, ($i + $L), $i),
    @($i, $i, $i, ($i + $L)),
    @($ip, $i, ($ip - $L), $i),
    @($ip, $i, $ip, ($i + $L)),
    @($i, $ip, ($i + $L), $ip),
    @($i, $ip, $i, ($ip - $L)),
    @($ip, $ip, ($ip - $L), $ip),
    @($ip, $ip, $ip, ($ip - $L))
)
foreach ($s in $segments) {
    $g.DrawLine($pen, [float]$s[0], [float]$s[1], [float]$s[2], [float]$s[3])
}

$c = [float]($w / 2)
$g.FillEllipse([System.Drawing.Brushes]::White, ($c - 13.0), ($c - 13.0), 26.0, 26.0)

$g.Dispose()
$out = Join-Path $PSScriptRoot 'rustshot-256.png'
$bmp.Save($out, [System.Drawing.Imaging.ImageFormat]::Png)
$bmp.Dispose()
Write-Host "wrote $out"
