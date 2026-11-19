# Úklid po buildech, aby se target\ a dist\ nekupily (každá nová verze vytvoří v target\
# nové sady incremental\ a build\<crate>\<hash>, staré cargo samo nemaže).
#
#   powershell -ExecutionPolicy Bypass -File scripts\cleanup.ps1 [-Since <čas>] [-KeepDist 3] [-MaxGB 10]
#   (-Since výchozí: poslední 2 hodiny)
#
#   1. target\<profil>\incremental: od každého crate jen nejnovější sezení (a vše změněné od -Since)
#   2. target\<profil>\build\wls-desktop\<hash>: jen nejnovější hash (a vše změněné od -Since);
#      závislosti se nemažou – jejich hashe se mění jen s toolchainem / features a mazání by
#      jen zbytečně vynutilo jejich nový překlad (hlídá je limit -MaxGB)
#   3. dist\: balíčky jen posledních -KeepDist verzí
#   4. když je target\ i tak větší než -MaxGB, smaže se celý debug profil (cargo clean --profile dev)
#
# Smazané věci jsou jen mezivýsledky překladu – cargo je při dalším buildu vytvoří znovu.
# Volá ho scripts\release.ps1 na konci; jde spustit i samostatně.
param(
    [datetime]$Since = (Get-Date).AddHours(-2),
    [int]$KeepDist = 3,
    [double]$MaxGB = 10
)
$ErrorActionPreference = 'Continue'
$root = Split-Path $PSScriptRoot -Parent
Set-Location $root

function Get-Size([string]$path) {
    if (-not (Test-Path $path)) { return 0 }
    $s = (Get-ChildItem $path -Recurse -File -Force -ErrorAction SilentlyContinue | Measure-Object Length -Sum).Sum
    if ($s) { [double]$s } else { 0 }
}
function Format-GB([double]$b) { '{0:N2} GB' -f ($b / 1GB) }

# Ze skupiny adresářů ponechá nejnovější a ty změněné od $Since, ostatní smaže; vrací uvolněné bajty.
function Remove-Stale($dirs) {
    $freed = 0
    $sorted = @($dirs | Sort-Object LastWriteTime -Descending)
    foreach ($d in ($sorted | Select-Object -Skip 1)) {
        if ($d.LastWriteTime -ge $Since) { continue }
        $freed += Get-Size $d.FullName
        Remove-Item $d.FullName -Recurse -Force -ErrorAction SilentlyContinue
    }
    $freed
}

$before = Get-Size 'target'
$freed = 0
$crate = (Select-String -Path Cargo.toml -Pattern '^name\s*=\s*"([^"]+)"' | Select-Object -First 1).Matches[0].Groups[1].Value
foreach ($prof in 'debug', 'release') {
    $inc = "target\$prof\incremental"
    if (Test-Path $inc) {
        # název sezení: <crate>-<hash>
        Get-ChildItem $inc -Directory | Group-Object { $_.Name -replace '-[^-]+$', '' } | ForEach-Object { $freed += Remove-Stale $_.Group }
    }
    $own = "target\$prof\build\$crate"
    if (Test-Path $own) {
        $hashes = @(Get-ChildItem $own -Directory)
        if ($hashes.Count -gt 1) { $freed += Remove-Stale $hashes }
    }
}
Write-Host "target: uklizeno $(Format-GB $freed) starých mezivýsledků"

$after = Get-Size 'target'
if ($after -gt $MaxGB * 1GB) {
    Write-Host "target má i tak $(Format-GB $after) (limit $MaxGB GB) – mažu debug profil"
    cargo clean --profile dev *> $null
    $after = Get-Size 'target'
}

# dist: balíčky posledních $KeepDist verzí
if (Test-Path 'dist') {
    $byVersion = Get-ChildItem dist -File | Where-Object { $_.Name -match '^WLS-Desktop-(\d+\.\d+\.\d+[^-]*)-win-x64' } |
        Group-Object { [regex]::Match($_.Name, '^WLS-Desktop-(\d+\.\d+\.\d+[^-]*)-win-x64').Groups[1].Value }
    $old = $byVersion | Sort-Object { try { [version]($_.Name -replace '[-+].*$', '') } catch { [version]'0.0' } } -Descending | Select-Object -Skip $KeepDist
    $n = 0
    foreach ($g in $old) { foreach ($f in $g.Group) { Remove-Item $f.FullName -Force; $n++ } }
    if ($n) { Write-Host "dist: smazáno $n souborů starších verzí (ponechány poslední $KeepDist)" }
}

Write-Host "target: $(Format-GB $before) -> $(Format-GB $after)"
