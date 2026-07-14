use std::{path::PathBuf, sync::Arc};

use grimmore_core::{credentials::RootSecret, protocol::SessionRole};
use thiserror::Error;

use crate::storage::Storage;

#[derive(Clone)]
pub struct IpcServerConfig {
    pub endpoint: PathBuf,
    pub storage: Arc<Storage>,
    pub secret: Arc<RootSecret>,
    pub vault_id: String,
    pub grant_id: String,
    pub scope_id: String,
}

#[derive(Debug, Error)]
pub enum IpcServerError {
    #[error("IPC endpoint is already serving: {0}")]
    AlreadyRunning(PathBuf),
    #[error("IPC endpoint is not a private current-user endpoint: {0}")]
    UntrustedEndpoint(PathBuf),
    #[error("IPC operation {operation} failed for {path}: {source}")]
    Io {
        operation: &'static str,
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("IPC shutdown handler failed: {0}")]
    Shutdown(std::io::Error),
    #[error("local IPC is not implemented for this platform")]
    UnsupportedPlatform,
}

#[cfg(any(unix, windows))]
mod connection;

#[cfg(unix)]
mod unix;

#[cfg(unix)]
pub use unix::{serve, serve_with_shutdown};

#[cfg(windows)]
mod windows;

#[cfg(windows)]
pub use windows::{serve, serve_with_shutdown};

#[cfg(not(any(unix, windows)))]
pub async fn serve(_config: IpcServerConfig) -> Result<(), IpcServerError> {
    Err(IpcServerError::UnsupportedPlatform)
}

#[must_use]
fn role_can_write_proposals(role: SessionRole) -> bool {
    matches!(role, SessionRole::Plugin)
}
