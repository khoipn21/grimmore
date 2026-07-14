[CmdletBinding()]
param()

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

function Assert-Condition {
  param(
    [Parameter(Mandatory)]
    [bool]$Condition,
    [Parameter(Mandatory)]
    [string]$Message
  )

  if (-not $Condition) {
    throw $Message
  }
}

function Invoke-RequiredProgram {
  param(
    [Parameter(Mandatory)]
    [string]$FilePath,
    [Parameter(Mandatory)]
    [string[]]$Arguments
  )

  & $FilePath @Arguments
  if ($LASTEXITCODE -ne 0) {
    throw "$FilePath failed with exit code $LASTEXITCODE"
  }
}

function Assert-ReleaseFailure {
  param(
    [Parameter(Mandatory)]
    [scriptblock]$Action,
    [Parameter(Mandatory)]
    [string]$Description
  )

  $failed = $false
  try {
    & $Action
  } catch {
    $failed = $true
  }
  Assert-Condition -Condition $failed -Message "$Description unexpectedly succeeded"
}

function New-TestSigningCertificate {
  param(
    [Parameter(Mandatory)]
    [string]$Workspace
  )

  $certificate = New-SelfSignedCertificate `
    -Type CodeSigningCert `
    -Subject "CN=Grimmore Phase One Test $([Guid]::NewGuid().ToString('N'))" `
    -CertStoreLocation "Cert:\CurrentUser\My" `
    -NotAfter (Get-Date).AddDays(1)
  $exportPath = Join-Path $Workspace "test-signer.cer"
  Export-Certificate -Cert $certificate -FilePath $exportPath | Out-Null
  Import-Certificate -FilePath $exportPath -CertStoreLocation "Cert:\CurrentUser\TrustedPublisher" | Out-Null
  Import-Certificate -FilePath $exportPath -CertStoreLocation "Cert:\CurrentUser\Root" | Out-Null
  return $certificate
}

function Remove-TestSigningCertificate {
  param(
    [string]$Thumbprint
  )

  if ([string]::IsNullOrEmpty($Thumbprint)) {
    return
  }
  foreach ($store in @("My", "TrustedPublisher", "Root")) {
    $path = "Cert:\CurrentUser\$store\$Thumbprint"
    if (Test-Path -LiteralPath $path) {
      Remove-Item -LiteralPath $path -Force
    }
  }
}

function Sign-TestFile {
  param(
    [Parameter(Mandatory)]
    [string]$Path,
    [Parameter(Mandatory)]
    [System.Security.Cryptography.X509Certificates.X509Certificate2]$Certificate
  )

  $signature = Set-AuthenticodeSignature -FilePath $Path -Certificate $Certificate
  Assert-Condition `
    -Condition ($signature.Status.ToString() -ceq "Valid") `
    -Message "test signing did not produce a valid signature for $Path"
}

function New-SignedArtifact {
  param(
    [Parameter(Mandatory)]
    [string]$Workspace,
    [Parameter(Mandatory)]
    [string]$Repository,
    [Parameter(Mandatory)]
    [string]$Target,
    [Parameter(Mandatory)]
    [string]$Version,
    [Parameter(Mandatory)]
    [System.Security.Cryptography.X509Certificates.X509Certificate2]$Certificate,
    [int]$ProtocolMinimum = 1,
    [int]$ProtocolMaximum = 1
  )

  Add-Type -AssemblyName System.IO.Compression.FileSystem
  $payloadParent = Join-Path $Workspace "payload-$Version"
  $payloadRoot = "grimmore-$Version-$Target"
  $payload = Join-Path $payloadParent $payloadRoot
  [void][IO.Directory]::CreateDirectory($payload)
  foreach ($binary in @("grimmored.exe", "grimmore-launcher.exe")) {
    $source = Join-Path $Repository "target\release\$binary"
    $destination = Join-Path $payload $binary
    Copy-Item -LiteralPath $source -Destination $destination
    Sign-TestFile -Path $destination -Certificate $Certificate
  }

  $archive = Join-Path $Workspace "grimmore-$Version-$Target.zip"
  [IO.Compression.ZipFile]::CreateFromDirectory(
    $payloadParent,
    $archive,
    [IO.Compression.CompressionLevel]::Optimal,
    $false
  )
  $manifest = Join-Path $Workspace "$Version.manifest.json"
  $manifestBuilder = Join-Path $Repository "release\create-slice-manifest.mjs"
  Invoke-RequiredProgram -FilePath "node" -Arguments @(
    $manifestBuilder,
    "--artifact", $archive,
    "--channel", "test",
    "--created-at", "2026-07-13T00:00:00Z",
    "--out", $manifest,
    "--target", $Target,
    "--version", $Version,
    "--protocol-min", $ProtocolMinimum.ToString(),
    "--protocol-max", $ProtocolMaximum.ToString()
  )
  $envelope = Join-Path $Workspace "$Version.release-envelope.ps1"
  $envelopeBuilder = Join-Path $Repository "release\create-windows-release-envelope.ps1"
  & $envelopeBuilder -Manifest $manifest -Out $envelope
  Sign-TestFile -Path $envelope -Certificate $Certificate
  return [pscustomobject]@{
    Archive = $archive
    Envelope = $envelope
    Manifest = $manifest
    Version = $Version
  }
}

function Read-InstallationState {
  param(
    [Parameter(Mandatory)]
    [string]$Root,
    [Parameter(Mandatory)]
    [string]$Name
  )

  return (Get-Content -LiteralPath (Join-Path $Root "$Name.json") -Raw | ConvertFrom-Json)
}

function New-ConsoleControlHelper {
  param(
    [Parameter(Mandatory)]
    [string]$Workspace
  )

  $output = Join-Path $Workspace "grimmore-console-control.exe"
  $source = @'
using System;
using System.ComponentModel;
using System.IO;
using System.Runtime.InteropServices;
using System.Text;

public static class GrimmoreConsoleControl
{
    private const uint CREATE_NEW_CONSOLE = 0x00000010;
    private const uint STARTF_USESHOWWINDOW = 0x00000001;
    private const ushort SW_HIDE = 0;
    private const uint CTRL_BREAK_EVENT = 1;

    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    private struct STARTUPINFO
    {
        public uint cb;
        public string lpReserved;
        public string lpDesktop;
        public string lpTitle;
        public uint dwX;
        public uint dwY;
        public uint dwXSize;
        public uint dwYSize;
        public uint dwXCountChars;
        public uint dwYCountChars;
        public uint dwFillAttribute;
        public uint dwFlags;
        public ushort wShowWindow;
        public ushort cbReserved2;
        public IntPtr lpReserved2;
        public IntPtr hStdInput;
        public IntPtr hStdOutput;
        public IntPtr hStdError;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct PROCESS_INFORMATION
    {
        public IntPtr hProcess;
        public IntPtr hThread;
        public uint dwProcessId;
        public uint dwThreadId;
    }

    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    private static extern bool CreateProcess(
        string applicationName,
        StringBuilder commandLine,
        IntPtr processAttributes,
        IntPtr threadAttributes,
        bool inheritHandles,
        uint creationFlags,
        IntPtr environment,
        string currentDirectory,
        ref STARTUPINFO startupInfo,
        out PROCESS_INFORMATION processInformation);

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern bool CloseHandle(IntPtr handle);

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern bool FreeConsole();

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern bool AttachConsole(uint processId);

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern bool SetConsoleCtrlHandler(ConsoleCtrlHandler handlerRoutine, bool add);

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern bool GenerateConsoleCtrlEvent(uint controlType, uint processGroupId);

    private delegate bool ConsoleCtrlHandler(uint controlType);

    private static bool ConsumeControlEvent(uint controlType)
    {
        return true;
    }

    private static string Quote(string value)
    {
        if (value.Length == 0)
        {
            return "\"\"";
        }
        if (value.IndexOfAny(new[] { ' ', '\t', '\n', '\v', '"' }) < 0)
        {
            return value;
        }
        var quoted = new StringBuilder();
        quoted.Append('"');
        var backslashes = 0;
        foreach (var character in value)
        {
            if (character == '\\')
            {
                backslashes++;
                continue;
            }
            if (character == '"')
            {
                quoted.Append('\\', backslashes * 2 + 1);
                quoted.Append(character);
                backslashes = 0;
                continue;
            }
            quoted.Append('\\', backslashes);
            quoted.Append(character);
            backslashes = 0;
        }
        quoted.Append('\\', backslashes * 2);
        quoted.Append('"');
        return quoted.ToString();
    }

    private static uint Start(string executable, string[] arguments)
    {
        var commandLine = new StringBuilder(Quote(executable));
        foreach (var argument in arguments)
        {
            commandLine.Append(' ');
            commandLine.Append(Quote(argument));
        }
        var startup = new STARTUPINFO();
        startup.cb = (uint)Marshal.SizeOf(typeof(STARTUPINFO));
        startup.dwFlags = STARTF_USESHOWWINDOW;
        startup.wShowWindow = SW_HIDE;
        PROCESS_INFORMATION process;
        if (!CreateProcess(
            executable,
            commandLine,
            IntPtr.Zero,
            IntPtr.Zero,
            false,
            CREATE_NEW_CONSOLE,
            IntPtr.Zero,
            null,
            ref startup,
            out process))
        {
            throw new Win32Exception(Marshal.GetLastWin32Error(), "CreateProcessW failed");
        }
        try
        {
            return process.dwProcessId;
        }
        finally
        {
            CloseHandle(process.hThread);
            CloseHandle(process.hProcess);
        }
    }

    private static void SendCtrlBreak(uint processId)
    {
        FreeConsole();
        if (!AttachConsole(processId))
        {
            throw new Win32Exception(Marshal.GetLastWin32Error(), "AttachConsole failed");
        }
        var handler = new ConsoleCtrlHandler(ConsumeControlEvent);
        var handlerAdded = false;
        try
        {
            if (!SetConsoleCtrlHandler(handler, true))
            {
                throw new Win32Exception(Marshal.GetLastWin32Error(), "SetConsoleCtrlHandler failed");
            }
            handlerAdded = true;
            // CREATE_NEW_PROCESS_GROUP is ignored with CREATE_NEW_CONSOLE.
            // The daemon owns this isolated console, so group zero reaches only
            // the daemon and this temporary sender; the sender consumes every
            // control event with its temporary handler above.
            if (!GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, 0))
            {
                throw new Win32Exception(Marshal.GetLastWin32Error(), "GenerateConsoleCtrlEvent failed");
            }
        }
        finally
        {
            if (handlerAdded)
            {
                SetConsoleCtrlHandler(handler, false);
            }
            FreeConsole();
        }
    }

    public static int Main(string[] arguments)
    {
        try
        {
            if (arguments.Length >= 4 && arguments[0] == "start")
            {
                var childArguments = new string[arguments.Length - 3];
                Array.Copy(arguments, 3, childArguments, 0, childArguments.Length);
                File.WriteAllText(arguments[1], Start(arguments[2], childArguments).ToString());
                return 0;
            }
            if (arguments.Length == 2 && arguments[0] == "signal")
            {
                uint processId;
                if (!UInt32.TryParse(arguments[1], out processId) || processId == 0)
                {
                    throw new ArgumentException("signal requires a positive process identifier");
                }
                SendCtrlBreak(processId);
                return 0;
            }
            throw new ArgumentException("expected start or signal command");
        }
        catch (Exception error)
        {
            Console.Error.WriteLine(error.Message);
            return 1;
        }
    }
}
'@
  $provider = New-Object Microsoft.CSharp.CSharpCodeProvider
  $parameters = New-Object System.CodeDom.Compiler.CompilerParameters
  $parameters.GenerateExecutable = $true
  $parameters.GenerateInMemory = $false
  $parameters.OutputAssembly = $output
  $parameters.CompilerOptions = "/target:exe /optimize+"
  $parameters.ReferencedAssemblies.Add("System.dll") | Out-Null
  $result = $provider.CompileAssemblyFromSource($parameters, $source)
  if ($result.Errors.HasErrors) {
    $errors = @($result.Errors | ForEach-Object { $_.ToString() }) -join [Environment]::NewLine
    throw "compile console-control helper: $errors"
  }
  return $output
}

function Start-InstalledDaemon {
  param(
    [Parameter(Mandatory)]
    [string]$Daemon,
    [Parameter(Mandatory)]
    [string]$Database,
    [Parameter(Mandatory)]
    [string]$Vault,
    [Parameter(Mandatory)]
    [string]$Endpoint,
    [Parameter(Mandatory)]
    [string]$ConsoleControl,
    [Parameter(Mandatory)]
    [string]$Workspace
  )

  $pidFile = Join-Path $Workspace ("daemon-" + [Guid]::NewGuid().ToString("N") + ".pid")
  & $ConsoleControl start $pidFile $Daemon `
    "--database" $Database "serve" "--vault-id" "reference" "--vault" $Vault `
    "--grant-id" "local" "--scope-id" "vault" "--endpoint" $Endpoint
  if ($LASTEXITCODE -ne 0) {
    throw "console-control helper failed to start installed companion"
  }
  try {
    $processIdText = (Get-Content -LiteralPath $pidFile -Raw).Trim()
    Assert-Condition -Condition ($processIdText -match "^[1-9][0-9]*$") -Message "console-control helper returned an invalid process id"
    return [System.Diagnostics.Process]::GetProcessById([int]$processIdText)
  } finally {
    Remove-Item -LiteralPath $pidFile -Force -ErrorAction SilentlyContinue
  }
}

function Stop-InstalledDaemonCleanly {
  param(
    [Parameter(Mandatory)]
    [System.Diagnostics.Process]$Process,
    [Parameter(Mandatory)]
    [string]$ConsoleControl
  )

  & $ConsoleControl signal $Process.Id
  if ($LASTEXITCODE -ne 0) {
    throw "console-control helper failed to deliver Ctrl-Break"
  }
  if (-not $Process.WaitForExit(5000)) {
    Stop-Process -Id $Process.Id -Force -ErrorAction SilentlyContinue
    $Process.WaitForExit(5000) | Out-Null
    throw "installed companion did not stop after Ctrl-Break"
  }
  Assert-Condition -Condition ($Process.ExitCode -eq 0) -Message "installed companion did not exit cleanly after Ctrl-Break"
}

function Stop-InstalledDaemonForcefully {
  param(
    [Parameter(Mandatory)]
    [System.Diagnostics.Process]$Process
  )

  Assert-Condition -Condition (-not $Process.HasExited) -Message "installed companion exited before forced-crash coverage"
  Stop-Process -Id $Process.Id -Force
  Assert-Condition -Condition $Process.WaitForExit(5000) -Message "installed companion did not stop after forced termination"
  Assert-Condition -Condition ($Process.ExitCode -ne 0) -Message "forced companion termination unexpectedly exited cleanly"
}

function Invoke-InstalledLauncherRequest {
  param(
    [Parameter(Mandatory)]
    [string]$Launcher,
    [Parameter(Mandatory)]
    [string]$Endpoint,
    [Parameter(Mandatory)]
    [int]$Id,
    [Parameter(Mandatory)]
    [string]$Method,
    [Parameter(Mandatory)]
    [hashtable]$Params,
    [int]$TimeoutMilliseconds = 5000
  )

  $request = @{
    jsonrpc = "2.0"
    id = $Id
    method = $Method
    params = $Params
    deadlineUnixMs = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds() + $TimeoutMilliseconds
    vaultId = "reference"
    grantId = "local"
    scopeId = "vault"
  } | ConvertTo-Json -Compress
  $payload = [Text.Encoding]::UTF8.GetBytes($request)
  $frame = New-Object byte[] (4 + $payload.Length)
  $length = [uint32]$payload.Length
  $frame[0] = [byte](($length -shr 24) -band 0xff)
  $frame[1] = [byte](($length -shr 16) -band 0xff)
  $frame[2] = [byte](($length -shr 8) -band 0xff)
  $frame[3] = [byte]($length -band 0xff)
  [Array]::Copy($payload, 0, $frame, 4, $payload.Length)

  $startInfo = New-Object System.Diagnostics.ProcessStartInfo
  $startInfo.FileName = $Launcher
  $startInfo.Arguments = "plugin-session --endpoint `"$Endpoint`""
  $startInfo.UseShellExecute = $false
  $startInfo.CreateNoWindow = $true
  $startInfo.RedirectStandardInput = $true
  $startInfo.RedirectStandardOutput = $true
  $startInfo.RedirectStandardError = $true
  $process = New-Object System.Diagnostics.Process
  $process.StartInfo = $startInfo
  [void]$process.Start()
  $output = $null
  try {
    $process.StandardInput.BaseStream.Write($frame, 0, $frame.Length)
    $process.StandardInput.BaseStream.Flush()
    $process.StandardInput.Close()
    $output = New-Object IO.MemoryStream
    $outputTask = $process.StandardOutput.BaseStream.CopyToAsync($output)
    $stderrTask = $process.StandardError.ReadToEndAsync()
    if (-not $process.WaitForExit($TimeoutMilliseconds)) {
      Stop-Process -Id $process.Id -Force -ErrorAction SilentlyContinue
      throw "stable launcher did not return an authenticated $Method response within ${TimeoutMilliseconds}ms"
    }
    if (-not $outputTask.Wait($TimeoutMilliseconds)) {
      throw "stable launcher stdout did not close after process exit"
    }
    if (-not $stderrTask.Wait($TimeoutMilliseconds)) {
      throw "stable launcher stderr did not close after process exit"
    }
    $stderr = $stderrTask.GetAwaiter().GetResult()
    $outputTask.GetAwaiter().GetResult()
    if ($process.ExitCode -ne 0) {
      throw "stable launcher did not return an authenticated health response: $stderr"
    }
    $bytes = $output.ToArray()
    Assert-Condition -Condition ($bytes.Length -ge 5) -Message "stable launcher returned a truncated response"
    $responseLength = ([int]$bytes[0] -shl 24) -bor ([int]$bytes[1] -shl 16) -bor `
      ([int]$bytes[2] -shl 8) -bor [int]$bytes[3]
    Assert-Condition `
      -Condition ($responseLength -gt 0 -and $bytes.Length -eq (4 + $responseLength)) `
      -Message "stable launcher returned an invalid response frame"
    $responseJson = [Text.Encoding]::UTF8.GetString($bytes, 4, $responseLength)
    return ($responseJson | ConvertFrom-Json)
  } finally {
    if ($null -ne $output) {
      $output.Dispose()
    }
    $process.Dispose()
  }
}

function Invoke-InstalledLauncherHealth {
  param(
    [Parameter(Mandatory)]
    [string]$Launcher,
    [Parameter(Mandatory)]
    [string]$Endpoint,
    [int]$TimeoutMilliseconds = 5000
  )

  return Invoke-InstalledLauncherRequest `
    -Launcher $Launcher `
    -Endpoint $Endpoint `
    -Id 1 `
    -Method "system.health" `
    -Params @{} `
    -TimeoutMilliseconds $TimeoutMilliseconds
}

function Wait-InstalledLauncherHealth {
  param(
    [Parameter(Mandatory)]
    [string]$Launcher,
    [Parameter(Mandatory)]
    [string]$Endpoint
  )

  $deadline = [DateTimeOffset]::UtcNow.AddSeconds(5)
  $lastFailure = "stable launcher did not complete an authenticated named-pipe health request"
  while ([DateTimeOffset]::UtcNow -lt $deadline) {
    try {
      $remainingMilliseconds = [int][Math]::Max(
        1,
        [Math]::Ceiling(($deadline - [DateTimeOffset]::UtcNow).TotalMilliseconds)
      )
      $health = Invoke-InstalledLauncherHealth `
        -Launcher $Launcher `
        -Endpoint $Endpoint `
        -TimeoutMilliseconds $remainingMilliseconds
      if ($health.result.status -eq "ok" -and $health.result.role -eq "plugin") {
        return $health
      }
      $lastFailure = "stable launcher returned an unexpected health response"
    } catch {
      $lastFailure = $_.Exception.Message
    }
    Start-Sleep -Milliseconds 50
  }
  throw "$lastFailure within 5000ms"
}

function Wait-InstalledWatcherSearch {
  param(
    [Parameter(Mandatory)]
    [string]$Launcher,
    [Parameter(Mandatory)]
    [string]$Endpoint,
    [Parameter(Mandatory)]
    [string]$Sentinel
  )

  $deadline = [DateTimeOffset]::UtcNow.AddSeconds(5)
  $lastFailure = "native watcher did not reconcile the changed fixture note"
  while ([DateTimeOffset]::UtcNow -lt $deadline) {
    try {
      $remainingMilliseconds = [int][Math]::Max(
        1,
        [Math]::Ceiling(($deadline - [DateTimeOffset]::UtcNow).TotalMilliseconds)
      )
      $response = Invoke-InstalledLauncherRequest `
        -Launcher $Launcher `
        -Endpoint $Endpoint `
        -Id 2 `
        -Method "knowledge.search" `
        -Params @{ query = $Sentinel; limit = 5 } `
        -TimeoutMilliseconds $remainingMilliseconds
      Assert-Condition `
        -Condition ($null -eq $response.PSObject.Properties["error"] -and $null -ne $response.result) `
        -Message "stable launcher watcher search returned an error"
      if ($response.result.hits | Where-Object { $_.path -ceq "daily/2026-07-13.md" }) {
        return
      }
      $lastFailure = "watcher search did not return the changed fixture note"
    } catch {
      $lastFailure = $_.Exception.Message
    }
    Start-Sleep -Milliseconds 50
  }
  throw "$lastFailure within 5000ms"
}

function Wait-InstalledIndexedFixture {
  param(
    [Parameter(Mandatory)]
    [string]$Launcher,
    [Parameter(Mandatory)]
    [string]$Endpoint
  )

  $deadline = [DateTimeOffset]::UtcNow.AddSeconds(5)
  $lastFailure = "initial vault index did not become queryable"
  while ([DateTimeOffset]::UtcNow -lt $deadline) {
    try {
      $remainingMilliseconds = [int][Math]::Max(
        1,
        [Math]::Ceiling(($deadline - [DateTimeOffset]::UtcNow).TotalMilliseconds)
      )
      $response = Invoke-InstalledLauncherRequest `
        -Launcher $Launcher `
        -Endpoint $Endpoint `
        -Id 3 `
        -Method "knowledge.search" `
        -Params @{ query = "context engineering"; limit = 5 } `
        -TimeoutMilliseconds $remainingMilliseconds
      Assert-Condition `
        -Condition ($null -eq $response.PSObject.Properties["error"] -and $null -ne $response.result) `
        -Message "stable launcher initial-index search returned an error"
      if ($response.result.hits | Where-Object { $_.path -ceq "knowledge/ai/context-engineering.md" }) {
        return
      }
      $lastFailure = "initial-index search did not return the reference fixture note"
    } catch {
      $lastFailure = $_.Exception.Message
    }
    Start-Sleep -Milliseconds 50
  }
  throw "$lastFailure within 5000ms"
}

function Read-InstalledMcpResponse {
  param(
    [Parameter(Mandatory)]
    [System.Diagnostics.Process]$Process,
    [Parameter(Mandatory)]
    [string]$Description,
    [Parameter(Mandatory)]
    [int]$TimeoutMilliseconds
  )

  $readTask = $Process.StandardOutput.ReadLineAsync()
  if (-not $readTask.Wait($TimeoutMilliseconds)) {
    throw "$Description did not return an MCP response within ${TimeoutMilliseconds}ms"
  }
  $line = $readTask.GetAwaiter().GetResult()
  if ([string]::IsNullOrWhiteSpace($line)) {
    throw "$Description returned an empty MCP response"
  }
  return ($line | ConvertFrom-Json)
}

function Invoke-InstalledMcpRequest {
  param(
    [Parameter(Mandatory)]
    [System.Diagnostics.Process]$Process,
    [Parameter(Mandatory)]
    [int]$Id,
    [Parameter(Mandatory)]
    [string]$Method,
    [Parameter(Mandatory)]
    [hashtable]$Params,
    [int]$TimeoutMilliseconds = 5000
  )

  $request = @{
    jsonrpc = "2.0"
    id = $Id
    method = $Method
    params = $Params
  } | ConvertTo-Json -Depth 10 -Compress
  $Process.StandardInput.WriteLine($request)
  $Process.StandardInput.Flush()
  $response = Read-InstalledMcpResponse `
    -Process $Process `
    -Description "MCP $Method" `
    -TimeoutMilliseconds $TimeoutMilliseconds
  Assert-Condition -Condition ($response.jsonrpc -ceq "2.0" -and $response.id -eq $Id) -Message "MCP $Method response did not match its request"
  Assert-Condition `
    -Condition ($null -eq $response.PSObject.Properties["error"] -and $null -ne $response.result) `
    -Message "MCP $Method returned an error"
  return $response.result
}

function Wait-InstalledMcpSearch {
  param(
    [Parameter(Mandatory)]
    [System.Diagnostics.Process]$Process
  )

  $deadline = [DateTimeOffset]::UtcNow.AddSeconds(5)
  $requestId = 4
  $lastFailure = "the indexed vault did not become queryable"
  while ([DateTimeOffset]::UtcNow -lt $deadline) {
    try {
      $currentRequestId = $requestId
      $requestId += 1
      $remainingMilliseconds = [int][Math]::Max(
        1,
        [Math]::Ceiling(($deadline - [DateTimeOffset]::UtcNow).TotalMilliseconds)
      )
      $search = Invoke-InstalledMcpRequest -Process $Process -Id $currentRequestId -Method "tools/call" -Params @{
        name = "grimmore_search_knowledge"
        arguments = @{ query = "context engineering"; limit = 5 }
      } -TimeoutMilliseconds $remainingMilliseconds
      if ($null -ne $search.structuredContent -and $search.structuredContent.hits[0].path -ceq "knowledge/ai/context-engineering.md") {
        return
      }
      $lastFailure = "the MCP search response did not contain the indexed fixture note"
    } catch {
      $lastFailure = $_.Exception.Message
    }
    Start-Sleep -Milliseconds 50
  }
  throw "installed MCP bridge did not query the indexed vault within 5000ms: $lastFailure"
}

function Invoke-InstalledMcpSearch {
  param(
    [Parameter(Mandatory)]
    [string]$Daemon,
    [Parameter(Mandatory)]
    [string]$Endpoint
  )

  $startInfo = New-Object System.Diagnostics.ProcessStartInfo
  $startInfo.FileName = $Daemon
  $startInfo.Arguments = "mcp-stdio --vault-id reference --grant-id local --scope-id vault --endpoint `"$Endpoint`""
  $startInfo.UseShellExecute = $false
  $startInfo.CreateNoWindow = $true
  $startInfo.RedirectStandardInput = $true
  $startInfo.RedirectStandardOutput = $true
  $startInfo.RedirectStandardError = $true
  $process = New-Object System.Diagnostics.Process
  $process.StartInfo = $startInfo
  [void]$process.Start()
  $stderrTask = $process.StandardError.ReadToEndAsync()
  $closeRequested = $false

  try {
    $initialized = Invoke-InstalledMcpRequest -Process $process -Id 1 -Method "initialize" -Params @{
      protocolVersion = "2025-11-25"
      capabilities = @{}
      clientInfo = @{ name = "grimmore-phase-1-native-gate"; version = "1.0.0" }
    }
    Assert-Condition -Condition ($null -ne $initialized.capabilities.tools) -Message "installed MCP bridge did not expose tools"
    $notification = @{ jsonrpc = "2.0"; method = "notifications/initialized"; params = @{} } | ConvertTo-Json -Depth 10 -Compress
    $process.StandardInput.WriteLine($notification)
    $process.StandardInput.Flush()

    $tools = Invoke-InstalledMcpRequest -Process $process -Id 2 -Method "tools/list" -Params @{}
    $toolNames = @($tools.tools | ForEach-Object { $_.name } | Sort-Object)
    Assert-Condition -Condition (($toolNames -join ",") -ceq "grimmore_health,grimmore_search_knowledge") -Message "installed MCP bridge exposed an unexpected tool surface"
    foreach ($tool in $tools.tools) {
      Assert-Condition -Condition ($tool.annotations.readOnlyHint -eq $true) -Message "installed MCP tool was not marked read-only"
    }

    $health = Invoke-InstalledMcpRequest -Process $process -Id 3 -Method "tools/call" -Params @{ name = "grimmore_health"; arguments = @{} }
    Assert-Condition -Condition ($health.structuredContent.role -ceq "mcp-readonly") -Message "installed MCP bridge did not retain the read-only role"
    Wait-InstalledMcpSearch -Process $process
    Start-Sleep -Milliseconds 50
    Assert-Condition -Condition (-not $process.HasExited) -Message "installed MCP bridge exited before the test closed stdin"
  } finally {
    try {
      if ($process.HasExited) {
        Assert-Condition -Condition $closeRequested -Message "installed MCP bridge exited before the test closed stdin"
      } else {
        $closeRequested = $true
        $process.StandardInput.Close()
        if (-not $process.WaitForExit(5000)) {
          Stop-Process -Id $process.Id -Force -ErrorAction SilentlyContinue
          throw "installed MCP bridge did not stop after stdin closed"
        }
      }
      if (-not $stderrTask.Wait(5000)) {
        throw "installed MCP bridge stderr did not close after process exit"
      }
      $stderr = $stderrTask.GetAwaiter().GetResult()
      Assert-Condition -Condition ($process.ExitCode -eq 0) -Message "installed MCP bridge exited unsuccessfully: $stderr"
    } finally {
      $process.Dispose()
    }
  }
}

if ($env:OS -cne "Windows_NT") {
  throw "this release smoke must run on native Windows"
}
$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$principal = New-Object Security.Principal.WindowsPrincipal($identity)
if ($principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
  throw "the release smoke must run without administrator privileges"
}

$repository = (Resolve-Path (Join-Path $PSScriptRoot "..\..")).Path
Import-Module (Join-Path $repository "installers\windows\release-common.psm1") -Force
$target = Get-GrimmoreWindowsTarget
$installer = Join-Path $repository "installers\windows\install.ps1"
$rollback = Join-Path $repository "installers\windows\rollback.ps1"
$workspace = Join-Path ([IO.Path]::GetTempPath()) ("grimmore-windows-release-" + [Guid]::NewGuid().ToString("N"))
$certificateThumbprint = $null
$daemon = $null
$originalPath = $env:PATH

try {
  [void][IO.Directory]::CreateDirectory($workspace)
  $certificate = New-TestSigningCertificate -Workspace $workspace
  $certificateThumbprint = $certificate.Thumbprint
  $consoleControl = New-ConsoleControlHelper -Workspace $workspace
  Invoke-RequiredProgram -FilePath "cargo" -Arguments @(
    "build", "--release", "--locked", "-p", "grimmored", "-p", "grimmore-launcher"
  )

  $first = New-SignedArtifact `
    -Workspace $workspace `
    -Repository $repository `
    -Target $target `
    -Version "0.1.0-windows-slice-a" `
    -Certificate $certificate
  $installRoot = Join-Path $workspace "installed"
  Assert-ReleaseFailure -Description "wrong test publisher pin" -Action {
    & $installer `
      -Archive $first.Archive `
      -ReleaseEnvelope $first.Envelope `
      -TrustedPublisherThumbprint ("0" * 40) `
      -InstallRoot (Join-Path $workspace "wrong-publisher")
  }
  & $installer `
    -Archive $first.Archive `
    -ReleaseEnvelope $first.Envelope `
    -TrustedPublisherThumbprint $certificateThumbprint `
    -InstallRoot $installRoot
  $current = Read-InstallationState -Root $installRoot -Name "current"
  Assert-Condition -Condition ($current.version -ceq $first.Version) -Message "first install did not select its version"
  $doctor = & (Join-Path $installRoot "versions\$($first.Version)\grimmored.exe") doctor
  Assert-Condition -Condition ($LASTEXITCODE -eq 0) -Message "installed companion doctor failed"
  $doctorReport = ($doctor -join [Environment]::NewLine) | ConvertFrom-Json
  Assert-Condition -Condition ($doctorReport.credentialStoreAvailable -eq $true) -Message "Credential Manager probe failed"

  $env:PATH = "$(Join-Path $installRoot 'bin');$originalPath"
  $help = & grimmore-launcher --help
  Assert-Condition -Condition ($LASTEXITCODE -eq 0 -and ($help -join "`n") -match "plugin-session") `
    -Message "bare stable grimmore-launcher did not delegate through current state"
  $vault = Join-Path $workspace "launcher-vault"
  [void][IO.Directory]::CreateDirectory($vault)
  Copy-Item `
    -Path (Join-Path $repository "tests\fixtures\vaults\reference-vault\*") `
    -Destination $vault `
    -Recurse
  $endpoint = "\\.\pipe\grimmore-release-$([Guid]::NewGuid().ToString('N'))"
  $daemonPath = Join-Path $installRoot "versions\$($first.Version)\grimmored.exe"
  $launcherPath = Join-Path $installRoot "bin\grimmore-launcher.exe"
  $database = Join-Path $workspace "launcher.sqlite3"
  $daemon = Start-InstalledDaemon `
    -Daemon $daemonPath `
    -Database $database `
    -Vault $vault `
    -Endpoint $endpoint `
    -ConsoleControl $consoleControl `
    -Workspace $workspace
  Wait-InstalledLauncherHealth -Launcher $launcherPath -Endpoint $endpoint | Out-Null
  Stop-InstalledDaemonCleanly -Process $daemon -ConsoleControl $consoleControl
  $daemon.Dispose()
  $daemon = $null

  $daemon = Start-InstalledDaemon `
    -Daemon $daemonPath `
    -Database $database `
    -Vault $vault `
    -Endpoint $endpoint `
    -ConsoleControl $consoleControl `
    -Workspace $workspace
  Wait-InstalledLauncherHealth -Launcher $launcherPath -Endpoint $endpoint | Out-Null
  Stop-InstalledDaemonForcefully -Process $daemon
  $daemon.Dispose()
  $daemon = $null

  $daemon = Start-InstalledDaemon `
    -Daemon $daemonPath `
    -Database $database `
    -Vault $vault `
    -Endpoint $endpoint `
    -ConsoleControl $consoleControl `
    -Workspace $workspace
  Wait-InstalledLauncherHealth -Launcher $launcherPath -Endpoint $endpoint | Out-Null
  Wait-InstalledIndexedFixture -Launcher $launcherPath -Endpoint $endpoint
  $sentinel = "grimmorewindowswatcher$([Guid]::NewGuid().ToString('N'))"
  $watcherNote = Join-Path $vault "daily\2026-07-13.md"
  $utf8 = New-Object System.Text.UTF8Encoding($false)
  [IO.File]::AppendAllText($watcherNote, "`n$sentinel`n", $utf8)
  Wait-InstalledWatcherSearch -Launcher $launcherPath -Endpoint $endpoint -Sentinel $sentinel
  Invoke-InstalledMcpSearch -Daemon $daemonPath -Endpoint $endpoint
  Stop-InstalledDaemonCleanly -Process $daemon -ConsoleControl $consoleControl
  $daemon.Dispose()
  $daemon = $null

  $tamperedDirectory = Join-Path $workspace "tampered"
  [void][IO.Directory]::CreateDirectory($tamperedDirectory)
  $tamperedArchive = Join-Path $tamperedDirectory ([IO.Path]::GetFileName($first.Archive))
  Copy-Item -LiteralPath $first.Archive -Destination $tamperedArchive
  $tamperedBytes = [IO.File]::ReadAllBytes($tamperedArchive)
  $tamperedBytes[[int]($tamperedBytes.Length / 2)] = $tamperedBytes[[int]($tamperedBytes.Length / 2)] -bxor 1
  [IO.File]::WriteAllBytes($tamperedArchive, $tamperedBytes)
  Assert-ReleaseFailure -Description "same-size archive tamper" -Action {
    & $installer `
      -Archive $tamperedArchive `
      -ReleaseEnvelope $first.Envelope `
      -TrustedPublisherThumbprint $certificateThumbprint `
      -InstallRoot (Join-Path $workspace "tampered-install")
  }

  $incompatibleProtocol = New-SignedArtifact `
    -Workspace $workspace `
    -Repository $repository `
    -Target $target `
    -Version "0.1.0-windows-slice-protocol" `
    -Certificate $certificate `
    -ProtocolMinimum 2 `
    -ProtocolMaximum 2
  Assert-ReleaseFailure -Description "incompatible signed protocol range" -Action {
    & $installer `
      -Archive $incompatibleProtocol.Archive `
      -ReleaseEnvelope $incompatibleProtocol.Envelope `
      -TrustedPublisherThumbprint $certificateThumbprint `
      -InstallRoot (Join-Path $workspace "incompatible-protocol")
  }

  $second = New-SignedArtifact `
    -Workspace $workspace `
    -Repository $repository `
    -Target $target `
    -Version "0.1.0-windows-slice-b" `
    -Certificate $certificate
  & $installer `
    -Archive $second.Archive `
    -ReleaseEnvelope $second.Envelope `
    -TrustedPublisherThumbprint $certificateThumbprint `
    -InstallRoot $installRoot
  $current = Read-InstallationState -Root $installRoot -Name "current"
  $previous = Read-InstallationState -Root $installRoot -Name "previous"
  Assert-Condition -Condition ($current.version -ceq $second.Version) -Message "upgrade did not select its version"
  Assert-Condition -Condition ($previous.version -ceq $first.Version) -Message "upgrade did not retain rollback version"
  & $installer `
    -Archive $second.Archive `
    -ReleaseEnvelope $second.Envelope `
    -TrustedPublisherThumbprint $certificateThumbprint `
    -InstallRoot $installRoot
  $current = Read-InstallationState -Root $installRoot -Name "current"
  $previous = Read-InstallationState -Root $installRoot -Name "previous"
  Assert-Condition -Condition ($current.version -ceq $second.Version) -Message "same-version install changed current state"
  Assert-Condition -Condition ($previous.version -ceq $first.Version) -Message "same-version install destroyed rollback state"
  & $rollback -TrustedPublisherThumbprint $certificateThumbprint -InstallRoot $installRoot
  $current = Read-InstallationState -Root $installRoot -Name "current"
  $previous = Read-InstallationState -Root $installRoot -Name "previous"
  Assert-Condition -Condition ($current.version -ceq $first.Version) -Message "rollback did not select the prior version"
  Assert-Condition -Condition ($previous.version -ceq $second.Version) -Message "rollback did not preserve the replaced version"
  Write-Output "Windows signed install, authenticated launcher, upgrade, and rollback smoke passed for $target."
} finally {
  if ($null -ne $daemon) {
    Stop-Process -Id $daemon.Id -Force -ErrorAction SilentlyContinue
    if (-not $daemon.WaitForExit(5000)) {
      $daemon.Dispose()
      throw "installed companion did not stop during cleanup"
    }
    $daemon.Dispose()
  }
  $env:PATH = $originalPath
  Remove-TestSigningCertificate -Thumbprint $certificateThumbprint
  if (Test-Path -LiteralPath $workspace) {
    Remove-Item -LiteralPath $workspace -Recurse -Force
  }
}
