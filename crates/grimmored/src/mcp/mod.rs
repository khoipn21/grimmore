//! Read-only Model Context Protocol bridge to the authenticated local companion.

use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct McpStdioConfig {
    pub endpoint: PathBuf,
    pub vault_id: String,
    pub grant_id: String,
    pub scope_id: String,
}

#[cfg(any(unix, windows))]
mod local;

#[cfg(any(unix, windows))]
pub use local::{GrimmoreMcp, McpBridgeError, serve_stdio};

#[cfg(not(any(unix, windows)))]
pub async fn serve_stdio(_config: McpStdioConfig) -> anyhow::Result<()> {
    anyhow::bail!("read-only MCP local transport is not implemented for this platform")
}
