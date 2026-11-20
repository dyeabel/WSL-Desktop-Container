# Podepíše soubory Authenticode podpisem (signtool) s časovým razítkem RFC 3161.
#
#   powershell -ExecutionPolicy Bypass -File scripts\sign.ps1 -Files a.exe,b.exe
#
# Způsob podpisu se volí parametry nebo proměnnými prostředí (první vyplněný vyhrává):
#
# 1) Azure Artifact Signing (dřív Trusted Signing) – doporučeno, ~10 USD/měsíc, certifikát je
#    v Microsoftu (žádný soubor s klíčem). Potřebuje Artifact Signing Client Tools
#    (winget install -e --id Microsoft.Azure.ArtifactSigningClientTools) nebo NuGet balíček
#    Microsoft.ArtifactSigning.Client, .NET 8 Runtime a přihlášení přes DefaultAzureCredential
#    (AZURE_TENANT_ID / AZURE_CLIENT_ID / AZURE_CLIENT_SECRET, nebo az login).
#      -Dlib <cesta k Azure.CodeSigning.Dlib.dll (x64)>   | WSLCD_SIGN_DLIB
#      -Metadata <metadata.json>                           | WSLCD_SIGN_METADATA
#    metadata.json: {"Endpoint":"https://weu.codesigning.azure.net","CodeSigningAccountName":"…","CertificateProfileName":"…"}
#
# 2) Certifikát v úložišti Windows (USB token / HSM / cloudové HSM s KSP, např. DigiCert KeyLocker,
#    SSL.com eSigner) podle otisku:
#      -Thumbprint <SHA1 otisk>                            | WSLCD_SIGN_THUMBPRINT
#
# 3) Soubor PFX (jen starší certifikáty nebo testovací – od 6/2023 vydávají veřejné CA klíče
#    jen na HSM):
#      -Pfx <soubor.pfx> -PfxPassword <heslo>             | WSLCD_SIGN_PFX, WSLCD_SIGN_PFX_PASSWORD
#
# -AllowUntrusted: po podpisu stačí, že podpis existuje (testovací self-signed certifikát);
# jinak se vyžaduje platný důvěryhodný podpis.
param(
    [Parameter(Mandatory = $true)][string[]]$Files,
    [string]$Dlib = $env:WSLCD_SIGN_DLIB,
    [string]$Metadata = $env:WSLCD_SIGN_METADATA,
    [string]$Thumbprint = $env:WSLCD_SIGN_THUMBPRINT,
    [string]$Pfx = $env:WSLCD_SIGN_PFX,
    [string]$PfxPassword = $env:WSLCD_SIGN_PFX_PASSWORD,
    [string]$TimestampUrl,
    [string]$Description = 'WSL Container Desktop',
    [string]$Url = 'https://github.com/dyeabel/WSL-Container-Desktop-Container',
    [switch]$AllowUntrusted
)
# signtool píše na stderr – úspěch se kontroluje přes $LASTEXITCODE
$ErrorActionPreference = 'Continue'

function Find-SignTool {
    $cmd = Get-Command signtool.exe -ErrorAction SilentlyContinue
    if ($cmd) { return $cmd.Source }
    $kits = Get-ChildItem "${env:ProgramFiles(x86)}\Windows Kits\10\bin\*\x64\signtool.exe" -ErrorAction SilentlyContinue |
        Sort-Object { [version]($_.Directory.Parent.Name) } -Descending
    if ($kits) { return $kits[0].FullName }
    throw 'signtool.exe nenalezen – nainstaluj Windows SDK (nebo NuGet Microsoft.Windows.SDK.BuildTools).'
}

# powershell -File předá pole jako jeden řetězec – dovolit seznam oddělený čárkou / středníkem
$Files = @($Files | ForEach-Object { $_ -split '[,;]' } | ForEach-Object { $_.Trim().Trim('"') } | Where-Object { $_ })
$signtool = Find-SignTool
$common = @('sign', '/fd', 'SHA256', '/td', 'SHA256', '/d', $Description, '/du', $Url)

if ($Dlib -and $Metadata) {
    $mode = 'Azure Artifact Signing'
    if (-not (Test-Path $Dlib)) { throw "Dlib nenalezena: $Dlib" }
    if (-not (Test-Path $Metadata)) { throw "metadata.json nenalezen: $Metadata" }
    if (-not $TimestampUrl) { $TimestampUrl = 'http://timestamp.acs.microsoft.com' }
    $method = @('/dlib', $Dlib, '/dmdf', $Metadata)
} elseif ($Thumbprint) {
    $mode = "certifikát z úložiště ($Thumbprint)"
    if (-not $TimestampUrl) { $TimestampUrl = 'http://timestamp.digicert.com' }
    $method = @('/sha1', $Thumbprint)
} elseif ($Pfx) {
    $mode = "PFX ($Pfx)"
    if (-not (Test-Path $Pfx)) { throw "PFX nenalezen: $Pfx" }
    if (-not $TimestampUrl) { $TimestampUrl = 'http://timestamp.digicert.com' }
    $method = @('/f', $Pfx)
    if ($PfxPassword) { $method += @('/p', $PfxPassword) }
} else {
    throw 'Není nastavený způsob podpisu (WSLCD_SIGN_DLIB+WSLCD_SIGN_METADATA, WSLCD_SIGN_THUMBPRINT nebo WSLCD_SIGN_PFX).'
}

Write-Host "Podpis: $mode, razítko $TimestampUrl"
foreach ($f in $Files) {
    if (-not (Test-Path $f)) { throw "Soubor nenalezen: $f" }
    $ok = $false
    # časové razítko občas selže na síti – zkusit znovu
    for ($i = 1; $i -le 3 -and -not $ok; $i++) {
        & $signtool @common '/tr' $TimestampUrl @method $f | Out-Host
        $ok = ($LASTEXITCODE -eq 0)
        if (-not $ok) { Start-Sleep -Seconds (5 * $i) }
    }
    if (-not $ok) { throw "Podpis selhal: $f" }
    $sig = Get-AuthenticodeSignature $f
    $valid = $sig.Status -eq 'Valid'
    if (-not $sig.SignerCertificate) { throw "Soubor nemá podpis: $f" }
    if (-not $valid -and -not $AllowUntrusted) { throw "Podpis není platný ($($sig.Status)): $f" }
    $ts = if ($sig.TimeStamperCertificate) { 's razítkem' } else { 'BEZ razítka' }
    Write-Host ("  {0}: {1} – {2}, {3}" -f (Split-Path $f -Leaf), $sig.Status, $sig.SignerCertificate.Subject, $ts)
}
