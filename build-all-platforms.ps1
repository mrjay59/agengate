#!/usr/bin/env pwsh
# Build agentgate for all platforms: Windows, Linux x86_64, ARM64

$ErrorActionPreference = "Stop"
$ProjectDir = "C:\Users\EDN\Documents\AION\project\agentgate"
$OutDir = "$ProjectDir\releases"
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null

Write-Host "=== Building agentgate for all platforms ===" -ForegroundColor Cyan

# 1. Windows x86_64 (native)
Write-Host "`n[1/3] Building Windows x86_64..." -ForegroundColor Yellow
cargo build --release --target x86_64-pc-windows-msvc 2>&1
if ($LASTEXITCODE -ne 0) { Write-Error "Windows build failed"; exit 1 }
$winPath = "$ProjectDir\target\x86_64-pc-windows-msvc\release\agentgate.exe"
if (-not (Test-Path $winPath)) { Write-Error "Windows binary not found"; exit 1 }
Compress-Archive -Path $winPath -DestinationPath "$OutDir\agentgate-windows-x86_64.zip" -Force
Write-Host "  ✓ agentgate-windows-x86_64.zip" -ForegroundColor Green

# 2. Linux x86_64 (cross-compile)
Write-Host "`n[2/3] Building Linux x86_64 (cross-compile)..." -ForegroundColor Yellow
rustup target add x86_64-unknown-linux-gnu 2>&1 | Out-Null
cargo build --release --target x86_64-unknown-linux-gnu 2>&1
if ($LASTEXITCODE -ne 0) { Write-Error "Linux x86_64 build failed"; exit 1 }
$linuxPath = "$ProjectDir\target\x86_64-unknown-linux-gnu\release\agentgate"
if (-not (Test-Path $linuxPath)) { Write-Error "Linux x86_64 binary not found"; exit 1 }
$tarPath = "$OutDir\agentgate-linux-x86_64.tar.gz"
tar -czf $tarPath -C "$ProjectDir\target\x86_64-unknown-linux-gnu\release" agentgate
Write-Host "  ✓ agentgate-linux-x86_64.tar.gz" -ForegroundColor Green

# 3. ARM64 (cross-compile)
Write-Host "`n[3/3] Building ARM64 (cross-compile)..." -ForegroundColor Yellow
rustup target add aarch64-unknown-linux-gnu 2>&1 | Out-Null
cargo build --release --target aarch64-unknown-linux-gnu 2>&1
if ($LASTEXITCODE -ne 0) { Write-Error "ARM64 build failed"; exit 1 }
$armPath = "$ProjectDir\target\aarch64-unknown-linux-gnu\release\agentgate"
if (-not (Test-Path $armPath)) { Write-Error "ARM64 binary not found"; exit 1 }
$tarPath = "$OutDir\agentgate-linux-aarch64.tar.gz"
tar -czf $tarPath -C "$ProjectDir\target\aarch64-unknown-linux-gnu\release" agentgate
Write-Host "  ✓ agentgate-linux-aarch64.tar.gz" -ForegroundColor Green

# Summary
Write-Host "`n=== Build Complete ===" -ForegroundColor Cyan
Get-ChildItem $OutDir | ForEach-Object {
    Write-Host "  $($_.Name) ($($_.Length) bytes)" -ForegroundColor White
}