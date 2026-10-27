//! SFTP-over-SSH provider on **russh** (pure Rust, no libssh2/OpenSSL).
//!
//! Transfers pipeline many concurrent SFTP read/write requests over one channel
//! (`RawSftpSession`), which hides latency — see `experiments/russh-spike`.
//! `SftpProvider` is shared as `Arc<dyn FileProvider>` (methods take `&self`);
//! the SFTP session is internally `Arc`-multiplexed and the SSH `Handle` sits
//! behind a mutex for the occasional exec channel (checksums, shells).

use super::{FileEntry, FileProvider, ProgressFn, Terminal, TerminalEvent, TerminalSession};
use crate::error::{AppError, Result};
use futures::stream::{FuturesUnordered, StreamExt};
use russh::client::{Config, Handle, Handler};
use russh::keys::key::PublicKey;
use russh_sftp::client::error::Error as SftpError;
use russh_sftp::client::RawSftpSession;
use russh_sftp::protocol::{FileAttributes, OpenFlags, StatusCode};
use std::collections::HashSet;
use std::sync::{Arc, Mutex as StdMutex};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{mpsc, Mutex as AsyncMutex};
use tokio_util::sync::CancellationToken;

const CHUNK: usize = 32 * 1024; // SFTP data packet size (widely accepted)
const CONC: usize = 64; // concurrent in-flight requests (pipelining)
const WINDOW: u32 = 64 * 1024 * 1024; // large channel window for high-latency links
const S_IFMT: u32 = 0o170000;
const S_IFLNK: u32 = 0o120000;
const S_IFDIR: u32 = 0o040000;

/// Commands sent to a shell channel's actor task.
enum TermMsg {
    Input(Vec<u8>),
    Resize(u32, u32),
    Close,
}

/// Handle to an interactive SSH shell; forwards to the channel's actor task.
struct SshTerminal {
    tx: mpsc::UnboundedSender<TermMsg>,
}

#[async_trait::async_trait]
impl Terminal for SshTerminal {
    async fn write(&self, data: &[u8]) -> Result<()> {
        self.tx
            .send(TermMsg::Input(data.to_vec()))
            .map_err(|_| AppError::other("Terminal ist geschlossen"))?;
        Ok(())
    }
    async fn resize(&self, cols: u32, rows: u32) -> Result<()> {
        self.tx
            .send(TermMsg::Resize(cols, rows))
            .map_err(|_| AppError::other("Terminal ist geschlossen"))?;
        Ok(())
    }
    async fn close(&self) -> Result<()> {
        let _ = self.tx.send(TermMsg::Close);
        Ok(())
    }
}

pub enum AuthMethod {
    Password(String),
    KeyFile {
        path: String,
        passphrase: Option<String>,
    },
    Agent,
}

pub struct ConnectParams {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub auth: AuthMethod,
    /// Fingerprint the user confirmed in the host-key dialog. Only a server key
    /// with exactly this fingerprint is accepted (and stored) when it is not yet
    /// in `known_hosts`; any other unknown key is rejected again.
    pub trusted_fingerprint: Option<String>,
}

pub struct SftpProvider {
    handle: AsyncMutex<Handle<ClientHandler>>,
    sftp: Arc<RawSftpSession>,
}

// ---------------------------------------------------------------------------
// host-key verification handler
// ---------------------------------------------------------------------------

struct HostKeyReject {
    fingerprint: String,
    key_type: String,
    changed: bool,
}

struct ClientHandler {
    host: String,
    port: u16,
    trusted_fingerprint: Option<String>,
    reject: Arc<StdMutex<Option<HostKeyReject>>>,
}

#[async_trait::async_trait]
impl Handler for ClientHandler {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        key: &PublicKey,
    ) -> std::result::Result<bool, russh::Error> {
        let (known, changed) = match russh::keys::check_known_hosts(&self.host, self.port, key) {
            Ok(true) => (true, false),
            Ok(false) => (false, false),
            Err(russh::keys::Error::KeyChanged { .. }) => (false, true),
            Err(e) => {
                tracing::warn!("known_hosts could not be read: {e}");
                (false, false)
            }
        };
        if known {
            return Ok(true);
        }
        let fingerprint = key.fingerprint();
        if self.trusted_fingerprint.as_deref() == Some(fingerprint.as_str()) {
            if changed {
                if let Err(e) = forget_changed_host_keys(&self.host, self.port, key) {
                    tracing::warn!("could not remove outdated known_hosts entry: {e}");
                }
            }
            if let Err(e) = russh::keys::learn_known_hosts(&self.host, self.port, key) {
                tracing::warn!("could not store host key: {e}");
            }
            return Ok(true);
        }
        *self.reject.lock().unwrap() = Some(HostKeyReject {
            fingerprint,
            key_type: key.name().to_string(),
            changed,
        });
        Ok(false)
    }
}

/// Removes the `known_hosts` lines that record a *different* key of the same
/// type for this host (what `ssh-keygen -R` does), so a confirmed key change is
/// not reported as "changed" again on every connect. Line numbers follow
/// russh's counting, which skips `#` comment lines.
fn forget_changed_host_keys(host: &str, port: u16, key: &PublicKey) -> std::io::Result<()> {
    let stale: HashSet<usize> = russh::keys::known_host_keys(host, port)
        .map_err(std::io::Error::other)?
        .into_iter()
        .filter(|(_, recorded)| recorded.name() == key.name() && recorded != key)
        .map(|(line, _)| line)
        .collect();
    if stale.is_empty() {
        return Ok(());
    }
    let path = dirs::home_dir()
        .ok_or_else(|| std::io::Error::other("no home directory"))?
        .join(".ssh")
        .join("known_hosts");
    let text = std::fs::read_to_string(&path)?;
    let mut out = String::with_capacity(text.len());
    let mut line_no = 1;
    for line in text.split_inclusive('\n') {
        if line.starts_with('#') {
            out.push_str(line);
            continue;
        }
        if !stale.contains(&line_no) {
            out.push_str(line);
        }
        line_no += 1;
    }
    let tmp = path.with_extension("boon-tmp");
    std::fs::write(&tmp, out)?;
    std::fs::set_permissions(&tmp, std::fs::metadata(&path)?.permissions())?;
    std::fs::rename(&tmp, &path)
}

// ---------------------------------------------------------------------------
// connect
// ---------------------------------------------------------------------------

impl SftpProvider {
    pub async fn connect(params: ConnectParams) -> Result<Self> {
        let reject = Arc::new(StdMutex::new(None));
        let handler = ClientHandler {
            host: params.host.clone(),
            port: params.port,
            trusted_fingerprint: params.trusted_fingerprint.clone(),
            reject: reject.clone(),
        };
        let config = Arc::new(Config {
            window_size: WINDOW,
            ..Default::default()
        });

        let mut handle = match russh::client::connect(
            config,
            (params.host.as_str(), params.port),
            handler,
        )
        .await
        {
            Ok(h) => h,
            Err(e) => {
                if let Some(r) = reject.lock().unwrap().take() {
                    return Err(AppError::HostKeyVerification {
                        fingerprint: r.fingerprint,
                        key_type: r.key_type,
                        changed: r.changed,
                    });
                }
                return Err(e.into());
            }
        };

        let ok = match &params.auth {
            AuthMethod::Password(pw) => handle.authenticate_password(&params.username, pw).await?,
            AuthMethod::KeyFile { path, passphrase } => {
                let key = russh::keys::load_secret_key(path, passphrase.as_deref())?;
                handle
                    .authenticate_publickey(&params.username, Arc::new(key))
                    .await?
            }
            AuthMethod::Agent => {
                let mut agent = russh::keys::agent::client::AgentClient::connect_env()
                    .await
                    .map_err(|e| AppError::other(format!("SSH-Agent nicht erreichbar: {e}")))?;
                let identities = agent
                    .request_identities()
                    .await
                    .map_err(|e| AppError::other(format!("SSH-Agent-Identitäten: {e}")))?;
                if identities.is_empty() {
                    return Err(AppError::other("SSH-Agent hat keine Schlüssel geladen"));
                }
                let mut authed = false;
                for key in identities {
                    let (returned, res) = handle
                        .authenticate_future(&params.username, key, agent)
                        .await;
                    agent = returned;
                    if matches!(res, Ok(true)) {
                        authed = true;
                        break;
                    }
                }
                authed
            }
        };
        if !ok {
            return Err(AppError::AuthFailed);
        }

        let channel = handle.channel_open_session().await?;
        channel.request_subsystem(true, "sftp").await?;
        let sftp = RawSftpSession::new(channel.into_stream());
        sftp.init().await?;

        Ok(SftpProvider {
            handle: AsyncMutex::new(handle),
            sftp: Arc::new(sftp),
        })
    }

    /// Streams an open remote file into `dst`. Reads are pipelined; `buffered`
    /// yields them in order for sequential writing.
    async fn download_open(
        &self,
        handle: &str,
        dst: &mut (dyn AsyncWrite + Unpin + Send),
        offset: u64,
        progress: &mut ProgressFn<'_>,
        cancel: &CancellationToken,
    ) -> Result<()> {
        let size = self.sftp.fstat(handle.to_string()).await?.attrs.size;
        let Some(size) = size else {
            return self
                .download_sequential(handle, dst, offset, progress, cancel)
                .await;
        };

        let offsets: Vec<u64> = (offset..size).step_by(CHUNK).collect();
        let mut stream = futures::stream::iter(offsets.into_iter().map(|off| {
            let sftp = self.sftp.clone();
            let h = handle.to_string();
            let len = (size - off).min(CHUNK as u64) as u32;
            async move { read_full(&sftp, &h, off, len).await.map(|d| (len, d)) }
        }))
        .buffered(CONC);

        while let Some(chunk) = stream.next().await {
            if cancel.is_cancelled() {
                return Err(AppError::Cancelled);
            }
            let (expected, bytes) = chunk?;
            dst.write_all(&bytes).await?;
            progress(bytes.len() as u64);
            if bytes.len() < expected as usize {
                return Err(AppError::other(
                    "Datei wurde während der Übertragung auf dem Server verkürzt",
                ));
            }
        }
        dst.flush().await?;
        Ok(())
    }

    /// Fallback for servers that do not report a file size: read until EOF.
    async fn download_sequential(
        &self,
        handle: &str,
        dst: &mut (dyn AsyncWrite + Unpin + Send),
        mut offset: u64,
        progress: &mut ProgressFn<'_>,
        cancel: &CancellationToken,
    ) -> Result<()> {
        loop {
            if cancel.is_cancelled() {
                return Err(AppError::Cancelled);
            }
            let bytes = read_full(&self.sftp, handle, offset, CHUNK as u32).await?;
            if bytes.is_empty() {
                break;
            }
            dst.write_all(&bytes).await?;
            progress(bytes.len() as u64);
            offset += bytes.len() as u64;
        }
        dst.flush().await?;
        Ok(())
    }

    /// Writes `src` into an open remote file with up to `CONC` writes in flight.
    /// Every request that was sent is awaited before returning — also on cancel
    /// or error — and the file is then cut back to the gap-free prefix, so a
    /// later resume from the remote size never leaves a hole.
    async fn upload_open(
        &self,
        handle: &str,
        src: &mut (dyn AsyncRead + Unpin + Send),
        offset: u64,
        progress: &mut ProgressFn<'_>,
        cancel: &CancellationToken,
    ) -> Result<()> {
        let mut inflight = FuturesUnordered::new();
        let mut pos = offset;
        let mut buf = vec![0u8; CHUNK];
        let mut first_err: Option<AppError> = None;
        let mut lowest_failed: Option<u64> = None;

        let mut settle = |res: (u64, usize, std::result::Result<(), SftpError>),
                          progress: &mut ProgressFn<'_>,
                          first_err: &mut Option<AppError>| {
            let (at, n, r) = res;
            match r {
                Ok(()) => progress(n as u64),
                Err(e) => {
                    lowest_failed = Some(lowest_failed.map_or(at, |l| l.min(at)));
                    if first_err.is_none() {
                        *first_err = Some(e.into());
                    }
                }
            }
        };

        loop {
            if cancel.is_cancelled() {
                first_err = Some(AppError::Cancelled);
                break;
            }
            let n = match src.read(&mut buf).await {
                Ok(n) => n,
                Err(e) => {
                    first_err = Some(e.into());
                    break;
                }
            };
            if n == 0 {
                break;
            }
            let sftp = self.sftp.clone();
            let h = handle.to_string();
            let chunk = buf[..n].to_vec();
            let at = pos;
            pos += n as u64;
            inflight.push(async move { (at, n, sftp.write(h, at, chunk).await.map(|_| ())) });
            if inflight.len() >= CONC {
                if let Some(res) = inflight.next().await {
                    settle(res, progress, &mut first_err);
                    if first_err.is_some() {
                        break;
                    }
                }
            }
        }
        while let Some(res) = inflight.next().await {
            settle(res, progress, &mut first_err);
        }

        let Some(err) = first_err else {
            return Ok(());
        };
        let keep = lowest_failed.unwrap_or(pos);
        let attrs = FileAttributes {
            size: Some(keep),
            ..FileAttributes::empty()
        };
        let _ = self.sftp.fsetstat(handle.to_string(), attrs).await;
        Err(err)
    }
}

/// Reads exactly `len` bytes at `off` unless EOF comes first. SFTP servers may
/// legally answer a read with fewer bytes than requested.
async fn read_full(sftp: &RawSftpSession, handle: &str, off: u64, len: u32) -> Result<Vec<u8>> {
    let mut buf = Vec::with_capacity(len as usize);
    while buf.len() < len as usize {
        let got = buf.len() as u32;
        match sftp
            .read(handle.to_string(), off + got as u64, len - got)
            .await
        {
            Ok(d) if d.data.is_empty() => break,
            Ok(d) => buf.extend_from_slice(&d.data),
            Err(SftpError::Status(s)) if s.status_code == StatusCode::Eof => break,
            Err(e) => return Err(e.into()),
        }
    }
    Ok(buf)
}

fn entry_from_attrs(name: String, attrs: &FileAttributes) -> FileEntry {
    let perm = attrs.permissions.unwrap_or(0);
    FileEntry {
        name,
        is_dir: perm & S_IFMT == S_IFDIR,
        is_symlink: perm & S_IFMT == S_IFLNK,
        size: attrs.size.unwrap_or(0),
        modified: attrs.mtime.map(|m| m as i64),
        permissions: attrs.permissions.map(|p| p & 0o7777),
        owner: attrs.uid.map(|u| u.to_string()),
        group: attrs.gid.map(|g| g.to_string()),
    }
}

fn basename(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string())
}

#[async_trait::async_trait]
impl FileProvider for SftpProvider {
    fn kind(&self) -> &'static str {
        "sftp"
    }

    fn can_set_mtime(&self) -> bool {
        true
    }

    async fn home_dir(&self) -> Result<String> {
        let name = self.sftp.realpath(".").await?;
        Ok(name
            .files
            .first()
            .map(|f| f.filename.clone())
            .unwrap_or_else(|| "/".to_string()))
    }

    async fn list_dir(&self, path: &str) -> Result<Vec<FileEntry>> {
        let dir = self.sftp.opendir(path).await?;
        let handle = dir.handle;
        let mut out = Vec::new();
        loop {
            match self.sftp.readdir(handle.clone()).await {
                Ok(name) if !name.files.is_empty() => {
                    for f in name.files {
                        if f.filename == "." || f.filename == ".." || f.filename.is_empty() {
                            continue;
                        }
                        out.push(entry_from_attrs(f.filename, &f.attrs));
                    }
                }
                _ => break, // empty page or EOF status
            }
        }
        let _ = self.sftp.close(handle).await;
        Ok(out)
    }

    async fn stat(&self, path: &str) -> Result<FileEntry> {
        let attrs = self.sftp.stat(path).await?.attrs;
        Ok(entry_from_attrs(basename(path), &attrs))
    }

    async fn make_dir(&self, path: &str) -> Result<()> {
        self.sftp.mkdir(path, FileAttributes::empty()).await?;
        Ok(())
    }

    async fn remove_file(&self, path: &str) -> Result<()> {
        self.sftp.remove(path).await?;
        Ok(())
    }

    async fn remove_dir(&self, path: &str) -> Result<()> {
        let entries = self.list_dir(path).await?;
        for e in entries {
            let child = super::posix_join(path, &e.name);
            if e.is_dir && !e.is_symlink {
                Box::pin(self.remove_dir(&child)).await?;
            } else {
                self.remove_file(&child).await?;
            }
        }
        self.sftp.rmdir(path).await?;
        Ok(())
    }

    async fn rename(&self, from: &str, to: &str) -> Result<()> {
        self.sftp.rename(from, to).await?;
        Ok(())
    }

    async fn set_permissions(&self, path: &str, mode: u32) -> Result<()> {
        let attrs = FileAttributes {
            permissions: Some(mode),
            ..FileAttributes::empty()
        };
        self.sftp.setstat(path, attrs).await?;
        Ok(())
    }

    async fn set_mtime(&self, path: &str, mtime: i64) -> Result<()> {
        let t = mtime.clamp(0, u32::MAX as i64) as u32;
        let attrs = FileAttributes {
            atime: Some(t),
            mtime: Some(t),
            ..FileAttributes::empty()
        };
        self.sftp.setstat(path, attrs).await?;
        Ok(())
    }

    async fn checksum_sha256(&self, path: &str) -> Result<Option<String>> {
        let quoted = format!("'{}'", path.replace('\'', "'\\''"));
        // Hold the handle lock only to open the channel, not while hashing.
        let channel = self.handle.lock().await.channel_open_session().await?;
        channel
            .exec(true, format!("sha256sum -- {quoted}").as_bytes())
            .await?;
        let mut out: Vec<u8> = Vec::new();
        let mut ch = channel;
        while let Some(msg) = ch.wait().await {
            if let russh::ChannelMsg::Data { ref data } = msg {
                out.extend_from_slice(data);
            }
            if matches!(msg, russh::ChannelMsg::Eof | russh::ChannelMsg::Close) {
                break;
            }
        }
        let text = String::from_utf8_lossy(&out);
        let hash = text.split_whitespace().next().map(str::to_string);
        Ok(hash.filter(|h| h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit())))
    }

    async fn open_terminal(&self, cols: u32, rows: u32) -> Result<TerminalSession> {
        let mut channel = {
            let handle = self.handle.lock().await;
            handle.channel_open_session().await?
        };
        channel
            .request_pty(false, "xterm-256color", cols, rows, 0, 0, &[])
            .await?;
        channel.request_shell(true).await?;

        let (in_tx, mut in_rx) = mpsc::unbounded_channel::<TermMsg>();
        let (out_tx, out_rx) = mpsc::unbounded_channel::<TerminalEvent>();

        // One task owns the channel: it multiplexes shell output (→ out_tx) and
        // caller commands (← in_rx: input, resize, close).
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    msg = channel.wait() => match msg {
                        Some(russh::ChannelMsg::Data { data }) => {
                            let _ = out_tx.send(TerminalEvent::Data(data.to_vec()));
                        }
                        Some(russh::ChannelMsg::ExtendedData { data, .. }) => {
                            let _ = out_tx.send(TerminalEvent::Data(data.to_vec()));
                        }
                        None
                        | Some(russh::ChannelMsg::Eof)
                        | Some(russh::ChannelMsg::Close) => {
                            let _ = out_tx.send(TerminalEvent::Exit);
                            break;
                        }
                        _ => {}
                    },
                    cmd = in_rx.recv() => match cmd {
                        Some(TermMsg::Input(d)) => {
                            let _ = channel.data(&d[..]).await;
                        }
                        Some(TermMsg::Resize(c, r)) => {
                            let _ = channel.window_change(c, r, 0, 0).await;
                        }
                        None | Some(TermMsg::Close) => {
                            let _ = channel.close().await;
                            let _ = out_tx.send(TerminalEvent::Exit);
                            break;
                        }
                    },
                }
            }
        });

        Ok(TerminalSession {
            handle: Box::new(SshTerminal { tx: in_tx }),
            output: out_rx,
        })
    }

    async fn download_to(
        &self,
        remote: &str,
        dst: &mut (dyn AsyncWrite + Unpin + Send),
        offset: u64,
        progress: &mut ProgressFn,
        cancel: &CancellationToken,
    ) -> Result<()> {
        let handle = self
            .sftp
            .open(remote, OpenFlags::READ, FileAttributes::empty())
            .await?
            .handle;
        let res = self
            .download_open(&handle, dst, offset, progress, cancel)
            .await;
        let _ = self.sftp.close(handle).await;
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
        let flags = if offset > 0 {
            OpenFlags::WRITE | OpenFlags::CREATE
        } else {
            OpenFlags::WRITE | OpenFlags::CREATE | OpenFlags::TRUNCATE
        };
        let handle = self
            .sftp
            .open(remote, flags, FileAttributes::empty())
            .await?
            .handle;
        let res = self
            .upload_open(&handle, src, offset, progress, cancel)
            .await;
        let _ = self.sftp.close(handle).await;
        res
    }
}
