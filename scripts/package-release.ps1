param([string]$Version = '')
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$metadata = & cargo metadata --manifest-path (Join-Path $root 'Cargo.toml') --no-deps --format-version 1 --locked
if ($LASTEXITCODE -ne 0) { throw 'Cargo metadata failed.' }
$current = ($metadata | ConvertFrom-Json).packages[0].version
if ($Version -eq '') { $Version = $current }
if ($Version -ne $current -or $Version -notmatch '^\d+\.\d+\.\d+$') {
    throw 'Release version must be a stable version matching Cargo.toml.'
}
$dist = Join-Path $root 'dist'
$target = Join-Path $root 'target'
if (-not (Test-Path -LiteralPath $dist) -or -not (Test-Path -LiteralPath $target)) { throw 'Run scripts/build.ps1 first.' }
$destination = Join-Path $target 'release-package'
if (-not (Test-Path -LiteralPath $destination)) { New-Item -ItemType Directory -Path $destination | Out-Null }
$names = @('t7patch.exe', 't7patch.dll')
$paths = @($names | ForEach-Object { Join-Path $dist $_ })
foreach ($path in $paths) {
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) { throw "Missing binary: $path" }
}
$archive = Join-Path $destination 't7patch-windows-x64.zip'
# Select only the pair: configuration, logs and local diagnostic markers are never packaged.
Compress-Archive -LiteralPath $paths -DestinationPath $archive -CompressionLevel Optimal -Force
function Describe-File([string]$Path, [string]$Name) {
    [ordered]@{
        name = $Name
        size = (Get-Item -LiteralPath $Path).Length
        sha256 = (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant()
    }
}
$manifest = [ordered]@{
    schema = 1
    version = $Version
    target = 'x86_64-pc-windows-msvc'
    archive = Describe-File $archive 't7patch-windows-x64.zip'
    files = @($names | ForEach-Object { Describe-File (Join-Path $dist $_) $_ })
}
$encoding = New-Object System.Text.UTF8Encoding($false)
[System.IO.File]::WriteAllText((Join-Path $destination 'update.json'), ($manifest | ConvertTo-Json -Depth 5), $encoding)
Write-Output "Release v${Version}: $destination"
