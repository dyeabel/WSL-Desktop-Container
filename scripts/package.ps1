# Sestaví balíčky WLS Desktop do dist\:
#   WLS-Desktop-<verze>-win-x64.zip         portable (s PORTABLE.txt – data vedle EXE)
#   WLS-Desktop-<verze>-win-x64-winget.zip  pro winget (bez PORTABLE.txt – data v %APPDATA%,
#                                           takže přežijí upgrade, kdy winget složku nahradí)
#   *.sha256                                kontrolní součty (formát sha256sum)
#
#   powershell -ExecutionPolicy Bypass -File scripts\package.ps1 [-NoBuild] [-Sign] [-AllowUntrusted]
#
# -Sign podepíše wls-desktop.exe a docker.exe přes scripts\sign.ps1 (způsob podpisu viz tam –
# proměnné WLS_SIGN_*). ZIP sám Authenticode podpis nést nemůže; ověřuje se SHA-256.
param([switch]$NoBuild, [switch]$Sign, [switch]$AllowUntrusted)
# nativní příkazy (cargo, git) píší průběh na stderr – úspěch se kontroluje přes $LASTEXITCODE
$ErrorActionPreference = 'Continue'
$root = Split-Path $PSScriptRoot -Parent
Set-Location $root

$version = (Select-String -Path Cargo.toml -Pattern '^version\s*=\s*"([^"]+)"' | Select-Object -First 1).Matches[0].Groups[1].Value
if (-not $NoBuild) {
    cargo build --release
    if ($LASTEXITCODE -ne 0) { throw "cargo build selhal" }
}

$exes = @('target\release\wls-desktop.exe', 'target\release\docker.exe')
if ($Sign) {
    $list = ($exes | ForEach-Object { (Resolve-Path $_).Path }) -join ';'
    $signArgs = @('-ExecutionPolicy', 'Bypass', '-File', (Join-Path $PSScriptRoot 'sign.ps1'), '-Files', $list)
    if ($AllowUntrusted) { $signArgs += '-AllowUntrusted' }
    powershell @signArgs
    if ($LASTEXITCODE -ne 0) { throw "podpis selhal" }
}

$name = "WLS-Desktop-$version-win-x64"
$dist = Join-Path $root 'dist'
New-Item -ItemType Directory -Force $dist | Out-Null

function New-Package([string]$zipName, [bool]$portable) {
    $stage = Join-Path $dist $name
    if (Test-Path $stage) { Remove-Item $stage -Recurse -Force }
    New-Item -ItemType Directory -Force $stage | Out-Null
    foreach ($e in $exes) { Copy-Item $e $stage }
    Copy-Item README.md $stage
    if (Test-Path LICENSE) { Copy-Item LICENSE $stage }
    if ($portable) {
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
    }
    $zip = Join-Path $dist $zipName
    if (Test-Path $zip) { Remove-Item $zip -Force }
    Compress-Archive -Path $stage -DestinationPath $zip
    Remove-Item $stage -Recurse -Force
    $hash = (Get-FileHash $zip -Algorithm SHA256).Hash.ToLower()
    "$hash  $zipName" | Set-Content -Encoding ASCII "$zip.sha256"
    Write-Host "Balicek: $zip ($([math]::Round((Get-Item $zip).Length / 1MB, 1)) MB)"
    Write-Host "SHA256:  $hash"
}

New-Package "$name.zip" $true
New-Package "$name-winget.zip" $false
