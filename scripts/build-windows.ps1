# Builds dist\basidekick-sim-<version>-windows-x86_64.zip: the app and the
# terminal runner. Run from PowerShell on Windows with Rust (MSVC) installed:
#   powershell -ExecutionPolicy Bypass -File scripts\build-windows.ps1
$ErrorActionPreference = "Stop"
Set-Location (Join-Path $PSScriptRoot "..")
$version = (Select-String -Path Cargo.toml -Pattern '^version = "(.+)"').Matches[0].Groups[1].Value
$name = "basidekick-sim-$version-windows-x86_64"

cargo build --release -p basidekick-sim -p basidekick-sim-cli
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

$stage = "dist\$name"
if (Test-Path $stage) { Remove-Item -Recurse -Force $stage }
New-Item -ItemType Directory -Force $stage | Out-Null
Copy-Item target\release\basidekick-sim.exe "$stage\BASidekick Building Sim.exe"
Copy-Item target\release\basidekick-sim-cli.exe, README.md, LICENSE-MIT, LICENSE-APACHE $stage
Compress-Archive -Force -Path $stage -DestinationPath "dist\$name.zip"
Write-Host "built dist\$name.zip"
