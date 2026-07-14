use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Mutex, MutexGuard},
    time::Duration,
};

use rusqlite::{Connection, OpenFlags};
use thiserror::Error;

const SCHEMA_VERSION: u32 = 1;

const SCHEMA: &str = r"
CREATE TABLE vaults (
    id TEXT PRIMARY KEY NOT NULL,
    root TEXT NOT NULL,
    index_revision INTEGER NOT NULL DEFAULT 0,
    last_indexed_unix_ms INTEGER NOT NULL DEFAULT 0
) STRICT;

CREATE TABLE notes (
    id INTEGER PRIMARY KEY,
    vault_id TEXT NOT NULL REFERENCES vaults(id) ON DELETE CASCADE,
    relative_path TEXT NOT NULL,
    title TEXT NOT NULL,
    body TEXT NOT NULL,
    revision TEXT NOT NULL,
    modified_unix_ns INTEGER NOT NULL,
    size_bytes INTEGER NOT NULL,
    UNIQUE(vault_id, relative_path)
) STRICT;

CREATE INDEX notes_vault_path_idx ON notes(vault_id, relative_path);

CREATE VIRTUAL TABLE notes_fts USING fts5(
    title,
    body,
    relative_path,
    content='notes',
    content_rowid='id',
    tokenize='unicode61 remove_diacritics 2'
);

CREATE TRIGGER notes_after_insert AFTER INSERT ON notes BEGIN
    INSERT INTO notes_fts(rowid, title, body, relative_path)
    VALUES (new.id, new.title, new.body, new.relative_path);
END;

CREATE TRIGGER notes_after_delete AFTER DELETE ON notes BEGIN
    INSERT INTO notes_fts(notes_fts, rowid, title, body, relative_path)
    VALUES ('delete', old.id, old.title, old.body, old.relative_path);
END;

CREATE TRIGGER notes_after_update AFTER UPDATE ON notes BEGIN
    INSERT INTO notes_fts(notes_fts, rowid, title, body, relative_path)
    VALUES ('delete', old.id, old.title, old.body, old.relative_path);
    INSERT INTO notes_fts(rowid, title, body, relative_path)
    VALUES (new.id, new.title, new.body, new.relative_path);
END;
";

#[derive(Debug, Error)]
pub enum StorageError {
    #[error("create database directory {path}: {source}")]
    CreateDirectory {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("database error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("database lock was poisoned by a failed operation")]
    LockPoisoned,
    #[error("database schema {found} is newer than supported schema {supported}")]
    UnsupportedSchema { found: u32, supported: u32 },
}

/// Serialized `SQLite` owner used by the first local companion process.
pub struct Storage {
    path: PathBuf,
    connection: Mutex<Connection>,
}

impl Storage {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StorageError> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent).map_err(|source| StorageError::CreateDirectory {
                path: parent.to_path_buf(),
                source,
            })?;
        }

        let connection = Connection::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_CREATE
                | OpenFlags::SQLITE_OPEN_FULL_MUTEX,
        )?;
        connection.busy_timeout(Duration::from_secs(5))?;
        connection.execute_batch(
            "PRAGMA foreign_keys = ON;
             PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA temp_store = MEMORY;",
        )?;

        let found = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        if found > SCHEMA_VERSION {
            return Err(StorageError::UnsupportedSchema {
                found,
                supported: SCHEMA_VERSION,
            });
        }
        if found == 0 {
            connection.execute_batch("BEGIN IMMEDIATE;")?;
            let migration_result = connection
                .execute_batch(SCHEMA)
                .and_then(|()| connection.pragma_update(None, "user_version", SCHEMA_VERSION));
            match migration_result {
                Ok(()) => connection.execute_batch("COMMIT;")?,
                Err(error) => {
                    let _ = connection.execute_batch("ROLLBACK;");
                    return Err(StorageError::Sqlite(error));
                }
            }
        }

        Ok(Self {
            path,
            connection: Mutex::new(connection),
        })
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn connection(&self) -> Result<MutexGuard<'_, Connection>, StorageError> {
        self.connection
            .lock()
            .map_err(|_| StorageError::LockPoisoned)
    }
}
