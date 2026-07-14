use std::{
    collections::HashMap,
    io::ErrorKind,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use grimmore_core::{
    framing::{FrameError, read_json, write_json},
    protocol::{
        CancelRequestParams, CancelRequestResult, HealthResult, JsonRpcErrorBody, JsonRpcFailure,
        JsonRpcRequest, JsonRpcResponse, JsonRpcSuccess, PROTOCOL_VERSION,
        ProposeNoteReplacementParams, SearchNotesParams, SessionRole, method,
    },
    session::authenticate_server,
};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    sync::Mutex,
};
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use super::{IpcServerConfig, role_can_write_proposals};
use crate::vault_index::{VaultIndexError, propose_note_replacement, search_notes};

const MAX_REQUEST_WINDOW_MS: u64 = 30_000;
const MAX_INFLIGHT_REQUESTS: usize = 32;
const STALE_NOTE_INDEX_ERROR_CODE: i32 = -32007;

pub async fn handle_connection<S>(
    mut stream: S,
    config: IpcServerConfig,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let session =
        authenticate_server(&mut stream, &config.secret, env!("CARGO_PKG_VERSION")).await?;
    let role = session.ready.role;
    let expires_at = session.ready.expires_at_unix_ms;
    let (mut reader, writer) = tokio::io::split(stream);
    let writer = Arc::new(Mutex::new(writer));
    let inflight = Arc::new(Mutex::new(HashMap::<u64, CancellationToken>::new()));
    loop {
        let request = match read_json::<_, JsonRpcRequest>(&mut reader).await {
            Ok(request) => request,
            Err(FrameError::Io(error)) if error.kind() == ErrorKind::UnexpectedEof => {
                cancel_all(&inflight).await;
                return Ok(());
            }
            Err(error) => return Err(Box::new(error)),
        };
        if unix_time_millis() >= expires_at {
            write_response(
                &writer,
                &failure(Some(request.id), -32005, "session expired"),
            )
            .await?;
            cancel_all(&inflight).await;
            return Ok(());
        }
        if let Some(response) = validate_envelope(&config, &request) {
            write_response(&writer, &response).await?;
            continue;
        }
        if request.method == method::CANCEL {
            let response = cancel_request(&inflight, request).await;
            write_response(&writer, &response).await?;
            continue;
        }

        let token = CancellationToken::new();
        {
            let mut requests = inflight.lock().await;
            if requests.contains_key(&request.id) {
                drop(requests);
                write_response(
                    &writer,
                    &failure(Some(request.id), -32600, "duplicate request id"),
                )
                .await?;
                continue;
            }
            if requests.len() >= MAX_INFLIGHT_REQUESTS {
                drop(requests);
                write_response(
                    &writer,
                    &failure(Some(request.id), -32006, "too many in-flight requests"),
                )
                .await?;
                continue;
            }
            requests.insert(request.id, token.clone());
        }

        let task_config = config.clone();
        let task_writer = Arc::clone(&writer);
        let task_inflight = Arc::clone(&inflight);
        tokio::spawn(async move {
            let request_id = request.id;
            let dispatched =
                tokio::task::spawn_blocking(move || dispatch(&task_config, role, request))
                    .await
                    .unwrap_or_else(|error| {
                        warn!(%error, request_id, "IPC request worker failed");
                        failure(Some(request_id), -32603, "internal request failure")
                    });
            if !token.is_cancelled()
                && let Err(error) = write_response(&task_writer, &dispatched).await
            {
                debug!(%error, request_id, "failed to write IPC response");
            }
            task_inflight.lock().await.remove(&request_id);
        });
    }
}

async fn write_response<S>(
    writer: &Mutex<tokio::io::WriteHalf<S>>,
    response: &JsonRpcResponse,
) -> Result<(), FrameError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    write_json(&mut *writer.lock().await, response).await
}

async fn cancel_request(
    inflight: &Mutex<HashMap<u64, CancellationToken>>,
    request: JsonRpcRequest,
) -> JsonRpcResponse {
    let request_id = request.id;
    let Ok(params) = serde_json::from_value::<CancelRequestParams>(request.params) else {
        return failure(Some(request_id), -32602, "request parameters are invalid");
    };
    let token = inflight.lock().await.remove(&params.request_id);
    let cancelled = token.is_some();
    if let Some(token) = token {
        token.cancel();
    }
    let result = serde_json::to_value(CancelRequestResult { cancelled })
        .expect("cancellation result is always serializable");
    JsonRpcResponse::Success(JsonRpcSuccess {
        jsonrpc: "2.0".to_owned(),
        id: request_id,
        result,
    })
}

async fn cancel_all(inflight: &Mutex<HashMap<u64, CancellationToken>>) {
    let tokens = inflight
        .lock()
        .await
        .drain()
        .map(|(_, token)| token)
        .collect::<Vec<_>>();
    for token in tokens {
        token.cancel();
    }
}

fn validate_envelope(
    config: &IpcServerConfig,
    request: &JsonRpcRequest,
) -> Option<JsonRpcResponse> {
    if request.jsonrpc != "2.0" {
        return Some(failure(
            Some(request.id),
            -32600,
            "invalid JSON-RPC version",
        ));
    }
    if request.vault_id != config.vault_id
        || request.grant_id != config.grant_id
        || request.scope_id != config.scope_id
    {
        return Some(failure(
            Some(request.id),
            -32001,
            "request scope is not authorized",
        ));
    }
    let now = unix_time_millis();
    if request.deadline_unix_ms <= now
        || request.deadline_unix_ms.saturating_sub(now) > MAX_REQUEST_WINDOW_MS
    {
        return Some(failure(
            Some(request.id),
            -32004,
            "request deadline is invalid",
        ));
    }
    None
}

fn dispatch(
    config: &IpcServerConfig,
    role: SessionRole,
    request: JsonRpcRequest,
) -> JsonRpcResponse {
    if request.deadline_unix_ms <= unix_time_millis() {
        return failure(Some(request.id), -32004, "request deadline expired");
    }
    let result = match request.method.as_str() {
        method::HEALTH => serde_json::to_value(HealthResult {
            status: "ok".to_owned(),
            product_version: env!("CARGO_PKG_VERSION").to_owned(),
            protocol_version: PROTOCOL_VERSION,
            role,
        })
        .map_err(|error| error.to_string()),
        method::SEARCH_NOTES => serde_json::from_value::<SearchNotesParams>(request.params)
            .map_err(|error| error.to_string())
            .and_then(|params| {
                search_notes(
                    &config.storage,
                    &config.vault_id,
                    &params.query,
                    params.limit,
                )
                .map_err(|error| error.to_string())
                .and_then(|result| serde_json::to_value(result).map_err(|error| error.to_string()))
            }),
        method::PROPOSE_NOTE_REPLACEMENT if role_can_write_proposals(role) => {
            match serde_json::from_value::<ProposeNoteReplacementParams>(request.params) {
                Ok(params) => {
                    match propose_note_replacement(&config.storage, &config.vault_id, params) {
                        Ok(result) => {
                            serde_json::to_value(result).map_err(|error| error.to_string())
                        }
                        Err(VaultIndexError::StaleRevision) => {
                            return failure(
                                Some(request.id),
                                STALE_NOTE_INDEX_ERROR_CODE,
                                "the companion note index is stale",
                            );
                        }
                        Err(error) => Err(error.to_string()),
                    }
                }
                Err(error) => Err(error.to_string()),
            }
        }
        method::PROPOSE_NOTE_REPLACEMENT => {
            return failure(
                Some(request.id),
                -32003,
                "method is not allowed for this role",
            );
        }
        _ => return failure(Some(request.id), -32601, "method not found"),
    };

    match result {
        Ok(result) => JsonRpcResponse::Success(JsonRpcSuccess {
            jsonrpc: "2.0".to_owned(),
            id: request.id,
            result,
        }),
        Err(error) => {
            debug!(%error, method = %request.method, "IPC request failed validation or execution");
            failure(Some(request.id), -32602, "request parameters are invalid")
        }
    }
}

fn failure(id: Option<u64>, code: i32, message: &str) -> JsonRpcResponse {
    JsonRpcResponse::Failure(JsonRpcFailure {
        jsonrpc: "2.0".to_owned(),
        id,
        error: JsonRpcErrorBody {
            code,
            message: message.to_owned(),
        },
    })
}

fn unix_time_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| u64::try_from(duration.as_millis()).ok())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use grimmore_core::protocol::{JsonRpcRequest, JsonRpcResponse};
    use serde_json::json;
    use tokio::sync::Mutex;
    use tokio_util::sync::CancellationToken;

    use super::cancel_request;

    #[tokio::test]
    async fn cancellation_removes_and_signals_an_inflight_request() {
        let token = CancellationToken::new();
        let inflight = Mutex::new(HashMap::from([(41, token.clone())]));
        let response = cancel_request(
            &inflight,
            JsonRpcRequest {
                jsonrpc: "2.0".to_owned(),
                id: 42,
                method: "system.cancel".to_owned(),
                params: json!({ "requestId": 41 }),
                deadline_unix_ms: u64::MAX,
                vault_id: "reference".to_owned(),
                grant_id: "local".to_owned(),
                scope_id: "vault".to_owned(),
            },
        )
        .await;

        assert!(token.is_cancelled());
        assert!(inflight.lock().await.is_empty());
        match response {
            JsonRpcResponse::Success(success) => {
                assert_eq!(success.result, json!({ "cancelled": true }));
            }
            JsonRpcResponse::Failure(_) => panic!("valid cancellation request failed"),
        }
    }
}
