$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
& cargo build --manifest-path (Join-Path $root 'Cargo.toml') --target-dir (Join-Path $root 'target') --release --locked --target x86_64-pc-windows-msvc
if ($LASTEXITCODE -ne 0) { throw 'Cargo build failed.' }
$release = Join-Path $root 'target\x86_64-pc-windows-msvc\release'
$destination = Join-Path $root 'dist'
if (-not (Test-Path -LiteralPath $destination)) {
    New-Item -ItemType Directory -Path $destination | Out-Null
}
# Separate Cargo target names avoid MSVC PDB collisions; distribution uses the familiar EXE name.
Copy-Item -LiteralPath (Join-Path $release 't7patch-launcher.exe') -Destination (Join-Path $destination 't7patch.exe')
Copy-Item -LiteralPath (Join-Path $release 't7patch.dll') -Destination (Join-Path $destination 't7patch.dll')
Write-Output "Built: $destination\t7patch.exe and $destination\t7patch.dll"
