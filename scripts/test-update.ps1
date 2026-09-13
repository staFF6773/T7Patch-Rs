$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$release = Join-Path $root 'target\x86_64-pc-windows-msvc\release'
$helperSource = Join-Path $release 't7patch-launcher.exe'
$successor = Join-Path $release 'examples\update_host.exe'
$dll = Join-Path $release 't7patch.dll'
foreach ($path in @($helperSource, $successor, $dll)) {
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) { throw "Build Release and examples first: $path" }
}
if (Get-Process -Name BlackOps3,t7patch,t7patch-launcher -ErrorAction SilentlyContinue) { throw 'Close BO3 and the launcher before this updater smoke test.' }
$parent = Join-Path $root 'target'
if (-not (Test-Path -LiteralPath $parent)) { throw 'Missing target directory.' }
$directory = Join-Path $parent ("update-smoke-" + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $directory | Out-Null
$stage = Join-Path $directory '.t7patch-update-0-1'
New-Item -ItemType Directory -Path $stage | Out-Null
$new = Join-Path $stage 'new'
$backup = Join-Path $stage 'backup'
New-Item -ItemType Directory -Path $new | Out-Null
New-Item -ItemType Directory -Path $backup | Out-Null
$encoding = New-Object System.Text.UTF8Encoding($false)
$process = $null
try {
    [System.IO.File]::WriteAllText((Join-Path $stage 'owner'), 't7patch-updater-v1', $encoding)
    [System.IO.File]::WriteAllText((Join-Path $directory 't7patch.conf'), 'playername=Keep This', $encoding)
    Copy-Item -LiteralPath $helperSource -Destination (Join-Path $directory 't7patch.exe')
    Copy-Item -LiteralPath $dll -Destination (Join-Path $directory 't7patch.dll')
    Copy-Item -LiteralPath $successor -Destination (Join-Path $new 't7patch.exe')
    Copy-Item -LiteralPath $dll -Destination (Join-Path $new 't7patch.dll')
    $helper = Join-Path $stage 'helper-smoke.exe'
    Copy-Item -LiteralPath $helperSource -Destination $helper
    $files = @('t7patch.exe', 't7patch.dll') | ForEach-Object {
        $path = Join-Path $new $_
        @{ name = $_; size = (Get-Item -LiteralPath $path).Length; sha256 = (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash.ToLowerInvariant() }
    }
    $manifest = @{
        schema = 1; version = '9.0.0'; target = 'x86_64-pc-windows-msvc'
        archive = @{ name = 't7patch-windows-x64.zip'; size = 1; sha256 = ('0' * 64) }
        files = @($files)
    }
    [System.IO.File]::WriteAllText((Join-Path $stage 'update.json'), ($manifest | ConvertTo-Json -Depth 5), $encoding)
    # PID 0 represents an already exited parent. The real helper performs all file operations.
    $arguments = '--apply-update 0 0 "{0}" "{1}"' -f $directory, $stage
    $process = Start-Process -FilePath $helper -ArgumentList $arguments -PassThru
    if (-not $process.WaitForExit(30000)) { $process.Kill(); throw 'Updater helper timed out.' }
    if ($process.ExitCode -ne 0) { throw "Updater helper failed: $($process.ExitCode)" }
    $deadline = [DateTime]::UtcNow.AddSeconds(10)
    while (-not (Test-Path -LiteralPath (Join-Path $directory 'update-host-started.txt'))) {
        if ([DateTime]::UtcNow -gt $deadline) { throw 'Successor was not launched.' }
        Start-Sleep -Milliseconds 100
    }
    $children = @(Get-Process -Name t7patch -ErrorAction SilentlyContinue | Where-Object { $_.Path -eq (Join-Path $directory 't7patch.exe') })
    foreach ($child in $children) {
        try {
            if (-not $child.HasExited -and -not $child.WaitForExit(10000)) { $child.Kill(); throw 'Test successor did not exit.' }
        } finally { $child.Dispose() }
    }
    foreach ($file in $files) {
        if ((Get-FileHash -LiteralPath (Join-Path $directory $file.name) -Algorithm SHA256).Hash.ToLowerInvariant() -ne $file.sha256) { throw "Installed hash mismatch: $($file.name)" }
    }
    if ([System.IO.File]::ReadAllText((Join-Path $directory 't7patch.conf')) -ne 'playername=Keep This') { throw 'Configuration changed.' }
    if (Test-Path -LiteralPath (Join-Path $directory '.t7patch-update.json')) { throw 'Transaction did not commit.' }
    Write-Output 'Updater helper: replaced the EXE/DLL pair, preserved configuration, and launched the harmless successor.'
} finally {
    if ($null -ne $process) { $process.Dispose() }
    Remove-Item -LiteralPath $directory -Recurse -Force
}
