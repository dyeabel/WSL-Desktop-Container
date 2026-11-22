<#
.SYNOPSIS
  Kompilace a testy na Linuxu v Podmanu uvnitř WSL distribuce WSLCD-Lite.

.DESCRIPTION
  check = cargo check --locked --bins --tests
  clippy = cargo clippy --locked --all-targets -- -D warnings
  test  = cargo test --locked
  live  = cargo test --locked <filtr> -- --ignored proti dočasné službě podman system service
  package <verze> = scripts/package-linux.sh v kontejneru; archiv skončí v dist\ (repo je jen pro čtení)

.EXAMPLE
  powershell -ExecutionPolicy Bypass -File scripts\linux-test.ps1 check
  powershell -ExecutionPolicy Bypass -File scripts\linux-test.ps1 clippy
  powershell -ExecutionPolicy Bypass -File scripts\linux-test.ps1 package 0.9.0-dev
  scripts\linux-test.ps1 test -- host::local
  scripts\linux-test.ps1 live -- host::local
  Přes "powershell -File" parametr "--" nefunguje (PowerShell ho odmítne), filtr tam zadejte bez něj:
  powershell -ExecutionPolicy Bypass -File scripts\linux-test.ps1 live host::local
#>
param(
    [Parameter(Mandatory = $true, Position = 0)]
    [ValidateSet('check', 'clippy', 'test', 'live', 'package')]
    [string]$Mode,
    [string]$Distro = 'WSLCD-Lite',
    [Parameter(ValueFromRemainingArguments = $true)]
    [string[]]$CargoArgs
)
$ErrorActionPreference = 'Stop'

# Cesta k repu → /mnt/<disk>/... (interop ve WSLCD-Lite je vypnutý, wslpath nejde volat).
$root = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
if ($root -notmatch '^([A-Za-z]):[\\/](.*)$') { throw "Nepodporovaná cesta: $root" }
$repo = '/mnt/' + $Matches[1].ToLower() + '/' + ($Matches[2] -replace '\\', '/')
$script = $repo + '/scripts/linux-test.sh'

# Odstranění úvodního "--" oddělovače argumentů pro cargo.
if ($CargoArgs -and $CargoArgs[0] -eq '--') { $CargoArgs = $CargoArgs[1..($CargoArgs.Count - 1)] }
if (-not $CargoArgs) { $CargoArgs = @() }

& wsl.exe -d $Distro -u root -e sh $script $Mode $repo @CargoArgs
exit $LASTEXITCODE
