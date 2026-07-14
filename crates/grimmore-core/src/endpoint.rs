//! Verified client connections to the per-user local IPC endpoint.

use std::path::{Path, PathBuf};

#[cfg(unix)]
use directories::ProjectDirs;
use thiserror::Error;

use crate::{
    credentials::RootSecret,
    protocol::SessionRole,
    session::{AuthenticatedSession, SessionError, authenticate_client},
};

#[derive(Debug, Error)]
pub enum EndpointError {
    #[error("this platform does not expose a per-user application data directory")]
    MissingDataDirectory,
    #[error("construct private IPC endpoint: {0}")]
    DefaultEndpoint(std::io::Error),
    #[error("inspect IPC endpoint {path}: {source}")]
    Inspect {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("IPC endpoint is not a verified private current-user endpoint: {0}")]
    Untrusted(PathBuf),
    #[error("connect IPC endpoint {path}: {source}")]
    Connect {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("IPC server process identity does not own the endpoint")]
    PeerMismatch,
    #[error("inspect IPC server process identity: {0}")]
    PeerCredentials(std::io::Error),
    #[error("IPC session failed: {0}")]
    Session(#[from] SessionError),
}

#[cfg(unix)]
pub type LocalIpcStream = tokio::net::UnixStream;

#[cfg(windows)]
pub type LocalIpcStream = tokio::net::windows::named_pipe::NamedPipeClient;

#[cfg(any(unix, windows))]
pub struct AuthenticatedConnection {
    pub stream: LocalIpcStream,
    pub session: AuthenticatedSession,
}

pub fn default_endpoint_path() -> Result<PathBuf, EndpointError> {
    #[cfg(unix)]
    if let Some(runtime) = std::env::var_os("XDG_RUNTIME_DIR") {
        let runtime = PathBuf::from(runtime);
        if runtime.is_absolute() {
            return Ok(runtime.join("grimmore/grimmore.sock"));
        }
    }

    #[cfg(windows)]
    {
        grimmore_windows_native::current_user_pipe_endpoint("grimmore-v1")
            .map(PathBuf::from)
            .map_err(EndpointError::DefaultEndpoint)
    }

    #[cfg(unix)]
    {
        let project_dirs = ProjectDirs::from("dev", "Grimmore", "Grimmore")
            .ok_or(EndpointError::MissingDataDirectory)?;
        Ok(project_dirs
            .data_local_dir()
            .join("run")
            .join("grimmore.sock"))
    }

    #[cfg(not(any(unix, windows)))]
    {
        Err(EndpointError::MissingDataDirectory)
    }
}

#[cfg(unix)]
pub async fn connect_authenticated(
    endpoint: &Path,
    secret: &RootSecret,
    role: SessionRole,
    client_version: &str,
) -> Result<AuthenticatedConnection, EndpointError> {
    use std::os::unix::fs::{FileTypeExt as _, MetadataExt as _};

    let parent = endpoint
        .parent()
        .ok_or_else(|| EndpointError::Untrusted(endpoint.to_path_buf()))?;
    let parent_metadata = parent.metadata().map_err(|source| EndpointError::Inspect {
        path: parent.to_path_buf(),
        source,
    })?;
    let endpoint_metadata =
        endpoint
            .symlink_metadata()
            .map_err(|source| EndpointError::Inspect {
                path: endpoint.to_path_buf(),
                source,
            })?;
    if !parent_metadata.is_dir()
        || parent_metadata.mode() & 0o077 != 0
        || !endpoint_metadata.file_type().is_socket()
        || endpoint_metadata.mode() & 0o077 != 0
        || endpoint_metadata.uid() != parent_metadata.uid()
    {
        return Err(EndpointError::Untrusted(endpoint.to_path_buf()));
    }

    let mut stream = tokio::net::UnixStream::connect(endpoint)
        .await
        .map_err(|source| EndpointError::Connect {
            path: endpoint.to_path_buf(),
            source,
        })?;
    if stream
        .peer_cred()
        .map_err(EndpointError::PeerCredentials)?
        .uid()
        != endpoint_metadata.uid()
    {
        return Err(EndpointError::PeerMismatch);
    }
    let session = authenticate_client(&mut stream, secret, role, client_version).await?;
    Ok(AuthenticatedConnection { stream, session })
}

#[cfg(windows)]
pub async fn connect_authenticated(
    endpoint: &Path,
    secret: &RootSecret,
    role: SessionRole,
    client_version: &str,
) -> Result<AuthenticatedConnection, EndpointError> {
    use std::os::windows::io::AsRawHandle as _;

    use grimmore_windows_native::{
        named_pipe_server_is_current_user, validate_local_named_pipe_name,
    };
    use tokio::net::windows::named_pipe::ClientOptions;

    validate_local_named_pipe_name(endpoint.as_os_str())
        .map_err(|_| EndpointError::Untrusted(endpoint.to_path_buf()))?;
    let mut stream =
        ClientOptions::new()
            .open(endpoint)
            .map_err(|source| EndpointError::Connect {
                path: endpoint.to_path_buf(),
                source,
            })?;
    if !named_pipe_server_is_current_user(stream.as_raw_handle() as usize)
        .map_err(EndpointError::PeerCredentials)?
    {
        return Err(EndpointError::PeerMismatch);
    }
    let session = authenticate_client(&mut stream, secret, role, client_version).await?;
    Ok(AuthenticatedConnection { stream, session })
}
