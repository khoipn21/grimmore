use std::{fs, path::Path};

use grimmored::{
    storage::Storage,
    vault_index::{VaultIndexError, index_vault, reconcile_vault_paths, search_notes},
};
use tempfile::TempDir;
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

#[test]
fn indexes_searches_and_reconciles_a_temporary_obsidian_vault() {
    let workspace = TempDir::new().expect("create isolated test workspace");
    let vault = workspace.path().join("vault");
    copy_reference_vault(&vault);
    let storage = Storage::open(workspace.path().join("operational.sqlite3"))
        .expect("open real bundled SQLite database");

    let first = index_vault(&storage, "reference", &vault).expect("index reference vault");
    assert_eq!(first.scanned, 4);
    assert_eq!(first.created, first.scanned);
    assert_eq!(first.updated, 0);
    assert_eq!(first.deleted, 0);

    let results = search_notes(&storage, "reference", "context engineering", 10)
        .expect("query the FTS5 index");
    assert_eq!(results.hits.len(), 1);
    assert_eq!(results.hits[0].path, "knowledge/ai/context-engineering.md");
    assert!(results.hits[0].snippet.contains('⟦'));

    let second = index_vault(&storage, "reference", &vault).expect("reconcile unchanged vault");
    assert_eq!(second.unchanged, 4);
    assert_eq!(second.created + second.updated + second.deleted, 0);

    let context_note = vault.join("knowledge/ai/context-engineering.md");
    let mut content = fs::read_to_string(&context_note).expect("read temporary note");
    content.push_str("\nA revision receipt makes an accepted change recoverable.\n");
    fs::write(&context_note, content).expect("change temporary note");
    fs::remove_file(vault.join("daily/2026-07-13.md")).expect("remove temporary daily note");

    let third = index_vault(&storage, "reference", &vault).expect("reconcile changed vault");
    assert_eq!(third.updated, 1);
    assert_eq!(third.deleted, 1);
    assert_eq!(third.index_revision, 3);
}

#[test]
fn search_syntax_is_data_not_an_fts_program() {
    let workspace = TempDir::new().expect("create isolated test workspace");
    let vault = workspace.path().join("vault");
    copy_reference_vault(&vault);
    let storage = Storage::open(workspace.path().join("operational.sqlite3"))
        .expect("open real bundled SQLite database");
    index_vault(&storage, "reference", &vault).expect("index reference vault");

    let results = search_notes(&storage, "reference", "context OR graph*", 10)
        .expect("operator-like input remains a bounded literal query");
    assert!(results.hits.is_empty());
}

#[test]
fn incrementally_reconciles_only_changed_markdown_paths() {
    let workspace = TempDir::new().expect("create isolated test workspace");
    let vault = workspace.path().join("vault");
    copy_reference_vault(&vault);
    let storage = Storage::open(workspace.path().join("operational.sqlite3"))
        .expect("open real bundled SQLite database");
    let initial = index_vault(&storage, "reference", &vault).expect("index reference vault");

    let added = vault.join("knowledge/ai/watcher-reconciliation.md");
    fs::write(
        &added,
        "# Watcher reconciliation\n\nA unique incremental watcher marker.\n",
    )
    .expect("create changed Markdown note");
    let created = reconcile_vault_paths(&storage, "reference", &vault, [&added])
        .expect("reconcile one created path");
    assert_eq!(created.examined, 1);
    assert_eq!(created.created, 1);
    assert_eq!(created.index_revision, initial.index_revision + 1);
    assert_eq!(
        search_notes(&storage, "reference", "incremental watcher marker", 10)
            .expect("search incrementally indexed note")
            .hits
            .len(),
        1
    );

    let unchanged = reconcile_vault_paths(&storage, "reference", &vault, [&added])
        .expect("ignore duplicate watcher hint");
    assert_eq!(unchanged.unchanged, 1);
    assert_eq!(unchanged.index_revision, created.index_revision);

    fs::remove_file(&added).expect("remove changed Markdown note");
    let deleted = reconcile_vault_paths(&storage, "reference", &vault, [&added])
        .expect("reconcile one removed path");
    assert_eq!(deleted.deleted, 1);
    assert_eq!(deleted.index_revision, created.index_revision + 1);
    assert!(
        search_notes(&storage, "reference", "incremental watcher marker", 10)
            .expect("search after incremental deletion")
            .hits
            .is_empty()
    );

    assert!(matches!(
        reconcile_vault_paths(&storage, "reference", &vault, [&vault]),
        Err(VaultIndexError::FullRescanRequired(_))
    ));
}
