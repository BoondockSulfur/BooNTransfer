//! Global transfer queue. Transfers run as cancellable/pausable async background
//! tasks; the UI is updated purely via Tauri events (`queue-update`,
//! `transfer-progress`) and never blocks.
//!
//! Pause is implemented as *cancel-and-remember-offset*: pausing cancels the
//! running transfer at its current byte offset and marks it `Paused`; resuming
//! restarts it from that offset (reusing the same machinery as resume/retry).

use crate::error::{AppError, Result};
use crate::protocol::local::LocalProvider;
use crate::protocol::{FileEntry, FileProvider};
use serde::Serialize;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter};
use tokio::io::AsyncSeekExt;
use tokio::sync::{Mutex as AsyncMutex, Semaphore};
use tokio_util::sync::CancellationToken;

/// Active connections shared between the command layer and the queue. Values are
/// protocol-agnostic async providers so new protocols slot in without touching
/// the queue.
pub type ConnMap = Arc<StdMutex<HashMap<String, Arc<dyn FileProvider>>>>;

const MAX_CONCURRENT: usize = 2;
const PROGRESS_INTERVAL: Duration = Duration::from_millis(120);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    Upload,
    Download,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Queued,
    Running,
    Paused,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TransferItem {
    pub id: String,
    pub connection_id: String,
    pub direction: Direction,
    pub local_path: String,
    pub remote_path: String,
    pub name: String,
    pub size: u64,
    pub transferred: u64,
    /// Resume start offset in bytes (0 = full transfer).
    pub offset: u64,
    pub status: Status,
    pub error: Option<String>,
}

struct Control {
    cancel: CancellationToken,
    paused: AtomicBool,
}

impl Control {
    fn new() -> Arc<Self> {
        Arc::new(Control {
            cancel: CancellationToken::new(),
            paused: AtomicBool::new(false),
        })
    }
}

struct Inner {
    items: StdMutex<Vec<TransferItem>>,
    controls: StdMutex<HashMap<String, Arc<Control>>>,
    /// Held by a transfer's task while it runs, so a resume/retry never starts
    /// while the previous run of the same item is still finishing its I/O.
    run_locks: StdMutex<HashMap<String, Arc<AsyncMutex<()>>>>,
    connections: ConnMap,
    app: AppHandle,
    semaphore: Arc<Semaphore>,
}

#[derive(Clone)]
pub struct Queue {
    inner: Arc<Inner>,
}

impl Queue {
    pub fn new(connections: ConnMap, app: AppHandle) -> Self {
        Queue {
            inner: Arc::new(Inner {
                items: StdMutex::new(Vec::new()),
                controls: StdMutex::new(HashMap::new()),
                run_locks: StdMutex::new(HashMap::new()),
                connections,
                app,
                semaphore: Arc::new(Semaphore::new(MAX_CONCURRENT)),
            }),
        }
    }

    pub fn snapshot(&self) -> Vec<TransferItem> {
        self.inner.items.lock().unwrap().clone()
    }

    fn get(&self, id: &str) -> Option<TransferItem> {
        self.inner
            .items
            .lock()
            .unwrap()
            .iter()
            .find(|i| i.id == id)
            .cloned()
    }

    fn emit_update(&self) {
        let snapshot = self.snapshot();
        let _ = self.inner.app.emit("queue-update", snapshot);
    }

    fn update_progress(&self, id: &str, transferred: u64) {
        if let Some(item) = self
            .inner
            .items
            .lock()
            .unwrap()
            .iter_mut()
            .find(|i| i.id == id)
        {
            item.transferred = transferred;
        }
        let _ = self
            .inner
            .app
            .emit("transfer-progress", (id.to_string(), transferred));
    }

    fn set_status(&self, id: &str, status: Status, error: Option<String>) {
        if let Some(item) = self
            .inner
            .items
            .lock()
            .unwrap()
            .iter_mut()
            .find(|i| i.id == id)
        {
            item.status = status;
            if error.is_some() {
                item.error = error;
            }
            if status == Status::Completed {
                item.transferred = item.size;
            }
        }
        self.emit_update();
    }

    /// Marks a transfer paused at `transferred` bytes so resume continues there.
    fn pause_at(&self, id: &str, transferred: u64) {
        if let Some(item) = self
            .inner
            .items
            .lock()
            .unwrap()
            .iter_mut()
            .find(|i| i.id == id)
        {
            item.status = Status::Paused;
            item.offset = transferred;
            item.transferred = transferred;
        }
        self.emit_update();
    }

    /// Records the offset a run actually starts from.
    fn set_offset(&self, id: &str, offset: u64) {
        if let Some(item) = self
            .inner
            .items
            .lock()
            .unwrap()
            .iter_mut()
            .find(|i| i.id == id)
        {
            item.offset = offset;
            item.transferred = offset;
        }
        self.emit_update();
    }

    /// Adds a file transfer and starts it (subject to the concurrency limit).
    #[allow(clippy::too_many_arguments)]
    pub fn enqueue(
        &self,
        connection_id: String,
        direction: Direction,
        local_path: String,
        remote_path: String,
        name: String,
        size: u64,
        offset: u64,
    ) -> String {
        let id = uuid::Uuid::new_v4().to_string();
        let item = TransferItem {
            id: id.clone(),
            connection_id,
            direction,
            local_path,
            remote_path,
            name,
            size,
            transferred: offset,
            offset,
            status: Status::Queued,
            error: None,
        };
        self.inner.items.lock().unwrap().push(item);
        self.inner
            .controls
            .lock()
            .unwrap()
            .insert(id.clone(), Control::new());
        self.inner
            .run_locks
            .lock()
            .unwrap()
            .insert(id.clone(), Arc::new(AsyncMutex::new(())));
        self.emit_update();
        self.spawn(id.clone());
        id
    }

    fn status_of(&self, id: &str) -> Option<Status> {
        self.get(id).map(|i| i.status)
    }

    fn spawn(&self, id: String) {
        let queue = self.clone();
        let sem = self.inner.semaphore.clone();
        let run_lock = self.inner.run_locks.lock().unwrap().get(&id).cloned();
        tauri::async_runtime::spawn(async move {
            let _running = match run_lock {
                Some(l) => l.lock_owned().await,
                None => return,
            };
            let _permit = match sem.acquire().await {
                Ok(p) => p,
                Err(_) => return,
            };
            run_transfer(queue, id).await;
        });
    }

    /// Restarts a transfer from its recorded offset with a fresh control token.
    fn restart(&self, id: &str) {
        let exists = {
            let mut items = self.inner.items.lock().unwrap();
            let restartable =
                |s: Status| matches!(s, Status::Paused | Status::Failed | Status::Cancelled);
            if let Some(item) = items
                .iter_mut()
                .find(|i| i.id == id && restartable(i.status))
            {
                item.status = Status::Queued;
                item.transferred = item.offset;
                item.error = None;
                true
            } else {
                false
            }
        };
        if exists {
            self.inner
                .controls
                .lock()
                .unwrap()
                .insert(id.to_string(), Control::new());
            self.emit_update();
            self.spawn(id.to_string());
        }
    }

    pub fn pause(&self, id: &str) {
        if !matches!(self.status_of(id), Some(Status::Queued | Status::Running)) {
            return;
        }
        if let Some(c) = self.inner.controls.lock().unwrap().get(id) {
            c.paused.store(true, Ordering::Relaxed);
            c.cancel.cancel();
        }
        self.set_status(id, Status::Paused, None);
    }

    pub fn resume(&self, id: &str) {
        self.restart(id);
    }

    pub fn cancel(&self, id: &str) {
        if let Some(c) = self.inner.controls.lock().unwrap().get(id) {
            c.paused.store(false, Ordering::Relaxed);
            c.cancel.cancel();
        }
        // A paused item has no running task that would report the cancel.
        if self.status_of(id) == Some(Status::Paused) {
            self.set_status(id, Status::Cancelled, None);
        }
    }

    pub fn retry(&self, id: &str) {
        self.restart(id);
    }

    pub fn clear_finished(&self) {
        {
            let mut items = self.inner.items.lock().unwrap();
            let mut controls = self.inner.controls.lock().unwrap();
            items.retain(|i| {
                let finished = matches!(
                    i.status,
                    Status::Completed | Status::Failed | Status::Cancelled
                );
                if finished {
                    controls.remove(&i.id);
                    self.inner.run_locks.lock().unwrap().remove(&i.id);
                }
                !finished
            });
        }
        self.emit_update();
    }
}

async fn run_transfer(queue: Queue, id: String) {
    let item = match queue.get(&id) {
        Some(i) => i,
        None => return,
    };
    let control = match queue.inner.controls.lock().unwrap().get(&id).cloned() {
        Some(c) => c,
        None => return,
    };
    if control.cancel.is_cancelled() {
        // Paused while still queued: stays paused at its offset.
        if !control.paused.load(Ordering::Relaxed) {
            queue.set_status(&id, Status::Cancelled, None);
        }
        return;
    }
    let provider = match queue
        .inner
        .connections
        .lock()
        .unwrap()
        .get(&item.connection_id)
        .cloned()
    {
        Some(p) => p,
        None => {
            queue.set_status(&id, Status::Failed, Some("connection not found".into()));
            return;
        }
    };

    queue.set_status(&id, Status::Running, None);

    let offset = resume_offset(provider.as_ref(), &item).await;
    if offset != item.offset {
        queue.set_offset(&id, offset);
    }
    let done = Arc::new(AtomicU64::new(offset));
    let result: Result<()> = {
        let done2 = done.clone();
        let queue2 = queue.clone();
        let id2 = id.clone();
        let mut last = Instant::now();
        let mut progress = move |delta: u64| {
            let total = done2.fetch_add(delta, Ordering::Relaxed) + delta;
            if last.elapsed() >= PROGRESS_INTERVAL {
                last = Instant::now();
                queue2.update_progress(&id2, total);
            }
        };

        match item.direction {
            Direction::Download => match open_local_dst(&item.local_path, offset).await {
                Ok(mut f) => {
                    provider
                        .download_to(
                            &item.remote_path,
                            &mut f,
                            offset,
                            &mut progress,
                            &control.cancel,
                        )
                        .await
                }
                Err(e) => Err(e),
            },
            Direction::Upload => match open_local_src(&item.local_path, offset).await {
                Ok(mut f) => {
                    provider
                        .upload_from(
                            &mut f,
                            &item.remote_path,
                            item.size,
                            offset,
                            &mut progress,
                            &control.cancel,
                        )
                        .await
                }
                Err(e) => Err(e),
            },
        }
    };

    let final_done = done.load(Ordering::Relaxed);
    queue.update_progress(&id, final_done);
    match result {
        Ok(()) => {
            preserve_mtime(provider.as_ref(), &item).await;
            queue.set_status(&id, Status::Completed, None)
        }
        Err(AppError::Cancelled) => {
            if control.paused.load(Ordering::Relaxed) {
                queue.pause_at(&id, final_done);
            } else {
                queue.set_status(&id, Status::Cancelled, None);
            }
        }
        Err(e) => queue.set_status(&id, Status::Failed, Some(e.to_string())),
    }
}

/// Where a resumed transfer really continues: the size of what already arrived
/// at the destination, not the progress counter (which can run ahead of data
/// that was written). Uploads to providers without resume restart at 0.
async fn resume_offset(provider: &dyn FileProvider, item: &TransferItem) -> u64 {
    if item.offset == 0 {
        return 0;
    }
    let present = match item.direction {
        Direction::Upload if !provider.supports_upload_resume() => return 0,
        Direction::Upload => provider.stat(&item.remote_path).await.map(|e| e.size),
        Direction::Download => tokio::fs::metadata(&item.local_path)
            .await
            .map(|m| m.len())
            .map_err(Into::into),
    };
    present.unwrap_or(0).min(item.size)
}

/// Copies the source's modification time to the destination (best effort), so
/// directory sync does not see a freshly transferred file as "newer".
async fn preserve_mtime(provider: &dyn FileProvider, item: &TransferItem) {
    let local = LocalProvider::new();
    let res = match item.direction {
        Direction::Upload => match local.stat(&item.local_path).await {
            Ok(FileEntry {
                modified: Some(t), ..
            }) => provider.set_mtime(&item.remote_path, t).await,
            other => other.map(|_| ()),
        },
        Direction::Download => match provider.stat(&item.remote_path).await {
            Ok(FileEntry {
                modified: Some(t), ..
            }) => local.set_mtime(&item.local_path, t).await,
            other => other.map(|_| ()),
        },
    };
    if let Err(e) = res {
        tracing::debug!("could not preserve mtime for {}: {e}", item.name);
    }
}

/// Local destination for a download: truncating create, or append for resume.
async fn open_local_dst(path: &str, offset: u64) -> Result<tokio::fs::File> {
    if offset > 0 {
        Ok(tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .await?)
    } else {
        Ok(tokio::fs::File::create(path).await?)
    }
}

/// Local source for an upload: opened read-only and seeked to the resume offset.
async fn open_local_src(path: &str, offset: u64) -> Result<tokio::fs::File> {
    let mut f = tokio::fs::File::open(path).await?;
    if offset > 0 {
        f.seek(std::io::SeekFrom::Start(offset)).await?;
    }
    Ok(f)
}
