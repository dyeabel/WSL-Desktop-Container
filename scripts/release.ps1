# Produkční build WLS Desktop: kontroly, testy, release build a portable balíček.
#
#   powershell -ExecutionPolicy Bypass -File scripts\release.ps1                 # build + balíček z aktuální verze
#   powershell -ExecutionPolicy Bypass -File scripts\release.ps1 -Version 0.3.0  # nastaví verzi v Cargo.toml (commit)
#   powershell -ExecutionPolicy Bypass -File scripts\release.ps1 -Tag            # navíc vytvoří git tag vX.Y.Z
#
# Kroky:
#   1. čistý pracovní strom (žádné necommitnuté změny) – balíček odpovídá commitu
#   2. volitelně nová verze v Cargo.toml (+ commit „Verze X.Y.Z“)
#   3. cargo clippy (varování = chyba) a cargo test
#   4. cargo build --release a scripts\package.ps1 -> dist\WLS-Desktop-<verze>-win-x64.zip (+ .sha256)
#   5. volitelně git tag vX.Y.Z – po `git push origin vX.Y.Z` vytvoří GitHub Actions release se ZIPem
#
# Živé testy (cargo test -- --ignored) se nespouští – potřebují WSL distribuci s enginem;
# přepínač -LiveTests je spustí (WLS_DISTRO určuje distribuci).
param(
    [string]$Version,
    [switch]$Tag,
    [switch]$LiveTests,
    [switch]$AllowDirty
)
# nativní příkazy (cargo, git) píší průběh na stderr – úspěch se kontroluje přes $LASTEXITCODE
$ErrorActionPreference = 'Continue'
$root = Split-Path $PSScriptRoot -Parent
Set-Location $root

function Step($t) { Write-Host "`n=== $t ===" -ForegroundColor Cyan }
function Check($what) { if ($LASTEXITCODE -ne 0) { throw "$what selhalo (kód $LASTEXITCODE)" } }

Step 'Kontrola pracovního stromu'
$dirty = git status --porcelain
if ($dirty -and -not $AllowDirty) {
    $dirty | Write-Host
    throw 'Pracovní strom obsahuje necommitnuté změny (použij -AllowDirty jen pro zkušební build).'
}

if ($Version) {
    Step "Verze $Version"
    if ($Version -notmatch '^\d+\.\d+\.\d+([-+][0-9A-Za-z.-]+)?$') { throw "Neplatná verze: $Version" }
    $toml = Get-Content Cargo.toml -Raw
    $toml = [regex]::Replace($toml, '(?m)^version\s*=\s*"[^"]+"', "version = `"$Version`"", 1)
    [IO.File]::WriteAllText((Join-Path $root 'Cargo.toml'), $toml)
    cargo update --workspace --offline *> $null
    git add Cargo.toml Cargo.lock
    git commit -q -m "Verze $Version"
    Check 'git commit'
}
$ver = (Select-String -Path Cargo.toml -Pattern '^version\s*=\s*"([^"]+)"' | Select-Object -First 1).Matches[0].Groups[1].Value

Step 'Clippy'
cargo clippy --all-targets -- -D warnings
Check 'cargo clippy'

Step 'Testy'
cargo test
Check 'cargo test'
if ($LiveTests) {
    Step 'Živé testy (WSL)'
    cargo test -- --ignored --test-threads=1
    Check 'živé testy'
}

Step 'Release build a balíček'
& (Join-Path $PSScriptRoot 'package.ps1')
Check 'package.ps1'

if ($Tag) {
    Step "Tag v$ver"
    git tag -a "v$ver" -m "WLS Desktop $ver"
    Check 'git tag'
    Write-Host "Vytvořen tag v$ver – vydání: git push origin main v$ver"
}

Write-Host "`nHotovo: dist\WLS-Desktop-$ver-win-x64.zip" -ForegroundColor Green
