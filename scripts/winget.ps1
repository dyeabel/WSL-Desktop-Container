# Vygeneruje manifest pro winget (Windows Package Manager) z balíčku -winget.zip a ověří ho.
#
#   powershell -ExecutionPolicy Bypass -File scripts\winget.ps1 [-Version 0.2.0]
#
# Výstup: dist\winget\manifests\<p>\<Publisher>\<Name>\<verze>\*.yaml (struktura repozitáře
# microsoft/winget-pkgs). Instalace z winget je typ "zip" s vnořeným portable EXE: winget
# rozbalí ZIP do %LOCALAPPDATA%\Microsoft\WinGet\Packages\… a vytvoří příkaz `wls-desktop`.
# Data aplikace jsou v %APPDATA%\WLS_Desktop (balíček -winget.zip nemá PORTABLE.txt), takže
# upgrade přes winget o nastavení nepřijde.
#
# Odeslání do winget-pkgs (PR do microsoft/winget-pkgs, např. `wingetcreate submit <složka>`)
# vyžaduje, aby byl InstallerUrl veřejně stažitelný – repozitář s releasy musí být veřejný.
param(
    [string]$Version,
    [string]$Repo = 'dyeabel/WLS-Desktop-Container',
    [string]$Identifier = 'dyeabel.WLSDesktop',
    [string]$Publisher = 'dyeabel',
    [string]$License = 'Proprietary',
    [string]$ReleaseDate = (Get-Date -Format 'yyyy-MM-dd'),
    [switch]$NoValidate
)
$ErrorActionPreference = 'Continue'
$root = Split-Path $PSScriptRoot -Parent
Set-Location $root
if (-not $Version) {
    $Version = (Select-String -Path Cargo.toml -Pattern '^version\s*=\s*"([^"]+)"' | Select-Object -First 1).Matches[0].Groups[1].Value
}
$name = "WLS-Desktop-$Version-win-x64"
$zip = Join-Path $root "dist\$name-winget.zip"
if (-not (Test-Path $zip)) { throw "Chybí $zip – nejdřív scripts\package.ps1" }
$sha = (Get-FileHash $zip -Algorithm SHA256).Hash.ToUpper()
$url = "https://github.com/$Repo/releases/download/v$Version/$name-winget.zip"
$parts = $Identifier.Split('.')
$dir = Join-Path $root ("dist\winget\manifests\{0}\{1}\{2}\{3}" -f $Identifier.Substring(0, 1).ToLower(), $parts[0], ($parts[1..($parts.Length - 1)] -join '\'), $Version)
if (Test-Path $dir) { Remove-Item $dir -Recurse -Force }
New-Item -ItemType Directory -Force $dir | Out-Null
$mv = '1.10.0'
$schema = 'https://aka.ms/winget-manifest'

function Write-Yaml([string]$file, [string]$text) {
    # winget-pkgs: UTF-8 bez BOM, LF
    [IO.File]::WriteAllText((Join-Path $dir $file), ($text -replace "`r`n", "`n"), (New-Object Text.UTF8Encoding $false))
}

Write-Yaml "$Identifier.yaml" @"
# yaml-language-server: `$schema=$schema.version.$mv.schema.json
PackageIdentifier: $Identifier
PackageVersion: $Version
DefaultLocale: en-US
ManifestType: version
ManifestVersion: $mv
"@

Write-Yaml "$Identifier.installer.yaml" @"
# yaml-language-server: `$schema=$schema.installer.$mv.schema.json
PackageIdentifier: $Identifier
PackageVersion: $Version
Platform:
- Windows.Desktop
MinimumOSVersion: 10.0.19041.0
InstallerType: zip
NestedInstallerType: portable
NestedInstallerFiles:
- RelativeFilePath: $name\wls-desktop.exe
  PortableCommandAlias: wls-desktop
ReleaseDate: $ReleaseDate
Installers:
- Architecture: x64
  InstallerUrl: $url
  InstallerSha256: $sha
ManifestType: installer
ManifestVersion: $mv
"@

Write-Yaml "$Identifier.locale.en-US.yaml" @"
# yaml-language-server: `$schema=$schema.defaultLocale.$mv.schema.json
PackageIdentifier: $Identifier
PackageVersion: $Version
PackageLocale: en-US
Publisher: $Publisher
PublisherUrl: https://github.com/$($Repo.Split('/')[0])
PackageName: WLS Desktop
PackageUrl: https://github.com/$Repo
License: $License
ShortDescription: Native desktop app to manage Docker or Podman running in WSL 2 without Docker Desktop.
Description: |-
  WLS Desktop manages a Docker or Podman engine inside a WSL 2 distribution: containers, images,
  volumes, networks, compose stacks, logs, files, engine installation with TLS-secured API,
  and a docker.exe command for Windows that forwards to the engine in WSL.
Tags:
- docker
- podman
- wsl
- containers
- compose
ReleaseNotesUrl: https://github.com/$Repo/releases/tag/v$Version
ManifestType: defaultLocale
ManifestVersion: $mv
"@

Write-Host "Manifest: $dir"
Get-ChildItem $dir | ForEach-Object { Write-Host "  $($_.Name)" }
if (-not $NoValidate) {
    winget validate --manifest $dir
    if ($LASTEXITCODE -ne 0) { throw 'winget validate selhal' }
}
