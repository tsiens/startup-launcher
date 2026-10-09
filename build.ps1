param(
    [Parameter(Mandatory = $true, Position = 0)]
    [string]$OutputDir
)

$ErrorActionPreference = 'Stop'
$projectDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$target = Join-Path $projectDir 'target\release\startup-launcher.exe'
$output = Join-Path $OutputDir 'StartupLauncher.exe'
cargo build --release --locked --manifest-path (Join-Path $projectDir 'Cargo.toml')
if ($LASTEXITCODE -ne 0) {
    throw "Cargo build failed with exit code $LASTEXITCODE"
}
New-Item -ItemType Directory -Force $OutputDir | Out-Null
Copy-Item -LiteralPath $target -Destination $output -Force
Write-Host "已生成: $output"
