[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing

$workspaceDir = [System.IO.Path]::GetFullPath((Split-Path -Parent $PSScriptRoot))
$assetsDir = Join-Path $workspaceDir 'assets'
$previewPath = Join-Path $assetsDir 'trans.png'
$iconPath = Join-Path $assetsDir 'trans.ico'

function New-RoundedRectangle {
    param([single]$X, [single]$Y, [single]$Width, [single]$Height, [single]$Radius)

    $path = New-Object System.Drawing.Drawing2D.GraphicsPath
    $diameter = $Radius * 2
    $path.AddArc($X, $Y, $diameter, $diameter, 180, 90)
    $path.AddArc(($X + $Width - $diameter), $Y, $diameter, $diameter, 270, 90)
    $path.AddArc(($X + $Width - $diameter), ($Y + $Height - $diameter), $diameter, $diameter, 0, 90)
    $path.AddArc($X, ($Y + $Height - $diameter), $diameter, $diameter, 90, 90)
    $path.CloseFigure()
    return ,$path
}

function Draw-TransIcon {
    param([System.Drawing.Graphics]$Graphics)

    $Graphics.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
    $Graphics.CompositingQuality = [System.Drawing.Drawing2D.CompositingQuality]::HighQuality
    $Graphics.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality
    $backPath = New-RoundedRectangle 40 36 140 140 16
    $frontPath = New-RoundedRectangle 76 76 140 144 16
    $blackBrush = New-Object System.Drawing.SolidBrush ([System.Drawing.Color]::Black)
    $whiteBrush = New-Object System.Drawing.SolidBrush ([System.Drawing.Color]::White)
    $backBorder = New-Object System.Drawing.Pen ([System.Drawing.Color]::White), 4
    $frontHalo = New-Object System.Drawing.Pen ([System.Drawing.Color]::White), 16
    $frontBorder = New-Object System.Drawing.Pen ([System.Drawing.Color]::Black), 8
    $backLine = New-Object System.Drawing.Pen ([System.Drawing.Color]::White), 6
    $frontLine = New-Object System.Drawing.Pen ([System.Drawing.Color]::Black), 6
    try {
        foreach ($pen in @($backBorder, $frontHalo, $frontBorder)) {
            $pen.LineJoin = [System.Drawing.Drawing2D.LineJoin]::Round
        }
        $Graphics.FillPath($blackBrush, $backPath)
        $Graphics.DrawPath($backBorder, $backPath)
        $Graphics.DrawLine($backLine, 56, 68, 164, 68)
        $Graphics.DrawPath($frontHalo, $frontPath)
        $Graphics.FillPath($whiteBrush, $frontPath)
        $Graphics.DrawPath($frontBorder, $frontPath)
        $Graphics.DrawLine($frontLine, 80, 108, 212, 108)
    } finally {
        foreach ($resource in @($backPath, $frontPath, $blackBrush, $whiteBrush, $backBorder, $frontHalo, $frontBorder, $backLine, $frontLine)) {
            $resource.Dispose()
        }
    }
}

function New-IconPng {
    param([int]$Size)

    $renderSize = $Size * 4
    $render = New-Object System.Drawing.Bitmap $renderSize, $renderSize, ([System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $renderGraphics = [System.Drawing.Graphics]::FromImage($render)
    $output = New-Object System.Drawing.Bitmap $Size, $Size, ([System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $outputGraphics = [System.Drawing.Graphics]::FromImage($output)
    $imageAttributes = New-Object System.Drawing.Imaging.ImageAttributes
    $stream = New-Object System.IO.MemoryStream
    try {
        $renderGraphics.Clear([System.Drawing.Color]::Transparent)
        $renderGraphics.ScaleTransform(($renderSize / 256.0), ($renderSize / 256.0))
        Draw-TransIcon $renderGraphics
        $outputGraphics.Clear([System.Drawing.Color]::Transparent)
        $outputGraphics.CompositingMode = [System.Drawing.Drawing2D.CompositingMode]::SourceCopy
        $outputGraphics.CompositingQuality = [System.Drawing.Drawing2D.CompositingQuality]::HighQuality
        $outputGraphics.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
        $outputGraphics.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality
        $imageAttributes.SetWrapMode([System.Drawing.Drawing2D.WrapMode]::TileFlipXY)
        $outputGraphics.DrawImage($render, (New-Object System.Drawing.Rectangle 0, 0, $Size, $Size), 0, 0, $renderSize, $renderSize, [System.Drawing.GraphicsUnit]::Pixel, $imageAttributes)
        $output.Save($stream, [System.Drawing.Imaging.ImageFormat]::Png)
        return ,$stream.ToArray()
    } finally {
        $stream.Dispose()
        $imageAttributes.Dispose()
        $outputGraphics.Dispose()
        $output.Dispose()
        $renderGraphics.Dispose()
        $render.Dispose()
    }
}

[System.IO.File]::WriteAllBytes($previewPath, (New-IconPng 512))
$sizes = @(16, 20, 24, 32, 40, 48, 64, 128, 256)
$entries = @()
foreach ($size in $sizes) {
    $entries += [PSCustomObject]@{ Size = $size; Png = (New-IconPng $size) }
}
$compactPreview = $entries | Where-Object { $_.Size -eq 128 } | Select-Object -First 1
[System.IO.File]::WriteAllBytes((Join-Path $assetsDir 'trans-preview.png'), $compactPreview.Png)
$iconStream = New-Object System.IO.MemoryStream
$writer = New-Object System.IO.BinaryWriter $iconStream
try {
    $writer.Write([uint16]0)
    $writer.Write([uint16]1)
    $writer.Write([uint16]$entries.Count)
    $offset = 6 + 16 * $entries.Count
    foreach ($entry in $entries) {
        $dimension = if ($entry.Size -eq 256) { 0 } else { $entry.Size }
        $writer.Write([byte]$dimension)
        $writer.Write([byte]$dimension)
        $writer.Write([byte]0)
        $writer.Write([byte]0)
        $writer.Write([uint16]1)
        $writer.Write([uint16]32)
        $writer.Write([uint32]$entry.Png.Length)
        $writer.Write([uint32]$offset)
        $offset += $entry.Png.Length
    }
    foreach ($entry in $entries) {
        $writer.Write([byte[]]$entry.Png)
    }
    $writer.Flush()
    [System.IO.File]::WriteAllBytes($iconPath, $iconStream.ToArray())
} finally {
    $writer.Dispose()
    $iconStream.Dispose()
}
Write-Host "图标素材已生成：$iconPath"
Write-Host "PNG 预览：$previewPath"
Write-Host ('ICO 尺寸：' + ($sizes -join ', '))
