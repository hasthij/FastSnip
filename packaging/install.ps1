<#
.SYNOPSIS
  Installs FastSnip from FastSnip.msix in this folder.

.DESCRIPTION
  Test builds are signed with a self-signed certificate, so Windows has to be
  told to trust it once. That one step needs admin (you'll see one prompt).
  FastSnip itself is then installed for your account only.

  Builds from the Microsoft Store don't need this script.
#>
param([switch]$TrustOnly)
$ErrorActionPreference = "Stop"
$here = $PSScriptRoot
$cer = Join-Path $here "FastSnip.cer"
$msix = Join-Path $here "FastSnip.msix"

function IsAdmin {
    $id = [Security.Principal.WindowsIdentity]::GetCurrent()
    (New-Object Security.Principal.WindowsPrincipal $id).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
}

if ($TrustOnly) {
    # Elevated child: trust the certificate for package installs only (Trusted People).
    Import-Certificate -FilePath $cer -CertStoreLocation Cert:\LocalMachine\TrustedPeople | Out-Null
    return
}

if (-not (Test-Path $msix)) { throw "FastSnip.msix isn't next to this script." }

$cert = New-Object System.Security.Cryptography.X509Certificates.X509Certificate2 $cer
$trusted = Get-ChildItem Cert:\LocalMachine\TrustedPeople | Where-Object Thumbprint -eq $cert.Thumbprint
if (-not $trusted) {
    Write-Host "Trusting the FastSnip test certificate ($($cert.Subject)). Windows will ask for admin once."
    $p = Start-Process powershell -Verb RunAs -Wait -PassThru -ArgumentList @(
        "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", "`"$PSCommandPath`"", "-TrustOnly")
    if ($p.ExitCode -ne 0) { throw "The certificate wasn't trusted, so FastSnip can't be installed." }
}

Write-Host "Installing FastSnip..."
Get-Process fastsnip, FastSnip.App -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
Add-AppxPackage -Path $msix -ForceUpdateFromAnyVersion -ForceApplicationShutdown

$pkg = Get-AppxPackage -Name FastSnip | Select-Object -First 1
Write-Host "Installed FastSnip $($pkg.Version). Starting it..."
Start-Process "shell:AppsFolder\$($pkg.PackageFamilyName)!App"
