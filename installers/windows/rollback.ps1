[CmdletBinding()]
param(
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

try {
  $transaction = New-GrimmorePrivateDirectory -Parent $installation.Root -Prefix ".rollback-" -Sid $installation.Sid
  $current = Read-GrimmorePointerState `
    -Path (Join-Path $installation.Root "current.json") `
    -ExpectedTarget $target
  $previous = Read-GrimmorePointerState `
    -Path (Join-Path $installation.Root "previous.json") `
    -ExpectedTarget $target
  if ($current.Version -ceq $previous.Version) {
    throw "grimmore release: rollback state does not identify a distinct prior version"
  }
  Assert-GrimmoreReadyVersion `
    -VersionDirectory (Join-Path $installation.Versions $current.Version) `
    -ExpectedTarget $target `
    -ExpectedVersion $current.Version `
    -Thumbprint $thumbprint `
    -StandardErrorPath (Join-Path $transaction "current-doctor.stderr") | Out-Null
  Assert-GrimmoreReadyVersion `
    -VersionDirectory (Join-Path $installation.Versions $previous.Version) `
    -ExpectedTarget $target `
    -ExpectedVersion $previous.Version `
    -Thumbprint $thumbprint `
    -StandardErrorPath (Join-Path $transaction "previous-doctor.stderr") | Out-Null
  $currentLauncherHash = Get-GrimmoreSha256 `
    -Path (Join-Path $installation.Versions "$($current.Version)\grimmore-launcher.exe")
  $previousLauncherHash = Get-GrimmoreSha256 `
    -Path (Join-Path $installation.Versions "$($previous.Version)\grimmore-launcher.exe")
  if ($current.LauncherHash -cne $currentLauncherHash -or
      $previous.LauncherHash -cne $previousLauncherHash) {
    throw "grimmore release: rollback state does not match its verified launcher"
  }
  Ensure-GrimmoreStableLauncher `
    -Bin $installation.Bin `
    -VersionDirectory (Join-Path $installation.Versions $current.Version) `
    -Thumbprint $thumbprint
  Switch-GrimmorePointerState `
    -Root $installation.Root `
    -Target $target `
    -Version $previous.Version `
    -LauncherHash $previousLauncherHash
  Write-Output "Rolled back to $($previous.Version)."
} finally {
  if ($null -ne $transaction) {
    Remove-GrimmorePrivateDirectory -Path $transaction
  }
  $lock.Dispose()
}
