//! Protocol abstraction. Every backend (local FS, SFTP, and later FTP/FTPS,
//! WebDAV, S3) implements [`FileProvider`]. The transfer engine and the Tauri
//! commands only ever talk to this trait, so new protocols can be added without
//! touching the UI or the queue.

pub mod ftp;
pub mod local;
pub mod s3;
pub mod sftp;
pub mod webdav;

use crate::error::{AppError, Result};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::mpsc::UnboundedReceiver;
use tokio_util::sync::CancellationToken;

/// One chunk of output from an interactive shell, or its end.
pub enum TerminalEvent {
    Data(Vec<u8>),
    Exit,
}

/// Handle to drive an interactive shell (write input, resize, close).
#[async_trait::async_trait]
pub trait Terminal: Send + Sync {
    async fn write(&self, data: &[u8]) -> Result<()>;
    async fn resize(&self, cols: u32, rows: u32) -> Result<()>;
    async fn close(&self) -> Result<()>;
}

/// A live shell: a handle plus a stream of output events.
pub struct TerminalSession {
    pub handle: Box<dyn Terminal>,
    pub output: UnboundedReceiver<TerminalEvent>,
}

/// A single directory entry, protocol-independent.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileEntry {
    pub name: String,
    pub is_dir: bool,
    pub is_symlink: bool,
    pub size: u64,
    /// Unix modification time in seconds, if known.
    pub modified: Option<i64>,
    /// Unix permission bits (e.g. 0o644), if known.
    pub permissions: Option<u32>,
    pub owner: Option<String>,
    pub group: Option<String>,
}

/// Progress callback: receives the number of *additional* bytes transferred.
pub type ProgressFn<'a> = dyn FnMut(u64) + Send + 'a;

/// An async file provider. Implementations are internally thread-safe (methods
/// take `&self`) and shared as `Arc<dyn FileProvider>`. Nothing blocks the UI —
/// blocking backends (keyring, FTP) run their work on `spawn_blocking`.
#[async_trait::async_trait]
pub trait FileProvider: Send + Sync {
    fn kind(&self) -> &'static str;

    async fn home_dir(&self) -> Result<String>;
    async fn list_dir(&self, path: &str) -> Result<Vec<FileEntry>>;
    async fn stat(&self, path: &str) -> Result<FileEntry>;

    async fn make_dir(&self, path: &str) -> Result<()>;
    async fn remove_file(&self, path: &str) -> Result<()>;
    /// Recursively removes a directory and its contents.
    async fn remove_dir(&self, path: &str) -> Result<()>;
    async fn rename(&self, from: &str, to: &str) -> Result<()>;
    async fn set_permissions(&self, path: &str, mode: u32) -> Result<()>;

    /// Returns the file's lowercase hex SHA-256, or `None` if the protocol/server
    /// cannot compute it (used for optional post-transfer verification).
    async fn checksum_sha256(&self, _path: &str) -> Result<Option<String>> {
        Ok(None)
    }

    /// Reads `remote` from `offset` (0 = start) and streams it into `dst`,
    /// reporting progress. `cancel` is honoured. The caller positions `dst`.
    async fn download_to(
        &self,
        remote: &str,
        dst: &mut (dyn AsyncWrite + Unpin + Send),
        offset: u64,
        progress: &mut ProgressFn,
        cancel: &CancellationToken,
    ) -> Result<()>;

    /// Streams `src` (length `size`) into `remote`, reporting progress. When
    /// `offset > 0` the write resumes at `offset`; the caller positions `src`.
    async fn upload_from(
        &self,
        src: &mut (dyn AsyncRead + Unpin + Send),
        remote: &str,
        size: u64,
        offset: u64,
        progress: &mut ProgressFn,
        cancel: &CancellationToken,
    ) -> Result<()>;

    /// Opens an interactive shell with a PTY of the given size. Only SSH-based
    /// providers support this; the default returns an error.
    async fn open_terminal(&self, _cols: u32, _rows: u32) -> Result<TerminalSession> {
        Err(AppError::other(
            "Terminal wird für dieses Protokoll nicht unterstützt",
        ))
    }
}

/// Joins a base directory and a child name using POSIX ('/') semantics. This is
/// used for *remote* paths (which are always POSIX) and works fine for local
/// paths on Linux too.
pub fn posix_join(base: &str, name: &str) -> String {
    if name.starts_with('/') {
        return normalize_posix(name);
    }
    let trimmed = base.trim_end_matches('/');
    let joined = if trimmed.is_empty() {
        format!("/{name}")
    } else {
        format!("{trimmed}/{name}")
    };
    normalize_posix(&joined)
}

/// Returns the parent directory of a POSIX path, or the path itself if it is
/// already the root.
pub fn posix_parent(path: &str) -> String {
    let normalized = normalize_posix(path);
    if normalized == "/" {
        return normalized;
    }
    match normalized.rsplit_once('/') {
        Some(("", _)) => "/".to_string(),
        Some((parent, _)) => parent.to_string(),
        None => "/".to_string(),
    }
}

pub fn posix_basename(path: &str) -> String {
    let normalized = normalize_posix(path);
    normalized
        .rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or("/")
        .to_string()
}

/// Lowercase hex encoding of a byte slice (for checksums).
pub fn hex_lower(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// Collapses `.`/`..`/duplicate slashes in a POSIX path. Always returns an
/// absolute path beginning with '/'.
pub fn normalize_posix(path: &str) -> String {
    let mut stack: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                stack.pop();
            }
            other => stack.push(other),
        }
    }
    if stack.is_empty() {
        "/".to_string()
    } else {
        format!("/{}", stack.join("/"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn join_basic() {
        assert_eq!(posix_join("/home/user", "file.txt"), "/home/user/file.txt");
        assert_eq!(posix_join("/home/user/", "file.txt"), "/home/user/file.txt");
        assert_eq!(posix_join("", "file.txt"), "/file.txt");
        assert_eq!(posix_join("/a", "/b"), "/b");
    }

    #[test]
    fn normalize() {
        assert_eq!(normalize_posix("/a/b/../c"), "/a/c");
        assert_eq!(normalize_posix("/a//b/./c/"), "/a/b/c");
        assert_eq!(normalize_posix("/.."), "/");
        assert_eq!(normalize_posix("/"), "/");
        assert_eq!(normalize_posix(""), "/");
    }

    #[test]
    fn parent_and_basename() {
        assert_eq!(posix_parent("/home/user/file.txt"), "/home/user");
        assert_eq!(posix_parent("/home"), "/");
        assert_eq!(posix_parent("/"), "/");
        assert_eq!(posix_basename("/home/user/file.txt"), "file.txt");
        assert_eq!(posix_basename("/"), "/");
    }
}
