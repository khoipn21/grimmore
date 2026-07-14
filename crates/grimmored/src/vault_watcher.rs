use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use notify::{
    Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher,
    event::{CreateKind, ModifyKind, RemoveKind},
};
use thiserror::Error;
use tokio::{
    runtime::Handle,
    sync::mpsc,
    task::JoinHandle,
    time::{Instant, sleep_until},
};
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use crate::{
    storage::Storage,
    vault_index::{VaultIndexError, index_vault, reconcile_vault_paths},
};

const EVENT_QUEUE_CAPACITY: usize = 256;
const BATCH_WINDOW: Duration = Duration::from_millis(150);

#[derive(Debug, Error)]
pub enum VaultWatcherError {
    #[error("start filesystem watcher for {path}: {source}")]
    Notify {
        path: PathBuf,
        source: notify::Error,
    },
    #[error("the vault watcher must start inside a Tokio runtime")]
    RuntimeUnavailable,
}

/// Keeps a native filesystem watcher and its reconciliation task alive.
pub struct VaultWatcher {
    _watcher: RecommendedWatcher,
    shutdown: CancellationToken,
    task: JoinHandle<()>,
}

impl VaultWatcher {
    pub fn start(
        storage: Arc<Storage>,
        vault_id: String,
        root: impl AsRef<Path>,
    ) -> Result<Self, VaultWatcherError> {
        let runtime = Handle::try_current().map_err(|_| VaultWatcherError::RuntimeUnavailable)?;
        let root = root.as_ref().to_path_buf();
        let (sender, receiver) = mpsc::channel(EVENT_QUEUE_CAPACITY);
        let overflowed = Arc::new(AtomicBool::new(false));
        let callback_overflowed = Arc::clone(&overflowed);
        let mut watcher = RecommendedWatcher::new(
            move |event| {
                if sender.try_send(event).is_err() {
                    callback_overflowed.store(true, Ordering::Release);
                }
            },
            Config::default(),
        )
        .map_err(|source| VaultWatcherError::Notify {
            path: root.clone(),
            source,
        })?;
        watcher
            .watch(&root, RecursiveMode::Recursive)
            .map_err(|source| VaultWatcherError::Notify {
                path: root.clone(),
                source,
            })?;

        let shutdown = CancellationToken::new();
        let task_shutdown = shutdown.clone();
        let task_root = root.clone();
        let task = runtime.spawn(async move {
            reconcile_events(
                storage,
                vault_id,
                task_root,
                receiver,
                overflowed,
                task_shutdown,
            )
            .await;
        });

        Ok(Self {
            _watcher: watcher,
            shutdown,
            task,
        })
    }
}

impl Drop for VaultWatcher {
    fn drop(&mut self) {
        self.shutdown.cancel();
        self.task.abort();
    }
}

#[derive(Default)]
struct WatchBatch {
    full_rescan: bool,
    paths: BTreeSet<PathBuf>,
}

impl WatchBatch {
    fn absorb(&mut self, event: notify::Result<Event>) {
        let event = match event {
            Ok(event) => event,
            Err(error) => {
                warn!(%error, "filesystem watcher reported an error; scheduling full reconciliation");
                self.full_rescan = true;
                return;
            }
        };
        if event.need_rescan() {
            self.full_rescan = true;
            return;
        }

        match event.kind {
            EventKind::Access(_) | EventKind::Other => {}
            EventKind::Modify(ModifyKind::Name(_))
            | EventKind::Create(CreateKind::Folder | CreateKind::Any)
            | EventKind::Remove(RemoveKind::Folder | RemoveKind::Any)
            | EventKind::Any => self.full_rescan = true,
            _ if event.paths.is_empty() => self.full_rescan = true,
            _ => self.paths.extend(event.paths),
        }
    }
}

async fn reconcile_events(
    storage: Arc<Storage>,
    vault_id: String,
    root: PathBuf,
    mut receiver: mpsc::Receiver<notify::Result<Event>>,
    overflowed: Arc<AtomicBool>,
    shutdown: CancellationToken,
) {
    loop {
        let first = tokio::select! {
            () = shutdown.cancelled() => return,
            event = receiver.recv() => match event {
                Some(event) => event,
                None => return,
            },
        };
        let mut batch = WatchBatch::default();
        batch.absorb(first);
        let deadline = Instant::now() + BATCH_WINDOW;
        loop {
            tokio::select! {
                () = shutdown.cancelled() => return,
                () = sleep_until(deadline) => break,
                event = receiver.recv() => match event {
                    Some(event) => batch.absorb(event),
                    None => return,
                },
            }
        }
        if overflowed.swap(false, Ordering::AcqRel) {
            warn!("filesystem watcher queue overflowed; scheduling full reconciliation");
            batch.full_rescan = true;
        }
        if !batch.full_rescan && batch.paths.is_empty() {
            continue;
        }

        let job_storage = Arc::clone(&storage);
        let job_vault_id = vault_id.clone();
        let job_root = root.clone();
        let result = tokio::task::spawn_blocking(move || {
            reconcile_batch(
                &job_storage,
                &job_vault_id,
                &job_root,
                batch.full_rescan,
                batch.paths,
            )
        })
        .await;
        match result {
            Ok(Ok(())) => {}
            Ok(Err(error)) => warn!(%error, "vault watcher reconciliation failed"),
            Err(error) => warn!(%error, "vault watcher reconciliation task failed"),
        }
    }
}

fn reconcile_batch(
    storage: &Storage,
    vault_id: &str,
    root: &Path,
    full_rescan: bool,
    paths: BTreeSet<PathBuf>,
) -> Result<(), VaultIndexError> {
    if full_rescan {
        let report = index_vault(storage, vault_id, root)?;
        debug!(
            vault_id,
            scanned = report.scanned,
            index_revision = report.index_revision,
            "fully reconciled vault after watcher hint"
        );
        return Ok(());
    }

    match reconcile_vault_paths(storage, vault_id, root, paths) {
        Ok(report) => {
            debug!(
                vault_id,
                examined = report.examined,
                changed = report.created + report.updated + report.deleted,
                index_revision = report.index_revision,
                "incrementally reconciled vault after watcher hints"
            );
            Ok(())
        }
        Err(VaultIndexError::FullRescanRequired(_)) => {
            let report = index_vault(storage, vault_id, root)?;
            debug!(
                vault_id,
                scanned = report.scanned,
                index_revision = report.index_revision,
                "fully reconciled vault after ambiguous watcher hint"
            );
            Ok(())
        }
        Err(error) => {
            warn!(%error, "incremental reconciliation failed; trying full reconciliation");
            index_vault(storage, vault_id, root)?;
            Ok(())
        }
    }
}
