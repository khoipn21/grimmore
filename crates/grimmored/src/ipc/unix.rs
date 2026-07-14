use std::{
    fs,
    future::Future,
    io::ErrorKind,
    os::unix::fs::{FileTypeExt as _, MetadataExt as _, PermissionsExt as _},
    path::{Path, PathBuf},
    time::Duration,
};

use grimmore_core::auth::random_token;
use tokio::{net::UnixListener, time::timeout};
use tracing::{debug, warn};

use super::{IpcServerConfig, IpcServerError, connection::handle_connection};

const STALE_CONNECT_TIMEOUT: Duration = Duration::from_millis(250);

pub async fn serve(config: IpcServerConfig) -> Result<(), IpcServerError> {
    serve_with_shutdown(config, async {
        if let Err(error) = tokio::signal::ctrl_c().await {
            warn!(%error, "failed to install IPC shutdown signal");
        }
    })
    .await
}

pub async fn serve_with_shutdown<F>(
    config: IpcServerConfig,
    shutdown: F,
) -> Result<(), IpcServerError>
where
    F: Future<Output = ()>,
{
    let (listener, owner_uid, _socket_guard) = bind_private_listener(&config.endpoint).await?;
    tokio::pin!(shutdown);

    loop {
        tokio::select! {
            () = &mut shutdown => return Ok(()),
            accepted = listener.accept() => {
                let (stream, _) = accepted.map_err(|source| IpcServerError::Io {
                    operation: "accept",
                    path: config.endpoint.clone(),
                    source,
                })?;
                let peer_uid = match stream.peer_cred() {
                    Ok(credentials) => credentials.uid(),
                    Err(error) => {
                        warn!(%error, "rejected IPC peer without process credentials");
                        continue;
                    }
                };
                if peer_uid != owner_uid {
                    warn!(peer_uid, owner_uid, "rejected IPC peer owned by another user");
                    continue;
                }

                let connection_config = config.clone();
                tokio::spawn(async move {
                    if let Err(error) = handle_connection(stream, connection_config).await {
                        debug!(%error, "IPC connection closed with an error");
                    }
                });
            }
        }
    }
}

async fn bind_private_listener(
    endpoint: &Path,
) -> Result<(UnixListener, u32, SocketGuard), IpcServerError> {
    let parent = endpoint
        .parent()
        .ok_or_else(|| IpcServerError::UntrustedEndpoint(endpoint.to_path_buf()))?;
    fs::create_dir_all(parent).map_err(|source| IpcServerError::Io {
        operation: "create endpoint directory",
        path: parent.to_path_buf(),
        source,
    })?;
    let owner_uid = verify_private_current_user_directory(parent)?;
    remove_stale_socket(endpoint, owner_uid).await?;

    let listener = UnixListener::bind(endpoint).map_err(|source| IpcServerError::Io {
        operation: "bind",
        path: endpoint.to_path_buf(),
        source,
    })?;
    fs::set_permissions(endpoint, fs::Permissions::from_mode(0o600)).map_err(|source| {
        IpcServerError::Io {
            operation: "set socket permissions",
            path: endpoint.to_path_buf(),
            source,
        }
    })?;
    let metadata = endpoint
        .symlink_metadata()
        .map_err(|source| IpcServerError::Io {
            operation: "inspect bound socket",
            path: endpoint.to_path_buf(),
            source,
        })?;
    if !metadata.file_type().is_socket()
        || metadata.uid() != owner_uid
        || metadata.mode() & 0o077 != 0
    {
        return Err(IpcServerError::UntrustedEndpoint(endpoint.to_path_buf()));
    }

    let guard = SocketGuard {
        path: endpoint.to_path_buf(),
        device: metadata.dev(),
        inode: metadata.ino(),
    };
    Ok((listener, owner_uid, guard))
}

fn verify_private_current_user_directory(path: &Path) -> Result<u32, IpcServerError> {
    let probe = path.join(format!(
        ".owner-probe-{}",
        random_token().map_err(|error| {
            IpcServerError::Io {
                operation: "generate owner probe",
                path: path.to_path_buf(),
                source: std::io::Error::other(error),
            }
        })?
    ));
    fs::create_dir(&probe).map_err(|source| IpcServerError::Io {
        operation: "create owner probe",
        path: probe.clone(),
        source,
    })?;
    let probe_metadata = probe.metadata().map_err(|source| IpcServerError::Io {
        operation: "inspect owner probe",
        path: probe.clone(),
        source,
    })?;
    fs::remove_dir(&probe).map_err(|source| IpcServerError::Io {
        operation: "remove owner probe",
        path: probe,
        source,
    })?;

    let metadata = path
        .symlink_metadata()
        .map_err(|source| IpcServerError::Io {
            operation: "inspect endpoint directory",
            path: path.to_path_buf(),
            source,
        })?;
    if !metadata.file_type().is_dir() || metadata.uid() != probe_metadata.uid() {
        return Err(IpcServerError::UntrustedEndpoint(path.to_path_buf()));
    }
    if metadata.mode() & 0o077 != 0 {
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|source| {
            IpcServerError::Io {
                operation: "set endpoint directory permissions",
                path: path.to_path_buf(),
                source,
            }
        })?;
    }
    Ok(probe_metadata.uid())
}

async fn remove_stale_socket(endpoint: &Path, owner_uid: u32) -> Result<(), IpcServerError> {
    let metadata = match endpoint.symlink_metadata() {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
        Err(source) => {
            return Err(IpcServerError::Io {
                operation: "inspect existing endpoint",
                path: endpoint.to_path_buf(),
                source,
            });
        }
    };
    if !metadata.file_type().is_socket()
        || metadata.uid() != owner_uid
        || metadata.mode() & 0o077 != 0
    {
        return Err(IpcServerError::UntrustedEndpoint(endpoint.to_path_buf()));
    }
    if matches!(
        timeout(
            STALE_CONNECT_TIMEOUT,
            tokio::net::UnixStream::connect(endpoint)
        )
        .await,
        Ok(Ok(_))
    ) {
        return Err(IpcServerError::AlreadyRunning(endpoint.to_path_buf()));
    }
    fs::remove_file(endpoint).map_err(|source| IpcServerError::Io {
        operation: "remove stale endpoint",
        path: endpoint.to_path_buf(),
        source,
    })
}

struct SocketGuard {
    path: PathBuf,
    device: u64,
    inode: u64,
}

impl Drop for SocketGuard {
    fn drop(&mut self) {
        let Ok(metadata) = self.path.symlink_metadata() else {
            return;
        };
        if metadata.file_type().is_socket()
            && metadata.dev() == self.device
            && metadata.ino() == self.inode
        {
            let _ = fs::remove_file(&self.path);
        }
    }
}
