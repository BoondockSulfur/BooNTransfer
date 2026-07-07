//! Local filesystem provider.

use super::{FileEntry, FileProvider, ProgressFn};
use crate::error::{AppError, Result};
use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::MetadataExt;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::UNIX_EPOCH;
use tokio_util::sync::CancellationToken;

const CHUNK: usize = 128 * 1024;

#[derive(Default)]
pub struct LocalProvider;

impl LocalProvider {
    pub fn new() -> Self {
        LocalProvider
    }
}

fn entry_from_meta(name: String, meta: &fs::Metadata, symlink: bool) -> FileEntry {
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

impl FileProvider for LocalProvider {
    fn kind(&self) -> &'static str {
        "local"
    }

    fn home_dir(&mut self) -> Result<String> {
        Ok(dirs::home_dir()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| "/".to_string()))
    }

    fn list_dir(&mut self, path: &str) -> Result<Vec<FileEntry>> {
        let mut out = Vec::new();
        for dent in fs::read_dir(path)? {
            let dent = dent?;
            let name = dent.file_name().to_string_lossy().into_owned();
            let symlink_meta = fs::symlink_metadata(dent.path())?;
            let is_symlink = symlink_meta.file_type().is_symlink();
            // For symlinks, follow to report target's dir/size; fall back to the
            // link's own metadata if the target is broken.
            let meta = fs::metadata(dent.path()).unwrap_or(symlink_meta);
            out.push(entry_from_meta(name, &meta, is_symlink));
        }
        Ok(out)
    }

    fn stat(&mut self, path: &str) -> Result<FileEntry> {
        let symlink_meta = fs::symlink_metadata(path)?;
        let is_symlink = symlink_meta.file_type().is_symlink();
        let meta = fs::metadata(path).unwrap_or(symlink_meta);
        let name = Path::new(path)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_string());
        Ok(entry_from_meta(name, &meta, is_symlink))
    }

    fn make_dir(&mut self, path: &str) -> Result<()> {
        fs::create_dir(path)?;
        Ok(())
    }

    fn remove_file(&mut self, path: &str) -> Result<()> {
        fs::remove_file(path)?;
        Ok(())
    }

    fn remove_dir(&mut self, path: &str) -> Result<()> {
        fs::remove_dir_all(path)?;
        Ok(())
    }

    fn rename(&mut self, from: &str, to: &str) -> Result<()> {
        fs::rename(from, to)?;
        Ok(())
    }

    fn set_permissions(&mut self, path: &str, mode: u32) -> Result<()> {
        fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
        Ok(())
    }

    fn checksum_sha256(&mut self, path: &str) -> Result<Option<String>> {
        use sha2::{Digest, Sha256};
        let mut f = fs::File::open(path)?;
        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; CHUNK];
        loop {
            let n = f.read(&mut buf)?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
        }
        Ok(Some(super::hex_lower(&hasher.finalize())))
    }

    fn download_to(
        &mut self,
        remote: &str,
        dst: &mut dyn Write,
        offset: u64,
        progress: &mut ProgressFn,
        cancel: &CancellationToken,
    ) -> Result<()> {
        let mut src = fs::File::open(remote)?;
        if offset > 0 {
            src.seek(SeekFrom::Start(offset))?;
        }
        copy_with_progress(&mut src, dst, progress, cancel)
    }

    fn upload_from(
        &mut self,
        src: &mut dyn Read,
        remote: &str,
        _size: u64,
        offset: u64,
        progress: &mut ProgressFn,
        cancel: &CancellationToken,
    ) -> Result<()> {
        let mut dst = if offset > 0 {
            let mut f = fs::OpenOptions::new().write(true).open(remote)?;
            f.seek(SeekFrom::Start(offset))?;
            f
        } else {
            fs::File::create(remote)?
        };
        copy_with_progress(src, &mut dst, progress, cancel)
    }
}

pub fn copy_with_progress(
    src: &mut dyn Read,
    dst: &mut dyn Write,
    progress: &mut ProgressFn,
    cancel: &CancellationToken,
) -> Result<()> {
    let mut buf = vec![0u8; CHUNK];
    loop {
        if cancel.is_cancelled() {
            return Err(AppError::Cancelled);
        }
        let n = src.read(&mut buf)?;
        if n == 0 {
            break;
        }
        dst.write_all(&buf[..n])?;
        progress(n as u64);
    }
    dst.flush()?;
    Ok(())
}
