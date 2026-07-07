//! Global transfer queue. Transfers run as cancellable/pausable background
//! tasks; the UI is updated purely via Tauri events (`queue-update`,
//! `transfer-progress`) and never blocks.

use crate::error::{AppError, Result};
use crate::protocol::FileProvider;
use serde::Serialize;
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{Seek, SeekFrom};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

/// Active connections shared between the command layer and the queue. Values are
/// protocol-agnostic providers so FTP/WebDAV/S3 slot in without touching the
/// queue.
pub type ConnMap = Arc<StdMutex<HashMap<String, Arc<StdMutex<dyn FileProvider>>>>>;

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
        self.emit_update();
        self.spawn(id.clone());
        id
    }

    fn spawn(&self, id: String) {
        let queue = self.clone();
        let sem = self.inner.semaphore.clone();
        // Tauri's runtime-agnostic spawner works whether the caller is an async
        // or a sync command, avoiding "no reactor running" panics.
        tauri::async_runtime::spawn(async move {
            let _permit = match sem.acquire().await {
                Ok(p) => p,
                Err(_) => return,
            };
            let queue2 = queue.clone();
            let id2 = id.clone();
            let _ = tauri::async_runtime::spawn_blocking(move || run_transfer(queue2, id2)).await;
        });
    }

    pub fn pause(&self, id: &str) {
        if let Some(c) = self.inner.controls.lock().unwrap().get(id) {
            c.paused.store(true, Ordering::Relaxed);
        }
        self.set_status(id, Status::Paused, None);
    }

    pub fn resume(&self, id: &str) {
        if let Some(c) = self.inner.controls.lock().unwrap().get(id) {
            c.paused.store(false, Ordering::Relaxed);
        }
        self.set_status(id, Status::Running, None);
    }

    pub fn cancel(&self, id: &str) {
        if let Some(c) = self.inner.controls.lock().unwrap().get(id) {
            c.cancel.cancel();
            c.paused.store(false, Ordering::Relaxed); // unblock a paused loop so it can observe cancel
        }
    }

    /// Requeues a failed or cancelled transfer with a fresh control token.
    pub fn retry(&self, id: &str) {
        let exists = {
            let mut items = self.inner.items.lock().unwrap();
            if let Some(item) = items.iter_mut().find(|i| i.id == id) {
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
                }
                !finished
            });
        }
        self.emit_update();
    }
}

fn run_transfer(queue: Queue, id: String) {
    let item = match queue.get(&id) {
        Some(i) => i,
        None => return,
    };
    let control = match queue.inner.controls.lock().unwrap().get(&id).cloned() {
        Some(c) => c,
        None => return,
    };
    if control.cancel.is_cancelled() {
        queue.set_status(&id, Status::Cancelled, None);
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

    let done = Arc::new(AtomicU64::new(item.offset));
    let result = {
        let done = done.clone();
        // The closure owns its own clones; `control`, `queue` and `id` stay
        // available in this scope for the transfer call and post-loop updates.
        let progress_control = control.clone();
        let queue2 = queue.clone();
        let id2 = id.clone();
        let mut last = Instant::now();
        let mut progress = move |delta: u64| {
            let total = done.fetch_add(delta, Ordering::Relaxed) + delta;
            // Cooperative pause: block between chunks while paused.
            while progress_control.paused.load(Ordering::Relaxed)
                && !progress_control.cancel.is_cancelled()
            {
                std::thread::sleep(Duration::from_millis(80));
            }
            if last.elapsed() >= PROGRESS_INTERVAL {
                last = Instant::now();
                queue2.update_progress(&id2, total);
            }
        };

        let mut prov = provider.lock().unwrap();
        let offset = item.offset;
        match item.direction {
            Direction::Download => open_local_dst(&item.local_path, offset).and_then(|mut f| {
                prov.download_to(
                    &item.remote_path,
                    &mut f,
                    offset,
                    &mut progress,
                    &control.cancel,
                )
            }),
            Direction::Upload => open_local_src(&item.local_path, offset).and_then(|mut f| {
                prov.upload_from(
                    &mut f,
                    &item.remote_path,
                    item.size,
                    offset,
                    &mut progress,
                    &control.cancel,
                )
            }),
        }
    };

    queue.update_progress(&id, done.load(Ordering::Relaxed));
    match result {
        Ok(()) => queue.set_status(&id, Status::Completed, None),
        Err(AppError::Cancelled) => queue.set_status(&id, Status::Cancelled, None),
        Err(e) => queue.set_status(&id, Status::Failed, Some(e.to_string())),
    }
}

/// Local destination for a download: truncating create, or append for resume.
fn open_local_dst(path: &str, offset: u64) -> Result<File> {
    if offset > 0 {
        Ok(OpenOptions::new().create(true).append(true).open(path)?)
    } else {
        Ok(File::create(path)?)
    }
}

/// Local source for an upload: opened read-only and seeked to the resume offset.
fn open_local_src(path: &str, offset: u64) -> Result<File> {
    let mut f = File::open(path)?;
    if offset > 0 {
        f.seek(SeekFrom::Start(offset))?;
    }
    Ok(f)
}
