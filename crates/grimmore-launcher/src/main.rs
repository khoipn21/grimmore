use std::path::PathBuf;

#[cfg(any(windows, test))]
use anyhow::Context as _;
use anyhow::Result;
use clap::{Parser, Subcommand};
use grimmore_core::{
    credentials::RootSecret, endpoint::default_endpoint_path, protocol::SessionRole,
};
#[cfg(any(windows, test))]
use serde::Deserialize;
#[cfg(windows)]
use sha2::{Digest as _, Sha256};

#[cfg(any(windows, test))]
const POINTER_SCHEMA_VERSION: u16 = 1;
#[cfg(windows)]
const MAX_VERSIONED_LAUNCHER_BYTES: u64 = 512 * 1024 * 1024;

#[cfg(any(windows, test))]
#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct InstalledPointer {
    schema_version: u16,
    target: String,
    version: String,
    launcher_sha256: String,
}

#[cfg(any(windows, test))]
impl InstalledPointer {
    fn parse(bytes: &[u8]) -> Result<Self> {
        let pointer: Self =
            serde_json::from_slice(bytes).context("parse installed launcher state")?;
        if pointer.schema_version != POINTER_SCHEMA_VERSION {
            anyhow::bail!("installed launcher state has an unsupported schema version");
        }
        if !matches!(pointer.target.as_str(), "windows-x64" | "windows-arm64") {
            anyhow::bail!("installed launcher state has an unsupported target");
        }
        if !is_normalized_version(&pointer.version) {
            anyhow::bail!("installed launcher state has an invalid version");
        }
        if !is_sha256(&pointer.launcher_sha256) {
            anyhow::bail!("installed launcher state has an invalid launcher hash");
        }
        Ok(pointer)
    }
}

#[cfg(any(windows, test))]
fn is_normalized_version(value: &str) -> bool {
    let (core, pre_release) = match value.split_once('-') {
        Some((core, pre_release)) => (core, Some(pre_release)),
        None => (value, None),
    };
    if core.split('.').count() != 3
        || core.split('.').any(|component| {
            component.is_empty() || !component.bytes().all(|byte| byte.is_ascii_digit())
        })
    {
        return false;
    }
    pre_release.is_none_or(|suffix| {
        !suffix.is_empty()
            && suffix
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-'))
    })
}

#[cfg(any(windows, test))]
fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

#[derive(Debug, Parser)]
#[command(
    name = "grimmore-launcher",
    version,
    about = "Stable launcher for the Grimmore companion"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Open an authenticated plugin-role session and bridge framed stdin/stdout.
    PluginSession {
        #[arg(long)]
        endpoint: Option<PathBuf>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    #[cfg(windows)]
    if let Some(status) = delegate_from_stable_launcher()? {
        std::process::exit(status.code().unwrap_or(1));
    }

    let cli = Cli::parse();
    match cli.command {
        Command::PluginSession { endpoint } => {
            plugin_session(endpoint).await?;
        }
    }
    Ok(())
}

#[cfg(windows)]
fn delegate_from_stable_launcher() -> Result<Option<std::process::ExitStatus>> {
    use std::{
        ffi::OsStr,
        fs,
        process::{Command, Stdio},
    };

    let executable = std::env::current_exe().context("resolve stable launcher path")?;
    let Some(file_name) = executable.file_name().and_then(OsStr::to_str) else {
        return Ok(None);
    };
    let Some(bin_directory) = executable.parent() else {
        return Ok(None);
    };
    let Some(bin_name) = bin_directory.file_name().and_then(OsStr::to_str) else {
        return Ok(None);
    };
    if !file_name.eq_ignore_ascii_case("grimmore-launcher.exe")
        || !bin_name.eq_ignore_ascii_case("bin")
    {
        return Ok(None);
    }
    let root = bin_directory
        .parent()
        .context("stable launcher is missing its installation root")?;
    assert_real_directory(root, "installation root")?;
    let versions_root = root.join("versions");
    assert_real_directory(&versions_root, "installed versions directory")?;
    let state_path = root.join("current.json");
    let state_metadata = assert_regular_file(&state_path, "installed launcher state")?;
    if state_metadata.len() > 1024 {
        anyhow::bail!("installed launcher state is too large");
    }
    let state = fs::read(&state_path)
        .with_context(|| format!("read installed launcher state at {}", state_path.display()))?;
    let pointer = InstalledPointer::parse(&state)?;
    if pointer.target != current_windows_target() {
        anyhow::bail!("installed launcher state targets a different Windows architecture");
    }
    let version_directory = versions_root.join(pointer.version);
    assert_real_directory(&version_directory, "selected version directory")?;
    assert_regular_file(
        &version_directory.join(".ready"),
        "selected version readiness marker",
    )?;
    assert_regular_file(
        &version_directory.join("release-manifest.json"),
        "selected version release manifest",
    )?;
    assert_regular_file(
        &version_directory.join("release-envelope.ps1"),
        "selected version release envelope",
    )?;
    let versioned_launcher = version_directory.join("grimmore-launcher.exe");
    let launcher_metadata =
        assert_regular_file(&versioned_launcher, "selected versioned launcher")?;
    if launcher_metadata.len() > MAX_VERSIONED_LAUNCHER_BYTES {
        anyhow::bail!("selected versioned launcher is too large");
    }
    if sha256_file(&versioned_launcher)? != pointer.launcher_sha256 {
        anyhow::bail!("selected versioned launcher does not match the installed pointer");
    }
    let status = Command::new(&versioned_launcher)
        .args(std::env::args_os().skip(1))
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .with_context(|| {
            format!(
                "start selected launcher at {}",
                versioned_launcher.display()
            )
        })?;
    Ok(Some(status))
}

#[cfg(windows)]
fn assert_real_directory(path: &std::path::Path, description: &str) -> Result<()> {
    let metadata = std::fs::symlink_metadata(path)
        .with_context(|| format!("inspect {description} at {}", path.display()))?;
    if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
        anyhow::bail!("{description} is not a real directory");
    }
    Ok(())
}

#[cfg(windows)]
fn assert_regular_file(path: &std::path::Path, description: &str) -> Result<std::fs::Metadata> {
    let metadata = std::fs::symlink_metadata(path)
        .with_context(|| format!("inspect {description} at {}", path.display()))?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        anyhow::bail!("{description} is not a regular file");
    }
    Ok(metadata)
}

#[cfg(windows)]
fn sha256_file(path: &std::path::Path) -> Result<String> {
    use std::io::{BufReader, Read as _};

    let file = std::fs::File::open(path)
        .with_context(|| format!("open selected versioned launcher at {}", path.display()))?;
    let mut reader = BufReader::new(file);
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 81920];
    let mut bytes_hashed = 0_u64;
    loop {
        let bytes_read = reader
            .read(&mut buffer)
            .with_context(|| format!("read selected versioned launcher at {}", path.display()))?;
        if bytes_read == 0 {
            break;
        }
        bytes_hashed = bytes_hashed
            .checked_add(u64::try_from(bytes_read).expect("buffer length fits in u64"))
            .context("selected versioned launcher is too large")?;
        if bytes_hashed > MAX_VERSIONED_LAUNCHER_BYTES {
            anyhow::bail!("selected versioned launcher is too large");
        }
        hasher.update(&buffer[..bytes_read]);
    }
    Ok(hex::encode(hasher.finalize()))
}

#[cfg(windows)]
fn current_windows_target() -> &'static str {
    #[cfg(target_arch = "x86_64")]
    {
        "windows-x64"
    }
    #[cfg(target_arch = "aarch64")]
    {
        "windows-arm64"
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    {
        "unsupported"
    }
}

#[cfg(any(unix, windows))]
async fn plugin_session(endpoint: Option<PathBuf>) -> Result<()> {
    use grimmore_core::endpoint::connect_authenticated;
    use tokio::io::{AsyncWriteExt as _, copy, stdin, stdout};

    let endpoint = endpoint.map_or_else(default_endpoint_path, Ok)?;
    let secret = RootSecret::load()?;
    let connection = connect_authenticated(
        &endpoint,
        &secret,
        SessionRole::Plugin,
        env!("CARGO_PKG_VERSION"),
    )
    .await?;
    #[cfg(unix)]
    let (mut socket_reader, mut socket_writer) = connection.stream.into_split();
    #[cfg(windows)]
    let (mut socket_reader, mut socket_writer) = tokio::io::split(connection.stream);
    let mut input = stdin();
    let mut output = stdout();

    let to_companion = async {
        copy(&mut input, &mut socket_writer).await?;
        socket_writer.shutdown().await
    };
    let from_companion = async {
        copy(&mut socket_reader, &mut output).await?;
        output.flush().await
    };
    tokio::try_join!(to_companion, from_companion)?;
    Ok(())
}

#[cfg(not(any(unix, windows)))]
async fn plugin_session(_endpoint: Option<PathBuf>) -> Result<()> {
    anyhow::bail!("local IPC is not implemented for this platform")
}

#[cfg(test)]
mod tests {
    use super::{InstalledPointer, is_normalized_version};

    #[test]
    fn parses_strict_windows_installed_pointer() {
        let pointer = InstalledPointer::parse(
            br#"{"schemaVersion":1,"target":"windows-arm64","version":"1.2.3-slice.1","launcherSha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}"#,
        )
        .expect("pointer is valid");

        assert_eq!(pointer.schema_version, 1);
        assert_eq!(pointer.target, "windows-arm64");
        assert_eq!(pointer.version, "1.2.3-slice.1");
    }

    #[test]
    fn rejects_malformed_or_unsafe_installed_pointer() {
        for pointer in [
            br#"{"schemaVersion":true,"target":"windows-x64","version":"1.2.3","launcherSha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}"#.as_slice(),
            br#"{"schemaVersion":1,"target":"linux-x64-gnu","version":"1.2.3","launcherSha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}"#.as_slice(),
            br#"{"schemaVersion":1,"target":"windows-x64","version":"../1.2.3","launcherSha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}"#.as_slice(),
            br#"{"schemaVersion":1,"target":"windows-x64","version":"1.2.3","launcherSha256":"not-a-hash"}"#.as_slice(),
            br#"{"schemaVersion":1,"target":"windows-x64","version":"1.2.3","launcherSha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","extra":true}"#
                .as_slice(),
        ] {
            assert!(InstalledPointer::parse(pointer).is_err());
        }
    }

    #[test]
    fn only_accepts_normalized_release_versions() {
        assert!(is_normalized_version("1.2.3"));
        assert!(is_normalized_version("1.2.3-slice.1"));
        assert!(!is_normalized_version("1.2"));
        assert!(!is_normalized_version("1.2.3/escape"));
        assert!(!is_normalized_version("1.2.3-"));
    }
}
