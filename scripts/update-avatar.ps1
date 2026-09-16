# Refresh the embedded Credits portrait from staFF6773's public GitHub avatar.
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$assets = Join-Path $root 'assets'
if (-not (Test-Path -LiteralPath $assets -PathType Container)) { throw 'Assets directory does not exist.' }
Add-Type -AssemblyName System.Drawing
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
$client = New-Object Net.WebClient
$stream = $null
$image = $null
$bitmap = $null
$graphics = $null
try {
    $client.Headers['User-Agent'] = 'T7Patch-Rs-avatar-update'
    $bytes = $client.DownloadData('https://avatars.githubusercontent.com/u/108166164?v=4&s=192')
    $stream = New-Object IO.MemoryStream(,$bytes)
    $image = [Drawing.Image]::FromStream($stream)
    $bitmap = New-Object Drawing.Bitmap(96, 96, [Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $graphics = [Drawing.Graphics]::FromImage($bitmap)
    $graphics.InterpolationMode = [Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
    $graphics.DrawImage($image, 0, 0, 96, 96)
    $bitmap.Save((Join-Path $assets 'staff6773-avatar.png'), [Drawing.Imaging.ImageFormat]::Png)
    Write-Output 'Updated assets/staff6773-avatar.png (96 x 96).'
} finally {
    if ($graphics) { $graphics.Dispose() }
    if ($bitmap) { $bitmap.Dispose() }
    if ($image) { $image.Dispose() }
    if ($stream) { $stream.Dispose() }
    $client.Dispose()
}
