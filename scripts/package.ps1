# Sestaví portable balíček WLS Desktop: dist\WLS-Desktop-<verze>-win-x64.zip
#
#   powershell -ExecutionPolicy Bypass -File scripts\package.ps1 [-NoBuild]
#
# ZIP obsahuje wls-desktop.exe, docker.exe (shim), README a PORTABLE.txt. Díky PORTABLE.txt
# ukládá aplikace data do podsložky data\ vedle EXE (ne do %APPDATA%) a příkaz docker se
# přidává do PATH přímo z této složky.
param([switch]$NoBuild)
# nativní příkazy (cargo, git) píší průběh na stderr – úspěch se kontroluje přes $LASTEXITCODE
$ErrorActionPreference = 'Continue'
$root = Split-Path $PSScriptRoot -Parent
Set-Location $root

$version = (Select-String -Path Cargo.toml -Pattern '^version\s*=\s*"([^"]+)"' | Select-Object -First 1).Matches[0].Groups[1].Value
if (-not $NoBuild) {
    cargo build --release
    if ($LASTEXITCODE -ne 0) { throw "cargo build selhal" }
}

$name = "WLS-Desktop-$version-win-x64"
$dist = Join-Path $root 'dist'
$stage = Join-Path $dist $name
if (Test-Path $stage) { Remove-Item $stage -Recurse -Force }
New-Item -ItemType Directory -Force $stage | Out-Null

Copy-Item target\release\wls-desktop.exe $stage
Copy-Item target\release\docker.exe $stage
Copy-Item README.md $stage
if (Test-Path LICENSE) { Copy-Item LICENSE $stage }

@"
WLS Desktop $version - portable

Tento soubor zapina portable rezim: nastaveni, certifikaty a ostatni data se ukladaji
do slozky data\ vedle wls-desktop.exe (ne do %APPDATA%\WLS_Desktop). Prikaz docker se
pridava do PATH primo z teto slozky (WSL engine -> Prikaz docker ve Windows).
Pro bezny rezim (data v %APPDATA%) tento soubor smaz.

This file enables portable mode: settings, certificates and other data are stored in
the data\ folder next to wls-desktop.exe (not in %APPDATA%\WLS_Desktop). The docker
command is added to PATH directly from this folder. Delete this file for the normal mode.

Requirements: Windows 10/11 with WSL 2. No installation needed - run wls-desktop.exe.
"@ | Set-Content -Encoding UTF8 (Join-Path $stage 'PORTABLE.txt')

$zip = Join-Path $dist "$name.zip"
if (Test-Path $zip) { Remove-Item $zip -Force }
Compress-Archive -Path $stage -DestinationPath $zip
$hash = (Get-FileHash $zip -Algorithm SHA256).Hash.ToLower()
"$hash  $name.zip" | Set-Content -Encoding ASCII "$zip.sha256"
Write-Host "Balicek: $zip ($([math]::Round((Get-Item $zip).Length / 1MB, 1)) MB)"
Write-Host "SHA256:  $hash"
