//! Tauri command layer — a thin, serializable façade over the core. No business
//! logic lives here beyond marshalling and dispatching to background work.

use crate::error::{AppError, Result};
use crate::protocol::ftp::FtpProvider;
use crate::protocol::sftp::{AuthMethod, ConnectParams, SftpProvider};
use crate::protocol::{self, local::LocalProvider, FileEntry, FileProvider};
use crate::session::{AuthType, Protocol, SessionInfo};
use crate::transfer::{Direction, TransferItem};
use crate::{secrets, AppState};
use serde::Serialize;
use std::path::Path;
use std::sync::{Arc, Mutex as StdMutex};
use tauri::{Emitter, State};

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn conn(state: &AppState, id: &str) -> Result<Arc<StdMutex<dyn FileProvider>>> {
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

async fn blocking<T, F>(f: F) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T> + Send + 'static,
{
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| AppError::other(e.to_string()))?
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

    // Keyring access must happen off the async runtime thread.
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
    state.connections.lock().unwrap().remove(&id);
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

#[tauri::command]
pub async fn connect(
    state: State<'_, AppState>,
    session_id: String,
    trust_host_key: Option<bool>,
) -> Result<ConnectResult> {
    let sess = state
        .sessions
        .get(&session_id)
        .ok_or_else(|| AppError::other("session not found"))?;

    let trust = trust_host_key.unwrap_or(false);
    let remote_dir = sess.remote_dir.clone();
    let local_dir = sess.local_dir.clone();
    let session_id_c = session_id.clone();

    // Connecting (incl. keyring lookups) happens on a blocking thread — the
    // keyring sync facade must not run inside the async runtime. Each protocol
    // builds a boxed FileProvider so the rest of the app stays protocol-agnostic.
    let (provider, home) = blocking(
        move || -> Result<(Arc<StdMutex<dyn FileProvider>>, String)> {
            match sess.protocol {
                Protocol::Sftp | Protocol::Scp => {
                    let auth = match sess.auth_type {
                        AuthType::Password => {
                            let pw = secrets::get(&session_id_c, secrets::KIND_PASSWORD)?
                                .ok_or_else(|| {
                                    AppError::other("no stored password for this session")
                                })?;
                            AuthMethod::Password(pw)
                        }
                        AuthType::Key => {
                            let path = sess
                                .key_path
                                .clone()
                                .ok_or_else(|| AppError::other("no key file configured"))?;
                            let passphrase = secrets::get(&session_id_c, secrets::KIND_PASSPHRASE)?;
                            AuthMethod::KeyFile { path, passphrase }
                        }
                        AuthType::Agent => AuthMethod::Agent,
                    };
                    let params = ConnectParams {
                        host: sess.host.clone(),
                        port: sess.port,
                        username: sess.username.clone(),
                        auth,
                        trust_host_key: trust,
                        use_scp: sess.protocol == Protocol::Scp,
                    };
                    let mut p = SftpProvider::connect(params)?;
                    let home = p.home_dir()?;
                    Ok((Arc::new(StdMutex::new(p)), home))
                }
                Protocol::Ftp | Protocol::Ftps => {
                    let pass =
                        secrets::get(&session_id_c, secrets::KIND_PASSWORD)?.unwrap_or_default();
                    let mut p = FtpProvider::connect(
                        &sess.host,
                        sess.port,
                        &sess.username,
                        &pass,
                        sess.protocol == Protocol::Ftps,
                    )?;
                    let home = p.home_dir()?;
                    Ok((Arc::new(StdMutex::new(p)), home))
                }
            }
        },
    )
    .await?;

    // A unique id per connection lets the UI open the same session in several
    // tabs (or many servers) simultaneously.
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
pub fn disconnect(state: State<'_, AppState>, connection_id: String) -> Result<()> {
    state.connections.lock().unwrap().remove(&connection_id);
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
    blocking(move || LocalProvider::new().list_dir(&path)).await
}

#[tauri::command]
pub async fn make_dir_local(path: String) -> Result<()> {
    blocking(move || LocalProvider::new().make_dir(&path)).await
}

#[tauri::command]
pub async fn delete_local(path: String, is_dir: bool) -> Result<()> {
    blocking(move || {
        let mut lp = LocalProvider::new();
        if is_dir {
            lp.remove_dir(&path)
        } else {
            lp.remove_file(&path)
        }
    })
    .await
}

#[tauri::command]
pub async fn rename_local(from: String, to: String) -> Result<()> {
    blocking(move || LocalProvider::new().rename(&from, &to)).await
}

#[tauri::command]
pub async fn set_permissions_local(path: String, mode: u32) -> Result<()> {
    blocking(move || LocalProvider::new().set_permissions(&path, mode)).await
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
    let p = conn(&state, &connection_id)?;
    blocking(move || p.lock().unwrap().list_dir(&path)).await
}

#[tauri::command]
pub async fn make_dir_remote(
    state: State<'_, AppState>,
    connection_id: String,
    path: String,
) -> Result<()> {
    let p = conn(&state, &connection_id)?;
    blocking(move || p.lock().unwrap().make_dir(&path)).await
}

#[tauri::command]
pub async fn delete_remote(
    state: State<'_, AppState>,
    connection_id: String,
    path: String,
    is_dir: bool,
) -> Result<()> {
    let p = conn(&state, &connection_id)?;
    blocking(move || {
        let mut g = p.lock().unwrap();
        if is_dir {
            g.remove_dir(&path)
        } else {
            g.remove_file(&path)
        }
    })
    .await
}

#[tauri::command]
pub async fn rename_remote(
    state: State<'_, AppState>,
    connection_id: String,
    from: String,
    to: String,
) -> Result<()> {
    let p = conn(&state, &connection_id)?;
    blocking(move || p.lock().unwrap().rename(&from, &to)).await
}

#[tauri::command]
pub async fn set_permissions_remote(
    state: State<'_, AppState>,
    connection_id: String,
    path: String,
    mode: u32,
) -> Result<()> {
    let p = conn(&state, &connection_id)?;
    blocking(move || p.lock().unwrap().set_permissions(&path, mode)).await
}

// ---------------------------------------------------------------------------
// transfers
// ---------------------------------------------------------------------------

fn plan_upload(local_path: &str, remote_dir: &str) -> Result<(Vec<String>, Vec<FilePlan>)> {
    let mut dirs = Vec::new();
    let mut files = Vec::new();
    let meta = std::fs::symlink_metadata(local_path)?;
    let name = basename(local_path);
    if meta.is_dir() {
        let remote_root = protocol::posix_join(remote_dir, &name);
        dirs.push(remote_root.clone());
        walk_local(local_path, &remote_root, &mut dirs, &mut files)?;
    } else {
        files.push(FilePlan {
            local: local_path.to_string(),
            remote: protocol::posix_join(remote_dir, &name),
            name,
            size: meta.len(),
        });
    }
    Ok((dirs, files))
}

fn walk_local(
    local_dir: &str,
    remote_dir: &str,
    dirs: &mut Vec<String>,
    files: &mut Vec<FilePlan>,
) -> Result<()> {
    for entry in std::fs::read_dir(local_dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let child_local = entry.path();
        let child_local_str = child_local.to_string_lossy().into_owned();
        let child_remote = protocol::posix_join(remote_dir, &name);
        let meta = std::fs::symlink_metadata(&child_local)?;
        if meta.file_type().is_symlink() {
            continue; // symlinks skipped in the MVP
        }
        if meta.is_dir() {
            dirs.push(child_remote.clone());
            walk_local(&child_local_str, &child_remote, dirs, files)?;
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

fn plan_download(
    provider: &mut dyn FileProvider,
    remote_path: &str,
    local_dir: &str,
) -> Result<(Vec<String>, Vec<FilePlan>)> {
    let mut dirs = Vec::new();
    let mut files = Vec::new();
    let stat = provider.stat(remote_path)?;
    let name = protocol::posix_basename(remote_path);
    if stat.is_dir {
        let local_root = local_join(local_dir, &name);
        dirs.push(local_root.clone());
        walk_remote(provider, remote_path, &local_root, &mut dirs, &mut files)?;
    } else {
        files.push(FilePlan {
            local: local_join(local_dir, &name),
            remote: remote_path.to_string(),
            name,
            size: stat.size,
        });
    }
    Ok((dirs, files))
}

fn walk_remote(
    provider: &mut dyn FileProvider,
    remote_dir: &str,
    local_dir: &str,
    dirs: &mut Vec<String>,
    files: &mut Vec<FilePlan>,
) -> Result<()> {
    for entry in provider.list_dir(remote_dir)? {
        let child_remote = protocol::posix_join(remote_dir, &entry.name);
        let child_local = local_join(local_dir, &entry.name);
        if entry.is_symlink {
            continue;
        }
        if entry.is_dir {
            dirs.push(child_local.clone());
            walk_remote(provider, &child_remote, &child_local, dirs, files)?;
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
}

/// A planned file transfer plus whether its destination already exists — the UI
/// uses `exists` to decide whether to raise the conflict dialog.
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

/// Finalized transfer tuple: (local, remote, name, size, offset).
type Finalized = (String, String, String, u64, u64);

/// A planned file with the conflict resolution chosen by the user.
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
    blocking(move || -> Result<Vec<PlannedFile>> {
        let mut out = Vec::new();
        let mut g = p.lock().unwrap();
        if is_upload {
            for lp in &paths {
                let (_dirs, files) = plan_upload(lp, &dest_dir)?;
                for f in files {
                    let (exists, dest_size) = match g.stat(&f.remote) {
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
                let (_dirs, files) = plan_download(&mut *g, rp, &dest_dir)?;
                for f in files {
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
    })
    .await
}

/// Step 2: apply conflict policies (skip/rename/resume/overwrite), create
/// destination directories, and enqueue the resulting transfers.
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

    let finalized = blocking(move || -> Result<Vec<Finalized>> {
        let mut g = p.lock().unwrap();
        let mut result = Vec::new();
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
                        remote = unique_remote(&mut *g, &remote)?;
                    } else {
                        local = unique_local(&local);
                    }
                }
                _ => {} // "overwrite"
            }
            // Ensure destination directories exist.
            if is_upload {
                ensure_remote_tree(&mut *g, &dest_dir, &remote);
            } else if let Some(parent) = std::path::Path::new(&local).parent() {
                std::fs::create_dir_all(parent)?;
            }
            result.push((local, remote, f.name, f.size, offset));
        }
        Ok(result)
    })
    .await?;

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

/// Creates every intermediate remote directory between `dest_dir` and the file
/// (best effort — existing directories simply fail and are ignored).
fn ensure_remote_tree(g: &mut dyn FileProvider, dest_dir: &str, remote_file: &str) {
    let parent = protocol::posix_parent(remote_file);
    let base = protocol::normalize_posix(dest_dir);
    let Some(rel) = parent.strip_prefix(&base) else {
        let _ = g.make_dir(&parent);
        return;
    };
    let mut cur = base;
    for comp in rel.split('/').filter(|c| !c.is_empty()) {
        cur = protocol::posix_join(&cur, comp);
        let _ = g.make_dir(&cur);
    }
}

/// Splits a path into (stem, extension) using the last dot in the basename.
fn split_ext(path: &str) -> (String, String) {
    let name = basename(path);
    let dir = &path[..path.len() - name.len()];
    match name.rfind('.') {
        Some(i) if i > 0 => (format!("{dir}{}", &name[..i]), name[i..].to_string()),
        _ => (path.to_string(), String::new()),
    }
}

fn unique_remote(g: &mut dyn FileProvider, path: &str) -> Result<String> {
    if g.stat(path).is_err() {
        return Ok(path.to_string());
    }
    let (stem, ext) = split_ext(path);
    for i in 1..1000 {
        let cand = format!("{stem} ({i}){ext}");
        if g.stat(&cand).is_err() {
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

/// SHA-256 comparison of a transferred file's two sides.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VerifyResult {
    pub local: Option<String>,
    pub remote: Option<String>,
    pub matches: bool,
    /// False when either side can't produce a checksum (e.g. FTP, no sha256sum).
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
    blocking(move || -> Result<VerifyResult> {
        let local = LocalProvider::new().checksum_sha256(&local_path)?;
        let remote = p.lock().unwrap().checksum_sha256(&remote_path)?;
        let supported = local.is_some() && remote.is_some();
        let matches = supported && local == remote;
        Ok(VerifyResult {
            local,
            remote,
            matches,
            supported,
        })
    })
    .await
}

// ---------------------------------------------------------------------------
// directory synchronisation
// ---------------------------------------------------------------------------

/// One planned action of a directory sync, keyed by relative path.
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SyncItem {
    pub direction: String, // "upload" | "download"
    pub local: String,
    pub remote: String,
    pub name: String, // relative path, for display
    pub size: u64,
    pub reason: String, // "new" | "newer" | "size"
}

type FileMap = std::collections::HashMap<String, (String, u64, i64)>;

fn build_local_map(root: &str, prefix: &str, map: &mut FileMap) -> Result<()> {
    let entries = match std::fs::read_dir(root) {
        Ok(e) => e,
        Err(_) => return Ok(()), // absent local dir → empty side
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

fn build_remote_map(
    provider: &mut dyn FileProvider,
    root: &str,
    prefix: &str,
    map: &mut FileMap,
) -> Result<()> {
    for e in provider.list_dir(root)? {
        if e.is_symlink {
            continue;
        }
        let rel = if prefix.is_empty() {
            e.name.clone()
        } else {
            format!("{prefix}/{}", e.name)
        };
        let abs = protocol::posix_join(root, &e.name);
        if e.is_dir {
            build_remote_map(provider, &abs, &rel, map)?;
        } else {
            map.insert(rel, (abs, e.size, e.modified.unwrap_or(0)));
        }
    }
    Ok(())
}

/// Compares a local and a remote directory tree and returns the transfers needed
/// to synchronise them. `direction` is "upload", "download" or "both". No files
/// are deleted (safe by default).
#[tauri::command]
pub async fn plan_sync(
    state: State<'_, AppState>,
    connection_id: String,
    local_dir: String,
    remote_dir: String,
    direction: String,
) -> Result<Vec<SyncItem>> {
    let p = conn(&state, &connection_id)?;
    blocking(move || -> Result<Vec<SyncItem>> {
        let mut local = FileMap::new();
        build_local_map(&local_dir, "", &mut local)?;
        let mut remote = FileMap::new();
        {
            let mut g = p.lock().unwrap();
            build_remote_map(&mut *g, &remote_dir, "", &mut remote)?;
        }

        let want_up = direction == "upload" || direction == "both";
        let want_down = direction == "download" || direction == "both";

        let mut keys: std::collections::HashSet<String> = std::collections::HashSet::new();
        keys.extend(local.keys().cloned());
        keys.extend(remote.keys().cloned());

        let mut items = Vec::new();
        for rel in keys {
            let l = local.get(&rel);
            let r = remote.get(&rel);
            let (dir, reason, size, local_path, remote_path) = match (l, r) {
                (Some((la, ls, lm)), Some((ra, rs, rm))) => {
                    if ls == rs && (lm - rm).abs() <= 2 {
                        continue; // identical enough
                    }
                    // Newer side wins; on a pure size difference prefer the local side.
                    let up = if ls != rs { lm >= rm } else { lm > rm };
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
    })
    .await
}

// ---------------------------------------------------------------------------
// open a remote file in the local editor (auto re-upload on save)
// ---------------------------------------------------------------------------

/// Downloads `remote_path` to a temp file and starts watching it; every save is
/// re-uploaded automatically. Returns the temp path (the UI opens it).
#[tauri::command]
pub async fn edit_remote(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    connection_id: String,
    remote_path: String,
) -> Result<String> {
    let p = conn(&state, &connection_id)?;
    let name = basename(&remote_path);
    let dir = std::env::temp_dir().join("linuxscp-edit");
    std::fs::create_dir_all(&dir)?;
    let temp = dir
        .join(format!("{}-{}", uuid::Uuid::new_v4().simple(), name))
        .to_string_lossy()
        .into_owned();

    let p2 = p.clone();
    let rp = remote_path.clone();
    let ts = temp.clone();
    blocking(move || -> Result<()> {
        let mut f = std::fs::File::create(&ts)?;
        let mut noop = |_: u64| {};
        let cancel = tokio_util::sync::CancellationToken::new();
        p2.lock()
            .unwrap()
            .download_to(&rp, &mut f, 0, &mut noop, &cancel)
    })
    .await?;

    start_editor_watch(&state, app, p, temp.clone(), remote_path)?;
    Ok(temp)
}

fn start_editor_watch(
    state: &AppState,
    app: tauri::AppHandle,
    provider: Arc<StdMutex<dyn FileProvider>>,
    temp: String,
    remote: String,
) -> Result<()> {
    use notify::{RecursiveMode, Watcher};
    let (tx, rx) = std::sync::mpsc::channel();
    let mut watcher = notify::recommended_watcher(move |res| {
        let _ = tx.send(res);
    })
    .map_err(|e| AppError::other(e.to_string()))?;
    watcher
        .watch(std::path::Path::new(&temp), RecursiveMode::NonRecursive)
        .map_err(|e| AppError::other(e.to_string()))?;
    // Keep the watcher alive for the app lifetime.
    state.editors.lock().unwrap().push(watcher);

    tauri::async_runtime::spawn_blocking(move || editor_loop(rx, provider, temp, remote, app));
    Ok(())
}

fn editor_loop(
    rx: std::sync::mpsc::Receiver<notify::Result<notify::Event>>,
    provider: Arc<StdMutex<dyn FileProvider>>,
    temp: String,
    remote: String,
    app: tauri::AppHandle,
) {
    use notify::EventKind;
    loop {
        match rx.recv() {
            Ok(Ok(event)) => {
                if !matches!(event.kind, EventKind::Modify(_) | EventKind::Create(_)) {
                    continue;
                }
                // Coalesce the burst of write events an editor emits per save.
                while rx
                    .recv_timeout(std::time::Duration::from_millis(400))
                    .is_ok()
                {}
                if let Ok(mut f) = std::fs::File::open(&temp) {
                    let size = f.metadata().map(|m| m.len()).unwrap_or(0);
                    let cancel = tokio_util::sync::CancellationToken::new();
                    let mut noop = |_: u64| {};
                    let res = provider
                        .lock()
                        .unwrap()
                        .upload_from(&mut f, &remote, size, 0, &mut noop, &cancel);
                    match res {
                        Ok(()) => {
                            let _ = app.emit("remote-file-saved", remote.clone());
                        }
                        Err(e) => {
                            let _ = app.emit("remote-file-error", format!("{remote}: {e}"));
                        }
                    }
                }
            }
            Ok(Err(_)) => {}
            Err(_) => break, // watcher dropped
        }
    }
}

// ---------------------------------------------------------------------------
// built-in text editor (read/write file contents)
// ---------------------------------------------------------------------------

const MAX_EDIT_BYTES: u64 = 8 * 1024 * 1024;

/// Heuristic: a NUL byte in the head means it's not editable text.
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
        blocking(move || {
            let meta = std::fs::metadata(&path)?;
            if meta.len() > MAX_EDIT_BYTES {
                return Err(AppError::other("Datei zu groß zum Bearbeiten (> 8 MB)"));
            }
            decode_text(std::fs::read(&path)?)
        })
        .await
    } else {
        let cid = connection_id.ok_or_else(|| AppError::other("no connection"))?;
        let p = conn(&state, &cid)?;
        blocking(move || {
            let mut g = p.lock().unwrap();
            if g.stat(&path)?.size > MAX_EDIT_BYTES {
                return Err(AppError::other("Datei zu groß zum Bearbeiten (> 8 MB)"));
            }
            let mut buf: Vec<u8> = Vec::new();
            let mut noop = |_: u64| {};
            let cancel = tokio_util::sync::CancellationToken::new();
            g.download_to(&path, &mut buf, 0, &mut noop, &cancel)?;
            decode_text(buf)
        })
        .await
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
        blocking(move || {
            std::fs::write(&path, &bytes)?;
            Ok(())
        })
        .await
    } else {
        let cid = connection_id.ok_or_else(|| AppError::other("no connection"))?;
        let p = conn(&state, &cid)?;
        blocking(move || {
            let size = bytes.len() as u64;
            let mut cur = std::io::Cursor::new(bytes);
            let mut noop = |_: u64| {};
            let cancel = tokio_util::sync::CancellationToken::new();
            p.lock()
                .unwrap()
                .upload_from(&mut cur, &path, size, 0, &mut noop, &cancel)
        })
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
