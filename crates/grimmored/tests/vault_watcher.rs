use std::{fs, path::Path, sync::Arc, time::Duration};

use grimmored::{
    storage::Storage,
    vault_index::{index_vault, search_notes},
    vault_watcher::VaultWatcher,
};
use tempfile::TempDir;
use tokio::time::{Instant, sleep};
use walkdir::WalkDir;

fn copy_reference_vault(destination: &Path) {
    let source =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/vaults/reference-vault");
    for entry in WalkDir::new(&source) {
        let entry = entry.expect("walk committed reference vault");
        let relative = entry
            .path()
            .strip_prefix(&source)
            .expect("fixture entry remains under fixture root");
        let target = destination.join(relative);
        if entry.file_type().is_dir() {
            fs::create_dir_all(&target).expect("create temporary vault directory");
        } else {
            fs::copy(entry.path(), &target).expect("copy provenance-backed fixture file");
        }
    }
}

async fn wait_for_hits(storage: &Storage, expected: usize) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let hits = search_notes(storage, "reference", "watchersentinelpresent", 10)
            .expect("query real SQLite FTS index while watcher runs")
            .hits
            .len();
        if hits == expected {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "watcher did not reconcile in time"
        );
        sleep(Duration::from_millis(25)).await;
    }
}

async fn wait_for_path(storage: &Storage, expected: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let hits = search_notes(storage, "reference", "watchersentinelpresent", 10)
            .expect("query real SQLite FTS index while watcher runs")
            .hits;
        if hits.len() == 1 && hits[0].path == expected {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "watcher did not reconcile renamed path in time"
        );
        sleep(Duration::from_millis(25)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_watcher_reconciles_create_modify_rename_and_delete() {
    let workspace = TempDir::new().expect("create isolated test workspace");
    let vault = workspace.path().join("vault");
    copy_reference_vault(&vault);
    let storage = Arc::new(
        Storage::open(workspace.path().join("operational.sqlite3"))
            .expect("open real bundled SQLite database"),
    );
    let initial = index_vault(&storage, "reference", &vault).expect("index reference vault");
    let _watcher = VaultWatcher::start(Arc::clone(&storage), "reference".to_owned(), initial.root)
        .expect("start native recursive filesystem watcher");

    let watched_note = vault.join("knowledge/ai/native-watcher.md");
    fs::write(
        &watched_note,
        "# Native watcher\n\nThe watchersentinelpresent marker is indexed.\n",
    )
    .expect("create watched note");
    wait_for_hits(&storage, 1).await;

    fs::write(
        &watched_note,
        "# Native watcher\n\nThe unique marker was deliberately removed.\n",
    )
    .expect("modify watched note");
    wait_for_hits(&storage, 0).await;

    fs::write(
        &watched_note,
        "# Native watcher\n\nThe watchersentinelpresent marker returned.\n",
    )
    .expect("modify watched note again");
    wait_for_hits(&storage, 1).await;

    let renamed_note = vault.join("knowledge/ai/native-watcher-renamed.md");
    fs::rename(&watched_note, &renamed_note).expect("rename watched note");
    wait_for_path(&storage, "knowledge/ai/native-watcher-renamed.md").await;

    fs::remove_file(&renamed_note).expect("delete watched note");
    wait_for_hits(&storage, 0).await;
}
