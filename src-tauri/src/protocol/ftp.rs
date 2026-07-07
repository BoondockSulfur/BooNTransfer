//! FTP / FTPS provider via `suppaftp` (blocking). FTPS upgrades the control and
//! data connections with TLS (native-tls, sharing the vendored OpenSSL).

use super::{FileEntry, FileProvider, ProgressFn};
use crate::error::{AppError, Result};
use std::io::{Read, Write};
use std::str::FromStr;
use suppaftp::list::File as FtpFile;
use suppaftp::types::FileType;
use suppaftp::{FtpStream, NativeTlsConnector, NativeTlsFtpStream};
use tokio_util::sync::CancellationToken;

const CHUNK: usize = 128 * 1024;

fn ferr(e: impl std::fmt::Display) -> AppError {
    AppError::other(format!("FTP: {e}"))
}

/// Plain FTP and TLS-secured FTPS use different concrete stream types; this enum
/// keeps both behind one provider. `op!` dispatches an operation to either.
enum Conn {
    Plain(FtpStream),
    Tls(NativeTlsFtpStream),
}

macro_rules! op {
    ($self:expr, $s:ident => $body:expr) => {
        match &mut $self.conn {
            Conn::Plain($s) => $body,
            Conn::Tls($s) => $body,
        }
    };
}

pub struct FtpProvider {
    conn: Conn,
}

/// Reader wrapper that reports transferred bytes during uploads.
struct ProgressReader<'a> {
    inner: &'a mut dyn Read,
    progress: &'a mut (dyn FnMut(u64) + Send),
}

impl Read for ProgressReader<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        if n > 0 {
            (self.progress)(n as u64);
        }
        Ok(n)
    }
}

impl FtpProvider {
    pub fn connect(host: &str, port: u16, user: &str, pass: &str, tls: bool) -> Result<Self> {
        let addr = format!("{host}:{port}");
        let conn = if tls {
            let stream = NativeTlsFtpStream::connect(&addr).map_err(ferr)?;
            let connector =
                NativeTlsConnector::from(native_tls::TlsConnector::new().map_err(ferr)?);
            let mut stream = stream.into_secure(connector, host).map_err(ferr)?;
            stream.login(user, pass).map_err(ferr)?;
            stream.transfer_type(FileType::Binary).map_err(ferr)?;
            Conn::Tls(stream)
        } else {
            let mut stream = FtpStream::connect(&addr).map_err(ferr)?;
            stream.login(user, pass).map_err(ferr)?;
            stream.transfer_type(FileType::Binary).map_err(ferr)?;
            Conn::Plain(stream)
        };
        Ok(FtpProvider { conn })
    }
}

fn entry_from_ftp(f: &FtpFile) -> FileEntry {
    let modified = f
        .modified()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs() as i64);
    FileEntry {
        name: f.name().to_string(),
        is_dir: f.is_directory(),
        is_symlink: f.is_symlink(),
        size: f.size() as u64,
        modified,
        permissions: None,
        owner: None,
        group: None,
    }
}

impl FileProvider for FtpProvider {
    fn kind(&self) -> &'static str {
        "ftp"
    }

    fn home_dir(&mut self) -> Result<String> {
        op!(self, s => s.pwd()).map_err(ferr)
    }

    fn list_dir(&mut self, path: &str) -> Result<Vec<FileEntry>> {
        let lines = op!(self, s => s.list(Some(path))).map_err(ferr)?;
        let mut out = Vec::new();
        for line in lines {
            if let Ok(f) = FtpFile::from_str(&line) {
                let name = f.name();
                if name == "." || name == ".." || name.is_empty() {
                    continue;
                }
                out.push(entry_from_ftp(&f));
            }
        }
        Ok(out)
    }

    fn stat(&mut self, path: &str) -> Result<FileEntry> {
        let parent = super::posix_parent(path);
        let base = super::posix_basename(path);
        for e in self.list_dir(&parent)? {
            if e.name == base {
                return Ok(e);
            }
        }
        Err(AppError::other(format!("FTP: not found: {path}")))
    }

    fn make_dir(&mut self, path: &str) -> Result<()> {
        op!(self, s => s.mkdir(path)).map_err(ferr)
    }

    fn remove_file(&mut self, path: &str) -> Result<()> {
        op!(self, s => s.rm(path)).map_err(ferr)
    }

    fn remove_dir(&mut self, path: &str) -> Result<()> {
        // Recursively empty the directory, then remove it.
        for entry in self.list_dir(path)? {
            let child = super::posix_join(path, &entry.name);
            if entry.is_dir && !entry.is_symlink {
                self.remove_dir(&child)?;
            } else {
                self.remove_file(&child)?;
            }
        }
        op!(self, s => s.rmdir(path)).map_err(ferr)
    }

    fn rename(&mut self, from: &str, to: &str) -> Result<()> {
        op!(self, s => s.rename(from, to)).map_err(ferr)
    }

    fn set_permissions(&mut self, path: &str, mode: u32) -> Result<()> {
        // Not part of the FTP standard; try SITE CHMOD as a best effort.
        let cmd = format!("CHMOD {mode:o} {path}");
        op!(self, s => s.site(&cmd)).map_err(ferr)?;
        Ok(())
    }

    fn download_to(
        &mut self,
        remote: &str,
        dst: &mut dyn Write,
        offset: u64,
        progress: &mut ProgressFn,
        cancel: &CancellationToken,
    ) -> Result<()> {
        if cancel.is_cancelled() {
            return Err(AppError::Cancelled);
        }
        if offset > 0 {
            op!(self, s => s.resume_transfer(offset as usize)).map_err(ferr)?;
        }
        op!(self, s => s.retr(remote, |reader| {
            let mut buf = vec![0u8; CHUNK];
            loop {
                let n = reader.read(&mut buf).map_err(suppaftp::FtpError::ConnectionError)?;
                if n == 0 {
                    break;
                }
                dst.write_all(&buf[..n]).map_err(suppaftp::FtpError::ConnectionError)?;
                progress(n as u64);
            }
            Ok(())
        }))
        .map_err(ferr)
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
        if cancel.is_cancelled() {
            return Err(AppError::Cancelled);
        }
        let mut reader = ProgressReader {
            inner: src,
            progress,
        };
        if offset > 0 {
            op!(self, s => s.append_file(remote, &mut reader)).map_err(ferr)?;
        } else {
            op!(self, s => s.put_file(remote, &mut reader)).map_err(ferr)?;
        }
        Ok(())
    }
}
