//! SFTP-over-SSH provider on **russh** (pure Rust, no libssh2/OpenSSL).
//!
//! Transfers pipeline many concurrent SFTP read/write requests over one channel
//! (`RawSftpSession`), which hides latency — see `experiments/russh-spike`.
//! `SftpProvider` is shared as `Arc<dyn FileProvider>` (methods take `&self`);
//! the SFTP session is internally `Arc`-multiplexed and the SSH `Handle` sits
//! behind a mutex for the occasional exec channel (checksums).

use super::{FileEntry, FileProvider, ProgressFn, Terminal, TerminalEvent, TerminalSession};
use crate::error::{AppError, Result};
use futures::stream::{FuturesUnordered, StreamExt};
use russh::client::{Config, Handle, Handler};
use russh::keys::key::PublicKey;
use russh_sftp::client::RawSftpSession;
use russh_sftp::protocol::{FileAttributes, OpenFlags};
use std::sync::{Arc, Mutex as StdMutex};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{mpsc, Mutex as AsyncMutex};
use tokio_util::sync::CancellationToken;

const CHUNK: usize = 32 * 1024; // SFTP data packet size (widely accepted)
const CONC: usize = 64; // concurrent in-flight requests (pipelining)
const WINDOW: u32 = 64 * 1024 * 1024; // large channel window for high-latency links
const S_IFMT: u32 = 0o170000;
const S_IFLNK: u32 = 0o120000;

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
const S_IFDIR: u32 = 0o040000;

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
    pub trust_host_key: bool,
    /// Kept for API compatibility; in v2 "SCP" transfers reuse fast pipelined SFTP.
    pub use_scp: bool,
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
    trust: bool,
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
            Err(_) => (false, true), // changed key or read error → treat as changed
        };
        if known {
            return Ok(true);
        }
        if self.trust {
            let _ = russh::keys::learn_known_hosts(&self.host, self.port, key);
            return Ok(true);
        }
        *self.reject.lock().unwrap() = Some(HostKeyReject {
            fingerprint: key.fingerprint(),
            key_type: key.name().to_string(),
            changed,
        });
        Ok(false)
    }
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
            trust: params.trust_host_key,
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
        self.sftp.mkdir(path, FileAttributes::default()).await?;
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
            ..Default::default()
        };
        self.sftp.setstat(path, attrs).await?;
        Ok(())
    }

    async fn checksum_sha256(&self, path: &str) -> Result<Option<String>> {
        let quoted = format!("'{}'", path.replace('\'', "'\\''"));
        let handle = self.handle.lock().await;
        let channel = handle.channel_open_session().await?;
        channel
            .exec(true, format!("sha256sum {quoted}").as_bytes())
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
        let opened = self
            .sftp
            .open(remote, OpenFlags::READ, FileAttributes::default())
            .await?;
        let handle = opened.handle;
        let size = self
            .sftp
            .fstat(handle.clone())
            .await?
            .attrs
            .size
            .unwrap_or(0);

        // Pipeline reads; `buffered` yields them IN ORDER for sequential writing.
        let read_handle = handle.clone();
        let offsets: Vec<u64> = (offset..size).step_by(CHUNK).collect();
        let mut stream = futures::stream::iter(offsets.into_iter().map(move |off| {
            let sftp = self.sftp.clone();
            let h = read_handle.clone();
            let len = (size - off).min(CHUNK as u64) as u32;
            async move { sftp.read(h, off, len).await.map(|d| d.data) }
        }))
        .buffered(CONC);

        while let Some(chunk) = stream.next().await {
            if cancel.is_cancelled() {
                return Err(AppError::Cancelled);
            }
            let bytes = chunk?;
            dst.write_all(&bytes).await?;
            progress(bytes.len() as u64);
        }
        dst.flush().await?;
        let _ = self.sftp.close(handle).await;
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
        let flags = if offset > 0 {
            OpenFlags::WRITE | OpenFlags::CREATE
        } else {
            OpenFlags::WRITE | OpenFlags::CREATE | OpenFlags::TRUNCATE
        };
        let opened = self
            .sftp
            .open(remote, flags, FileAttributes::default())
            .await?;
        let handle = opened.handle;

        // Read src sequentially, keep up to CONC writes in flight at explicit offsets.
        let mut inflight = FuturesUnordered::new();
        let mut pos = offset;
        let mut buf = vec![0u8; CHUNK];
        loop {
            if cancel.is_cancelled() {
                return Err(AppError::Cancelled);
            }
            let n = src.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            let sftp = self.sftp.clone();
            let h = handle.clone();
            let chunk = buf[..n].to_vec();
            let at = pos;
            pos += n as u64;
            inflight.push(async move { sftp.write(h, at, chunk).await });
            progress(n as u64);
            if inflight.len() >= CONC {
                if let Some(r) = inflight.next().await {
                    r?;
                }
            }
        }
        while let Some(r) = inflight.next().await {
            r?;
        }
        let _ = self.sftp.close(handle).await;
        Ok(())
    }
}
