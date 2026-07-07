//! Protocol abstraction. Every backend (local FS, SFTP, and later FTP/FTPS,
//! WebDAV, S3) implements [`FileProvider`]. The transfer engine and the Tauri
//! commands only ever talk to this trait, so new protocols can be added without
//! touching the UI or the queue.

pub mod ftp;
pub mod local;
pub mod sftp;

use crate::error::Result;
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use tokio_util::sync::CancellationToken;

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

/// A synchronous (blocking) file provider. All methods are expected to be
/// called from a blocking context (`tokio::task::spawn_blocking`) so that the
/// async runtime — and therefore the UI — is never blocked.
pub trait FileProvider: Send {
    fn kind(&self) -> &'static str;

    fn home_dir(&mut self) -> Result<String>;
    fn list_dir(&mut self, path: &str) -> Result<Vec<FileEntry>>;
    fn stat(&mut self, path: &str) -> Result<FileEntry>;

    fn make_dir(&mut self, path: &str) -> Result<()>;
    fn remove_file(&mut self, path: &str) -> Result<()>;
    /// Recursively removes a directory and its contents.
    fn remove_dir(&mut self, path: &str) -> Result<()>;
    fn rename(&mut self, from: &str, to: &str) -> Result<()>;
    fn set_permissions(&mut self, path: &str, mode: u32) -> Result<()>;

    /// Returns the file's lowercase hex SHA-256, or `None` if the protocol/server
    /// cannot compute it (used for optional post-transfer verification).
    fn checksum_sha256(&mut self, _path: &str) -> Result<Option<String>> {
        Ok(None)
    }

    /// Reads `remote` starting at `offset` (for resume; 0 = from start) and
    /// streams it into `dst`, reporting progress. `cancel` is checked between
    /// chunks. The caller positions `dst` appropriately.
    fn download_to(
        &mut self,
        remote: &str,
        dst: &mut dyn Write,
        offset: u64,
        progress: &mut ProgressFn,
        cancel: &CancellationToken,
    ) -> Result<()>;

    /// Streams `src` (of length `size`) into `remote`, reporting progress. When
    /// `offset > 0` the write resumes by appending at `offset`; the caller
    /// positions `src` accordingly.
    fn upload_from(
        &mut self,
        src: &mut dyn Read,
        remote: &str,
        size: u64,
        offset: u64,
        progress: &mut ProgressFn,
        cancel: &CancellationToken,
    ) -> Result<()>;
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
