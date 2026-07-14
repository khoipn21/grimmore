//! Versioned wire contracts shared by the daemon, launcher, and plugin.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The only protocol version understood by this vertical slice.
pub const PROTOCOL_VERSION: u16 = 1;

/// Hard upper bound for one framed JSON-RPC message.
pub const MAX_FRAME_BYTES: usize = 4 * 1024 * 1024;

/// Methods exposed by the companion's private JSON-RPC endpoint.
pub mod method {
    pub const CANCEL: &str = "system.cancel";
    pub const HEALTH: &str = "system.health";
    pub const SEARCH_NOTES: &str = "knowledge.search";
    pub const PROPOSE_NOTE_REPLACEMENT: &str = "knowledge.proposeNoteReplacement";
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum SessionRole {
    Plugin,
    McpReadonly,
}

impl SessionRole {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Plugin => "plugin",
            Self::McpReadonly => "mcp-readonly",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClientHello {
    pub protocol_version: u16,
    pub client_version: String,
    pub role: SessionRole,
    pub client_nonce: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ServerChallenge {
    pub protocol_version: u16,
    pub server_version: String,
    pub session_id: String,
    pub server_nonce: String,
    pub server_proof: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClientAuthenticate {
    pub session_id: String,
    pub client_proof: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionReady {
    pub session_id: String,
    pub role: SessionRole,
    pub expires_at_unix_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum ClientHandshakeMessage {
    Hello(ClientHello),
    Authenticate(ClientAuthenticate),
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum ServerHandshakeMessage {
    Challenge(ServerChallenge),
    Ready(SessionReady),
    Rejected { message: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    pub id: u64,
    pub method: String,
    pub params: Value,
    pub deadline_unix_ms: u64,
    pub vault_id: String,
    pub grant_id: String,
    pub scope_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SearchNotesParams {
    pub query: String,
    pub limit: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SearchHit {
    pub path: String,
    pub title: String,
    pub snippet: String,
    pub revision: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SearchNotesResult {
    pub hits: Vec<SearchHit>,
    pub indexed_revision: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HealthResult {
    pub status: String,
    pub product_version: String,
    pub protocol_version: u16,
    pub role: SessionRole,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CancelRequestParams {
    pub request_id: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CancelRequestResult {
    pub cancelled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProposeNoteReplacementParams {
    pub path: String,
    pub expected_revision: String,
    pub replacement: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PatchProposal {
    pub path: String,
    pub expected_revision: String,
    pub proposed_revision: String,
    pub replacement: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct JsonRpcSuccess {
    pub jsonrpc: String,
    pub id: u64,
    pub result: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct JsonRpcErrorBody {
    pub code: i32,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct JsonRpcFailure {
    pub jsonrpc: String,
    pub id: Option<u64>,
    pub error: JsonRpcErrorBody,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(untagged)]
pub enum JsonRpcResponse {
    Success(JsonRpcSuccess),
    Failure(JsonRpcFailure),
}

/// Schema aggregation point used to generate the committed TypeScript contract.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WireContract {
    pub client_hello: ClientHello,
    pub server_challenge: ServerChallenge,
    pub client_authenticate: ClientAuthenticate,
    pub session_ready: SessionReady,
    pub client_handshake_message: ClientHandshakeMessage,
    pub server_handshake_message: ServerHandshakeMessage,
    pub request: JsonRpcRequest,
    pub success: JsonRpcSuccess,
    pub failure: JsonRpcFailure,
    pub response: JsonRpcResponse,
    pub search_notes_params: SearchNotesParams,
    pub search_notes_result: SearchNotesResult,
    pub health_result: HealthResult,
    pub cancel_request_params: CancelRequestParams,
    pub cancel_request_result: CancelRequestResult,
    pub propose_note_replacement_params: ProposeNoteReplacementParams,
    pub patch_proposal: PatchProposal,
}

#[cfg(test)]
mod tests {
    use super::{ClientHello, PROTOCOL_VERSION, SessionRole};

    #[test]
    fn hello_uses_stable_camel_case_wire_names() {
        let hello = ClientHello {
            protocol_version: PROTOCOL_VERSION,
            client_version: "0.1.0".to_owned(),
            role: SessionRole::McpReadonly,
            client_nonce: "nonce".to_owned(),
        };

        let value = serde_json::to_value(hello).expect("serialize client hello");
        assert_eq!(value["protocolVersion"], PROTOCOL_VERSION);
        assert_eq!(value["role"], "mcp-readonly");
        assert!(value.get("protocol_version").is_none());
    }
}
