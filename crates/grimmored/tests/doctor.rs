#![cfg(target_os = "linux")]

use std::process::Command;

use tempfile::TempDir;

#[test]
fn doctor_reports_a_unavailable_secret_service_without_failing() {
    let workspace = TempDir::new().expect("create isolated diagnostic workspace");
    let unavailable_socket = workspace.path().join("no-secret-service.sock");
    let output = Command::new(env!("CARGO_BIN_EXE_grimmored"))
        .arg("doctor")
        .env(
            "DBUS_SESSION_BUS_ADDRESS",
            format!("unix:path={}", unavailable_socket.display()),
        )
        .output()
        .expect("run doctor with an unavailable Secret Service endpoint");

    assert!(
        output.status.success(),
        "doctor must retain its JSON diagnostic contract: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("doctor emits valid JSON");
    assert_eq!(report["fts5Available"], true);
    assert_eq!(report["protocolVersion"], 1);
    assert_eq!(report["credentialStoreAvailable"], false);
}
