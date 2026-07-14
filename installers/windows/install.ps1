[CmdletBinding()]
param(
  [Parameter(Mandatory)]
  [string]$Archive,
  [Parameter(Mandatory)]
  [string]$ReleaseEnvelope,
  [Parameter(Mandatory)]
  [string]$TrustedPublisherThumbprint,
  [string]$InstallRoot = (Join-Path $env:LOCALAPPDATA "Grimmore")
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

Import-Module (Join-Path $PSScriptRoot "release-common.psm1") -Force

$thumbprint = ConvertTo-GrimmoreThumbprint -Thumbprint $TrustedPublisherThumbprint
Assert-GrimmoreTrustedPublisher -Thumbprint $thumbprint
$target = Get-GrimmoreWindowsTarget
$installation = Initialize-GrimmoreInstallRoot -Root $InstallRoot
$lock = Enter-GrimmoreInstallLock -Root $installation.Root
$transaction = $null
$staging = $null

try {
  $transaction = New-GrimmorePrivateDirectory -Parent $installation.Root -Prefix ".install-" -Sid $installation.Sid
  $transactionArchive = Join-Path $transaction "artifact.zip"
  $transactionEnvelope = Join-Path $transaction "release-envelope.ps1"
  Copy-GrimmoreInputFile `
    -Source $Archive `
    -Destination $transactionArchive `
    -Description "release archive" `
    -MaximumBytes 536870912
  Copy-GrimmoreInputFile `
    -Source $ReleaseEnvelope `
    -Destination $transactionEnvelope `
    -Description "release envelope" `
    -MaximumBytes 131072

  $envelope = Read-GrimmoreReleaseEnvelope -Path $transactionEnvelope -Thumbprint $thumbprint
  $manifest = Read-GrimmoreReleaseManifest -Json $envelope.ManifestJson -ExpectedTarget $target
  if ([IO.Path]::GetFileName($Archive) -cne $manifest.ArtifactFile) {
    throw "grimmore release: release archive name does not match the signed manifest"
  }
  $archiveItem = Assert-GrimmoreRegularFile -Path $transactionArchive -Description "release archive"
  if ($archiveItem.Length -ne $manifest.ArtifactSize) {
    throw "grimmore release: release archive size does not match the signed manifest"
  }
  if ((Get-GrimmoreSha256 -Path $transactionArchive) -cne $manifest.ArtifactHash) {
    throw "grimmore release: release archive hash does not match the signed manifest"
  }

  $transactionManifest = Join-Path $transaction "release-manifest.json"
  [IO.File]::WriteAllBytes($transactionManifest, $envelope.ManifestBytes)
  Remove-GrimmoreStaleStagingDirectories -Versions $installation.Versions -Version $manifest.Version
  $versionDirectory = Join-Path $installation.Versions $manifest.Version
  if (Test-Path -LiteralPath $versionDirectory) {
    Assert-GrimmoreReadyVersion `
      -VersionDirectory $versionDirectory `
      -ExpectedTarget $target `
      -ExpectedVersion $manifest.Version `
      -Thumbprint $thumbprint `
      -TransactionManifest $transactionManifest `
      -TransactionEnvelope $transactionEnvelope `
      -StandardErrorPath (Join-Path $transaction "existing-doctor.stderr") | Out-Null
  } else {
    $staging = New-GrimmorePrivateDirectory `
      -Parent $installation.Versions `
      -Prefix ".staging-$($manifest.Version)-" `
      -Sid $installation.Sid
    $payloadRoot = "grimmore-$($manifest.Version)-$target"
    Expand-GrimmoreZipPayload -Archive $transactionArchive -PayloadRoot $payloadRoot -Destination $staging
    Assert-GrimmoreAuthenticodeSignature `
      -Path (Join-Path $staging "grimmored.exe") `
      -Thumbprint $thumbprint `
      -Description "staged companion"
    Assert-GrimmoreAuthenticodeSignature `
      -Path (Join-Path $staging "grimmore-launcher.exe") `
      -Thumbprint $thumbprint `
      -Description "staged versioned launcher"
    Assert-GrimmoreDoctorReport `
      -Daemon (Join-Path $staging "grimmored.exe") `
      -StandardErrorPath (Join-Path $transaction "staged-doctor.stderr") `
      -ProtocolMinimum $manifest.ProtocolMinimum `
      -ProtocolMaximum $manifest.ProtocolMaximum
    Copy-GrimmoreInputFile `
      -Source $transactionManifest `
      -Destination (Join-Path $staging "release-manifest.json") `
      -Description "verified release manifest"
    Copy-GrimmoreInputFile `
      -Source $transactionEnvelope `
      -Destination (Join-Path $staging "release-envelope.ps1") `
      -Description "verified release envelope"
    Write-GrimmoreReadyMarker -VersionDirectory $staging
    [IO.Directory]::Move($staging, $versionDirectory)
    $staging = $null
    Assert-GrimmoreReadyVersion `
      -VersionDirectory $versionDirectory `
      -ExpectedTarget $target `
      -ExpectedVersion $manifest.Version `
      -Thumbprint $thumbprint `
      -TransactionManifest $transactionManifest `
      -TransactionEnvelope $transactionEnvelope `
      -StandardErrorPath (Join-Path $transaction "ready-doctor.stderr") | Out-Null
  }

  $versionedLauncher = Join-Path $versionDirectory "grimmore-launcher.exe"
  $versionedLauncherHash = Get-GrimmoreSha256 -Path $versionedLauncher

  $currentStatePath = Join-Path $installation.Root "current.json"
  $current = $null
  if (Test-Path -LiteralPath $currentStatePath) {
    $current = Read-GrimmorePointerState -Path $currentStatePath -ExpectedTarget $target
    Assert-GrimmoreReadyVersion `
      -VersionDirectory (Join-Path $installation.Versions $current.Version) `
      -ExpectedTarget $target `
      -ExpectedVersion $current.Version `
      -Thumbprint $thumbprint `
      -StandardErrorPath (Join-Path $transaction "current-doctor.stderr") | Out-Null
    $currentLauncherHash = Get-GrimmoreSha256 `
      -Path (Join-Path $installation.Versions "$($current.Version)\grimmore-launcher.exe")
    if ($current.LauncherHash -cne $currentLauncherHash) {
      throw "grimmore release: current installation state does not match its verified launcher"
    }
  }
  Ensure-GrimmoreStableLauncher `
    -Bin $installation.Bin `
    -VersionDirectory $versionDirectory `
    -Thumbprint $thumbprint
  if ($null -eq $current -or $current.Version -cne $manifest.Version) {
    Switch-GrimmorePointerState `
      -Root $installation.Root `
      -Target $target `
      -Version $manifest.Version `
      -LauncherHash $versionedLauncherHash
  }
  $pathWasUpdated = Add-GrimmoreBinToUserPath -Bin $installation.Bin
  Write-Output "Installed Grimmore $($manifest.Version) for $target."
  if ($pathWasUpdated) {
    Write-Output "Restart Obsidian or its parent shell before invoking the stable grimmore-launcher command."
  }
} finally {
  if ($null -ne $staging) {
    Remove-GrimmorePrivateDirectory -Path $staging
  }
  if ($null -ne $transaction) {
    Remove-GrimmorePrivateDirectory -Path $transaction
  }
  $lock.Dispose()
}
