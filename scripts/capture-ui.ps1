# Visual verification: this mode disables game detection and closes its own window automatically.
param(
    [string]$Executable = '',
    [string]$Output = '',
    [ValidateSet('Settings', 'Protection', 'Updates', 'Credits')][string]$Tab = 'Settings'
)
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
if (-not $Executable) { $Executable = Join-Path $root 'dist\t7patch.exe' }
if (-not $Output) {
    $filename = if ($Tab -eq 'Settings') { 'launcher-preview.png' } else { "launcher-preview-$($Tab.ToLowerInvariant()).png" }
    $Output = Join-Path $root "target\$filename"
}
if (-not (Test-Path -LiteralPath (Split-Path -Parent $Output))) { throw 'Output directory does not exist.' }
Add-Type -AssemblyName System.Drawing
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class T7WindowCapture {
    [StructLayout(LayoutKind.Sequential)] public struct Rect { public int Left, Top, Right, Bottom; }
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr hwnd, out Rect rect);
    [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
    [DllImport("user32.dll")] public static extern IntPtr GetDlgItem(IntPtr hwnd, int id);
    [DllImport("user32.dll")] public static extern bool GetClientRect(IntPtr hwnd, out Rect rect);
    [DllImport("user32.dll")] public static extern IntPtr SendMessageW(IntPtr hwnd, uint msg, IntPtr wparam, IntPtr lparam);
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
    $tabControl = [T7WindowCapture]::GetDlgItem($process.MainWindowHandle, 114)
    if ($tabControl -eq [IntPtr]::Zero) { throw 'Launcher tabs not found.' }
    $index = @('Settings', 'Protection', 'Updates', 'Credits').IndexOf($Tab)
    $tabRect = New-Object T7WindowCapture+Rect
    if (-not [T7WindowCapture]::GetClientRect($tabControl, [ref]$tabRect)) { throw 'GetClientRect failed.' }
    $x = [int](($index + 0.5) * $tabRect.Right / 4)
    $y = [int]($tabRect.Bottom / 2)
    $point = [IntPtr]($x -bor ($y -shl 16))
    [T7WindowCapture]::SendMessageW($tabControl, 0x0201, [IntPtr]1, $point) | Out-Null
    [T7WindowCapture]::SendMessageW($tabControl, 0x0202, [IntPtr]0, $point) | Out-Null
    if ([T7WindowCapture]::SendMessageW($tabControl, 0x130B, [IntPtr]0, [IntPtr]0).ToInt32() -ne $index) { throw 'Tab selection failed.' }
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
