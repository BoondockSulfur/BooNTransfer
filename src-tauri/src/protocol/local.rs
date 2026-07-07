//! Local filesystem provider (async, tokio::fs).

use super::{FileEntry, FileProvider, ProgressFn};
use crate::error::{AppError, Result};
use std::os::unix::fs::MetadataExt;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::UNIX_EPOCH;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncSeekExt, AsyncWrite, AsyncWriteExt};
use tokio_util::sync::CancellationToken;

const CHUNK: usize = 128 * 1024;

#[derive(Default)]
pub struct LocalProvider;

impl LocalProvider {
    pub fn new() -> Self {
        LocalProvider
    }
}

fn entry_from_meta(name: String, meta: &std::fs::Metadata, symlink: bool) -> FileEntry {
    let modified = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64);
    FileEntry {
        name,
        is_dir: meta.is_dir(),
        is_symlink: symlink,
        size: meta.len(),
        modified,
        permissions: Some(meta.permissions().mode() & 0o7777),
        owner: Some(meta.uid().to_string()),
        group: Some(meta.gid().to_string()),
    }
}

/// Streams `src` into `dst` with progress + cancellation.
pub async fn copy_with_progress(
    src: &mut (dyn AsyncRead + Unpin + Send),
    dst: &mut (dyn AsyncWrite + Unpin + Send),
    progress: &mut ProgressFn<'_>,
    cancel: &CancellationToken,
) -> Result<()> {
    let mut buf = vec![0u8; CHUNK];
    loop {
        if cancel.is_cancelled() {
            return Err(AppError::Cancelled);
        }
        let n = src.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        dst.write_all(&buf[..n]).await?;
        progress(n as u64);
    }
    dst.flush().await?;
    Ok(())
}

#[async_trait::async_trait]
impl FileProvider for LocalProvider {
    fn kind(&self) -> &'static str {
        "local"
    }

    async fn home_dir(&self) -> Result<String> {
        Ok(dirs::home_dir()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| "/".to_string()))
    }

    async fn list_dir(&self, path: &str) -> Result<Vec<FileEntry>> {
        let mut out = Vec::new();
        let mut rd = tokio::fs::read_dir(path).await?;
        while let Some(dent) = rd.next_entry().await? {
            let name = dent.file_name().to_string_lossy().into_owned();
            let symlink_meta = tokio::fs::symlink_metadata(dent.path()).await?;
            let is_symlink = symlink_meta.file_type().is_symlink();
            // Follow symlinks to report the target's kind/size; fall back if broken.
            let meta = tokio::fs::metadata(dent.path())
                .await
                .unwrap_or(symlink_meta);
            out.push(entry_from_meta(name, &meta, is_symlink));
        }
        Ok(out)
    }

    async fn stat(&self, path: &str) -> Result<FileEntry> {
        let symlink_meta = tokio::fs::symlink_metadata(path).await?;
        let is_symlink = symlink_meta.file_type().is_symlink();
        let meta = tokio::fs::metadata(path).await.unwrap_or(symlink_meta);
        let name = Path::new(path)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_string());
        Ok(entry_from_meta(name, &meta, is_symlink))
    }

    async fn make_dir(&self, path: &str) -> Result<()> {
        tokio::fs::create_dir(path).await?;
        Ok(())
    }

    async fn remove_file(&self, path: &str) -> Result<()> {
        tokio::fs::remove_file(path).await?;
        Ok(())
    }

    async fn remove_dir(&self, path: &str) -> Result<()> {
        tokio::fs::remove_dir_all(path).await?;
        Ok(())
    }

    async fn rename(&self, from: &str, to: &str) -> Result<()> {
        tokio::fs::rename(from, to).await?;
        Ok(())
    }

    async fn set_permissions(&self, path: &str, mode: u32) -> Result<()> {
        tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).await?;
        Ok(())
    }

    async fn checksum_sha256(&self, path: &str) -> Result<Option<String>> {
        use sha2::{Digest, Sha256};
        let mut f = tokio::fs::File::open(path).await?;
        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; CHUNK];
        loop {
            let n = f.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
        }
        Ok(Some(super::hex_lower(&hasher.finalize())))
    }

    async fn download_to(
        &self,
        remote: &str,
        dst: &mut (dyn AsyncWrite + Unpin + Send),
        offset: u64,
        progress: &mut ProgressFn,
        cancel: &CancellationToken,
    ) -> Result<()> {
        let mut src = tokio::fs::File::open(remote).await?;
        if offset > 0 {
            src.seek(std::io::SeekFrom::Start(offset)).await?;
        }
        copy_with_progress(&mut src, dst, progress, cancel).await
    }

    async fn upload_from(
        &self,
        src: &mut (dyn AsyncRead + Unpin + Send),
        remote: &str,
        _size: u64,
        offset: u64,
        progress: &mut ProgressFn,
        cancel: &CancellationToken,
    ) -> Result<()> {
        let mut dst = if offset > 0 {
            let mut f = tokio::fs::OpenOptions::new()
                .write(true)
                .open(remote)
                .await?;
            f.seek(std::io::SeekFrom::Start(offset)).await?;
            f
        } else {
            tokio::fs::File::create(remote).await?
        };
        copy_with_progress(src, &mut dst, progress, cancel).await
    }
}
