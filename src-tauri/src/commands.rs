//! Tauri command layer — a thin, serializable façade over the async core.

use crate::error::{AppError, Result};
use crate::protocol::ftp::FtpProvider;
use crate::protocol::s3::S3Provider;
use crate::protocol::sftp::{AuthMethod, ConnectParams, SftpProvider};
use crate::protocol::webdav::WebdavProvider;
use crate::protocol::{self, local::LocalProvider, FileEntry, FileProvider};
use crate::session::{AuthType, Protocol, SessionInfo};
use crate::transfer::{Direction, TransferItem};
use crate::{secrets, AppState};
use serde::Serialize;
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use tauri::State;
use tokio_util::sync::CancellationToken;

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn conn(state: &AppState, id: &str) -> Result<Arc<dyn FileProvider>> {
    state
        .connections
        .lock()
        .unwrap()
        .get(id)
        .cloned()
        .ok_or_else(|| AppError::NotConnected(id.to_string()))
}

fn local_join(dir: &str, name: &str) -> String {
    Path::new(dir).join(name).to_string_lossy().into_owned()
}

fn basename(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string())
}

/// Runs blocking work (keyring) off the async runtime.
async fn blocking<T, F>(f: F) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T> + Send + 'static,
{
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| AppError::other(e.to_string()))?
}

/// In-memory `AsyncWrite` used to collect small remote files (text editor).
struct MemWriter(Vec<u8>);

impl tokio::io::AsyncWrite for MemWriter {
    fn poll_write(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        self.0.extend_from_slice(buf);
        Poll::Ready(Ok(buf.len()))
    }
    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }
    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

struct FilePlan {
    local: String,
    remote: String,
    name: String,
    size: u64,
}

// ---------------------------------------------------------------------------
// sessions
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn list_sessions(state: State<'_, AppState>) -> Vec<SessionInfo> {
    state.sessions.list()
}

#[tauri::command]
pub async fn save_session(
    state: State<'_, AppState>,
    mut session: SessionInfo,
    password: Option<String>,
    passphrase: Option<String>,
) -> Result<String> {
    if session.id.trim().is_empty() {
        session.id = uuid::Uuid::new_v4().to_string();
    }
    let id = session.id.clone();
    state.sessions.upsert(session)?;

    let id_c = id.clone();
    blocking(move || -> Result<()> {
        if let Some(pw) = password {
            if !pw.is_empty() {
                secrets::set(&id_c, secrets::KIND_PASSWORD, &pw)?;
            }
        }
        if let Some(pp) = passphrase {
            if !pp.is_empty() {
                secrets::set(&id_c, secrets::KIND_PASSPHRASE, &pp)?;
            }
        }
        Ok(())
    })
    .await?;
    Ok(id)
}

#[tauri::command]
pub async fn delete_session(state: State<'_, AppState>, id: String) -> Result<()> {
    // Open tabs of this session keep their live connection until closed.
    let id_c = id.clone();
    blocking(move || secrets::delete_all(&id_c)).await?;
    state.sessions.delete(&id)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// connection lifecycle
// ---------------------------------------------------------------------------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectResult {
    pub connection_id: String,
    pub home_dir: String,
    pub remote_dir: Option<String>,
    pub local_dir: Option<String>,
}

async fn secret(id: &str, kind: &'static str) -> Result<Option<String>> {
    let id = id.to_string();
    blocking(move || secrets::get(&id, kind)).await
}

#[tauri::command]
pub async fn connect(
    state: State<'_, AppState>,
    session_id: String,
    trusted_fingerprint: Option<String>,
) -> Result<ConnectResult> {
    let sess = state
        .sessions
        .get(&session_id)
        .ok_or_else(|| AppError::other("session not found"))?;

    let mut remote_dir = sess.remote_dir.clone();
    let local_dir = sess.local_dir.clone();

    let provider: Arc<dyn FileProvider> = match sess.protocol {
        Protocol::Sftp | Protocol::Scp => {
            let auth = match sess.auth_type {
                AuthType::Password => {
                    let pw = secret(&session_id, secrets::KIND_PASSWORD)
                        .await?
                        .ok_or_else(|| AppError::other("no stored password for this session"))?;
                    AuthMethod::Password(pw)
                }
                AuthType::Key => {
                    let path = sess
                        .key_path
                        .clone()
                        .ok_or_else(|| AppError::other("no key file configured"))?;
                    let passphrase = secret(&session_id, secrets::KIND_PASSPHRASE).await?;
                    AuthMethod::KeyFile { path, passphrase }
                }
                AuthType::Agent => AuthMethod::Agent,
            };
            let params = ConnectParams {
                host: sess.host.clone(),
                port: sess.port,
                username: sess.username.clone(),
                auth,
                trusted_fingerprint,
            };
            Arc::new(SftpProvider::connect(params).await?)
        }
        Protocol::Ftp | Protocol::Ftps => {
            let pass = secret(&session_id, secrets::KIND_PASSWORD)
                .await?
                .unwrap_or_default();
            Arc::new(
                FtpProvider::connect(
                    &sess.host,
                    sess.port,
                    &sess.username,
                    &pass,
                    sess.protocol == Protocol::Ftps,
                )
                .await?,
            )
        }
        Protocol::Webdav | Protocol::Webdavs => {
            let pass = secret(&session_id, secrets::KIND_PASSWORD)
                .await?
                .unwrap_or_default();
            // The "remote start folder" doubles as the WebDAV base path; it is
            // baked into the provider, so the pane itself opens at "/".
            let root = sess.remote_dir.clone().unwrap_or_default();
            let provider = WebdavProvider::connect(
                &sess.host,
                sess.port,
                &sess.username,
                &pass,
                sess.protocol == Protocol::Webdavs,
                &root,
            )
            .await?;
            remote_dir = None;
            Arc::new(provider)
        }
        Protocol::S3 => {
            let secret = secret(&session_id, secrets::KIND_PASSWORD)
                .await?
                .unwrap_or_default();
            let bucket = sess.bucket.clone().unwrap_or_default();
            let region = sess.region.clone().unwrap_or_default();
            let provider = S3Provider::connect(
                &sess.host,
                sess.port,
                &sess.username,
                &secret,
                // Older sessions have no explicit setting: https only on 443.
                sess.use_tls.unwrap_or(sess.port == 443),
                &bucket,
                &region,
            )
            .await?;
            remote_dir = None;
            Arc::new(provider)
        }
    };

    let home = provider.home_dir().await?;

    // A unique id per connection lets the UI open the same session in several tabs.
    let connection_id = uuid::Uuid::new_v4().to_string();
    state
        .connections
        .lock()
        .unwrap()
        .insert(connection_id.clone(), provider);
    let _ = state.sessions.touch(&session_id, now_secs());

    Ok(ConnectResult {
        connection_id,
        home_dir: home,
        remote_dir,
        local_dir,
    })
}

#[tauri::command]
pub async fn disconnect(state: State<'_, AppState>, connection_id: String) -> Result<()> {
    state.connections.lock().unwrap().remove(&connection_id);
    // Shells keep the SSH session alive, so close the ones of this connection.
    let shells: Vec<_> = {
        let mut terms = state.terminals.lock().unwrap();
        let ids: Vec<String> = terms
            .iter()
            .filter(|(_, t)| t.connection_id == connection_id)
            .map(|(id, _)| id.clone())
            .collect();
        ids.into_iter().filter_map(|id| terms.remove(&id)).collect()
    };
    for t in shells {
        let _ = t.handle.close().await;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// local filesystem
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn local_home() -> String {
    dirs::home_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| "/".to_string())
}

#[tauri::command]
pub async fn list_local(path: String) -> Result<Vec<FileEntry>> {
    LocalProvider::new().list_dir(&path).await
}

#[tauri::command]
pub async fn make_dir_local(path: String) -> Result<()> {
    LocalProvider::new().make_dir(&path).await
}

#[tauri::command]
pub async fn delete_local(path: String, is_dir: bool) -> Result<()> {
    let lp = LocalProvider::new();
    if is_dir {
        lp.remove_dir(&path).await
    } else {
        lp.remove_file(&path).await
    }
}

#[tauri::command]
pub async fn rename_local(from: String, to: String) -> Result<()> {
    LocalProvider::new().rename(&from, &to).await
}

#[tauri::command]
pub async fn set_permissions_local(path: String, mode: u32) -> Result<()> {
    LocalProvider::new().set_permissions(&path, mode).await
}

// ---------------------------------------------------------------------------
// remote filesystem
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn list_remote(
    state: State<'_, AppState>,
    connection_id: String,
    path: String,
) -> Result<Vec<FileEntry>> {
    let mut entries = conn(&state, &connection_id)?.list_dir(&path).await?;
    entries.retain(|e| {
        let ok = protocol::is_safe_name(&e.name);
        if !ok {
            tracing::warn!("ignoring unsafe remote entry name {:?} in {path}", e.name);
        }
        ok
    });
    Ok(entries)
}

#[tauri::command]
pub async fn make_dir_remote(
    state: State<'_, AppState>,
    connection_id: String,
    path: String,
) -> Result<()> {
    conn(&state, &connection_id)?.make_dir(&path).await
}

#[tauri::command]
pub async fn delete_remote(
    state: State<'_, AppState>,
    connection_id: String,
    path: String,
    is_dir: bool,
) -> Result<()> {
    let p = conn(&state, &connection_id)?;
    if is_dir {
        p.remove_dir(&path).await
    } else {
        p.remove_file(&path).await
    }
}

#[tauri::command]
pub async fn rename_remote(
    state: State<'_, AppState>,
    connection_id: String,
    from: String,
    to: String,
) -> Result<()> {
    conn(&state, &connection_id)?.rename(&from, &to).await
}

#[tauri::command]
pub async fn set_permissions_remote(
    state: State<'_, AppState>,
    connection_id: String,
    path: String,
    mode: u32,
) -> Result<()> {
    conn(&state, &connection_id)?
        .set_permissions(&path, mode)
        .await
}

// ---------------------------------------------------------------------------
// transfers
// ---------------------------------------------------------------------------

fn plan_upload(local_path: &str, remote_dir: &str) -> Result<Vec<FilePlan>> {
    let mut files = Vec::new();
    let meta = std::fs::symlink_metadata(local_path)?;
    let name = basename(local_path);
    if meta.is_dir() {
        let remote_root = protocol::posix_join(remote_dir, &name);
        walk_local(local_path, &remote_root, &mut files)?;
    } else {
        files.push(FilePlan {
            local: local_path.to_string(),
            remote: protocol::posix_join(remote_dir, &name),
            name,
            size: meta.len(),
        });
    }
    Ok(files)
}

fn walk_local(local_dir: &str, remote_dir: &str, files: &mut Vec<FilePlan>) -> Result<()> {
    for entry in std::fs::read_dir(local_dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let child_local = entry.path();
        let child_local_str = child_local.to_string_lossy().into_owned();
        let child_remote = protocol::posix_join(remote_dir, &name);
        let meta = std::fs::symlink_metadata(&child_local)?;
        if meta.file_type().is_symlink() {
            continue;
        }
        if meta.is_dir() {
            walk_local(&child_local_str, &child_remote, files)?;
        } else {
            files.push(FilePlan {
                local: child_local_str,
                remote: child_remote,
                name,
                size: meta.len(),
            });
        }
    }
    Ok(())
}

async fn plan_download(
    provider: &dyn FileProvider,
    remote_path: &str,
    local_dir: &str,
) -> Result<Vec<FilePlan>> {
    let mut files = Vec::new();
    let stat = provider.stat(remote_path).await?;
    let name = protocol::posix_basename(remote_path);
    if stat.is_dir {
        let local_root = local_join(local_dir, &name);
        walk_remote(provider, remote_path, &local_root, &mut files).await?;
    } else {
        files.push(FilePlan {
            local: local_join(local_dir, &name),
            remote: remote_path.to_string(),
            name,
            size: stat.size,
        });
    }
    Ok(files)
}

fn walk_remote<'a>(
    provider: &'a dyn FileProvider,
    remote_dir: &'a str,
    local_dir: &'a str,
    files: &'a mut Vec<FilePlan>,
) -> Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + 'a>> {
    Box::pin(async move {
        for entry in provider.list_dir(remote_dir).await? {
            // Server-supplied names become local paths: never let one escape.
            if !protocol::is_safe_name(&entry.name) {
                tracing::warn!("skipping unsafe remote name {:?}", entry.name);
                continue;
            }
            let child_remote = protocol::posix_join(remote_dir, &entry.name);
            let child_local = local_join(local_dir, &entry.name);
            if entry.is_symlink {
                continue;
            }
            if entry.is_dir {
                walk_remote(provider, &child_remote, &child_local, files).await?;
            } else {
                files.push(FilePlan {
                    local: child_local,
                    remote: child_remote,
                    name: entry.name,
                    size: entry.size,
                });
            }
        }
        Ok(())
    })
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct PlannedFile {
    pub local: String,
    pub remote: String,
    pub name: String,
    pub size: u64,
    pub exists: bool,
    pub dest_size: u64,
}

type Finalized = (String, String, String, u64, u64);

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedFile {
    pub local: String,
    pub remote: String,
    pub name: String,
    pub size: u64,
    /// "overwrite" | "skip" | "rename" | "resume"
    pub policy: String,
    pub dest_size: u64,
}

/// Step 1: expand the selection into files and flag destination conflicts.
#[tauri::command]
pub async fn plan_transfers(
    state: State<'_, AppState>,
    connection_id: String,
    direction: String,
    paths: Vec<String>,
    dest_dir: String,
) -> Result<Vec<PlannedFile>> {
    let p = conn(&state, &connection_id)?;
    let is_upload = direction == "upload";
    let mut out = Vec::new();

    if is_upload {
        for lp in &paths {
            for f in plan_upload(lp, &dest_dir)? {
                let (exists, dest_size) = match p.stat(&f.remote).await {
                    Ok(s) => (true, s.size),
                    Err(_) => (false, 0),
                };
                out.push(PlannedFile {
                    local: f.local,
                    remote: f.remote,
                    name: f.name,
                    size: f.size,
                    exists,
                    dest_size,
                });
            }
        }
    } else {
        for rp in &paths {
            for f in plan_download(p.as_ref(), rp, &dest_dir).await? {
                let (exists, dest_size) = match std::fs::metadata(&f.local) {
                    Ok(m) => (true, m.len()),
                    Err(_) => (false, 0),
                };
                out.push(PlannedFile {
                    local: f.local,
                    remote: f.remote,
                    name: f.name,
                    size: f.size,
                    exists,
                    dest_size,
                });
            }
        }
    }
    Ok(out)
}

/// Step 2: apply conflict policies, create destination dirs, enqueue transfers.
#[tauri::command]
pub async fn enqueue_transfers(
    state: State<'_, AppState>,
    connection_id: String,
    direction: String,
    dest_dir: String,
    files: Vec<ResolvedFile>,
) -> Result<usize> {
    let p = conn(&state, &connection_id)?;
    let is_upload = direction == "upload";

    let mut finalized: Vec<Finalized> = Vec::new();
    for f in files {
        if f.policy == "skip" {
            continue;
        }
        let mut local = f.local.clone();
        let mut remote = f.remote.clone();
        let mut offset = 0u64;
        match f.policy.as_str() {
            "resume" => offset = f.dest_size.min(f.size),
            "rename" => {
                if is_upload {
                    remote = unique_remote(p.as_ref(), &remote).await?;
                } else {
                    local = unique_local(&local);
                }
            }
            _ => {}
        }
        if is_upload {
            ensure_remote_tree(p.as_ref(), &dest_dir, &remote).await;
        } else if let Some(parent) = std::path::Path::new(&local).parent() {
            std::fs::create_dir_all(parent)?;
        }
        finalized.push((local, remote, f.name, f.size, offset));
    }

    let count = finalized.len();
    let dir = if is_upload {
        Direction::Upload
    } else {
        Direction::Download
    };
    for (local, remote, name, size, offset) in finalized {
        state.queue.enqueue(
            connection_id.clone(),
            dir,
            local,
            remote,
            name,
            size,
            offset,
        );
    }
    Ok(count)
}

/// Creates every intermediate remote directory between `dest_dir` and the file.
async fn ensure_remote_tree(g: &dyn FileProvider, dest_dir: &str, remote_file: &str) {
    let parent = protocol::posix_parent(remote_file);
    let base = protocol::normalize_posix(dest_dir);
    let Some(rel) = parent.strip_prefix(&base) else {
        let _ = g.make_dir(&parent).await;
        return;
    };
    let mut cur = base;
    for comp in rel.split('/').filter(|c| !c.is_empty()) {
        cur = protocol::posix_join(&cur, comp);
        let _ = g.make_dir(&cur).await;
    }
}

fn split_ext(path: &str) -> (String, String) {
    let name = basename(path);
    let dir = &path[..path.len() - name.len()];
    match name.rfind('.') {
        Some(i) if i > 0 => (format!("{dir}{}", &name[..i]), name[i..].to_string()),
        _ => (path.to_string(), String::new()),
    }
}

async fn unique_remote(g: &dyn FileProvider, path: &str) -> Result<String> {
    if g.stat(path).await.is_err() {
        return Ok(path.to_string());
    }
    let (stem, ext) = split_ext(path);
    for i in 1..1000 {
        let cand = format!("{stem} ({i}){ext}");
        if g.stat(&cand).await.is_err() {
            return Ok(cand);
        }
    }
    Err(AppError::other("could not find a free remote name"))
}

fn unique_local(path: &str) -> String {
    if !std::path::Path::new(path).exists() {
        return path.to_string();
    }
    let (stem, ext) = split_ext(path);
    for i in 1..1000 {
        let cand = format!("{stem} ({i}){ext}");
        if !std::path::Path::new(&cand).exists() {
            return cand;
        }
    }
    path.to_string()
}

#[tauri::command]
pub fn list_transfers(state: State<'_, AppState>) -> Vec<TransferItem> {
    state.queue.snapshot()
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VerifyResult {
    pub local: Option<String>,
    pub remote: Option<String>,
    pub matches: bool,
    pub supported: bool,
}

#[tauri::command]
pub async fn verify_transfer(
    state: State<'_, AppState>,
    connection_id: String,
    local_path: String,
    remote_path: String,
) -> Result<VerifyResult> {
    let p = conn(&state, &connection_id)?;
    let local = LocalProvider::new().checksum_sha256(&local_path).await?;
    let remote = p.checksum_sha256(&remote_path).await?;
    let supported = local.is_some() && remote.is_some();
    let matches = supported && local == remote;
    Ok(VerifyResult {
        local,
        remote,
        matches,
        supported,
    })
}

// ---------------------------------------------------------------------------
// directory synchronisation
// ---------------------------------------------------------------------------

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SyncItem {
    pub direction: String,
    pub local: String,
    pub remote: String,
    pub name: String,
    pub size: u64,
    pub reason: String,
}

type FileMap = std::collections::HashMap<String, (String, u64, i64)>;

fn build_local_map(root: &str, prefix: &str, map: &mut FileMap) -> Result<()> {
    let entries = match std::fs::read_dir(root) {
        Ok(e) => e,
        Err(_) => return Ok(()),
    };
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let meta = std::fs::symlink_metadata(entry.path())?;
        if meta.file_type().is_symlink() {
            continue;
        }
        let rel = if prefix.is_empty() {
            name
        } else {
            format!("{prefix}/{name}")
        };
        let abs = entry.path().to_string_lossy().into_owned();
        if meta.is_dir() {
            build_local_map(&abs, &rel, map)?;
        } else {
            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            map.insert(rel, (abs, meta.len(), mtime));
        }
    }
    Ok(())
}

fn build_remote_map<'a>(
    provider: &'a dyn FileProvider,
    root: &'a str,
    prefix: &'a str,
    map: &'a mut FileMap,
) -> Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + 'a>> {
    Box::pin(async move {
        for e in provider.list_dir(root).await? {
            if e.is_symlink || !protocol::is_safe_name(&e.name) {
                continue;
            }
            let rel = if prefix.is_empty() {
                e.name.clone()
            } else {
                format!("{prefix}/{}", e.name)
            };
            let abs = protocol::posix_join(root, &e.name);
            if e.is_dir {
                build_remote_map(provider, &abs, &rel, map).await?;
            } else {
                map.insert(rel, (abs, e.size, e.modified.unwrap_or(0)));
            }
        }
        Ok(())
    })
}

#[tauri::command]
pub async fn plan_sync(
    state: State<'_, AppState>,
    connection_id: String,
    local_dir: String,
    remote_dir: String,
    direction: String,
) -> Result<Vec<SyncItem>> {
    let p = conn(&state, &connection_id)?;
    let mut local = FileMap::new();
    build_local_map(&local_dir, "", &mut local)?;
    let mut remote = FileMap::new();
    build_remote_map(p.as_ref(), &remote_dir, "", &mut remote).await?;

    let want_up = direction == "upload" || direction == "both";
    let want_down = direction == "download" || direction == "both";
    // Timestamps only decide when the remote side keeps them on upload;
    // otherwise every uploaded file would look "newer" on the next run.
    let trust_mtime = p.can_set_mtime();

    let mut keys: std::collections::HashSet<String> = std::collections::HashSet::new();
    keys.extend(local.keys().cloned());
    keys.extend(remote.keys().cloned());

    let mut items = Vec::new();
    for rel in keys {
        let l = local.get(&rel);
        let r = remote.get(&rel);
        let (dir, reason, size, local_path, remote_path) = match (l, r) {
            (Some((la, ls, lm)), Some((ra, rs, rm))) => {
                let comparable = trust_mtime && *lm > 0 && *rm > 0;
                let same_time = !comparable || (lm - rm).abs() <= 2;
                if ls == rs && same_time {
                    continue;
                }
                let up = if same_time { want_up } else { lm > rm };
                let reason = if ls != rs { "size" } else { "newer" };
                if up {
                    (
                        "upload",
                        reason,
                        *ls,
                        la.clone(),
                        protocol::posix_join(&remote_dir, &rel),
                    )
                } else {
                    (
                        "download",
                        reason,
                        *rs,
                        local_join(&local_dir, &rel),
                        ra.clone(),
                    )
                }
            }
            (Some((la, ls, _)), None) => (
                "upload",
                "new",
                *ls,
                la.clone(),
                protocol::posix_join(&remote_dir, &rel),
            ),
            (None, Some((ra, rs, _))) => (
                "download",
                "new",
                *rs,
                local_join(&local_dir, &rel),
                ra.clone(),
            ),
            (None, None) => continue,
        };
        if (dir == "upload" && !want_up) || (dir == "download" && !want_down) {
            continue;
        }
        items.push(SyncItem {
            direction: dir.to_string(),
            local: local_path,
            remote: remote_path,
            name: rel,
            size,
            reason: reason.to_string(),
        });
    }
    items.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(items)
}

// ---------------------------------------------------------------------------
// built-in text editor (read/write file contents)
// ---------------------------------------------------------------------------

const MAX_EDIT_BYTES: u64 = 8 * 1024 * 1024;

fn looks_binary(bytes: &[u8]) -> bool {
    bytes.iter().take(8000).any(|&b| b == 0)
}

fn decode_text(bytes: Vec<u8>) -> Result<String> {
    if looks_binary(&bytes) {
        return Err(AppError::other(
            "Binärdatei – kann nicht als Text bearbeitet werden",
        ));
    }
    String::from_utf8(bytes).map_err(|_| AppError::other("Keine gültige UTF-8-Textdatei"))
}

#[tauri::command]
pub async fn read_text(
    state: State<'_, AppState>,
    connection_id: Option<String>,
    path: String,
    is_local: bool,
) -> Result<String> {
    if is_local {
        let meta = tokio::fs::metadata(&path).await?;
        if meta.len() > MAX_EDIT_BYTES {
            return Err(AppError::other("Datei zu groß zum Bearbeiten (> 8 MB)"));
        }
        decode_text(tokio::fs::read(&path).await?)
    } else {
        let cid = connection_id.ok_or_else(|| AppError::other("no connection"))?;
        let p = conn(&state, &cid)?;
        if p.stat(&path).await?.size > MAX_EDIT_BYTES {
            return Err(AppError::other("Datei zu groß zum Bearbeiten (> 8 MB)"));
        }
        let mut w = MemWriter(Vec::new());
        let mut noop = |_: u64| {};
        let cancel = CancellationToken::new();
        p.download_to(&path, &mut w, 0, &mut noop, &cancel).await?;
        decode_text(w.0)
    }
}

#[tauri::command]
pub async fn write_text(
    state: State<'_, AppState>,
    connection_id: Option<String>,
    path: String,
    is_local: bool,
    content: String,
) -> Result<()> {
    let bytes = content.into_bytes();
    if is_local {
        tokio::fs::write(&path, &bytes).await?;
        Ok(())
    } else {
        let cid = connection_id.ok_or_else(|| AppError::other("no connection"))?;
        let p = conn(&state, &cid)?;
        let size = bytes.len() as u64;
        let mut cur = std::io::Cursor::new(bytes);
        let mut noop = |_: u64| {};
        let cancel = CancellationToken::new();
        p.upload_from(&mut cur, &path, size, 0, &mut noop, &cancel)
            .await
    }
}

#[tauri::command]
pub fn transfer_pause(state: State<'_, AppState>, id: String) {
    state.queue.pause(&id);
}

#[tauri::command]
pub fn transfer_resume(state: State<'_, AppState>, id: String) {
    state.queue.resume(&id);
}

#[tauri::command]
pub fn transfer_cancel(state: State<'_, AppState>, id: String) {
    state.queue.cancel(&id);
}

#[tauri::command]
pub fn transfer_retry(state: State<'_, AppState>, id: String) {
    state.queue.retry(&id);
}

#[tauri::command]
pub fn clear_finished(state: State<'_, AppState>) {
    state.queue.clear_finished();
}

// ---------------------------------------------------------------------------
// interactive terminal (SSH)
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn terminal_open(
    state: State<'_, AppState>,
    app: tauri::AppHandle,
    connection_id: String,
    terminal_id: String,
    cols: u32,
    rows: u32,
) -> Result<()> {
    let provider = conn(&state, &connection_id)?;
    let session = provider.open_terminal(cols, rows).await?;
    let mut output = session.output;
    state.terminals.lock().unwrap().insert(
        terminal_id.clone(),
        crate::TermEntry {
            connection_id,
            handle: Arc::from(session.handle),
        },
    );
    // Forward shell output to the frontend as per-terminal events.
    tauri::async_runtime::spawn(async move {
        use tauri::{Emitter, Manager};
        while let Some(ev) = output.recv().await {
            match ev {
                protocol::TerminalEvent::Data(d) => {
                    let _ = app.emit(&format!("terminal-output:{terminal_id}"), d);
                }
                protocol::TerminalEvent::Exit => {
                    let _ = app.emit(&format!("terminal-exit:{terminal_id}"), ());
                    break;
                }
            }
        }
        app.state::<AppState>()
            .terminals
            .lock()
            .unwrap()
            .remove(&terminal_id);
    });
    Ok(())
}

#[tauri::command]
pub async fn terminal_write(
    state: State<'_, AppState>,
    terminal_id: String,
    data: Vec<u8>,
) -> Result<()> {
    let term = state
        .terminals
        .lock()
        .unwrap()
        .get(&terminal_id)
        .map(|t| t.handle.clone());
    if let Some(t) = term {
        t.write(&data).await?;
    }
    Ok(())
}

#[tauri::command]
pub async fn terminal_resize(
    state: State<'_, AppState>,
    terminal_id: String,
    cols: u32,
    rows: u32,
) -> Result<()> {
    let term = state
        .terminals
        .lock()
        .unwrap()
        .get(&terminal_id)
        .map(|t| t.handle.clone());
    if let Some(t) = term {
        t.resize(cols, rows).await?;
    }
    Ok(())
}

#[tauri::command]
pub async fn terminal_close(state: State<'_, AppState>, terminal_id: String) -> Result<()> {
    let term = state.terminals.lock().unwrap().remove(&terminal_id);
    if let Some(t) = term {
        t.handle.close().await?;
    }
    Ok(())
}
