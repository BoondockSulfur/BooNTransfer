//! FTP / FTPS provider on async `suppaftp` with rustls (pure Rust).
//!
//! Plain FTP and TLS-secured FTPS use different concrete stream types; both live
//! behind one provider (`Conn` enum, `op!` dispatch). suppaftp is built on
//! futures-IO — transfers bridge to tokio via a small generic read helper
//! (download) and `tokio_util::compat` (upload).

use super::{FileEntry, FileProvider, ProgressFn};
use crate::error::{AppError, Result};
use std::pin::Pin;
use std::str::FromStr;
use std::sync::Arc;
use std::task::{Context, Poll};
use suppaftp::list::File as FtpFile;
use suppaftp::types::FileType;
use suppaftp::{AsyncFtpStream, AsyncRustlsConnector, AsyncRustlsFtpStream};
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::sync::Mutex as AsyncMutex;
use tokio_util::compat::TokioAsyncReadCompatExt;
use tokio_util::sync::CancellationToken;

const CHUNK: usize = 128 * 1024;

fn ferr(e: impl std::fmt::Display) -> AppError {
    AppError::other(format!("FTP: {e}"))
}

enum Conn {
    Plain(AsyncFtpStream),
    Tls(AsyncRustlsFtpStream),
}

/// Dispatches an operation to whichever concrete stream is active.
macro_rules! op {
    ($guard:expr, $s:ident => $body:expr) => {
        match &mut *$guard {
            Conn::Plain($s) => $body,
            Conn::Tls($s) => $body,
        }
    };
}

pub struct FtpProvider {
    conn: AsyncMutex<Conn>,
}

impl FtpProvider {
    pub async fn connect(host: &str, port: u16, user: &str, pass: &str, tls: bool) -> Result<Self> {
        let addr = format!("{host}:{port}");
        let conn = if tls {
            let roots =
                rustls::RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
            let config = rustls::ClientConfig::builder()
                .with_root_certificates(roots)
                .with_no_client_auth();
            let connector =
                AsyncRustlsConnector::from(futures_rustls::TlsConnector::from(Arc::new(config)));
            let mut s = AsyncRustlsFtpStream::connect(&addr)
                .await
                .map_err(ferr)?
                .into_secure(connector, host)
                .await
                .map_err(ferr)?;
            s.login(user, pass).await.map_err(ferr)?;
            s.transfer_type(FileType::Binary).await.map_err(ferr)?;
            Conn::Tls(s)
        } else {
            let mut s = AsyncFtpStream::connect(&addr).await.map_err(ferr)?;
            s.login(user, pass).await.map_err(ferr)?;
            s.transfer_type(FileType::Binary).await.map_err(ferr)?;
            Conn::Plain(s)
        };
        Ok(FtpProvider {
            conn: AsyncMutex::new(conn),
        })
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

/// tokio `AsyncRead` wrapper that reports bytes read (for upload progress).
struct ProgressReader<'a> {
    inner: &'a mut (dyn AsyncRead + Unpin + Send),
    progress: &'a mut ProgressFn<'a>,
}

impl AsyncRead for ProgressReader<'_> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let before = buf.filled().len();
        let r = Pin::new(&mut self.inner).poll_read(cx, buf);
        if let Poll::Ready(Ok(())) = &r {
            let n = buf.filled().len() - before;
            if n > 0 {
                (self.progress)(n as u64);
            }
        }
        r
    }
}

/// Reads a suppaftp (futures-IO) data stream into a tokio writer with progress.
async fn stream_to_dst<R>(
    stream: &mut R,
    dst: &mut (dyn AsyncWrite + Unpin + Send),
    progress: &mut ProgressFn<'_>,
    cancel: &CancellationToken,
) -> Result<()>
where
    R: futures::AsyncRead + Unpin,
{
    use futures::AsyncReadExt as _;
    let mut buf = vec![0u8; CHUNK];
    loop {
        if cancel.is_cancelled() {
            return Err(AppError::Cancelled);
        }
        let n = stream.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        dst.write_all(&buf[..n]).await?;
        progress(n as u64);
    }
    Ok(())
}

#[async_trait::async_trait]
impl FileProvider for FtpProvider {
    fn kind(&self) -> &'static str {
        "ftp"
    }

    async fn home_dir(&self) -> Result<String> {
        let mut g = self.conn.lock().await;
        op!(g, s => s.pwd().await).map_err(ferr)
    }

    async fn list_dir(&self, path: &str) -> Result<Vec<FileEntry>> {
        let mut g = self.conn.lock().await;
        let lines = op!(g, s => s.list(Some(path)).await).map_err(ferr)?;
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

    async fn stat(&self, path: &str) -> Result<FileEntry> {
        let parent = super::posix_parent(path);
        let base = super::posix_basename(path);
        for e in self.list_dir(&parent).await? {
            if e.name == base {
                return Ok(e);
            }
        }
        Err(AppError::other(format!("FTP: not found: {path}")))
    }

    async fn make_dir(&self, path: &str) -> Result<()> {
        let mut g = self.conn.lock().await;
        op!(g, s => s.mkdir(path).await).map_err(ferr)
    }

    async fn remove_file(&self, path: &str) -> Result<()> {
        let mut g = self.conn.lock().await;
        op!(g, s => s.rm(path).await).map_err(ferr)
    }

    async fn remove_dir(&self, path: &str) -> Result<()> {
        for entry in self.list_dir(path).await? {
            let child = super::posix_join(path, &entry.name);
            if entry.is_dir && !entry.is_symlink {
                self.remove_dir(&child).await?;
            } else {
                self.remove_file(&child).await?;
            }
        }
        let mut g = self.conn.lock().await;
        op!(g, s => s.rmdir(path).await).map_err(ferr)
    }

    async fn rename(&self, from: &str, to: &str) -> Result<()> {
        let mut g = self.conn.lock().await;
        op!(g, s => s.rename(from, to).await).map_err(ferr)
    }

    async fn set_permissions(&self, path: &str, mode: u32) -> Result<()> {
        let cmd = format!("CHMOD {mode:o} {path}");
        let mut g = self.conn.lock().await;
        op!(g, s => s.site(&cmd).await).map_err(ferr)?;
        Ok(())
    }

    async fn download_to(
        &self,
        remote: &str,
        dst: &mut (dyn AsyncWrite + Unpin + Send),
        offset: u64,
        progress: &mut ProgressFn,
        cancel: &CancellationToken,
    ) -> Result<()> {
        let mut g = self.conn.lock().await;
        match &mut *g {
            Conn::Plain(s) => {
                if offset > 0 {
                    s.resume_transfer(offset as usize).await.map_err(ferr)?;
                }
                let mut stream = s.retr_as_stream(remote).await.map_err(ferr)?;
                stream_to_dst(&mut stream, dst, progress, cancel).await?;
                s.finalize_retr_stream(stream).await.map_err(ferr)?;
            }
            Conn::Tls(s) => {
                if offset > 0 {
                    s.resume_transfer(offset as usize).await.map_err(ferr)?;
                }
                let mut stream = s.retr_as_stream(remote).await.map_err(ferr)?;
                stream_to_dst(&mut stream, dst, progress, cancel).await?;
                s.finalize_retr_stream(stream).await.map_err(ferr)?;
            }
        }
        Ok(())
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
        if cancel.is_cancelled() {
            return Err(AppError::Cancelled);
        }
        let mut reader = ProgressReader {
            inner: src,
            progress,
        }
        .compat();
        let mut g = self.conn.lock().await;
        if offset > 0 {
            op!(g, s => s.append_file(remote, &mut reader).await).map_err(ferr)?;
        } else {
            op!(g, s => s.put_file(remote, &mut reader).await).map_err(ferr)?;
        }
        Ok(())
    }
}
