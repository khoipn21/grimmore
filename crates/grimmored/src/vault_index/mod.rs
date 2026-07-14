use std::{
    collections::{BTreeMap, HashMap},
    fs,
    path::{Component, Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use grimmore_core::{
    protocol::{PatchProposal, ProposeNoteReplacementParams, SearchHit, SearchNotesResult},
    revision::content_revision,
};
use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};
use serde::Serialize;
use thiserror::Error;
use walkdir::{DirEntry, WalkDir};

use crate::storage::{Storage, StorageError};

const MAX_NOTE_BYTES: u64 = 4 * 1024 * 1024;
const MAX_NOTE_TEXT_BYTES: usize = 4 * 1024 * 1024;
const MAX_QUERY_BYTES: usize = 512;
const MAX_QUERY_TERMS: usize = 16;
const MAX_SEARCH_RESULTS: u16 = 50;

#[derive(Debug, Error)]
pub enum VaultIndexError {
    #[error("storage error: {0}")]
    Storage(#[from] StorageError),
    #[error("database error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("vault root is not a directory: {0}")]
    RootNotDirectory(PathBuf),
    #[error("access vault path {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("walk vault: {0}")]
    Walk(#[from] walkdir::Error),
    #[error("vault path is not valid UTF-8: {0}")]
    NonUtf8Path(PathBuf),
    #[error("vault id must contain 1-128 ASCII letters, digits, dots, dashes, or underscores")]
    InvalidVaultId,
    #[error("note exceeds the {MAX_NOTE_BYTES}-byte indexing limit: {path}")]
    NoteTooLarge { path: PathBuf },
    #[error("search query must contain at least one letter or digit")]
    EmptyQuery,
    #[error("search query exceeds the {MAX_QUERY_BYTES}-byte limit")]
    QueryTooLong,
    #[error("search query exceeds the {MAX_QUERY_TERMS}-term limit")]
    TooManyQueryTerms,
    #[error("search result limit must be between 1 and {MAX_SEARCH_RESULTS}")]
    InvalidSearchLimit,
    #[error("vault has not been indexed: {0}")]
    VaultNotIndexed(String),
    #[error("vault {vault_id} is indexed at {indexed_root}, not {requested_root}")]
    VaultRootMismatch {
        vault_id: String,
        indexed_root: String,
        requested_root: PathBuf,
    },
    #[error("filesystem event requires a full vault reconciliation: {0}")]
    FullRescanRequired(PathBuf),
    #[error("vault index revision is invalid: {0}")]
    InvalidIndexRevision(i64),
    #[error("note path is not a portable vault-relative Markdown path")]
    InvalidNotePath,
    #[error("replacement exceeds the {MAX_NOTE_BYTES}-byte note limit")]
    ReplacementTooLarge,
    #[error("note has not been indexed: {0}")]
    NoteNotIndexed(String),
    #[error("note revision is stale")]
    StaleRevision,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct IndexReport {
    pub vault_id: String,
    pub root: PathBuf,
    pub scanned: usize,
    pub created: usize,
    pub updated: usize,
    pub unchanged: usize,
    pub deleted: usize,
    pub index_revision: u64,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct IncrementalIndexReport {
    pub vault_id: String,
    pub root: PathBuf,
    pub examined: usize,
    pub created: usize,
    pub updated: usize,
    pub unchanged: usize,
    pub deleted: usize,
    pub index_revision: u64,
}

#[derive(Debug)]
struct ScannedNote {
    relative_path: String,
    title: String,
    body: String,
    revision: String,
    modified_unix_ns: i64,
    size_bytes: i64,
}

#[derive(Default)]
struct ReconcileCounts {
    created: usize,
    updated: usize,
    unchanged: usize,
    deleted: usize,
}

impl ReconcileCounts {
    const fn changed(&self) -> usize {
        self.created + self.updated + self.deleted
    }
}

pub fn index_vault(
    storage: &Storage,
    vault_id: &str,
    root: impl AsRef<Path>,
) -> Result<IndexReport, VaultIndexError> {
    validate_vault_id(vault_id)?;
    let root = canonical_directory(root.as_ref())?;
    let root_text = root
        .to_str()
        .ok_or_else(|| VaultIndexError::NonUtf8Path(root.clone()))?;
    let indexed_at = unix_time_millis();

    let mut connection = storage.connection()?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    transaction.execute(
        "INSERT INTO vaults(id, root) VALUES (?1, ?2)
         ON CONFLICT(id) DO UPDATE SET root = excluded.root",
        params![vault_id, root_text],
    )?;

    let mut existing = {
        let mut statement = transaction.prepare(
            "SELECT relative_path, revision FROM notes WHERE vault_id = ?1 ORDER BY relative_path",
        )?;
        let rows = statement.query_map([vault_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut revisions = HashMap::new();
        for row in rows {
            let (path, revision) = row?;
            revisions.insert(path, revision);
        }
        revisions
    };

    let mut created = 0;
    let mut updated = 0;
    let mut unchanged = 0;
    let scanned = scan_vault(&root, |note| {
        match existing.remove(&note.relative_path) {
            None => created += 1,
            Some(revision) if revision != note.revision => updated += 1,
            Some(_) => {
                unchanged += 1;
                return Ok(());
            }
        }

        transaction.execute(
            "INSERT INTO notes(
                 vault_id, relative_path, title, body, revision, modified_unix_ns, size_bytes
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(vault_id, relative_path) DO UPDATE SET
                 title = excluded.title,
                 body = excluded.body,
                 revision = excluded.revision,
                 modified_unix_ns = excluded.modified_unix_ns,
                 size_bytes = excluded.size_bytes",
            params![
                vault_id,
                note.relative_path,
                note.title,
                note.body,
                note.revision,
                note.modified_unix_ns,
                note.size_bytes,
            ],
        )?;
        Ok(())
    })?;

    let deleted = existing.len();
    for relative_path in existing.keys() {
        transaction.execute(
            "DELETE FROM notes WHERE vault_id = ?1 AND relative_path = ?2",
            params![vault_id, relative_path],
        )?;
    }

    transaction.execute(
        "UPDATE vaults
         SET index_revision = index_revision + 1,
             last_indexed_unix_ms = ?2
         WHERE id = ?1",
        params![vault_id, indexed_at],
    )?;
    let raw_index_revision = transaction.query_row(
        "SELECT index_revision FROM vaults WHERE id = ?1",
        [vault_id],
        |row| row.get(0),
    )?;
    let index_revision = u64::try_from(raw_index_revision)
        .map_err(|_| VaultIndexError::InvalidIndexRevision(raw_index_revision))?;
    transaction.commit()?;

    Ok(IndexReport {
        vault_id: vault_id.to_owned(),
        root,
        scanned,
        created,
        updated,
        unchanged,
        deleted,
        index_revision,
    })
}

/// Reconcile a bounded set of filesystem paths after watcher hints.
///
/// Directory-shaped or otherwise ambiguous hints return
/// [`VaultIndexError::FullRescanRequired`] so callers can preserve correctness
/// with [`index_vault`].
pub fn reconcile_vault_paths<I, P>(
    storage: &Storage,
    vault_id: &str,
    root: impl AsRef<Path>,
    paths: I,
) -> Result<IncrementalIndexReport, VaultIndexError>
where
    I: IntoIterator<Item = P>,
    P: AsRef<Path>,
{
    validate_vault_id(vault_id)?;
    let root = canonical_directory(root.as_ref())?;
    let root_text = root
        .to_str()
        .ok_or_else(|| VaultIndexError::NonUtf8Path(root.clone()))?;
    let mut changes = BTreeMap::new();
    for path in paths {
        if let Some((relative_path, note)) = scan_changed_path(&root, path.as_ref())? {
            changes.insert(relative_path, note);
        }
    }

    let indexed_at = unix_time_millis();
    let mut connection = storage.connection()?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let indexed_root = transaction
        .query_row("SELECT root FROM vaults WHERE id = ?1", [vault_id], |row| {
            row.get::<_, String>(0)
        })
        .optional()?
        .ok_or_else(|| VaultIndexError::VaultNotIndexed(vault_id.to_owned()))?;
    if indexed_root != root_text {
        return Err(VaultIndexError::VaultRootMismatch {
            vault_id: vault_id.to_owned(),
            indexed_root,
            requested_root: root,
        });
    }

    let mut counts = ReconcileCounts::default();
    for (relative_path, scanned_note) in &changes {
        reconcile_note(
            &transaction,
            vault_id,
            relative_path,
            scanned_note.as_ref(),
            &mut counts,
        )?;
    }

    if counts.changed() > 0 {
        transaction.execute(
            "UPDATE vaults
             SET index_revision = index_revision + 1,
                 last_indexed_unix_ms = ?2
             WHERE id = ?1",
            params![vault_id, indexed_at],
        )?;
    }
    let raw_index_revision = transaction.query_row(
        "SELECT index_revision FROM vaults WHERE id = ?1",
        [vault_id],
        |row| row.get(0),
    )?;
    let index_revision = u64::try_from(raw_index_revision)
        .map_err(|_| VaultIndexError::InvalidIndexRevision(raw_index_revision))?;
    transaction.commit()?;

    Ok(IncrementalIndexReport {
        vault_id: vault_id.to_owned(),
        root,
        examined: changes.len(),
        created: counts.created,
        updated: counts.updated,
        unchanged: counts.unchanged,
        deleted: counts.deleted,
        index_revision,
    })
}

fn reconcile_note(
    transaction: &Transaction<'_>,
    vault_id: &str,
    relative_path: &str,
    scanned_note: Option<&ScannedNote>,
    counts: &mut ReconcileCounts,
) -> Result<(), rusqlite::Error> {
    let existing_revision = transaction
        .query_row(
            "SELECT revision FROM notes WHERE vault_id = ?1 AND relative_path = ?2",
            params![vault_id, relative_path],
            |row| row.get::<_, String>(0),
        )
        .optional()?;

    let Some(note) = scanned_note else {
        if existing_revision.is_some() {
            transaction.execute(
                "DELETE FROM notes WHERE vault_id = ?1 AND relative_path = ?2",
                params![vault_id, relative_path],
            )?;
            counts.deleted += 1;
        } else {
            counts.unchanged += 1;
        }
        return Ok(());
    };

    match existing_revision {
        None => counts.created += 1,
        Some(revision) if revision != note.revision => counts.updated += 1,
        Some(_) => {
            counts.unchanged += 1;
            return Ok(());
        }
    }
    transaction.execute(
        "INSERT INTO notes(
             vault_id, relative_path, title, body, revision, modified_unix_ns, size_bytes
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(vault_id, relative_path) DO UPDATE SET
             title = excluded.title,
             body = excluded.body,
             revision = excluded.revision,
             modified_unix_ns = excluded.modified_unix_ns,
             size_bytes = excluded.size_bytes",
        params![
            vault_id,
            note.relative_path,
            note.title,
            note.body,
            note.revision,
            note.modified_unix_ns,
            note.size_bytes,
        ],
    )?;
    Ok(())
}

pub fn search_notes(
    storage: &Storage,
    vault_id: &str,
    query: &str,
    limit: u16,
) -> Result<SearchNotesResult, VaultIndexError> {
    validate_vault_id(vault_id)?;
    if !(1..=MAX_SEARCH_RESULTS).contains(&limit) {
        return Err(VaultIndexError::InvalidSearchLimit);
    }
    let fts_query = compile_fts_query(query)?;
    let connection = storage.connection()?;
    let raw_index_revision = connection
        .query_row(
            "SELECT index_revision FROM vaults WHERE id = ?1",
            [vault_id],
            |row| row.get(0),
        )
        .optional()?
        .ok_or_else(|| VaultIndexError::VaultNotIndexed(vault_id.to_owned()))?;
    let index_revision = u64::try_from(raw_index_revision)
        .map_err(|_| VaultIndexError::InvalidIndexRevision(raw_index_revision))?;

    let mut statement = connection.prepare(
        "WITH ranked(note_id, score, relative_path) AS MATERIALIZED (
             SELECT
                 notes.id,
                 bm25(notes_fts, 8.0, 1.0, 2.0),
                 notes.relative_path
             FROM notes_fts
             CROSS JOIN notes ON notes.id = notes_fts.rowid
             WHERE notes_fts MATCH ?1 AND notes.vault_id = ?2
             ORDER BY bm25(notes_fts, 8.0, 1.0, 2.0), notes.relative_path
             LIMIT ?3
         )
         SELECT
             notes.relative_path,
             notes.title,
             (
                 SELECT snippet(notes_fts, 1, '⟦', '⟧', ' … ', 18)
                 FROM notes_fts
                 WHERE notes_fts MATCH ?1 AND notes_fts.rowid = ranked.note_id
             ),
             notes.revision
         FROM ranked
         JOIN notes ON notes.id = ranked.note_id
         ORDER BY ranked.score, ranked.relative_path",
    )?;
    let rows = statement.query_map(params![fts_query, vault_id, limit], |row| {
        Ok(SearchHit {
            path: row.get(0)?,
            title: row.get(1)?,
            snippet: row.get(2)?,
            revision: row.get(3)?,
        })
    })?;
    let hits = rows.collect::<Result<Vec<_>, _>>()?;

    Ok(SearchNotesResult {
        hits,
        indexed_revision: index_revision,
    })
}

pub fn propose_note_replacement(
    storage: &Storage,
    vault_id: &str,
    params: ProposeNoteReplacementParams,
) -> Result<PatchProposal, VaultIndexError> {
    validate_vault_id(vault_id)?;
    validate_note_path(&params.path)?;
    if params.replacement.len() > MAX_NOTE_TEXT_BYTES {
        return Err(VaultIndexError::ReplacementTooLarge);
    }

    let connection = storage.connection()?;
    let indexed_revision = connection
        .query_row(
            "SELECT revision FROM notes WHERE vault_id = ?1 AND relative_path = ?2",
            params![vault_id, &params.path],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .ok_or_else(|| VaultIndexError::NoteNotIndexed(params.path.clone()))?;
    if indexed_revision != params.expected_revision {
        return Err(VaultIndexError::StaleRevision);
    }

    Ok(PatchProposal {
        path: params.path,
        expected_revision: params.expected_revision,
        proposed_revision: content_revision(&params.replacement),
        replacement: params.replacement,
    })
}

fn canonical_directory(path: &Path) -> Result<PathBuf, VaultIndexError> {
    let canonical = fs::canonicalize(path).map_err(|source| VaultIndexError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    if !canonical.is_dir() {
        return Err(VaultIndexError::RootNotDirectory(canonical));
    }
    Ok(canonical)
}

fn scan_vault<F>(root: &Path, mut consume: F) -> Result<usize, VaultIndexError>
where
    F: FnMut(ScannedNote) -> Result<(), VaultIndexError>,
{
    let mut scanned = 0;
    for entry in WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(should_visit)
    {
        let entry = entry?;
        if !entry.file_type().is_file() || !is_markdown(entry.path()) {
            continue;
        }

        let metadata =
            fs::symlink_metadata(entry.path()).map_err(|source| VaultIndexError::Io {
                path: entry.path().to_path_buf(),
                source,
            })?;
        if metadata.file_type().is_symlink() {
            continue;
        }
        if let Some(note) = scan_regular_note(root, entry.path(), &metadata)? {
            consume(note)?;
            scanned += 1;
        }
    }
    Ok(scanned)
}

fn scan_changed_path(
    root: &Path,
    event_path: &Path,
) -> Result<Option<(String, Option<ScannedNote>)>, VaultIndexError> {
    let path = if event_path.is_absolute() {
        event_path.to_path_buf()
    } else {
        root.join(event_path)
    };
    let Ok(relative) = path.strip_prefix(root) else {
        return Ok(None);
    };
    if relative.as_os_str().is_empty() {
        return Err(VaultIndexError::FullRescanRequired(path));
    }

    let mut parts = Vec::new();
    for component in relative.components() {
        let Component::Normal(part) = component else {
            return Err(VaultIndexError::FullRescanRequired(path));
        };
        let part = part
            .to_str()
            .ok_or_else(|| VaultIndexError::NonUtf8Path(path.clone()))?;
        if matches!(part, ".git" | ".obsidian" | ".trash") {
            return Ok(None);
        }
        parts.push(part);
    }
    let relative_path = parts.join("/");

    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(is_markdown(&path).then_some((relative_path, None)));
        }
        Err(source) => {
            return Err(VaultIndexError::Io {
                path: path.clone(),
                source,
            });
        }
    };
    if metadata.is_dir() {
        return Err(VaultIndexError::FullRescanRequired(path));
    }
    if !is_markdown(&path) {
        return Ok(None);
    }
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Ok(Some((relative_path, None)));
    }

    match scan_regular_note(root, &path, &metadata) {
        Ok(Some(note)) => Ok(Some((note.relative_path.clone(), Some(note)))),
        Ok(None) => Ok(Some((relative_path, None))),
        Err(VaultIndexError::Io { source, .. })
            if source.kind() == std::io::ErrorKind::NotFound =>
        {
            Ok(Some((relative_path, None)))
        }
        Err(error) => Err(error),
    }
}

fn scan_regular_note(
    root: &Path,
    path: &Path,
    metadata: &fs::Metadata,
) -> Result<Option<ScannedNote>, VaultIndexError> {
    if metadata.len() > MAX_NOTE_BYTES {
        return Err(VaultIndexError::NoteTooLarge {
            path: path.to_path_buf(),
        });
    }
    let canonical = fs::canonicalize(path).map_err(|source| VaultIndexError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    if !canonical.starts_with(root) {
        return Ok(None);
    }
    let relative_path = portable_relative_path(root, &canonical)?;
    let body = fs::read_to_string(&canonical).map_err(|source| VaultIndexError::Io {
        path: canonical.clone(),
        source,
    })?;
    let title = extract_title(&body, &canonical)?;
    let modified_unix_ns = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .and_then(|duration| i64::try_from(duration.as_nanos()).ok())
        .unwrap_or(0);

    Ok(Some(ScannedNote {
        relative_path,
        title,
        revision: content_revision(&body),
        body,
        modified_unix_ns,
        size_bytes: i64::try_from(metadata.len()).unwrap_or(i64::MAX),
    }))
}

fn should_visit(entry: &DirEntry) -> bool {
    if entry.depth() == 0 || !entry.file_type().is_dir() {
        return true;
    }
    !matches!(
        entry.file_name().to_str(),
        Some(".git" | ".obsidian" | ".trash")
    )
}

fn is_markdown(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("md"))
}

fn portable_relative_path(root: &Path, path: &Path) -> Result<String, VaultIndexError> {
    let relative = path
        .strip_prefix(root)
        .map_err(|_| VaultIndexError::NonUtf8Path(path.to_path_buf()))?;
    let mut parts = Vec::new();
    for component in relative.components() {
        match component {
            Component::Normal(part) => parts.push(
                part.to_str()
                    .ok_or_else(|| VaultIndexError::NonUtf8Path(path.to_path_buf()))?,
            ),
            _ => return Err(VaultIndexError::NonUtf8Path(path.to_path_buf())),
        }
    }
    Ok(parts.join("/"))
}

fn extract_title(body: &str, path: &Path) -> Result<String, VaultIndexError> {
    if let Some(title) = body
        .lines()
        .filter_map(|line| line.strip_prefix("# "))
        .map(str::trim)
        .find(|title| !title.is_empty())
    {
        return Ok(title.to_owned());
    }
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .map(ToOwned::to_owned)
        .ok_or_else(|| VaultIndexError::NonUtf8Path(path.to_path_buf()))
}

fn compile_fts_query(query: &str) -> Result<String, VaultIndexError> {
    if query.len() > MAX_QUERY_BYTES {
        return Err(VaultIndexError::QueryTooLong);
    }
    let terms = query
        .split(|character: char| !character.is_alphanumeric() && character != '_')
        .filter(|term| !term.is_empty())
        .collect::<Vec<_>>();
    if terms.is_empty() {
        return Err(VaultIndexError::EmptyQuery);
    }
    if terms.len() > MAX_QUERY_TERMS {
        return Err(VaultIndexError::TooManyQueryTerms);
    }
    Ok(terms
        .into_iter()
        .map(|term| format!("\"{term}\""))
        .collect::<Vec<_>>()
        .join(" AND "))
}

fn validate_vault_id(vault_id: &str) -> Result<(), VaultIndexError> {
    if vault_id.is_empty()
        || vault_id.len() > 128
        || !vault_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
    {
        return Err(VaultIndexError::InvalidVaultId);
    }
    Ok(())
}

fn validate_note_path(path: &str) -> Result<(), VaultIndexError> {
    if path.is_empty()
        || path.len() > 1024
        || path.starts_with('/')
        || path.contains('\\')
        || !path.to_ascii_lowercase().ends_with(".md")
        || path
            .split('/')
            .any(|component| component.is_empty() || matches!(component, "." | ".."))
    {
        return Err(VaultIndexError::InvalidNotePath);
    }
    Ok(())
}

fn unix_time_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_millis()).ok())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::{VaultIndexError, compile_fts_query, validate_note_path, validate_vault_id};

    #[test]
    fn query_compiler_treats_fts_operators_as_terms() {
        assert_eq!(
            compile_fts_query("context OR graph*").expect("compile bounded query"),
            "\"context\" AND \"OR\" AND \"graph\""
        );
    }

    #[test]
    fn vault_ids_are_deliberately_portable() {
        assert!(validate_vault_id("personal-vault_1.test").is_ok());
        assert!(matches!(
            validate_vault_id("../outside"),
            Err(VaultIndexError::InvalidVaultId)
        ));
    }

    #[test]
    fn note_paths_cannot_escape_the_granted_vault() {
        assert!(validate_note_path("knowledge/ai/context.md").is_ok());
        assert!(matches!(
            validate_note_path("../outside.md"),
            Err(VaultIndexError::InvalidNotePath)
        ));
        assert!(matches!(
            validate_note_path("knowledge\\outside.md"),
            Err(VaultIndexError::InvalidNotePath)
        ));
    }
}
