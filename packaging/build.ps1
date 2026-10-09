<#
.SYNOPSIS
  Builds FastSnip and packages it as an MSIX.

.DESCRIPTION
  Test build (default): signs the .msix with a free self-signed certificate
  ("CN=Hasthi J", created in your personal certificate store on first run)
  and puts FastSnip.msix, FastSnip.cer and install.ps1 in out\release.

  Store build (-Store): makes an unsigned .msixupload for Partner Center.
  The Store signs it for free. Pass the identity Partner Center shows under
  Product identity.

.EXAMPLE
  .\packaging\build.ps1 -Version 0.1.0.0
.EXAMPLE
  .\packaging\build.ps1 -Version 0.1.0.0 -Store -IdentityName 12345FastSnip.FastSnip -Publisher "CN=ABCD-1234"
#>
param(
    [string]$Version = "0.1.0.0",
    [ValidateSet("x64", "ARM64")] [string]$Arch = "x64",
    [switch]$Store,
    [string]$IdentityName = "FastSnip",
    [string]$Publisher = "CN=Hasthi J",
    [string]$PublisherDisplayName = "Hasthi J"
)
$ErrorActionPreference = "Stop"
$root = Split-Path $PSScriptRoot -Parent
$out = Join-Path $root "out"
$app = Join-Path $root "app\FastSnip.App"
$manifest = Join-Path $app "Package.appxmanifest"
$rid = if ($Arch -eq "x64") { "win-x64" } else { "win-arm64" }
$rustTarget = if ($Arch -eq "x64") { "x86_64-pc-windows-msvc" } else { "aarch64-pc-windows-msvc" }

function Step($text) { Write-Host "`n== $text" -ForegroundColor Cyan }

# Run a native tool; its progress on stderr is shown, not treated as an error.
function Native([string]$exe, [string[]]$argv) {
    $old = $ErrorActionPreference
    $ErrorActionPreference = "Continue"
    & $exe @argv 2>&1 | ForEach-Object { "$_" } | Out-Host
    $code = $LASTEXITCODE
    $ErrorActionPreference = $old
    if ($code) { throw "$exe failed with exit code $code" }
}

Step "Core (Rust, $Arch)"
Push-Location (Join-Path $root "core")
Native cargo @("build", "--release", "--target", $rustTarget)
Pop-Location
$coreExe = Join-Path $root "core\target\$rustTarget\release\fastsnip.exe"

Step "Package identity $IdentityName / $Publisher, version $Version"
$original = Get-Content $manifest -Raw
$m = $original -replace 'Name="FastSnip" Publisher="[^"]*" Version="[^"]*"', "Name=`"$IdentityName`" Publisher=`"$Publisher`" Version=`"$Version`""
$m = $m -replace '<PublisherDisplayName>[^<]*</PublisherDisplayName>', "<PublisherDisplayName>$PublisherDisplayName</PublisherDisplayName>"
Set-Content $manifest $m -NoNewline

try {
    Step "App window (.NET, self-contained) and MSIX"
    if (Test-Path "$out\pkg") { Remove-Item "$out\pkg" -Recurse -Force }
    $pub = @(
        "publish", $app, "-c", "Release", "-r", $rid, "-p:Platform=$Arch",
        "-p:WindowsPackageType=MSIX", "-p:GenerateAppxPackageOnBuild=true",
        "-p:AppxPackageSigningEnabled=false", "-p:AppxBundle=Never",
        "-p:CoreExe=$coreExe", "-p:AppxPackageDir=$out\pkg\"
    )
    if ($Store) { $pub += "-p:UapAppxPackageBuildMode=StoreUpload" }
    Native dotnet $pub
}
finally {
    Set-Content $manifest $original -NoNewline
}

$rel = Join-Path $out "release"
New-Item $rel -ItemType Directory -Force | Out-Null

if ($Store) {
    $upload = Get-ChildItem "$out\pkg" -Recurse -Include *.msixupload | Select-Object -First 1
    if (-not $upload) { throw "No .msixupload was produced" }
    Copy-Item $upload.FullName (Join-Path $rel "FastSnip_$Version`_$Arch.msixupload") -Force
    Step "Done: upload out\release\FastSnip_$Version`_$Arch.msixupload in Partner Center"
    return
}

$msix = Get-ChildItem "$out\pkg" -Recurse -Filter *.msix | Select-Object -First 1
if (-not $msix) { throw "No .msix was produced" }

Step "Signing with the self-signed certificate"
$cert = Get-ChildItem Cert:\CurrentUser\My | Where-Object { $_.Subject -eq $Publisher -and $_.HasPrivateKey -and $_.NotAfter -gt (Get-Date) } | Select-Object -First 1
if (-not $cert) {
    Write-Host "Creating a free self-signed certificate $Publisher (in your personal store, not trusted by anything yet)"
    $cert = New-SelfSignedCertificate -Type Custom -Subject $Publisher -KeyUsage DigitalSignature `
        -FriendlyName "Hasthi J (FastSnip signing)" -CertStoreLocation Cert:\CurrentUser\My -NotAfter (Get-Date).AddYears(3) `
        -TextExtension @("2.5.29.37={text}1.3.6.1.5.5.7.3.3", "2.5.29.19={text}")
}
$kits = Get-ChildItem "${env:ProgramFiles(x86)}\Windows Kits\10\bin" -Directory | Where-Object Name -match '^10\.' | Sort-Object Name -Descending
$signtool = $kits | ForEach-Object { Join-Path $_.FullName "x64\signtool.exe" } | Where-Object { Test-Path $_ } | Select-Object -First 1
if (-not $signtool) { throw "signtool.exe not found (install the Windows SDK)" }
$target = Join-Path $rel "FastSnip.msix"
Copy-Item $msix.FullName $target -Force
Native $signtool @("sign", "/fd", "SHA256", "/sha1", $cert.Thumbprint, "/s", "My", $target)
Export-Certificate -Cert $cert -FilePath (Join-Path $rel "FastSnip.cer") | Out-Null
Copy-Item (Join-Path $PSScriptRoot "install.ps1") $rel -Force
Copy-Item (Join-Path $PSScriptRoot "Install FastSnip.cmd") $rel -Force

$size = [math]::Round((Get-Item $target).Length / 1MB, 1)
Step "Done: out\release\FastSnip.msix ($size MB), FastSnip.cer, install.ps1, Install FastSnip.cmd"
