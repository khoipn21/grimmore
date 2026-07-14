#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

#[cfg(target_os = "linux")]
use std::collections::HashSet;
use std::{
    env, fs,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
#[cfg(target_os = "linux")]
use std::{
    io::{BufRead, BufReader, Write},
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc::{self, Receiver},
    thread::{self, JoinHandle},
    time::{SystemTime, UNIX_EPOCH},
};

#[cfg(target_os = "linux")]
use grimmore_core::{
    credentials::RootSecret,
    endpoint::connect_authenticated,
    framing::{read_json, write_json},
    protocol::{JsonRpcRequest, JsonRpcResponse, SearchNotesResult, SessionRole, method},
};
use grimmored::{
    storage::Storage,
    vault_index::{index_vault, reconcile_vault_paths, search_notes},
};
use serde::Serialize;
#[cfg(target_os = "linux")]
use serde_json::json;
use sha2::{Digest, Sha256};
use tempfile::{Builder, TempDir};
#[cfg(target_os = "linux")]
use tokio::time::{sleep, timeout};

const CANONICAL_NOTES: usize = 25_000;
const CANONICAL_CORPUS_BYTES: usize = 1024 * 1024 * 1024;
const CANONICAL_CORPUS_REVISION: &str =
    "sha256:0535eca2300205a35f1a59b4aa93127d13e56644d67b6372eda79779b28409b9";
const CANONICAL_QUERY_SAMPLES: usize = 200;
const CANONICAL_INCREMENTAL_SAMPLES: usize = 100;
const INCREMENTAL_NOTE_BYTES: usize = 50 * 1024;
const INITIAL_INDEX_BUDGET: Duration = Duration::from_secs(45);
const QUERY_P95_BUDGET: Duration = Duration::from_millis(50);
const INCREMENTAL_P95_BUDGET: Duration = Duration::from_millis(75);
const COLD_HANDSHAKE_SAMPLES: usize = 20;
const WARM_IPC_SAMPLES: usize = 200;
const WARM_IPC_WARMUP_SAMPLES: usize = 20;
const WARM_MCP_SAMPLES: usize = 200;
const WARM_MCP_WARMUP_SAMPLES: usize = 20;
const WATCHER_RECOVERY_SAMPLES: usize = 100;
const WATCHER_RECOVERY_NOTE_BYTES: usize = 50 * 1024;
const COLD_HANDSHAKE_BUDGET: Duration = Duration::from_millis(150);
const WARM_IPC_BUDGET: Duration = Duration::from_millis(5);
const WARM_MCP_BUDGET: Duration = Duration::from_millis(10);
const IDLE_RSS_BUDGET_BYTES: u64 = 35 * 1024 * 1024;
const IDLE_CPU_BUDGET_PERCENT: f64 = 0.5;
const STARTUP_TIMEOUT: Duration = Duration::from_secs(90);
const MCP_RESPONSE_TIMEOUT: Duration = Duration::from_secs(5);
const INDEX_READY_TIMEOUT: Duration = Duration::from_secs(90);
const WATCHER_RECOVERY_TIMEOUT: Duration = Duration::from_secs(10);
const IDLE_SETTLE_TIME: Duration = Duration::from_secs(1);
const IDLE_CPU_SAMPLE_TIME: Duration = Duration::from_secs(10);
#[cfg(target_os = "linux")]
const CANONICAL_MEMORY_MINIMUM_BYTES: u64 = 15 * 1024 * 1024 * 1024;
#[cfg(target_os = "linux")]
const CANONICAL_MEMORY_MAXIMUM_BYTES: u64 = 17 * 1024 * 1024 * 1024;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PerformanceReport {
    platform: &'static str,
    architecture: &'static str,
    logical_cpus: usize,
    canonical_host: CanonicalHostFacts,
    note_count: usize,
    corpus_bytes: usize,
    corpus_revision: String,
    canonical_corpus: bool,
    initial_index_millis: u128,
    query_samples: usize,
    rare_query_p95_micros: u128,
    broad_query_p95_micros: u128,
    incremental_samples: usize,
    incremental_p95_micros: u128,
    companion_runtime: CompanionRuntimeReport,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CompanionRuntimeReport {
    cold_handshake: LatencyDistribution,
    warm_ipc_request_bytes: usize,
    warm_ipc_warmup_samples: usize,
    warm_ipc: LatencyDistribution,
    warm_mcp_warmup_samples: usize,
    warm_mcp: LatencyDistribution,
    watcher_recovery: LatencyDistribution,
    idle_rss_bytes: u64,
    idle_cpu_sample_millis: u128,
    idle_cpu_percent: f64,
    retry_policy: &'static str,
    plugin_synchronous_load: &'static str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct LatencyDistribution {
    sample_count: usize,
    minimum_micros: u128,
    p50_micros: u128,
    p95_micros: u128,
    maximum_micros: u128,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CanonicalHostFacts {
    os_id: String,
    os_version: String,
    total_memory_bytes: u64,
    workspace: PathBuf,
    workspace_device: String,
    nvme_backed: bool,
    power_mode: String,
}

struct Corpus {
    workspace: TempDir,
    root: PathBuf,
    storage: Storage,
    revision: String,
}

impl Corpus {
    fn build(note_count: usize, corpus_bytes: usize) -> Self {
        assert!(note_count > 0, "benchmark corpus needs at least one note");
        let workspace = performance_workspace();
        let root = workspace.path().join("vault");
        let revision = write_corpus(&root, note_count, corpus_bytes);
        let storage = Storage::open(workspace.path().join("operational.sqlite3"))
            .expect("open performance SQLite database");
        Self {
            workspace,
            root,
            storage,
            revision,
        }
    }
}

fn performance_workspace() -> TempDir {
    let mut builder = Builder::new();
    builder.prefix("grimmore-performance-");
    match env::var_os("GRIMMORE_BENCH_WORKDIR") {
        Some(parent) => {
            let parent = PathBuf::from(parent);
            let parent = if parent.is_absolute() {
                parent
            } else {
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../..")
                    .join(parent)
            };
            fs::create_dir_all(&parent).expect("create configured performance workspace parent");
            let parent = fs::canonicalize(parent)
                .expect("canonicalize configured performance workspace parent");
            builder
                .tempdir_in(parent)
                .expect("create configured performance workspace")
        }
        None => builder.tempdir().expect("create performance workspace"),
    }
}

#[cfg(target_os = "linux")]
fn canonical_host_facts() -> CanonicalHostFacts {
    use std::os::unix::fs::MetadataExt as _;

    assert_eq!(
        std::env::consts::ARCH,
        "x86_64",
        "canonical host must be x64"
    );
    let release = fs::read_to_string("/etc/os-release").expect("read operating-system release");
    let (os_id, os_version) = parse_os_release(&release);
    assert_eq!(os_id, "ubuntu", "canonical host must run Ubuntu");
    assert_eq!(
        os_version, "24.04",
        "canonical host must run the frozen Ubuntu 24.04 baseline"
    );
    let logical_cpus = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
    assert_eq!(
        logical_cpus, 4,
        "canonical host must expose exactly four logical CPUs to the benchmark"
    );
    let total_memory_bytes = linux_total_memory_bytes();
    assert!(
        (CANONICAL_MEMORY_MINIMUM_BYTES..=CANONICAL_MEMORY_MAXIMUM_BYTES)
            .contains(&total_memory_bytes),
        "canonical host must provide approximately 16 GiB of memory"
    );
    let workspace = canonical_workspace_parent();
    let device = fs::metadata(&workspace)
        .expect("inspect canonical benchmark workspace")
        .dev();
    let workspace_device = linux_device_name(device);
    let nvme_backed = linux_device_is_nvme(device);
    assert!(
        nvme_backed,
        "canonical benchmark workspace must be backed by an NVMe block device"
    );
    let power_mode = env::var("GRIMMORE_BENCH_POWER_MODE")
        .expect("GRIMMORE_BENCH_POWER_MODE must describe the measured power mode");
    assert!(
        !power_mode.trim().is_empty() && power_mode.len() <= 128,
        "GRIMMORE_BENCH_POWER_MODE must be a short non-empty description"
    );

    CanonicalHostFacts {
        os_id,
        os_version,
        total_memory_bytes,
        workspace,
        workspace_device,
        nvme_backed,
        power_mode,
    }
}

#[cfg(not(target_os = "linux"))]
fn canonical_host_facts() -> CanonicalHostFacts {
    panic!("the absolute Phase 1 performance gate runs only on Ubuntu 24.04 x64")
}

#[cfg(target_os = "linux")]
fn canonical_workspace_parent() -> PathBuf {
    let workspace = env::var_os("GRIMMORE_BENCH_WORKDIR")
        .map(PathBuf::from)
        .expect("GRIMMORE_BENCH_WORKDIR must name an NVMe-backed workspace directory");
    assert!(
        workspace.is_absolute(),
        "GRIMMORE_BENCH_WORKDIR must be an absolute path"
    );
    fs::create_dir_all(&workspace).expect("create canonical benchmark workspace parent");
    fs::canonicalize(workspace).expect("canonicalize canonical benchmark workspace parent")
}

#[cfg(target_os = "linux")]
fn parse_os_release(contents: &str) -> (String, String) {
    let mut os_id = None;
    let mut os_version = None;
    for line in contents.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim().trim_matches('"').to_owned();
        match key {
            "ID" => os_id = Some(value),
            "VERSION_ID" => os_version = Some(value),
            _ => {}
        }
    }
    (
        os_id.expect("/etc/os-release must define ID"),
        os_version.expect("/etc/os-release must define VERSION_ID"),
    )
}

#[cfg(target_os = "linux")]
fn linux_total_memory_bytes() -> u64 {
    let meminfo = fs::read_to_string("/proc/meminfo").expect("read Linux memory information");
    let kibibytes = meminfo
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            (name == "MemTotal").then_some(value)
        })
        .and_then(|value| value.split_whitespace().next())
        .expect("/proc/meminfo must define MemTotal")
        .parse::<u64>()
        .expect("MemTotal must be an integer number of KiB");
    kibibytes
        .checked_mul(1024)
        .expect("MemTotal byte conversion must fit in u64")
}

#[cfg(target_os = "linux")]
fn linux_device_name(device: u64) -> String {
    format!(
        "{}:{}",
        linux_device_major(device),
        linux_device_minor(device)
    )
}

#[cfg(target_os = "linux")]
fn linux_device_major(device: u64) -> u64 {
    ((device >> 8) & 0x0fff) | ((device >> 32) & 0xffff_f000)
}

#[cfg(target_os = "linux")]
fn linux_device_minor(device: u64) -> u64 {
    (device & 0x00ff) | ((device >> 12) & 0xffff_ff00)
}

#[cfg(target_os = "linux")]
fn linux_device_is_nvme(device: u64) -> bool {
    let device_path = Path::new("/sys/dev/block").join(linux_device_name(device));
    let resolved = fs::canonicalize(device_path).expect("resolve Linux workspace block device");
    linux_sysfs_device_is_nvme(&resolved, &mut HashSet::new())
}

#[cfg(target_os = "linux")]
fn linux_sysfs_device_is_nvme(device: &Path, seen: &mut HashSet<PathBuf>) -> bool {
    let device = fs::canonicalize(device).expect("resolve Linux block-device ancestry");
    if !seen.insert(device.clone()) {
        return false;
    }
    if device
        .file_name()
        .is_some_and(|name| name.to_string_lossy().starts_with("nvme"))
    {
        return true;
    }
    let slaves = device.join("slaves");
    let Ok(entries) = fs::read_dir(slaves) else {
        return false;
    };
    entries
        .filter_map(Result::ok)
        .any(|entry| linux_sysfs_device_is_nvme(&entry.path(), seen))
}

fn environment_usize(name: &str, default: usize) -> usize {
    env::var(name).map_or(default, |value| {
        let parsed = value
            .parse()
            .unwrap_or_else(|_| panic!("{name} must be a positive integer"));
        assert!(parsed > 0, "{name} must be a positive integer");
        parsed
    })
}

fn assert_canonical_corpus_shape(note_count: usize, corpus_bytes: usize) {
    assert_eq!(
        note_count, CANONICAL_NOTES,
        "canonical performance gate must index exactly 25,000 notes"
    );
    assert_eq!(
        corpus_bytes, CANONICAL_CORPUS_BYTES,
        "canonical performance gate must index exactly 1 GiB of Markdown"
    );
}

fn assert_canonical_sample_shape(query_samples: usize, incremental_samples: usize) {
    assert_eq!(
        query_samples, CANONICAL_QUERY_SAMPLES,
        "canonical performance gate must use exactly 200 query samples"
    );
    assert_eq!(
        incremental_samples, CANONICAL_INCREMENTAL_SAMPLES,
        "canonical performance gate must use exactly 100 incremental samples"
    );
}

fn write_corpus(root: &Path, note_count: usize, corpus_bytes: usize) -> String {
    let base_size = corpus_bytes / note_count;
    let remainder = corpus_bytes % note_count;
    let mut written = 0;
    let mut hasher = Sha256::new();
    for index in 0..note_count {
        let directory = root.join(format!("knowledge/{:03}", index / 250));
        fs::create_dir_all(&directory).expect("create performance corpus directory");
        let note_bytes = base_size + usize::from(index < remainder);
        let marker = (index == 0).then_some(" raregrimmoretoken ");
        let body = note_body(index, note_bytes, marker);
        written += body.len();
        let relative_path = format!("knowledge/{:03}/note-{index:05}.md", index / 250);
        hasher.update(relative_path.as_bytes());
        hasher.update([0]);
        hasher.update(body.as_bytes());
        fs::write(root.join(relative_path), body).expect("write deterministic performance note");
    }
    assert_eq!(written, corpus_bytes, "corpus byte contract drifted");
    format!("sha256:{}", hex::encode(hasher.finalize()))
}

fn note_body(index: usize, bytes: usize, marker: Option<&str>) -> String {
    let mut body = format!(
        "# Performance note {index}\n\n broadgrimmoretoken {}",
        marker.unwrap_or_default()
    );
    let filler = "local first knowledge evidence retrieval benchmark ";
    assert!(body.len() <= bytes, "performance note size is too small");
    while body.len() + filler.len() <= bytes {
        body.push_str(filler);
    }
    body.extend(std::iter::repeat_n('x', bytes - body.len()));
    body
}

fn p95(samples: &mut [Duration]) -> Duration {
    assert!(!samples.is_empty(), "performance sample set is empty");
    samples.sort_unstable();
    samples[(samples.len() * 95).div_ceil(100) - 1]
}

fn latency_distribution(samples: &mut [Duration]) -> LatencyDistribution {
    assert!(!samples.is_empty(), "latency sample set is empty");
    samples.sort_unstable();
    LatencyDistribution {
        sample_count: samples.len(),
        minimum_micros: samples[0].as_micros(),
        p50_micros: samples[(samples.len() * 50).div_ceil(100) - 1].as_micros(),
        p95_micros: samples[(samples.len() * 95).div_ceil(100) - 1].as_micros(),
        maximum_micros: samples[samples.len() - 1].as_micros(),
    }
}

#[cfg(target_os = "linux")]
struct RunningCompanion {
    process: Child,
    _workspace: TempDir,
    endpoint: PathBuf,
}

#[cfg(target_os = "linux")]
impl Drop for RunningCompanion {
    fn drop(&mut self) {
        if self.process.try_wait().ok().flatten().is_none() {
            let _ = self.process.kill();
        }
        let _ = self.process.wait();
    }
}

#[cfg(target_os = "linux")]
struct McpBridge {
    process: Child,
    input: Option<ChildStdin>,
    responses: Receiver<Result<serde_json::Value, String>>,
    reader: Option<JoinHandle<()>>,
}

#[cfg(target_os = "linux")]
impl McpBridge {
    fn start(endpoint: &Path) -> Self {
        let mut process = Command::new(env!("CARGO_BIN_EXE_grimmored"))
            .arg("mcp-stdio")
            .arg("--vault-id")
            .arg("performance")
            .arg("--grant-id")
            .arg("local")
            .arg("--scope-id")
            .arg("vault")
            .arg("--endpoint")
            .arg(endpoint)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("start companion MCP bridge");
        let input = process.stdin.take().expect("capture MCP bridge stdin");
        let output = process.stdout.take().expect("capture MCP bridge stdout");
        let (sender, responses) = mpsc::channel();
        let reader = thread::spawn(move || {
            let mut output = BufReader::new(output);
            loop {
                let mut line = String::new();
                match output.read_line(&mut line) {
                    Ok(0) => return,
                    Ok(_) => {
                        let response = serde_json::from_str(&line)
                            .map_err(|error| format!("decode MCP response: {error}"));
                        if sender.send(response).is_err() {
                            return;
                        }
                    }
                    Err(error) => {
                        let _ = sender.send(Err(format!("read MCP response: {error}")));
                        return;
                    }
                }
            }
        });

        Self {
            process,
            input: Some(input),
            responses,
            reader: Some(reader),
        }
    }

    fn write(&mut self, message: &serde_json::Value) {
        let input = self.input.as_mut().expect("MCP bridge input remains open");
        serde_json::to_writer(&mut *input, message).expect("serialize MCP request");
        input.write_all(b"\n").expect("terminate MCP request");
        input.flush().expect("flush MCP request");
    }

    fn request(
        &mut self,
        id: u64,
        method_name: &str,
        params: &serde_json::Value,
    ) -> serde_json::Value {
        self.write(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method_name,
            "params": params,
        }));
        let response = self
            .responses
            .recv_timeout(MCP_RESPONSE_TIMEOUT)
            .expect("MCP bridge returned a response before its timeout")
            .expect("MCP bridge returned valid JSON");
        assert_eq!(response["jsonrpc"], "2.0", "MCP response is JSON-RPC 2.0");
        assert_eq!(response["id"], id, "MCP response matches request id");
        assert!(
            response.get("error").is_none(),
            "MCP request {method_name} returned an error: {response}"
        );
        assert!(
            response.get("result").is_some(),
            "MCP request {method_name} returned no result: {response}"
        );
        response
    }

    fn close(mut self) {
        drop(self.input.take());
        let deadline = Instant::now() + MCP_RESPONSE_TIMEOUT;
        let status = loop {
            if let Some(status) = self.process.try_wait().expect("inspect MCP bridge exit") {
                break status;
            }
            if Instant::now() >= deadline {
                let _ = self.process.kill();
                let _ = self.process.wait();
                panic!("MCP bridge did not exit after its input closed");
            }
            thread::sleep(Duration::from_millis(10));
        };
        assert!(
            status.success(),
            "MCP bridge exited unsuccessfully: {status}"
        );
        if let Some(reader) = self.reader.take() {
            reader.join().expect("join MCP response reader");
        }
    }
}

#[cfg(target_os = "linux")]
impl Drop for McpBridge {
    fn drop(&mut self) {
        drop(self.input.take());
        if self.process.try_wait().ok().flatten().is_none() {
            let _ = self.process.kill();
        }
        let _ = self.process.wait();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

#[cfg(target_os = "linux")]
async fn start_companion(corpus: &Corpus, secret: &RootSecret) -> (RunningCompanion, Duration) {
    let workspace = Builder::new()
        .prefix("runtime-")
        .tempdir_in(corpus.workspace.path())
        .expect("create companion runtime workspace");
    let endpoint = workspace.path().join("r/s");
    fs::create_dir_all(endpoint.parent().expect("runtime endpoint parent"))
        .expect("create companion runtime endpoint parent");
    let database = workspace.path().join("runtime.sqlite3");
    let started = Instant::now();
    let process = Command::new(env!("CARGO_BIN_EXE_grimmored"))
        .arg("--database")
        .arg(database)
        .arg("serve")
        .arg("--vault-id")
        .arg("performance")
        .arg("--vault")
        .arg(&corpus.root)
        .arg("--grant-id")
        .arg("local")
        .arg("--scope-id")
        .arg("vault")
        .arg("--endpoint")
        .arg(&endpoint)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("start companion runtime");
    let mut companion = RunningCompanion {
        process,
        _workspace: workspace,
        endpoint,
    };
    loop {
        let connection = timeout(
            Duration::from_millis(250),
            connect_authenticated(
                &companion.endpoint,
                secret,
                SessionRole::Plugin,
                "phase-1-performance-gate",
            ),
        )
        .await;
        if matches!(connection, Ok(Ok(_))) {
            return (companion, started.elapsed());
        }
        if let Some(status) = companion
            .process
            .try_wait()
            .expect("inspect companion exit")
        {
            panic!("companion exited before its authenticated handshake: {status}");
        }
        assert!(
            started.elapsed() < STARTUP_TIMEOUT,
            "companion did not complete an authenticated handshake within {STARTUP_TIMEOUT:?}"
        );
        sleep(Duration::from_millis(20)).await;
    }
}

#[cfg(target_os = "linux")]
fn deadline_unix_ms() -> u64 {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("current time follows the Unix epoch");
    u64::try_from(elapsed.as_millis()).expect("current time fits milliseconds") + 5_000
}

#[cfg(target_os = "linux")]
fn rpc_request(id: u64, method_name: &str, params: serde_json::Value) -> JsonRpcRequest {
    JsonRpcRequest {
        jsonrpc: "2.0".to_owned(),
        id,
        method: method_name.to_owned(),
        params,
        deadline_unix_ms: deadline_unix_ms(),
        vault_id: "performance".to_owned(),
        grant_id: "local".to_owned(),
        scope_id: "vault".to_owned(),
    }
}

#[cfg(target_os = "linux")]
fn one_kib_health_request(id: u64) -> JsonRpcRequest {
    let mut request = rpc_request(id, method::HEALTH, json!({ "padding": "" }));
    let fixed_size = serde_json::to_vec(&request)
        .expect("serialize 1 KiB health request")
        .len();
    assert!(
        fixed_size <= 1024,
        "1 KiB health request header is too large"
    );
    request.params = json!({ "padding": "x".repeat(1024 - fixed_size) });
    assert_eq!(
        serde_json::to_vec(&request)
            .expect("serialize padded 1 KiB health request")
            .len(),
        1024,
        "warm IPC samples must carry exactly a 1 KiB JSON request"
    );
    request
}

#[cfg(target_os = "linux")]
async fn invoke_ipc(
    stream: &mut grimmore_core::endpoint::LocalIpcStream,
    request: JsonRpcRequest,
) -> serde_json::Value {
    let request_id = request.id;
    write_json(stream, &request)
        .await
        .expect("write authenticated IPC request");
    match read_json::<_, JsonRpcResponse>(stream)
        .await
        .expect("read authenticated IPC response")
    {
        JsonRpcResponse::Success(response) => {
            assert_eq!(response.id, request_id, "IPC response matches request id");
            response.result
        }
        JsonRpcResponse::Failure(response) => {
            panic!(
                "IPC request {request_id} failed: {}",
                response.error.message
            );
        }
    }
}

#[cfg(target_os = "linux")]
async fn wait_for_indexed_vault(endpoint: &Path, secret: &RootSecret) {
    let mut connection = connect_authenticated(
        endpoint,
        secret,
        SessionRole::Plugin,
        "phase-1-performance-gate",
    )
    .await
    .expect("open indexed-vault readiness session");
    let started = Instant::now();
    let mut request_id = 50_000;
    loop {
        let request = rpc_request(
            request_id,
            method::SEARCH_NOTES,
            json!({ "query": "raregrimmoretoken", "limit": 1 }),
        );
        write_json(&mut connection.stream, &request)
            .await
            .expect("write indexed-vault readiness request");
        match read_json::<_, JsonRpcResponse>(&mut connection.stream)
            .await
            .expect("read indexed-vault readiness response")
        {
            JsonRpcResponse::Success(response) => {
                assert_eq!(
                    response.id, request_id,
                    "readiness response matches request id"
                );
                let result: SearchNotesResult = serde_json::from_value(response.result)
                    .expect("decode readiness search result");
                if result.hits.len() == 1 && result.hits[0].path == "knowledge/000/note-00000.md" {
                    return;
                }
            }
            JsonRpcResponse::Failure(response) => {
                assert_eq!(
                    response.id,
                    Some(request_id),
                    "readiness error matches request id"
                );
            }
        }
        request_id += 1;
        assert!(
            started.elapsed() < INDEX_READY_TIMEOUT,
            "initial vault index did not become queryable within {INDEX_READY_TIMEOUT:?}"
        );
        sleep(Duration::from_millis(20)).await;
    }
}

#[cfg(target_os = "linux")]
async fn measure_warm_ipc(endpoint: &Path, secret: &RootSecret) -> Vec<Duration> {
    let mut connection = connect_authenticated(
        endpoint,
        secret,
        SessionRole::Plugin,
        "phase-1-performance-gate",
    )
    .await
    .expect("open warm authenticated plugin session");
    let mut request_id = 1;
    for _ in 0..WARM_IPC_WARMUP_SAMPLES {
        let result = invoke_ipc(&mut connection.stream, one_kib_health_request(request_id)).await;
        assert_eq!(
            result["status"], "ok",
            "warm IPC health response is healthy"
        );
        request_id += 1;
    }
    let mut samples = Vec::with_capacity(WARM_IPC_SAMPLES);
    for _ in 0..WARM_IPC_SAMPLES {
        let started = Instant::now();
        let result = invoke_ipc(&mut connection.stream, one_kib_health_request(request_id)).await;
        samples.push(started.elapsed());
        assert_eq!(
            result["status"], "ok",
            "warm IPC health response is healthy"
        );
        request_id += 1;
    }
    samples
}

#[cfg(target_os = "linux")]
async fn measure_watcher_recovery(
    endpoint: &Path,
    secret: &RootSecret,
    corpus: &Corpus,
) -> Vec<Duration> {
    let mut connection = connect_authenticated(
        endpoint,
        secret,
        SessionRole::Plugin,
        "phase-1-performance-gate",
    )
    .await
    .expect("open watcher measurement session");
    let note = corpus.root.join("watcher-runtime-performance.md");
    let mut request_id = 10_000;
    let mut samples = Vec::with_capacity(WATCHER_RECOVERY_SAMPLES);
    for iteration in 0..WATCHER_RECOVERY_SAMPLES {
        let marker = format!("watcherrecoverymarker{iteration:03}");
        fs::write(
            &note,
            note_body(
                iteration,
                WATCHER_RECOVERY_NOTE_BYTES,
                Some(&format!(" {marker} ")),
            ),
        )
        .expect("write watcher recovery note");
        let started = Instant::now();
        loop {
            let result = invoke_ipc(
                &mut connection.stream,
                rpc_request(
                    request_id,
                    method::SEARCH_NOTES,
                    json!({ "query": marker, "limit": 1 }),
                ),
            )
            .await;
            request_id += 1;
            let result: SearchNotesResult =
                serde_json::from_value(result).expect("decode watcher search result");
            if result
                .hits
                .iter()
                .any(|hit| hit.path == "watcher-runtime-performance.md")
            {
                samples.push(started.elapsed());
                break;
            }
            assert!(
                started.elapsed() < WATCHER_RECOVERY_TIMEOUT,
                "watcher did not reconcile sample {iteration} within {WATCHER_RECOVERY_TIMEOUT:?}"
            );
            sleep(Duration::from_millis(10)).await;
        }
    }
    samples
}

#[cfg(target_os = "linux")]
fn measure_warm_mcp(endpoint: &Path) -> Vec<Duration> {
    let mut bridge = McpBridge::start(endpoint);
    let initialize_params = json!({
        "protocolVersion": "2025-11-25",
        "capabilities": {},
        "clientInfo": { "name": "grimmore-phase-1-performance-gate", "version": "1.0.0" },
    });
    let initialized = bridge.request(1, "initialize", &initialize_params);
    assert!(
        initialized["result"]["capabilities"]["tools"].is_object(),
        "MCP bridge exposes its tool capability"
    );
    bridge.write(&json!({
        "jsonrpc": "2.0",
        "method": "notifications/initialized",
        "params": {},
    }));
    let mut request_id = 2;
    let health_params = json!({ "name": "grimmore_health", "arguments": {} });
    for _ in 0..WARM_MCP_WARMUP_SAMPLES {
        let health = bridge.request(request_id, "tools/call", &health_params);
        assert_eq!(
            health["result"]["structuredContent"]["role"], "mcp-readonly",
            "warm MCP health response retains its role"
        );
        request_id += 1;
    }
    let mut samples = Vec::with_capacity(WARM_MCP_SAMPLES);
    for _ in 0..WARM_MCP_SAMPLES {
        let started = Instant::now();
        let health = bridge.request(request_id, "tools/call", &health_params);
        samples.push(started.elapsed());
        assert_eq!(
            health["result"]["structuredContent"]["role"], "mcp-readonly",
            "warm MCP health response retains its role"
        );
        request_id += 1;
    }
    bridge.close();
    samples
}

#[cfg(target_os = "linux")]
fn linux_resident_memory_bytes(process_id: u32) -> u64 {
    let status = fs::read_to_string(format!("/proc/{process_id}/status"))
        .expect("read companion memory status");
    let kibibytes = status
        .lines()
        .find_map(|line| line.strip_prefix("VmRSS:"))
        .and_then(|line| line.split_whitespace().next())
        .expect("companion memory status includes VmRSS")
        .parse::<u64>()
        .expect("companion VmRSS is an integer number of KiB");
    kibibytes
        .checked_mul(1024)
        .expect("companion VmRSS byte conversion fits u64")
}

#[cfg(target_os = "linux")]
fn linux_process_cpu_ticks(process_id: u32) -> u64 {
    let stat = fs::read_to_string(format!("/proc/{process_id}/stat"))
        .expect("read companion process CPU status");
    let close_parenthesis = stat
        .rfind(')')
        .expect("process status has command terminator");
    let fields = stat[close_parenthesis + 2..]
        .split_whitespace()
        .collect::<Vec<_>>();
    let user_ticks = fields[11]
        .parse::<u64>()
        .expect("companion user CPU ticks are an integer");
    let system_ticks = fields[12]
        .parse::<u64>()
        .expect("companion system CPU ticks are an integer");
    user_ticks
        .checked_add(system_ticks)
        .expect("companion CPU ticks fit u64")
}

#[cfg(target_os = "linux")]
fn linux_clock_ticks_per_second() -> u32 {
    let output = Command::new("getconf")
        .arg("CLK_TCK")
        .output()
        .expect("run getconf for Linux clock ticks");
    assert!(
        output.status.success(),
        "getconf CLK_TCK failed with {}",
        output.status
    );
    std::str::from_utf8(&output.stdout)
        .expect("getconf CLK_TCK output is UTF-8")
        .trim()
        .parse::<u32>()
        .expect("getconf CLK_TCK output is an integer")
}

#[cfg(target_os = "linux")]
fn measure_idle_cpu_percent(process_id: u32) -> (f64, Duration) {
    thread::sleep(IDLE_SETTLE_TIME);
    let process_start = linux_process_cpu_ticks(process_id);
    let started = Instant::now();
    thread::sleep(IDLE_CPU_SAMPLE_TIME);
    let process_delta =
        u32::try_from(linux_process_cpu_ticks(process_id).saturating_sub(process_start))
            .expect("idle companion CPU ticks fit u32");
    let elapsed = started.elapsed();
    assert!(
        !elapsed.is_zero(),
        "idle companion CPU sample must have a positive duration"
    );
    let clock_ticks = linux_clock_ticks_per_second();
    (
        (f64::from(process_delta) / f64::from(clock_ticks) / elapsed.as_secs_f64()) * 100.0,
        elapsed,
    )
}

#[cfg(target_os = "linux")]
async fn measure_companion_runtime(corpus: &Corpus, secret: &RootSecret) -> CompanionRuntimeReport {
    let (companion, first_cold_handshake) = start_companion(corpus, secret).await;
    let mut cold_handshakes = Vec::with_capacity(COLD_HANDSHAKE_SAMPLES);
    cold_handshakes.push(first_cold_handshake);
    for _ in 1..COLD_HANDSHAKE_SAMPLES {
        let (ephemeral, handshake) = start_companion(corpus, secret).await;
        cold_handshakes.push(handshake);
        drop(ephemeral);
    }
    wait_for_indexed_vault(&companion.endpoint, secret).await;
    let mut warm_ipc = measure_warm_ipc(&companion.endpoint, secret).await;
    let mut warm_mcp = measure_warm_mcp(&companion.endpoint);
    let mut watcher_recovery = measure_watcher_recovery(&companion.endpoint, secret, corpus).await;
    thread::sleep(IDLE_SETTLE_TIME);
    let idle_rss_bytes = linux_resident_memory_bytes(companion.process.id());
    let (idle_cpu_percent, idle_cpu_sample) = measure_idle_cpu_percent(companion.process.id());
    let cold_handshake = latency_distribution(&mut cold_handshakes);
    let warm_ipc = latency_distribution(&mut warm_ipc);
    let warm_mcp = latency_distribution(&mut warm_mcp);
    let watcher_recovery = latency_distribution(&mut watcher_recovery);

    CompanionRuntimeReport {
        cold_handshake,
        warm_ipc_request_bytes: 1024,
        warm_ipc_warmup_samples: WARM_IPC_WARMUP_SAMPLES,
        warm_ipc,
        warm_mcp_warmup_samples: WARM_MCP_WARMUP_SAMPLES,
        warm_mcp,
        watcher_recovery,
        idle_rss_bytes,
        idle_cpu_sample_millis: idle_cpu_sample.as_millis(),
        idle_cpu_percent,
        retry_policy: "No benchmark sample retries; readiness polling has only bounded timeouts.",
        plugin_synchronous_load: "not measured: requires a real Obsidian desktop runner",
    }
}

fn measure_query(storage: &Storage, query: &str, expected_hits: usize, samples: usize) -> Duration {
    for _ in 0..20 {
        search_notes(storage, "performance", query, 20).expect("warm performance FTS query");
    }
    let mut durations = Vec::with_capacity(samples);
    for _ in 0..samples {
        let started = Instant::now();
        let result =
            search_notes(storage, "performance", query, 20).expect("run performance FTS query");
        durations.push(started.elapsed());
        assert_eq!(result.hits.len(), expected_hits);
    }
    p95(&mut durations)
}

fn measure_incremental(corpus: &Corpus, samples: usize) -> Duration {
    let note = corpus.root.join("incremental-performance-note.md");
    fs::write(
        &note,
        note_body(0, INCREMENTAL_NOTE_BYTES, Some(" revision-0 ")),
    )
    .expect("create incremental performance note");
    reconcile_vault_paths(&corpus.storage, "performance", &corpus.root, [&note])
        .expect("prime incremental performance note");

    let mut durations = Vec::with_capacity(samples);
    for iteration in 1..=samples {
        fs::write(
            &note,
            note_body(
                iteration,
                INCREMENTAL_NOTE_BYTES,
                Some(" incrementalrevisionmarker "),
            ),
        )
        .expect("prepare incremental performance note");
        let started = Instant::now();
        let report = reconcile_vault_paths(&corpus.storage, "performance", &corpus.root, [&note])
            .expect("measure incremental performance note");
        durations.push(started.elapsed());
        assert_eq!(
            report.updated, 1,
            "incremental iteration {iteration} did not change the indexed revision: {report:?}"
        );
    }
    p95(&mut durations)
}

#[cfg(target_os = "linux")]
#[tokio::test]
#[ignore = "run explicitly on the documented canonical performance host"]
async fn canonical_vault_performance_gate() {
    let debug_assertions = std::hint::black_box(cfg!(debug_assertions));
    assert!(
        !debug_assertions,
        "the canonical performance gate must run with cargo test --release"
    );
    let canonical_host = canonical_host_facts();
    RootSecret::verify_store()
        .expect("canonical host has a working Secret Service credential store");
    let secret = RootSecret::load_or_create().expect("load canonical companion root secret");
    let note_count = environment_usize("GRIMMORE_BENCH_NOTES", CANONICAL_NOTES);
    let corpus_bytes = environment_usize("GRIMMORE_BENCH_BYTES", CANONICAL_CORPUS_BYTES);
    assert_canonical_corpus_shape(note_count, corpus_bytes);
    let query_samples = environment_usize("GRIMMORE_BENCH_QUERY_SAMPLES", CANONICAL_QUERY_SAMPLES);
    let incremental_samples = environment_usize(
        "GRIMMORE_BENCH_INCREMENTAL_SAMPLES",
        CANONICAL_INCREMENTAL_SAMPLES,
    );
    assert_canonical_sample_shape(query_samples, incremental_samples);
    let corpus = Corpus::build(note_count, corpus_bytes);
    assert_eq!(
        corpus.revision, CANONICAL_CORPUS_REVISION,
        "canonical performance corpus revision drifted"
    );

    let started = Instant::now();
    let indexed = index_vault(&corpus.storage, "performance", &corpus.root)
        .expect("index deterministic performance corpus");
    let initial_index = started.elapsed();
    assert_eq!(indexed.scanned, note_count);
    let rare_query_p95 = measure_query(&corpus.storage, "raregrimmoretoken", 1, query_samples);
    let broad_query_p95 = measure_query(&corpus.storage, "broadgrimmoretoken", 20, query_samples);
    let incremental_p95 = measure_incremental(&corpus, incremental_samples);
    let companion_runtime = measure_companion_runtime(&corpus, &secret).await;

    let report = PerformanceReport {
        platform: std::env::consts::OS,
        architecture: std::env::consts::ARCH,
        logical_cpus: std::thread::available_parallelism().map_or(1, std::num::NonZero::get),
        canonical_host,
        note_count,
        corpus_bytes,
        corpus_revision: corpus.revision.clone(),
        canonical_corpus: note_count == CANONICAL_NOTES && corpus_bytes == CANONICAL_CORPUS_BYTES,
        initial_index_millis: initial_index.as_millis(),
        query_samples,
        rare_query_p95_micros: rare_query_p95.as_micros(),
        broad_query_p95_micros: broad_query_p95.as_micros(),
        incremental_samples,
        incremental_p95_micros: incremental_p95.as_micros(),
        companion_runtime,
    };
    println!(
        "{}",
        serde_json::to_string_pretty(&report).expect("serialize performance report")
    );

    assert!(initial_index <= INITIAL_INDEX_BUDGET);
    assert!(rare_query_p95 <= QUERY_P95_BUDGET);
    assert!(broad_query_p95 <= QUERY_P95_BUDGET);
    assert!(incremental_p95 <= INCREMENTAL_P95_BUDGET);
    assert!(
        report.companion_runtime.cold_handshake.p95_micros <= COLD_HANDSHAKE_BUDGET.as_micros()
    );
    assert_eq!(
        report.companion_runtime.cold_handshake.sample_count,
        COLD_HANDSHAKE_SAMPLES
    );
    assert!(report.companion_runtime.warm_ipc.p95_micros <= WARM_IPC_BUDGET.as_micros());
    assert_eq!(
        report.companion_runtime.warm_ipc.sample_count,
        WARM_IPC_SAMPLES
    );
    assert!(report.companion_runtime.warm_mcp.p95_micros <= WARM_MCP_BUDGET.as_micros());
    assert_eq!(
        report.companion_runtime.warm_mcp.sample_count,
        WARM_MCP_SAMPLES
    );
    assert_eq!(
        report.companion_runtime.watcher_recovery.sample_count,
        WATCHER_RECOVERY_SAMPLES
    );
    assert!(report.companion_runtime.idle_rss_bytes <= IDLE_RSS_BUDGET_BYTES);
    assert!(report.companion_runtime.idle_cpu_percent < IDLE_CPU_BUDGET_PERCENT);
}

#[cfg(not(target_os = "linux"))]
#[test]
#[ignore = "the absolute Phase 1 performance gate runs only on Ubuntu 24.04 x64"]
fn canonical_vault_performance_gate() {}

#[cfg(target_os = "linux")]
#[test]
fn parses_quoted_ubuntu_release() {
    let (os_id, os_version) = parse_os_release("NAME=Ubuntu\nID=ubuntu\nVERSION_ID=\"24.04\"\n");
    assert_eq!(os_id, "ubuntu");
    assert_eq!(os_version, "24.04");
}

#[cfg(target_os = "linux")]
#[test]
fn decodes_linux_device_numbers() {
    let device = (259_u64 << 8) | 2;
    assert_eq!(linux_device_name(device), "259:2");
}

#[test]
fn freezes_the_canonical_corpus_shape() {
    assert_canonical_corpus_shape(CANONICAL_NOTES, CANONICAL_CORPUS_BYTES);
}

#[test]
#[should_panic(expected = "canonical performance gate must index exactly 25,000 notes")]
fn rejects_a_reduced_canonical_corpus() {
    assert_canonical_corpus_shape(CANONICAL_NOTES - 1, CANONICAL_CORPUS_BYTES);
}

#[test]
fn freezes_the_canonical_sample_shape() {
    assert_canonical_sample_shape(CANONICAL_QUERY_SAMPLES, CANONICAL_INCREMENTAL_SAMPLES);
}

#[test]
#[should_panic(expected = "canonical performance gate must use exactly 200 query samples")]
fn rejects_reduced_canonical_query_samples() {
    assert_canonical_sample_shape(CANONICAL_QUERY_SAMPLES - 1, CANONICAL_INCREMENTAL_SAMPLES);
}
