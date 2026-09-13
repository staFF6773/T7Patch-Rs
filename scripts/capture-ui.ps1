# Visual verification: this mode disables game detection and closes its own window automatically.
param([string]$Executable = '', [string]$Output = '')
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
if (-not $Executable) { $Executable = Join-Path $root 'dist\t7patch.exe' }
if (-not $Output) { $Output = Join-Path $root 'target\launcher-preview.png' }
if (-not (Test-Path -LiteralPath (Split-Path -Parent $Output))) { throw 'Output directory does not exist.' }
Add-Type -AssemblyName System.Drawing
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class T7WindowCapture {
    [StructLayout(LayoutKind.Sequential)] public struct Rect { public int Left, Top, Right, Bottom; }
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr hwnd, out Rect rect);
    [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
}
'@
[T7WindowCapture]::SetProcessDPIAware() | Out-Null
$process = Start-Process -FilePath $Executable -ArgumentList '--ui-smoke-test' -PassThru
try {
    $deadline = [DateTime]::UtcNow.AddSeconds(3)
    do {
        $process.Refresh()
        if ($process.HasExited) { throw 'Launcher exited before capture.' }
        if ($process.MainWindowHandle -ne [IntPtr]::Zero) { break }
        Start-Sleep -Milliseconds 20
    } while ([DateTime]::UtcNow -lt $deadline)
    if ($process.MainWindowHandle -eq [IntPtr]::Zero) { throw 'Launcher window not found.' }
    Start-Sleep -Milliseconds 200
    $rect = New-Object T7WindowCapture+Rect
    if (-not [T7WindowCapture]::GetWindowRect($process.MainWindowHandle, [ref]$rect)) { throw 'GetWindowRect failed.' }
    $bitmap = New-Object System.Drawing.Bitmap(($rect.Right - $rect.Left), ($rect.Bottom - $rect.Top))
    $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
    try {
        $graphics.CopyFromScreen($rect.Left, $rect.Top, 0, 0, $bitmap.Size)
        $bitmap.Save($Output, [System.Drawing.Imaging.ImageFormat]::Png)
    } finally { $graphics.Dispose(); $bitmap.Dispose() }
    $process.WaitForExit()
    if ($process.ExitCode -ne 0) { throw "Launcher smoke test failed: $($process.ExitCode)" }
    Write-Output "UI captured: $Output"
} finally { $process.Dispose() }
