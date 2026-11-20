<#
.SYNOPSIS
  Benchmark a zátěžový test Docker vs Podman ve WSL Container Desktop → docs/BENCHMARK.md.

.DESCRIPTION
  Sestaví release shim (docker.exe) a spustí ignorovaný test live_bench v release režimu.
  Distribuce se berou z konfigurace aplikace (distro_engines), případně z -Docker / -Podman.
  Test nic trvale nemění (dočasná proxy, kontejnery s předponou wslcd-bench se na konci odstraní).

.EXAMPLE
  powershell -ExecutionPolicy Bypass -File scripts\bench.ps1
  powershell -ExecutionPolicy Bypass -File scripts\bench.ps1 -Quick
  powershell -ExecutionPolicy Bypass -File scripts\bench.ps1 -Docker Ubuntu -Podman Debian
#>
param(
    [switch]$Quick,
    [string]$Docker,
    [string]$Podman,
    [int]$Port = 2386
)
$ErrorActionPreference = 'Stop'
Set-Location (Split-Path $PSScriptRoot -Parent)

if ($Quick) { $env:WSLCD_BENCH_QUICK = '1' } else { Remove-Item Env:WSLCD_BENCH_QUICK -ErrorAction SilentlyContinue }
if ($Docker) { $env:WSLCD_BENCH_DOCKER = $Docker }
if ($Podman) { $env:WSLCD_BENCH_PODMAN = $Podman }
$env:WSLCD_BENCH_PORT = "$Port"

cargo build --release --bin docker
if ($LASTEXITCODE) { throw 'cargo build selhal' }
cargo test --release live_bench -- --ignored --nocapture --test-threads=1
$code = $LASTEXITCODE
if (Test-Path docs\BENCHMARK.md) { Write-Host "`nVýsledek: docs\BENCHMARK.md, docs\bench\results.json" }
exit $code
