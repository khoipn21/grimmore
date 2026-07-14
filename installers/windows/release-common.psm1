Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

Set-Variable -Name GrimmoreReleaseSchema -Option ReadOnly -Scope Script -Value `
  "https://grimmore.dev/schemas/release-manifest-v1.json"
Set-Variable -Name GrimmoreMaximumEnvelopeBytes -Option ReadOnly -Scope Script -Value 65536
Set-Variable -Name GrimmoreMaximumEnvelopeFileBytes -Option ReadOnly -Scope Script -Value 131072
Set-Variable -Name GrimmoreMaximumZipBytes -Option ReadOnly -Scope Script -Value 536870912

function Stop-GrimmoreRelease {
  param(
    [Parameter(Mandatory)]
    [string]$Message
  )

  throw "grimmore release: $Message"
}

function Get-GrimmoreCurrentUserSid {
  $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
  if ($null -eq $identity.User) {
    Stop-GrimmoreRelease "cannot determine the current Windows user SID"
  }
  return $identity.User.Value
}

function Get-GrimmoreDefaultInstallRoot {
  if ([string]::IsNullOrWhiteSpace($env:LOCALAPPDATA)) {
    Stop-GrimmoreRelease "LOCALAPPDATA is unavailable for the current user"
  }
  return (Join-Path $env:LOCALAPPDATA "Grimmore")
}

function Resolve-GrimmoreInstallRoot {
  param(
    [Parameter(Mandatory)]
    [string]$Path
  )

  if (-not [IO.Path]::IsPathRooted($Path)) {
    Stop-GrimmoreRelease "installation root must be an absolute path"
  }
  $fullPath = [IO.Path]::GetFullPath($Path)
  $trimmedPath = $fullPath.TrimEnd([char[]]@("\\", "/"))
  $rootPath = [IO.Path]::GetPathRoot($fullPath).TrimEnd([char[]]@("\\", "/"))
  if ([string]::IsNullOrEmpty($trimmedPath) -or $trimmedPath -ceq $rootPath) {
    Stop-GrimmoreRelease "installation root may not be a filesystem root"
  }
  return $trimmedPath
}

function Test-GrimmoreReparsePoint {
  param(
    [Parameter(Mandatory)]
    [string]$Path
  )

  $item = Get-Item -LiteralPath $Path -Force
  return (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0)
}

function Assert-GrimmoreRegularFile {
  param(
    [Parameter(Mandatory)]
    [string]$Path,
    [Parameter(Mandatory)]
    [string]$Description
  )

  if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
    Stop-GrimmoreRelease "$Description is not a regular file: $Path"
  }
  $item = Get-Item -LiteralPath $Path -Force
  if ($item.PSIsContainer -or (Test-GrimmoreReparsePoint -Path $Path)) {
    Stop-GrimmoreRelease "$Description is unsafe: $Path"
  }
  return $item
}

function Set-GrimmorePrivateDirectory {
  param(
    [Parameter(Mandatory)]
    [string]$Path,
    [Parameter(Mandatory)]
    [string]$Sid
  )

  if (Test-Path -LiteralPath $Path) {
    $item = Get-Item -LiteralPath $Path -Force
    if (-not $item.PSIsContainer -or (Test-GrimmoreReparsePoint -Path $Path)) {
      Stop-GrimmoreRelease "installation directory is unsafe: $Path"
    }
    $existingAcl = Get-Acl -LiteralPath $Path
    $owner = $existingAcl.GetOwner([Security.Principal.SecurityIdentifier]).Value
    if ($owner -cne $Sid) {
      Stop-GrimmoreRelease "installation directory is not owned by the current user: $Path"
    }
  } else {
    [void][IO.Directory]::CreateDirectory($Path)
  }

  $sidObject = New-Object Security.Principal.SecurityIdentifier($Sid)
  $rights = [Security.AccessControl.FileSystemRights]::FullControl
  $inheritance = [Security.AccessControl.InheritanceFlags]::ContainerInherit -bor `
    [Security.AccessControl.InheritanceFlags]::ObjectInherit
  $rule = New-Object Security.AccessControl.FileSystemAccessRule(
    $sidObject,
    $rights,
    $inheritance,
    [Security.AccessControl.PropagationFlags]::None,
    [Security.AccessControl.AccessControlType]::Allow
  )
  $privateAcl = New-Object Security.AccessControl.DirectorySecurity
  $privateAcl.SetOwner($sidObject)
  $privateAcl.SetAccessRuleProtection($true, $false)
  [void]$privateAcl.AddAccessRule($rule)
  Set-Acl -LiteralPath $Path -AclObject $privateAcl
}

function Initialize-GrimmoreInstallRoot {
  param(
    [Parameter(Mandatory)]
    [string]$Root
  )

  $root = Resolve-GrimmoreInstallRoot -Path $Root
  $sid = Get-GrimmoreCurrentUserSid
  $versions = Join-Path $root "versions"
  $bin = Join-Path $root "bin"
  Set-GrimmorePrivateDirectory -Path $root -Sid $sid
  Set-GrimmorePrivateDirectory -Path $versions -Sid $sid
  Set-GrimmorePrivateDirectory -Path $bin -Sid $sid
  return [pscustomobject]@{
    Root = $root
    Versions = $versions
    Bin = $bin
    Sid = $sid
  }
}

function Enter-GrimmoreInstallLock {
  param(
    [Parameter(Mandatory)]
    [string]$Root
  )

  $path = Join-Path $Root ".install.lock"
  if (Test-Path -LiteralPath $path) {
    [void](Assert-GrimmoreRegularFile -Path $path -Description "installation lock")
  }
  for ($attempt = 0; $attempt -lt 100; $attempt += 1) {
    try {
      return [IO.File]::Open(
        $path,
        [IO.FileMode]::OpenOrCreate,
        [IO.FileAccess]::ReadWrite,
        [IO.FileShare]::None
      )
    } catch [IO.IOException] {
      Start-Sleep -Milliseconds 100
    }
  }
  Stop-GrimmoreRelease "another install or rollback is still active"
}

function New-GrimmorePrivateDirectory {
  param(
    [Parameter(Mandatory)]
    [string]$Parent,
    [Parameter(Mandatory)]
    [string]$Prefix,
    [Parameter(Mandatory)]
    [string]$Sid
  )

  $path = Join-Path $Parent ("{0}{1}" -f $Prefix, [Guid]::NewGuid().ToString("N"))
  Set-GrimmorePrivateDirectory -Path $path -Sid $Sid
  return $path
}

function Remove-GrimmorePrivateDirectory {
  param(
    [Parameter(Mandatory)]
    [string]$Path
  )

  if (-not (Test-Path -LiteralPath $Path)) {
    return
  }
  $item = Get-Item -LiteralPath $Path -Force
  if (-not $item.PSIsContainer -or (Test-GrimmoreReparsePoint -Path $Path)) {
    Stop-GrimmoreRelease "refusing to remove an unsafe staging directory: $Path"
  }
  Remove-Item -LiteralPath $Path -Recurse -Force
}

function Copy-GrimmoreInputFile {
  param(
    [Parameter(Mandatory)]
    [string]$Source,
    [Parameter(Mandatory)]
    [string]$Destination,
    [Parameter(Mandatory)]
    [string]$Description,
    [long]$MaximumBytes = 9223372036854775807
  )

  $sourceItem = Assert-GrimmoreRegularFile -Path $Source -Description $Description
  if ($sourceItem.Length -gt $MaximumBytes) {
    Stop-GrimmoreRelease "$Description exceeds the permitted input size"
  }
  $input = $null
  $output = $null
  try {
    $input = [IO.File]::Open(
      $Source,
      [IO.FileMode]::Open,
      [IO.FileAccess]::Read,
      [IO.FileShare]::Read
    )
    $output = [IO.File]::Open(
      $Destination,
      [IO.FileMode]::CreateNew,
      [IO.FileAccess]::Write,
      [IO.FileShare]::None
    )
    $buffer = New-Object byte[] 81920
    [long]$copiedBytes = 0
    while (($read = $input.Read($buffer, 0, $buffer.Length)) -gt 0) {
      $copiedBytes += $read
      if ($copiedBytes -gt $MaximumBytes) {
        Stop-GrimmoreRelease "$Description exceeds the permitted input size"
      }
      $output.Write($buffer, 0, $read)
    }
    $output.Flush($true)
  } finally {
    if ($null -ne $output) {
      $output.Dispose()
    }
    if ($null -ne $input) {
      $input.Dispose()
    }
  }
}

function Get-GrimmoreSha256 {
  param(
    [Parameter(Mandatory)]
    [string]$Path
  )

  return (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant()
}

function Get-GrimmoreSha256Bytes {
  param(
    [Parameter(Mandatory)]
    [byte[]]$Bytes
  )

  $hasher = [Security.Cryptography.SHA256]::Create()
  try {
    return [BitConverter]::ToString($hasher.ComputeHash($Bytes)).Replace("-", "").ToLowerInvariant()
  } finally {
    $hasher.Dispose()
  }
}

function ConvertTo-GrimmoreThumbprint {
  param(
    [Parameter(Mandatory)]
    [string]$Thumbprint
  )

  if ($Thumbprint -cnotmatch "^[0-9A-Fa-f]{40}$") {
    Stop-GrimmoreRelease "trusted publisher thumbprint must be exactly 40 hexadecimal characters"
  }
  return $Thumbprint.ToUpperInvariant()
}

function Assert-GrimmoreTrustedPublisher {
  param(
    [Parameter(Mandatory)]
    [string]$Thumbprint
  )

  $trusted = @(Get-ChildItem -Path Cert:\CurrentUser\TrustedPublisher -ErrorAction Stop |
      Where-Object { $_.Thumbprint -ceq $Thumbprint })
  if ($trusted.Count -lt 1) {
    Stop-GrimmoreRelease "the pinned test publisher is not trusted for the current user"
  }
}

function Assert-GrimmoreAuthenticodeSignature {
  param(
    [Parameter(Mandatory)]
    [string]$Path,
    [Parameter(Mandatory)]
    [string]$Thumbprint,
    [Parameter(Mandatory)]
    [string]$Description
  )

  [void](Assert-GrimmoreRegularFile -Path $Path -Description $Description)
  $signature = Get-AuthenticodeSignature -LiteralPath $Path
  if ($signature.Status.ToString() -cne "Valid" -or $null -eq $signature.SignerCertificate) {
    Stop-GrimmoreRelease "$Description does not have a valid Authenticode signature"
  }
  $actualThumbprint = ConvertTo-GrimmoreThumbprint -Thumbprint $signature.SignerCertificate.Thumbprint
  if ($actualThumbprint -cne $Thumbprint) {
    Stop-GrimmoreRelease "$Description signer does not match the pinned test publisher"
  }
}

function ConvertFrom-GrimmoreUtf8 {
  param(
    [Parameter(Mandatory)]
    [byte[]]$Bytes,
    [Parameter(Mandatory)]
    [string]$Description
  )

  $encoding = New-Object Text.UTF8Encoding($false, $true)
  try {
    return $encoding.GetString($Bytes)
  } catch {
    Stop-GrimmoreRelease "$Description is not valid UTF-8"
  }
}

function Read-GrimmoreReleaseEnvelope {
  param(
    [Parameter(Mandatory)]
    [string]$Path,
    [Parameter(Mandatory)]
    [string]$Thumbprint
  )

  $envelopeItem = Assert-GrimmoreRegularFile -Path $Path -Description "release envelope"
  if ($envelopeItem.Length -lt 1 -or $envelopeItem.Length -gt $script:GrimmoreMaximumEnvelopeFileBytes) {
    Stop-GrimmoreRelease "release envelope is outside the permitted size"
  }
  Assert-GrimmoreAuthenticodeSignature -Path $Path -Thumbprint $Thumbprint -Description "release envelope"
  $bytes = [IO.File]::ReadAllBytes($Path)
  $text = ConvertFrom-GrimmoreUtf8 -Bytes $bytes -Description "release envelope"
  $lines = @($text -split "\r?\n")
  $signatureIndex = -1
  for ($index = 0; $index -lt $lines.Count; $index += 1) {
    if ($lines[$index] -ceq "# SIG # Begin signature block") {
      if ($signatureIndex -ne -1) {
        Stop-GrimmoreRelease "release envelope has multiple signature blocks"
      }
      $signatureIndex = $index
    }
  }
  if ($signatureIndex -lt 1) {
    Stop-GrimmoreRelease "release envelope does not contain a signed data section"
  }
  $records = @($lines[0..($signatureIndex - 1)] | Where-Object { $_.Length -gt 0 })
  if ($records.Count -ne 2) {
    Stop-GrimmoreRelease "release envelope data section has unexpected records"
  }
  if ($records[0] -cnotmatch "^# grimmore-release-manifest-v1: ([A-Za-z0-9+/]+={0,2})$") {
    Stop-GrimmoreRelease "release envelope does not contain a valid manifest record"
  }
  $encodedManifest = $Matches[1]
  if ($records[1] -cnotmatch "^# grimmore-release-manifest-sha256: ([a-f0-9]{64})$") {
    Stop-GrimmoreRelease "release envelope does not contain a valid manifest hash"
  }
  $expectedHash = $Matches[1]
  try {
    $manifestBytes = [Convert]::FromBase64String($encodedManifest)
  } catch {
    Stop-GrimmoreRelease "release envelope manifest record is not valid base64"
  }
  if ($manifestBytes.Length -lt 1 -or $manifestBytes.Length -gt $script:GrimmoreMaximumEnvelopeBytes) {
    Stop-GrimmoreRelease "release envelope manifest is outside the permitted size"
  }
  if ((Get-GrimmoreSha256Bytes -Bytes $manifestBytes) -cne $expectedHash) {
    Stop-GrimmoreRelease "release envelope manifest hash does not match its signed record"
  }
  return [pscustomobject]@{
    ManifestBytes = $manifestBytes
    ManifestJson = ConvertFrom-GrimmoreUtf8 -Bytes $manifestBytes -Description "release manifest"
    ManifestHash = $expectedHash
  }
}

function ConvertFrom-GrimmoreJson {
  param(
    [Parameter(Mandatory)]
    [string]$Json,
    [Parameter(Mandatory)]
    [string]$Description
  )

  try {
    $convertFromJson = Get-Command ConvertFrom-Json -ErrorAction Stop
    if ($convertFromJson.Parameters.ContainsKey("DateKind")) {
      return ($Json | ConvertFrom-Json -DateKind String -ErrorAction Stop)
    }
    Add-Type -AssemblyName System.Web.Extensions -ErrorAction Stop
    $serializer = New-Object System.Web.Script.Serialization.JavaScriptSerializer
    $serializer.MaxJsonLength = $script:GrimmoreMaximumEnvelopeBytes
    return $serializer.DeserializeObject($Json)
  } catch {
    Stop-GrimmoreRelease "$Description is not valid JSON"
  }
}

function Assert-GrimmoreExactProperties {
  param(
    [Parameter(Mandatory)]
    [object]$Value,
    [Parameter(Mandatory)]
    [string[]]$Expected,
    [Parameter(Mandatory)]
    [string]$Description
  )

  if ($Value -is [System.Collections.IDictionary]) {
    $actual = @($Value.Keys)
  } elseif ($Value -is [pscustomobject]) {
    $actual = @($Value.PSObject.Properties | ForEach-Object { $_.Name })
  } else {
    Stop-GrimmoreRelease "$Description must be a JSON object"
  }
  if ($actual.Count -ne $Expected.Count) {
    Stop-GrimmoreRelease "$Description has missing or unknown fields"
  }
  foreach ($expectedName in $Expected) {
    $found = $false
    foreach ($actualName in $actual) {
      if ($actualName -ceq $expectedName) {
        $found = $true
        break
      }
    }
    if (-not $found) {
      Stop-GrimmoreRelease "$Description has missing or unknown fields"
    }
  }
}

function Get-GrimmoreJsonValue {
  param(
    [Parameter(Mandatory)]
    [object]$Value,
    [Parameter(Mandatory)]
    [string]$Name,
    [Parameter(Mandatory)]
    [string]$Description
  )

  if ($Value -is [System.Collections.IDictionary]) {
    foreach ($key in $Value.Keys) {
      if ($key -ceq $Name) {
        return $Value[$key]
      }
    }
  } elseif ($Value -is [pscustomobject]) {
    foreach ($property in $Value.PSObject.Properties) {
      if ($property.Name -ceq $Name) {
        return $property.Value
      }
    }
  }
  Stop-GrimmoreRelease "$Description is missing $Name"
}

function Test-GrimmoreIntegerInRange {
  param(
    [Parameter(Mandatory)]
    [object]$Value,
    [Parameter(Mandatory)]
    [long]$Minimum,
    [Parameter(Mandatory)]
    [long]$Maximum
  )

  if (-not ($Value -is [byte] -or $Value -is [int16] -or $Value -is [int] -or $Value -is [long])) {
    return $false
  }
  $integer = [long]$Value
  return $integer -ge $Minimum -and $integer -le $Maximum
}

function Test-GrimmoreExactStringMember {
  param(
    [Parameter(Mandatory)]
    [AllowEmptyCollection()]
    [string[]]$Values,
    [Parameter(Mandatory)]
    [string]$Value
  )

  foreach ($candidate in $Values) {
    if ($candidate -ceq $Value) {
      return $true
    }
  }
  return $false
}

function Read-GrimmoreReleaseManifest {
  param(
    [Parameter(Mandatory)]
    [string]$Json,
    [Parameter(Mandatory)]
    [string]$ExpectedTarget
  )

  $manifest = ConvertFrom-GrimmoreJson -Json $Json -Description "release manifest"
  Assert-GrimmoreExactProperties -Value $manifest -Description "release manifest" -Expected @(
    "`$schema", "schemaVersion", "channel", "version", "target", "createdAt", "artifact", "protocol"
  )
  $schema = Get-GrimmoreJsonValue -Value $manifest -Name "`$schema" -Description "release manifest"
  $schemaVersion = Get-GrimmoreJsonValue -Value $manifest -Name "schemaVersion" -Description "release manifest"
  $channel = Get-GrimmoreJsonValue -Value $manifest -Name "channel" -Description "release manifest"
  $version = Get-GrimmoreJsonValue -Value $manifest -Name "version" -Description "release manifest"
  $target = Get-GrimmoreJsonValue -Value $manifest -Name "target" -Description "release manifest"
  $createdAt = Get-GrimmoreJsonValue -Value $manifest -Name "createdAt" -Description "release manifest"
  $artifact = Get-GrimmoreJsonValue -Value $manifest -Name "artifact" -Description "release manifest"
  $protocol = Get-GrimmoreJsonValue -Value $manifest -Name "protocol" -Description "release manifest"
  if ($schema -isnot [string] -or $schema -cne $script:GrimmoreReleaseSchema) {
    Stop-GrimmoreRelease "release manifest schema is unsupported"
  }
  if (-not (Test-GrimmoreIntegerInRange -Value $schemaVersion -Minimum 1 -Maximum 1)) {
    Stop-GrimmoreRelease "release manifest schema version is unsupported"
  }
  if ($channel -isnot [string] -or $channel -cne "test") {
    Stop-GrimmoreRelease "release manifest is not signed for the Phase-1 test channel"
  }
  if ($target -isnot [string] -or $target -cne $ExpectedTarget) {
    Stop-GrimmoreRelease "release manifest target does not match this host"
  }
  if ($version -isnot [string] -or $version -cnotmatch "^[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z.-]+)?$") {
    Stop-GrimmoreRelease "release manifest version is invalid"
  }
  if ($createdAt -isnot [string] -or $createdAt -cnotmatch "^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d{3})?Z$") {
    Stop-GrimmoreRelease "release manifest timestamp is invalid"
  }
  try {
    $timestampFormat = if ($createdAt.Contains(".")) {
      "yyyy-MM-dd'T'HH:mm:ss.fff'Z'"
    } else {
      "yyyy-MM-dd'T'HH:mm:ss'Z'"
    }
    [void][DateTimeOffset]::ParseExact(
      $createdAt,
      $timestampFormat,
      [Globalization.CultureInfo]::InvariantCulture,
      [Globalization.DateTimeStyles]::AssumeUniversal
    )
  } catch {
    Stop-GrimmoreRelease "release manifest timestamp is invalid"
  }

  Assert-GrimmoreExactProperties -Value $artifact -Description "release manifest artifact" -Expected @(
    "file", "sha256", "size"
  )
  $artifactFile = Get-GrimmoreJsonValue -Value $artifact -Name "file" -Description "release manifest artifact"
  $artifactHash = Get-GrimmoreJsonValue -Value $artifact -Name "sha256" -Description "release manifest artifact"
  $artifactSize = Get-GrimmoreJsonValue -Value $artifact -Name "size" -Description "release manifest artifact"
  if ($artifactFile -isnot [string] -or $artifactFile -cnotmatch "^[A-Za-z0-9][A-Za-z0-9._-]*\.zip$") {
    Stop-GrimmoreRelease "Windows release artifact must have a portable .zip name"
  }
  if ($artifactHash -isnot [string] -or $artifactHash -cnotmatch "^[a-f0-9]{64}$") {
    Stop-GrimmoreRelease "release manifest artifact hash is invalid"
  }
  if (-not (Test-GrimmoreIntegerInRange -Value $artifactSize -Minimum 1 -Maximum ([long]::MaxValue))) {
    Stop-GrimmoreRelease "release manifest artifact size is invalid"
  }

  Assert-GrimmoreExactProperties -Value $protocol -Description "release manifest protocol" -Expected @(
    "minimum", "maximum"
  )
  $minimumProtocol = Get-GrimmoreJsonValue -Value $protocol -Name "minimum" -Description "release manifest protocol"
  $maximumProtocol = Get-GrimmoreJsonValue -Value $protocol -Name "maximum" -Description "release manifest protocol"
  if (-not (Test-GrimmoreIntegerInRange -Value $minimumProtocol -Minimum 1 -Maximum 65535) -or
      -not (Test-GrimmoreIntegerInRange -Value $maximumProtocol -Minimum 1 -Maximum 65535) -or
      [long]$minimumProtocol -gt [long]$maximumProtocol) {
    Stop-GrimmoreRelease "release manifest protocol range is invalid"
  }
  return [pscustomobject]@{
    Version = $version
    Target = $target
    ArtifactFile = $artifactFile
    ArtifactHash = $artifactHash
    ArtifactSize = [long]$artifactSize
    ProtocolMinimum = [long]$minimumProtocol
    ProtocolMaximum = [long]$maximumProtocol
  }
}

function Get-GrimmoreWindowsTarget {
  $operatingSystem = Get-CimInstance -ClassName Win32_OperatingSystem -ErrorAction Stop
  $version = [Version]$operatingSystem.Version
  $build = [int]$operatingSystem.BuildNumber
  if ($version.Major -ne 10 -or $build -lt 22000) {
    Stop-GrimmoreRelease "this Phase-1 installer supports Windows 11 or newer only"
  }
  $computer = Get-CimInstance -ClassName Win32_ComputerSystem -ErrorAction Stop
  if ($computer.SystemType -match "ARM64") {
    return "windows-arm64"
  }
  if ($computer.SystemType -match "x64|x86-64|64-bit") {
    return "windows-x64"
  }
  Stop-GrimmoreRelease "unsupported Windows processor architecture: $($computer.SystemType)"
}

function Assert-GrimmoreZipPayload {
  param(
    [Parameter(Mandatory)]
    [string]$Archive,
    [Parameter(Mandatory)]
    [string]$PayloadRoot
  )

  Add-Type -AssemblyName System.IO.Compression.FileSystem -ErrorAction Stop
  $expectedEntries = @(
    "$PayloadRoot/grimmored.exe",
    "$PayloadRoot/grimmore-launcher.exe"
  )
  $archiveHandle = [IO.Compression.ZipFile]::OpenRead($Archive)
  try {
    $seenEntries = @()
    $sawRootDirectory = $false
    [long]$totalBytes = 0
    foreach ($entry in $archiveHandle.Entries) {
      $name = $entry.FullName
      if ([string]::IsNullOrEmpty($name) -or $name.Contains("\\") -or $name.StartsWith("/") -or
          $name -match "^[A-Za-z]:" -or $name.Contains("//")) {
        Stop-GrimmoreRelease "release archive contains an unsafe path"
      }
      foreach ($segment in $name.Split("/")) {
        if ($segment -eq ".." -or $segment -eq ".") {
          Stop-GrimmoreRelease "release archive contains an unsafe path"
        }
      }
      if ($name -ceq "$PayloadRoot/") {
        if ($sawRootDirectory -or $entry.Length -ne 0) {
          Stop-GrimmoreRelease "release archive has a duplicate or invalid payload directory"
        }
        $sawRootDirectory = $true
        continue
      }
      if (-not (Test-GrimmoreExactStringMember -Values $expectedEntries -Value $name)) {
        Stop-GrimmoreRelease "release archive has an unexpected member"
      }
      if (Test-GrimmoreExactStringMember -Values $seenEntries -Value $name) {
        Stop-GrimmoreRelease "release archive has a duplicate payload member"
      }
      $unixFileType = ([int64]$entry.ExternalAttributes -shr 16) -band 0xF000
      if ($unixFileType -eq 0xA000 -or $entry.Length -lt 1) {
        Stop-GrimmoreRelease "release archive contains an unsupported payload member"
      }
      $totalBytes += [long]$entry.Length
      if ($totalBytes -gt $script:GrimmoreMaximumZipBytes) {
        Stop-GrimmoreRelease "release archive expands beyond the permitted size"
      }
      $seenEntries += $name
    }
    if ($seenEntries.Count -ne $expectedEntries.Count) {
      Stop-GrimmoreRelease "release archive is missing a required payload member"
    }
  } finally {
    $archiveHandle.Dispose()
  }
}

function Expand-GrimmoreZipPayload {
  param(
    [Parameter(Mandatory)]
    [string]$Archive,
    [Parameter(Mandatory)]
    [string]$PayloadRoot,
    [Parameter(Mandatory)]
    [string]$Destination
  )

  Assert-GrimmoreZipPayload -Archive $Archive -PayloadRoot $PayloadRoot
  $archiveHandle = [IO.Compression.ZipFile]::OpenRead($Archive)
  try {
    foreach ($entry in $archiveHandle.Entries) {
      if ($entry.FullName -ceq "$PayloadRoot/grimmored.exe") {
        $outputPath = Join-Path $Destination "grimmored.exe"
      } elseif ($entry.FullName -ceq "$PayloadRoot/grimmore-launcher.exe") {
        $outputPath = Join-Path $Destination "grimmore-launcher.exe"
      } else {
        continue
      }
      $input = $null
      $output = $null
      try {
        $input = $entry.Open()
        $output = [IO.File]::Open(
          $outputPath,
          [IO.FileMode]::CreateNew,
          [IO.FileAccess]::Write,
          [IO.FileShare]::None
        )
        $buffer = New-Object byte[] 81920
        [long]$written = 0
        while (($read = $input.Read($buffer, 0, $buffer.Length)) -gt 0) {
          $written += $read
          if ($written -gt [long]$entry.Length -or $written -gt $script:GrimmoreMaximumZipBytes) {
            Stop-GrimmoreRelease "release archive expands beyond the permitted size"
          }
          $output.Write($buffer, 0, $read)
        }
        if ($written -ne [long]$entry.Length) {
          Stop-GrimmoreRelease "release archive payload length does not match its metadata"
        }
        $output.Flush($true)
      } finally {
        if ($null -ne $output) {
          $output.Dispose()
        }
        if ($null -ne $input) {
          $input.Dispose()
        }
      }
    }
  } finally {
    $archiveHandle.Dispose()
  }
}

function Assert-GrimmoreDoctorReport {
  param(
    [Parameter(Mandatory)]
    [string]$Daemon,
    [Parameter(Mandatory)]
    [string]$StandardErrorPath,
    [Parameter(Mandatory)]
    [long]$ProtocolMinimum,
    [Parameter(Mandatory)]
    [long]$ProtocolMaximum
  )

  [void](Assert-GrimmoreRegularFile -Path $Daemon -Description "staged companion")
  $output = @(& $Daemon doctor 2> $StandardErrorPath)
  $exitCode = $LASTEXITCODE
  if ($exitCode -ne 0) {
    Stop-GrimmoreRelease "staged companion failed its health check"
  }
  $report = ConvertFrom-GrimmoreJson `
    -Json ([string]::Join([Environment]::NewLine, [string[]]$output)) `
    -Description "staged companion doctor report"
  try {
    $ftsAvailable = Get-GrimmoreJsonValue -Value $report -Name "fts5Available" -Description "staged companion doctor report"
    $credentialStoreAvailable = Get-GrimmoreJsonValue -Value $report -Name "credentialStoreAvailable" -Description "staged companion doctor report"
    $protocolVersion = Get-GrimmoreJsonValue -Value $report -Name "protocolVersion" -Description "staged companion doctor report"
  } catch {
    Stop-GrimmoreRelease "staged companion did not emit valid doctor JSON"
  }
  if ($ftsAvailable -isnot [bool] -or $ftsAvailable -ne $true -or
      $credentialStoreAvailable -isnot [bool] -or $credentialStoreAvailable -ne $true -or
      -not (Test-GrimmoreIntegerInRange `
        -Value $protocolVersion `
        -Minimum $ProtocolMinimum `
        -Maximum $ProtocolMaximum)) {
    Stop-GrimmoreRelease "staged companion failed its SQLite, credential-store, or protocol health check"
  }
}

function Write-GrimmorePointerState {
  param(
    [Parameter(Mandatory)]
    [string]$Path,
    [Parameter(Mandatory)]
    [string]$Target,
    [Parameter(Mandatory)]
    [string]$Version,
    [Parameter(Mandatory)]
    [string]$LauncherHash
  )

  if ($LauncherHash -cnotmatch "^[a-f0-9]{64}$") {
    Stop-GrimmoreRelease "installation state launcher hash is invalid"
  }
  $contents = '{{"schemaVersion":1,"target":"{0}","version":"{1}","launcherSha256":"{2}"}}' -f `
    $Target, $Version, $LauncherHash
  $bytes = (New-Object Text.UTF8Encoding($false)).GetBytes($contents)
  $stream = $null
  try {
    $stream = [IO.File]::Open(
      $Path,
      [IO.FileMode]::CreateNew,
      [IO.FileAccess]::Write,
      [IO.FileShare]::None
    )
    $stream.Write($bytes, 0, $bytes.Length)
    $stream.Flush($true)
  } finally {
    if ($null -ne $stream) {
      $stream.Dispose()
    }
  }
}

function Read-GrimmorePointerState {
  param(
    [Parameter(Mandatory)]
    [string]$Path,
    [Parameter(Mandatory)]
    [string]$ExpectedTarget
  )

  $item = Assert-GrimmoreRegularFile -Path $Path -Description "installation state"
  if ($item.Length -lt 1 -or $item.Length -gt 1024) {
    Stop-GrimmoreRelease "installation state has an invalid size"
  }
  $json = ConvertFrom-GrimmoreUtf8 -Bytes ([IO.File]::ReadAllBytes($Path)) -Description "installation state"
  $state = ConvertFrom-GrimmoreJson -Json $json -Description "installation state"
  Assert-GrimmoreExactProperties -Value $state -Description "installation state" -Expected @(
    "schemaVersion", "target", "version", "launcherSha256"
  )
  $schemaVersion = Get-GrimmoreJsonValue -Value $state -Name "schemaVersion" -Description "installation state"
  $target = Get-GrimmoreJsonValue -Value $state -Name "target" -Description "installation state"
  $version = Get-GrimmoreJsonValue -Value $state -Name "version" -Description "installation state"
  $launcherHash = Get-GrimmoreJsonValue -Value $state -Name "launcherSha256" -Description "installation state"
  if (-not (Test-GrimmoreIntegerInRange -Value $schemaVersion -Minimum 1 -Maximum 1) -or
      $target -isnot [string] -or $target -cne $ExpectedTarget -or
      $version -isnot [string] -or $version -cnotmatch "^[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z.-]+)?$" -or
      $launcherHash -isnot [string] -or $launcherHash -cnotmatch "^[a-f0-9]{64}$") {
    Stop-GrimmoreRelease "installation state is invalid"
  }
  return [pscustomobject]@{
    Target = $target
    Version = $version
    LauncherHash = $launcherHash
  }
}

function Switch-GrimmorePointerState {
  param(
    [Parameter(Mandatory)]
    [string]$Root,
    [Parameter(Mandatory)]
    [string]$Target,
    [Parameter(Mandatory)]
    [string]$Version,
    [Parameter(Mandatory)]
    [string]$LauncherHash
  )

  $current = Join-Path $Root "current.json"
  $previous = Join-Path $Root "previous.json"
  $temporary = Join-Path $Root (".current-{0}.tmp" -f [Guid]::NewGuid().ToString("N"))
  try {
    if (Test-Path -LiteralPath $current) {
      [void](Assert-GrimmoreRegularFile -Path $current -Description "current installation state")
    }
    if (Test-Path -LiteralPath $previous) {
      [void](Assert-GrimmoreRegularFile -Path $previous -Description "previous installation state")
    }
    Write-GrimmorePointerState `
      -Path $temporary `
      -Target $Target `
      -Version $Version `
      -LauncherHash $LauncherHash
    if (Test-Path -LiteralPath $current) {
      [IO.File]::Replace($temporary, $current, $previous, $true)
    } else {
      if (Test-Path -LiteralPath $previous) {
        Stop-GrimmoreRelease "previous installation state exists without a current state"
      }
      [IO.File]::Move($temporary, $current)
    }
  } finally {
    if (Test-Path -LiteralPath $temporary) {
      Remove-Item -LiteralPath $temporary -Force
    }
  }
}

function Compare-GrimmoreFiles {
  param(
    [Parameter(Mandatory)]
    [string]$First,
    [Parameter(Mandatory)]
    [string]$Second
  )

  $firstItem = Assert-GrimmoreRegularFile -Path $First -Description "stored release evidence"
  $secondItem = Assert-GrimmoreRegularFile -Path $Second -Description "release transaction evidence"
  return $firstItem.Length -eq $secondItem.Length -and
    (Get-GrimmoreSha256 -Path $First) -ceq (Get-GrimmoreSha256 -Path $Second)
}

function Assert-GrimmoreReadyVersion {
  param(
    [Parameter(Mandatory)]
    [string]$VersionDirectory,
    [Parameter(Mandatory)]
    [string]$ExpectedTarget,
    [Parameter(Mandatory)]
    [string]$ExpectedVersion,
    [Parameter(Mandatory)]
    [string]$Thumbprint,
    [string]$TransactionManifest,
    [string]$TransactionEnvelope,
    [Parameter(Mandatory)]
    [string]$StandardErrorPath
  )

  if (-not (Test-Path -LiteralPath $VersionDirectory)) {
    Stop-GrimmoreRelease "installed version directory is missing: $VersionDirectory"
  }
  $versionItem = Get-Item -LiteralPath $VersionDirectory -Force
  if (-not $versionItem.PSIsContainer -or (Test-GrimmoreReparsePoint -Path $VersionDirectory)) {
    Stop-GrimmoreRelease "installed version directory is unsafe: $VersionDirectory"
  }
  [void](Assert-GrimmoreRegularFile -Path (Join-Path $VersionDirectory ".ready") -Description "installed readiness marker")
  $storedManifest = Join-Path $VersionDirectory "release-manifest.json"
  $storedEnvelope = Join-Path $VersionDirectory "release-envelope.ps1"
  [void](Assert-GrimmoreRegularFile -Path $storedManifest -Description "stored release manifest")
  $envelope = Read-GrimmoreReleaseEnvelope -Path $storedEnvelope -Thumbprint $Thumbprint
  $manifest = Read-GrimmoreReleaseManifest -Json $envelope.ManifestJson -ExpectedTarget $ExpectedTarget
  if ($manifest.Version -cne $ExpectedVersion) {
    Stop-GrimmoreRelease "installed version does not match its signed release manifest"
  }
  $storedManifestBytes = [IO.File]::ReadAllBytes($storedManifest)
  if ((Get-GrimmoreSha256Bytes -Bytes $storedManifestBytes) -cne $envelope.ManifestHash) {
    Stop-GrimmoreRelease "stored release manifest does not match its signed envelope"
  }
  if (-not [string]::IsNullOrEmpty($TransactionManifest) -and
      -not (Compare-GrimmoreFiles -First $storedManifest -Second $TransactionManifest)) {
    Stop-GrimmoreRelease "existing ready version has different signed release evidence"
  }
  if (-not [string]::IsNullOrEmpty($TransactionEnvelope) -and
      -not (Compare-GrimmoreFiles -First $storedEnvelope -Second $TransactionEnvelope)) {
    Stop-GrimmoreRelease "existing ready version has different signed release evidence"
  }
  $daemon = Join-Path $VersionDirectory "grimmored.exe"
  $launcher = Join-Path $VersionDirectory "grimmore-launcher.exe"
  Assert-GrimmoreAuthenticodeSignature -Path $daemon -Thumbprint $Thumbprint -Description "installed companion"
  Assert-GrimmoreAuthenticodeSignature -Path $launcher -Thumbprint $Thumbprint -Description "installed versioned launcher"
  Assert-GrimmoreDoctorReport `
    -Daemon $daemon `
    -StandardErrorPath $StandardErrorPath `
    -ProtocolMinimum $manifest.ProtocolMinimum `
    -ProtocolMaximum $manifest.ProtocolMaximum
  return $manifest
}

function Write-GrimmoreReadyMarker {
  param(
    [Parameter(Mandatory)]
    [string]$VersionDirectory
  )

  $marker = Join-Path $VersionDirectory ".ready"
  $stream = $null
  try {
    $stream = [IO.File]::Open(
      $marker,
      [IO.FileMode]::CreateNew,
      [IO.FileAccess]::Write,
      [IO.FileShare]::None
    )
    $stream.Flush($true)
  } finally {
    if ($null -ne $stream) {
      $stream.Dispose()
    }
  }
}

function Read-GrimmoreStableLauncherMarker {
  param(
    [Parameter(Mandatory)]
    [string]$Marker
  )

  $markerItem = Assert-GrimmoreRegularFile -Path $Marker -Description "stable launcher ownership marker"
  if ($markerItem.Length -ne 64) {
    Stop-GrimmoreRelease "stable launcher ownership marker is invalid"
  }
  $contents = ConvertFrom-GrimmoreUtf8 `
    -Bytes ([IO.File]::ReadAllBytes($Marker)) `
    -Description "stable launcher ownership marker"
  if ($contents -cnotmatch "^[a-f0-9]{64}$") {
    Stop-GrimmoreRelease "stable launcher ownership marker is invalid"
  }
  return $contents
}

function Write-GrimmoreStableLauncherMarker {
  param(
    [Parameter(Mandatory)]
    [string]$Marker,
    [Parameter(Mandatory)]
    [string]$Hash
  )

  $bytes = (New-Object Text.UTF8Encoding($false)).GetBytes($Hash)
  $stream = $null
  try {
    $stream = [IO.File]::Open(
      $Marker,
      [IO.FileMode]::CreateNew,
      [IO.FileAccess]::Write,
      [IO.FileShare]::None
    )
    $stream.Write($bytes, 0, $bytes.Length)
    $stream.Flush($true)
  } finally {
    if ($null -ne $stream) {
      $stream.Dispose()
    }
  }
}

function Ensure-GrimmoreStableLauncher {
  param(
    [Parameter(Mandatory)]
    [string]$Bin,
    [Parameter(Mandatory)]
    [string]$VersionDirectory,
    [Parameter(Mandatory)]
    [string]$Thumbprint
  )

  $stableLauncher = Join-Path $Bin "grimmore-launcher.exe"
  $marker = Join-Path $Bin "grimmore-launcher.bootstrap.sha256"
  $launcherExists = Test-Path -LiteralPath $stableLauncher
  $markerExists = Test-Path -LiteralPath $marker
  $source = Join-Path $VersionDirectory "grimmore-launcher.exe"
  Assert-GrimmoreAuthenticodeSignature -Path $source -Thumbprint $Thumbprint -Description "verified versioned launcher"
  $sourceHash = Get-GrimmoreSha256 -Path $source
  if ($launcherExists) {
    [void](Assert-GrimmoreRegularFile -Path $stableLauncher -Description "stable launcher")
    Assert-GrimmoreAuthenticodeSignature -Path $stableLauncher -Thumbprint $Thumbprint -Description "stable launcher"
    $stableHash = Get-GrimmoreSha256 -Path $stableLauncher
    if ($markerExists) {
      $markerContents = Read-GrimmoreStableLauncherMarker -Marker $marker
      if ($stableHash -cne $markerContents) {
        Stop-GrimmoreRelease "stable launcher has been altered"
      }
      return
    }
    if ($stableHash -cne $sourceHash) {
      Stop-GrimmoreRelease "incomplete stable launcher does not match the verified source"
    }
    Write-GrimmoreStableLauncherMarker -Marker $marker -Hash $stableHash
    return
  }

  if ($markerExists) {
    $markerContents = Read-GrimmoreStableLauncherMarker -Marker $marker
    if ($markerContents -cne $sourceHash) {
      Stop-GrimmoreRelease "incomplete stable launcher marker does not match the verified source"
    }
  }

  $temporary = Join-Path $Bin (".grimmore-launcher-{0}.tmp" -f [Guid]::NewGuid().ToString("N"))
  $createdMarker = $false
  try {
    if (-not $markerExists) {
      Write-GrimmoreStableLauncherMarker -Marker $marker -Hash $sourceHash
      $createdMarker = $true
    }
    Copy-GrimmoreInputFile -Source $source -Destination $temporary -Description "verified versioned launcher"
    Assert-GrimmoreAuthenticodeSignature -Path $temporary -Thumbprint $Thumbprint -Description "stable launcher candidate"
    if ((Get-GrimmoreSha256 -Path $temporary) -cne $sourceHash) {
      Stop-GrimmoreRelease "stable launcher has been altered"
    }
    [IO.File]::Move($temporary, $stableLauncher)
  } catch {
    if ($createdMarker -and -not (Test-Path -LiteralPath $stableLauncher) -and
        (Test-Path -LiteralPath $marker)) {
      Remove-Item -LiteralPath $marker -Force
    }
    throw
  } finally {
    if (Test-Path -LiteralPath $temporary) {
      Remove-Item -LiteralPath $temporary -Force
    }
  }
}

function Add-GrimmoreBinToUserPath {
  param(
    [Parameter(Mandatory)]
    [string]$Bin
  )

  $existing = [Environment]::GetEnvironmentVariable("Path", [EnvironmentVariableTarget]::User)
  $entries = @()
  if (-not [string]::IsNullOrEmpty($existing)) {
    $entries = @($existing -split ";" | Where-Object { $_.Length -gt 0 })
  }
  foreach ($entry in $entries) {
    if ($entry -ieq $Bin) {
      return $false
    }
  }
  $updated = if ([string]::IsNullOrEmpty($existing)) { $Bin } else { "$existing;$Bin" }
  [Environment]::SetEnvironmentVariable("Path", $updated, [EnvironmentVariableTarget]::User)
  return $true
}

function Remove-GrimmoreStaleStagingDirectories {
  param(
    [Parameter(Mandatory)]
    [string]$Versions,
    [Parameter(Mandatory)]
    [string]$Version
  )

  $prefix = ".staging-$Version-"
  foreach ($item in Get-ChildItem -LiteralPath $Versions -Force) {
    if (-not $item.Name.StartsWith($prefix, [StringComparison]::Ordinal)) {
      continue
    }
    if (-not $item.PSIsContainer -or (Test-GrimmoreReparsePoint -Path $item.FullName)) {
      Stop-GrimmoreRelease "stale staging path is unsafe: $($item.FullName)"
    }
    Remove-Item -LiteralPath $item.FullName -Recurse -Force
  }
}

Export-ModuleMember -Function @(
  "Add-GrimmoreBinToUserPath",
  "Assert-GrimmoreAuthenticodeSignature",
  "Assert-GrimmoreDoctorReport",
  "Assert-GrimmoreReadyVersion",
  "Assert-GrimmoreRegularFile",
  "Assert-GrimmoreTrustedPublisher",
  "Assert-GrimmoreZipPayload",
  "Compare-GrimmoreFiles",
  "ConvertTo-GrimmoreThumbprint",
  "Copy-GrimmoreInputFile",
  "Ensure-GrimmoreStableLauncher",
  "Enter-GrimmoreInstallLock",
  "Expand-GrimmoreZipPayload",
  "Get-GrimmoreDefaultInstallRoot",
  "Get-GrimmoreSha256",
  "Get-GrimmoreWindowsTarget",
  "Initialize-GrimmoreInstallRoot",
  "New-GrimmorePrivateDirectory",
  "Read-GrimmorePointerState",
  "Read-GrimmoreReleaseEnvelope",
  "Read-GrimmoreReleaseManifest",
  "Remove-GrimmorePrivateDirectory",
  "Remove-GrimmoreStaleStagingDirectories",
  "Switch-GrimmorePointerState",
  "Write-GrimmoreReadyMarker"
)
