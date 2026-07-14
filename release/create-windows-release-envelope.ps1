[CmdletBinding()]
param(
  [Parameter(Mandatory)]
  [string]$Manifest,
  [Parameter(Mandatory)]
  [string]$Out
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

if (Test-Path -LiteralPath $Out) {
  throw "release envelope output already exists: $Out"
}
if (-not (Test-Path -LiteralPath $Manifest -PathType Leaf)) {
  throw "release manifest is not a regular file: $Manifest"
}
$manifestItem = Get-Item -LiteralPath $Manifest -Force
if ($manifestItem.PSIsContainer -or
    (($manifestItem.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0)) {
  throw "release manifest is unsafe: $Manifest"
}
$manifestBytes = [IO.File]::ReadAllBytes($Manifest)
if ($manifestBytes.Length -lt 1 -or $manifestBytes.Length -gt 65536) {
  throw "release manifest is outside the permitted envelope size"
}
$utf8 = New-Object Text.UTF8Encoding($false, $true)
try {
  [void]$utf8.GetString($manifestBytes)
} catch {
  throw "release manifest is not valid UTF-8"
}
$hash = (Get-FileHash -LiteralPath $Manifest -Algorithm SHA256).Hash.ToLowerInvariant()
$encoded = [Convert]::ToBase64String($manifestBytes)
$contents = @(
  "# grimmore-release-manifest-v1: $encoded",
  "# grimmore-release-manifest-sha256: $hash",
  ""
) -join "`r`n"
[IO.File]::WriteAllText($Out, $contents, (New-Object Text.UTF8Encoding($false)))
Write-Output "Created data-only release envelope. Sign this .ps1 with the externally provisioned test certificate before installation."
