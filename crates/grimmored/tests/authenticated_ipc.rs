#![cfg(any(unix, windows))]

use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

#[cfg(windows)]
use std::sync::atomic::{AtomicUsize, Ordering};

use grimmore_core::{
    credentials::RootSecret,
    endpoint::connect_authenticated,
    framing::{read_json, write_json},
    protocol::{JsonRpcRequest, JsonRpcResponse, SessionRole, method},
    revision::content_revision,
};
use grimmored::{
    ipc::{IpcServerConfig, serve_with_shutdown},
    mcp::GrimmoreMcp,
    storage::Storage,
    vault_index::index_vault,
};
use rmcp::{ServiceExt, model::CallToolRequestParams};
use serde_json::json;
use tempfile::TempDir;
use tokio::{sync::oneshot, time::sleep};

fn reference_vault() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/vaults/reference-vault")
}

#[cfg(unix)]
fn private_endpoint(workspace: &TempDir) -> PathBuf {
    workspace.path().join("runtime/grimmore.sock")
}

#[cfg(windows)]
fn private_endpoint(_workspace: &TempDir) -> PathBuf {
    static NEXT_PIPE: AtomicUsize = AtomicUsize::new(1);

    PathBuf::from(format!(
        r"\\.\pipe\grimmore-authenticated-ipc-{}-{}",
        std::process::id(),
        NEXT_PIPE.fetch_add(1, Ordering::Relaxed)
    ))
}

fn deadline() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("current time follows the Unix epoch");
    u64::try_from(now.as_millis()).expect("current time fits u64") + 5_000
}

fn request(id: u64, method: &str, params: serde_json::Value) -> JsonRpcRequest {
    JsonRpcRequest {
        jsonrpc: "2.0".to_owned(),
        id,
        method: method.to_owned(),
        params,
        deadline_unix_ms: deadline(),
        vault_id: "reference".to_owned(),
        grant_id: "local".to_owned(),
        scope_id: "vault".to_owned(),
    }
}

async fn verify_plugin_session(endpoint: &Path, project_note: &Path, original_project: &str) {
    let plugin_secret = RootSecret::from_bytes([13; 32]);
    let mut plugin =
        connect_authenticated(endpoint, &plugin_secret, SessionRole::Plugin, "plugin-test")
            .await
            .expect("authenticate plugin role");
    assert_eq!(plugin.session.ready.role, SessionRole::Plugin);
    write_json(&mut plugin.stream, &request(1, method::HEALTH, json!({})))
        .await
        .expect("send health request");
    assert!(matches!(
        read_json::<_, JsonRpcResponse>(&mut plugin.stream)
            .await
            .expect("read health response"),
        JsonRpcResponse::Success(_)
    ));

    let replacement = format!("{original_project}\nReviewed through authenticated IPC.\n");
    write_json(
        &mut plugin.stream,
        &request(
            2,
            method::PROPOSE_NOTE_REPLACEMENT,
            json!({
                "path": "projects/grimmore.md",
                "expectedRevision": content_revision(original_project),
                "replacement": replacement
            }),
        ),
    )
    .await
    .expect("send proposal request");
    match read_json::<_, JsonRpcResponse>(&mut plugin.stream)
        .await
        .expect("read proposal response")
    {
        JsonRpcResponse::Success(success) => {
            assert_eq!(success.result["path"], "projects/grimmore.md");
            assert_eq!(success.result["replacement"], replacement);
        }
        JsonRpcResponse::Failure(failure) => {
            panic!("valid plugin proposal failed: {}", failure.error.message);
        }
    }
    assert_eq!(
        fs::read_to_string(project_note).expect("reread source fixture note"),
        original_project,
        "the companion proposed but did not write Markdown"
    );
}

async fn verify_read_only_mcp_session(endpoint: &Path) {
    let mcp_secret = RootSecret::from_bytes([13; 32]);
    let mut mcp =
        connect_authenticated(endpoint, &mcp_secret, SessionRole::McpReadonly, "mcp-test")
            .await
            .expect("authenticate MCP role");
    write_json(
        &mut mcp.stream,
        &request(
            3,
            method::PROPOSE_NOTE_REPLACEMENT,
            json!({
                "path": "projects/grimmore.md",
                "expectedRevision": "sha256:unused",
                "replacement": "blocked"
            }),
        ),
    )
    .await
    .expect("send forbidden proposal request");
    match read_json::<_, JsonRpcResponse>(&mut mcp.stream)
        .await
        .expect("read role-enforcement response")
    {
        JsonRpcResponse::Failure(failure) => assert_eq!(failure.error.code, -32003),
        JsonRpcResponse::Success(_) => panic!("read-only MCP role accepted a write proposal"),
    }
}

async fn verify_read_only_mcp_tools(endpoint: &Path) {
    let mcp_secret = RootSecret::from_bytes([13; 32]);
    let mcp_server = GrimmoreMcp::connect_with_secret(
        endpoint,
        &mcp_secret,
        "reference".to_owned(),
        "local".to_owned(),
        "vault".to_owned(),
    )
    .await
    .expect("connect MCP bridge to authenticated daemon");
    let (server_transport, client_transport) = tokio::io::duplex(32 * 1024);
    let mcp_task = tokio::spawn(async move {
        mcp_server
            .serve(server_transport)
            .await
            .expect("start in-process MCP server")
            .waiting()
            .await
            .expect("stop in-process MCP server cleanly");
    });
    let client = ().serve(client_transport).await.expect("start MCP client");

    let tools = client
        .peer()
        .list_tools(None)
        .await
        .expect("list read-only MCP tools")
        .tools;
    let mut names = tools
        .iter()
        .map(|tool| tool.name.as_ref())
        .collect::<Vec<_>>();
    names.sort_unstable();
    assert_eq!(names, vec!["grimmore_health", "grimmore_search_knowledge"]);
    assert!(tools.iter().all(|tool| {
        tool.annotations
            .as_ref()
            .and_then(|annotations| annotations.read_only_hint)
            == Some(true)
    }));

    let health = client
        .call_tool(CallToolRequestParams::new("grimmore_health"))
        .await
        .expect("call MCP health tool");
    assert_eq!(
        health
            .structured_content
            .as_ref()
            .expect("health has structured output")["role"],
        "mcp-readonly"
    );

    let search = client
        .call_tool(
            CallToolRequestParams::new("grimmore_search_knowledge").with_arguments(
                json!({"query": "context engineering", "limit": 5})
                    .as_object()
                    .expect("search parameters are an object")
                    .clone(),
            ),
        )
        .await
        .expect("call MCP search tool");
    let search_result = search
        .structured_content
        .expect("search has structured output");
    assert_eq!(
        search_result["hits"][0]["path"],
        "knowledge/ai/context-engineering.md"
    );

    client.cancel().await.expect("close MCP client");
    mcp_task.await.expect("join MCP server task");
}

async fn wait_for_authenticated_server(endpoint: &Path) {
    let secret = RootSecret::from_bytes([13; 32]);
    for _ in 0..100 {
        if connect_authenticated(endpoint, &secret, SessionRole::Plugin, "readiness-test")
            .await
            .is_ok()
        {
            return;
        }
        sleep(Duration::from_millis(10)).await;
    }
    panic!("server did not accept a verified local IPC connection");
}

#[tokio::test]
async fn private_ipc_authenticates_roles_and_enforces_read_only_mcp() {
    let workspace = TempDir::new().expect("create isolated IPC workspace");
    let endpoint = private_endpoint(&workspace);
    let project_note = reference_vault().join("projects/grimmore.md");
    let original_project = fs::read_to_string(&project_note).expect("read source fixture note");
    let storage = Arc::new(
        Storage::open(workspace.path().join("operational.sqlite3"))
            .expect("open bundled SQLite database"),
    );
    index_vault(&storage, "reference", reference_vault()).expect("index reference vault");
    let server_secret = Arc::new(RootSecret::from_bytes([13; 32]));
    let (shutdown_sender, shutdown_receiver) = oneshot::channel();
    let server = tokio::spawn(serve_with_shutdown(
        IpcServerConfig {
            endpoint: endpoint.clone(),
            storage,
            secret: server_secret,
            vault_id: "reference".to_owned(),
            grant_id: "local".to_owned(),
            scope_id: "vault".to_owned(),
        },
        async {
            let _ = shutdown_receiver.await;
        },
    ));

    wait_for_authenticated_server(&endpoint).await;

    verify_plugin_session(&endpoint, &project_note, &original_project).await;
    verify_read_only_mcp_session(&endpoint).await;
    verify_read_only_mcp_tools(&endpoint).await;
    shutdown_sender.send(()).expect("request server shutdown");
    server
        .await
        .expect("join IPC server task")
        .expect("shut down IPC server cleanly");
    #[cfg(unix)]
    assert!(!endpoint.exists(), "server removed only its owned socket");
}
