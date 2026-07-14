use std::{
    path::Path,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use grimmore_core::{
    credentials::RootSecret,
    endpoint::{EndpointError, LocalIpcStream, connect_authenticated},
    framing::{FrameError, read_json, write_json},
    protocol::{
        HealthResult, JsonRpcRequest, JsonRpcResponse, SearchNotesParams, SearchNotesResult,
        SessionRole, method,
    },
};
use rmcp::{
    ErrorData, Json, ServerHandler, ServiceExt,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{Implementation, ServerCapabilities, ServerInfo},
    tool, tool_handler, tool_router,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use thiserror::Error;
use tokio::{sync::Mutex, time::timeout};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(6);
const DEFAULT_SEARCH_LIMIT: u16 = 20;

#[derive(Debug, Error)]
pub enum McpBridgeError {
    #[error(transparent)]
    Endpoint(#[from] EndpointError),
    #[error(transparent)]
    Frame(#[from] FrameError),
    #[error("serialize or decode local companion message: {0}")]
    Json(#[from] serde_json::Error),
    #[error("local companion response timed out")]
    Timeout,
    #[error("local companion query connection is no longer usable")]
    ConnectionClosed,
    #[error("local companion returned a response for a different request")]
    ResponseMismatch,
    #[error("local companion rejected request with code {code}: {message}")]
    Remote { code: i32, message: String },
    #[error("system clock is earlier than the Unix epoch")]
    Clock,
}

#[derive(Debug)]
struct IpcQueryClient {
    stream: LocalIpcStream,
    next_id: u64,
    vault_id: String,
    grant_id: String,
    scope_id: String,
    usable: bool,
}

impl IpcQueryClient {
    async fn connect(
        endpoint: &Path,
        secret: &RootSecret,
        vault_id: String,
        grant_id: String,
        scope_id: String,
    ) -> Result<Self, McpBridgeError> {
        let connection = connect_authenticated(
            endpoint,
            secret,
            SessionRole::McpReadonly,
            env!("CARGO_PKG_VERSION"),
        )
        .await?;
        Ok(Self {
            stream: connection.stream,
            next_id: 1,
            vault_id,
            grant_id,
            scope_id,
            usable: true,
        })
    }

    async fn call<P, R>(&mut self, rpc_method: &str, params: P) -> Result<R, McpBridgeError>
    where
        P: Serialize,
        R: DeserializeOwned,
    {
        if !self.usable {
            return Err(McpBridgeError::ConnectionClosed);
        }
        let request_id = self.next_id;
        self.next_id = self.next_id.checked_add(1).unwrap_or(1);
        let deadline_unix_ms = unix_time_millis()?
            + u64::try_from(REQUEST_TIMEOUT.as_millis()).expect("request timeout fits u64");
        let request = JsonRpcRequest {
            jsonrpc: "2.0".to_owned(),
            id: request_id,
            method: rpc_method.to_owned(),
            params: serde_json::to_value(params)?,
            deadline_unix_ms,
            vault_id: self.vault_id.clone(),
            grant_id: self.grant_id.clone(),
            scope_id: self.scope_id.clone(),
        };

        if let Err(error) = write_json(&mut self.stream, &request).await {
            self.usable = false;
            return Err(error.into());
        }
        let response = match timeout(
            RESPONSE_TIMEOUT,
            read_json::<_, JsonRpcResponse>(&mut self.stream),
        )
        .await
        {
            Ok(Ok(response)) => response,
            Ok(Err(error)) => {
                self.usable = false;
                return Err(error.into());
            }
            Err(_) => {
                self.usable = false;
                return Err(McpBridgeError::Timeout);
            }
        };

        match response {
            JsonRpcResponse::Success(success) if success.id == request_id => {
                Ok(serde_json::from_value(success.result)?)
            }
            JsonRpcResponse::Failure(failure) if failure.id == Some(request_id) => {
                Err(McpBridgeError::Remote {
                    code: failure.error.code,
                    message: failure.error.message,
                })
            }
            _ => {
                self.usable = false;
                Err(McpBridgeError::ResponseMismatch)
            }
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SearchKnowledgeInput {
    /// Literal words to find in the selected vault's indexed Markdown.
    pub query: String,
    /// Maximum number of results, from 1 through 100.
    #[serde(default = "default_search_limit")]
    pub limit: u16,
}

const fn default_search_limit() -> u16 {
    DEFAULT_SEARCH_LIMIT
}

#[derive(Debug, Clone)]
pub struct GrimmoreMcp {
    client: Arc<Mutex<IpcQueryClient>>,
    tool_router: ToolRouter<Self>,
}

#[tool_router(router = tool_router)]
impl GrimmoreMcp {
    pub async fn connect_with_secret(
        endpoint: &Path,
        secret: &RootSecret,
        vault_id: String,
        grant_id: String,
        scope_id: String,
    ) -> Result<Self, McpBridgeError> {
        let client =
            IpcQueryClient::connect(endpoint, secret, vault_id, grant_id, scope_id).await?;
        Ok(Self {
            client: Arc::new(Mutex::new(client)),
            tool_router: Self::tool_router(),
        })
    }

    /// Search indexed knowledge in the currently granted local Obsidian vault.
    #[tool(
        name = "grimmore_search_knowledge",
        annotations(title = "Search Grimmore knowledge", read_only_hint = true)
    )]
    async fn search_knowledge(
        &self,
        Parameters(input): Parameters<SearchKnowledgeInput>,
    ) -> Result<Json<SearchNotesResult>, ErrorData> {
        if input.query.trim().is_empty() || !(1..=100).contains(&input.limit) {
            return Err(ErrorData::invalid_params(
                "query must be non-empty and limit must be between 1 and 100",
                None,
            ));
        }
        let result = self
            .client
            .lock()
            .await
            .call(
                method::SEARCH_NOTES,
                SearchNotesParams {
                    query: input.query,
                    limit: input.limit,
                },
            )
            .await
            .map_err(|error| to_mcp_error(&error))?;
        Ok(Json(result))
    }

    /// Check the local companion and authenticated read-only session health.
    #[tool(
        name = "grimmore_health",
        annotations(title = "Check Grimmore health", read_only_hint = true)
    )]
    async fn health(&self) -> Result<Json<HealthResult>, ErrorData> {
        let result = self
            .client
            .lock()
            .await
            .call(method::HEALTH, serde_json::json!({}))
            .await
            .map_err(|error| to_mcp_error(&error))?;
        Ok(Json(result))
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for GrimmoreMcp {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(
                Implementation::new("grimmore", env!("CARGO_PKG_VERSION"))
                    .with_title("Grimmore local knowledge"),
            )
            .with_instructions(
                "Read-only, local, vault-scoped knowledge access. This server cannot modify vault files.",
            )
    }
}

pub async fn serve_stdio(config: super::McpStdioConfig) -> anyhow::Result<()> {
    let secret = RootSecret::load_or_create()?;
    let server = GrimmoreMcp::connect_with_secret(
        &config.endpoint,
        &secret,
        config.vault_id,
        config.grant_id,
        config.scope_id,
    )
    .await?;
    let service = server.serve(rmcp::transport::stdio()).await?;
    service.waiting().await?;
    Ok(())
}

fn to_mcp_error(error: &McpBridgeError) -> ErrorData {
    tracing::debug!(%error, "read-only MCP query failed");
    ErrorData::internal_error("local companion query failed", None)
}

fn unix_time_millis() -> Result<u64, McpBridgeError> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| McpBridgeError::Clock)?;
    u64::try_from(elapsed.as_millis()).map_err(|_| McpBridgeError::Clock)
}
