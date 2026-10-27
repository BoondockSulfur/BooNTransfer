//! FTP / FTPS provider on async `suppaftp` with rustls (pure Rust).
//!
//! Plain FTP and TLS-secured FTPS use different concrete stream types; both live
//! behind one provider (`Conn` enum, `op!` dispatch). One control connection
//! serves browsing; transfers run on their own pooled connections, so listing
//! stays responsive and parallel transfers do not serialize. A transfer
//! connection that failed or was cancelled is discarded instead of reused,
//! because its reply sequence can no longer be trusted.

use super::{FileEntry, FileProvider, ProgressFn};
use crate::error::{AppError, Result};
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use suppaftp::list::File as FtpFile;
use suppaftp::types::FileType;
use suppaftp::{AsyncFtpStream, AsyncRustlsConnector, AsyncRustlsFtpStream, Status};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::Mutex as AsyncMutex;
use tokio_util::sync::CancellationToken;

const CHUNK: usize = 128 * 1024;
/// Idle transfer connections kept for reuse (matches the queue's concurrency).
const POOL_MAX: usize = 2;

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

struct Params {
    host: String,
    port: u16,
    user: String,
    pass: String,
    tls: bool,
}

async fn open_conn(p: &Params) -> Result<Conn> {
    let addr = format!("{}:{}", p.host, p.port);
    let conn = if p.tls {
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
            .into_secure(connector, &p.host)
            .await
            .map_err(ferr)?;
        s.login(&p.user, &p.pass).await.map_err(ferr)?;
        s.transfer_type(FileType::Binary).await.map_err(ferr)?;
        Conn::Tls(s)
    } else {
        let mut s = AsyncFtpStream::connect(&addr).await.map_err(ferr)?;
        s.login(&p.user, &p.pass).await.map_err(ferr)?;
        s.transfer_type(FileType::Binary).await.map_err(ferr)?;
        Conn::Plain(s)
    };
    Ok(conn)
}

pub struct FtpProvider {
    params: Params,
    conn: AsyncMutex<Conn>,
    idle: StdMutex<Vec<Conn>>,
    /// Server lists via MLSD (exact UTC timestamps) instead of LIST.
    mlsd: AtomicBool,
    /// Server can set modification times (MFMT).
    mfmt: bool,
}

impl FtpProvider {
    pub async fn connect(host: &str, port: u16, user: &str, pass: &str, tls: bool) -> Result<Self> {
        let params = Params {
            host: host.to_string(),
            port,
            user: user.to_string(),
            pass: pass.to_string(),
            tls,
        };
        let mut conn = open_conn(&params).await?;
        let features = op!((&mut conn), s => s.feat().await).unwrap_or_default();
        let has = |name: &str| features.keys().any(|k| k.eq_ignore_ascii_case(name));
        let (mlsd, mfmt) = (has("MLST"), has("MFMT"));
        Ok(FtpProvider {
            params,
            conn: AsyncMutex::new(conn),
            idle: StdMutex::new(Vec::new()),
            mlsd: AtomicBool::new(mlsd),
            mfmt,
        })
    }

    /// An idle pooled connection that still answers NOOP, or a fresh one.
    async fn take_conn(&self) -> Result<Conn> {
        loop {
            let pooled = self.idle.lock().unwrap().pop();
            let Some(mut c) = pooled else {
                return open_conn(&self.params).await;
            };
            if op!((&mut c), s => s.noop().await).is_ok() {
                return Ok(c);
            }
        }
    }

    fn give_back(&self, c: Conn) {
        let mut idle = self.idle.lock().unwrap();
        if idle.len() < POOL_MAX {
            idle.push(c);
        }
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

/// Formats Unix seconds as the `YYYYMMDDHHMMSS` UTC timestamp MFMT expects.
fn mfmt_time(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    // Civil-from-days (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}{month:02}{day:02}{:02}{:02}{:02}",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
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
    dst.flush().await?;
    Ok(())
}

/// Writes a tokio reader into a suppaftp (futures-IO) data stream with progress.
async fn src_to_stream<W>(
    src: &mut (dyn AsyncRead + Unpin + Send),
    stream: &mut W,
    progress: &mut ProgressFn<'_>,
    cancel: &CancellationToken,
) -> Result<()>
where
    W: futures::AsyncWrite + Unpin,
{
    use futures::AsyncWriteExt as _;
    let mut buf = vec![0u8; CHUNK];
    loop {
        if cancel.is_cancelled() {
            return Err(AppError::Cancelled);
        }
        let n = src.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        stream.write_all(&buf[..n]).await?;
        progress(n as u64);
    }
    stream.flush().await?;
    Ok(())
}

#[async_trait::async_trait]
impl FileProvider for FtpProvider {
    fn kind(&self) -> &'static str {
        "ftp"
    }

    fn can_set_mtime(&self) -> bool {
        // LIST only has minute (or day) precision, so preserved times are only
        // comparable when the server also lists via MLSD.
        self.mfmt && self.mlsd.load(Ordering::Relaxed)
    }

    async fn home_dir(&self) -> Result<String> {
        let mut g = self.conn.lock().await;
        op!(g, s => s.pwd().await).map_err(ferr)
    }

    async fn list_dir(&self, path: &str) -> Result<Vec<FileEntry>> {
        let mut g = self.conn.lock().await;
        let mut out = Vec::new();
        if self.mlsd.load(Ordering::Relaxed) {
            match op!(g, s => s.mlsd(Some(path)).await) {
                Ok(lines) => {
                    for line in lines {
                        // cdir/pdir entries fail to parse and are skipped.
                        if let Ok(f) = FtpFile::from_mlsx_line(&line) {
                            let name = f.name();
                            if name != "." && name != ".." && !name.is_empty() {
                                out.push(entry_from_ftp(&f));
                            }
                        }
                    }
                    return Ok(out);
                }
                Err(e) => {
                    tracing::warn!("FTP: MLSD failed, falling back to LIST: {e}");
                    self.mlsd.store(false, Ordering::Relaxed);
                }
            }
        }
        let lines = op!(g, s => s.list(Some(path)).await).map_err(ferr)?;
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

    async fn set_mtime(&self, path: &str, mtime: i64) -> Result<()> {
        if !self.mfmt {
            return Ok(());
        }
        let cmd = format!("MFMT {} {path}", mfmt_time(mtime));
        let mut g = self.conn.lock().await;
        op!(g, s => s.custom_command(&cmd, &[Status::File]).await).map_err(ferr)?;
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
        let mut c = self.take_conn().await?;
        let res: Result<()> = op!((&mut c), s => async {
            if offset > 0 {
                s.resume_transfer(offset as usize).await.map_err(ferr)?;
            }
            let mut stream = s.retr_as_stream(remote).await.map_err(ferr)?;
            stream_to_dst(&mut stream, dst, progress, cancel).await?;
            s.finalize_retr_stream(stream).await.map_err(ferr)
        }
        .await);
        if res.is_ok() {
            self.give_back(c);
        }
        res
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
        let mut c = self.take_conn().await?;
        let res: Result<()> = op!((&mut c), s => async {
            let mut stream = if offset > 0 {
                s.append_with_stream(remote).await
            } else {
                s.put_with_stream(remote).await
            }
            .map_err(ferr)?;
            let copied = src_to_stream(src, &mut stream, progress, cancel).await;
            // Close the data connection in every case: the server keeps what
            // arrived, and a resume continues from the remote file size.
            let finalized = s.finalize_put_stream(stream).await.map_err(ferr);
            copied.and(finalized)
        }
        .await);
        if res.is_ok() {
            self.give_back(c);
        }
        res
    }
}

#[cfg(test)]
mod tests {
    use super::mfmt_time;

    #[test]
    fn mfmt_format() {
        assert_eq!(mfmt_time(0), "19700101000000");
        assert_eq!(mfmt_time(951_782_400), "20000229000000");
        assert_eq!(mfmt_time(1_793_103_045), "20261027121045");
    }
}
