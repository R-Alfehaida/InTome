# Builds the release exe and packages it with its runtime files into dist\InTome-<version>.zip.
# The zip holds one InTome-<version> folder: intome.exe, pdfium.dll and its license, the user guide.
$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent
Set-Location $root

cargo build --release
if ($LASTEXITCODE -ne 0) { throw "cargo build --release failed" }

$version = [regex]::Match((Get-Content Cargo.toml -Raw), '(?m)^version\s*=\s*"([^"]+)"').Groups[1].Value
$name = "InTome-$version"
$stage = Join-Path $root "target\package\$name"
$zip = Join-Path $root "dist\$name.zip"

if (Test-Path $stage) { Remove-Item $stage -Recurse -Force }
New-Item -ItemType Directory -Force $stage, (Split-Path $zip) | Out-Null
# Copy from vendor\ and resources\ rather than target\release, which can hold stale DLLs from older builds.
Copy-Item target\release\intome.exe, vendor\*, resources\* $stage

if (Test-Path $zip) { Remove-Item $zip -Force }
Compress-Archive -Path $stage -DestinationPath $zip
Write-Host "Packaged $zip"
Get-ChildItem $stage | ForEach-Object { Write-Host "  $($_.Name)" }
