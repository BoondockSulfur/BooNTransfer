//! SFTP-over-SSH provider (also the base for SCP-style transfers).
//!
//! `ssh2::Session` is blocking; we keep it behind a `&mut self` and open a fresh
//! `Sftp` handle per operation. That side-steps the self-referential lifetime
//! between `Session` and `Sftp` and keeps every borrow inside a single method
//! call. The whole provider is stored behind an `Arc<Mutex<_>>` at the state
//! layer, so operations on one connection are serialized (an acceptable
//! trade-off for the MVP; a dedicated transfer channel can be added later).

use super::{FileEntry, FileProvider, ProgressFn};
use crate::error::{AppError, Result};
use base64::Engine;
use ssh2::{
    CheckResult, FileStat, HashType, HostKeyType, KnownHostFileKind, KnownHostKeyFormat, OpenFlags,
    OpenType, Session,
};
use std::io::{Read, Seek, SeekFrom, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

// 128 KB streams help SCP; SFTP is round-trip bound in libssh2 (no request
// pipelining), so raw SFTP throughput is capped regardless.
const CHUNK: usize = 128 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
const OP_TIMEOUT_MS: u32 = 30_000;
const S_IFMT: u32 = 0o170000;
const S_IFLNK: u32 = 0o120000;
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
    /// When true, an unknown or changed host key is accepted and written to
    /// `known_hosts` instead of raising [`AppError::HostKeyVerification`].
    pub trust_host_key: bool,
    /// When true, byte transfers use the SCP protocol; browsing/metadata still
    /// use SFTP over the same SSH connection.
    pub use_scp: bool,
}

pub struct SftpProvider {
    session: Session,
    use_scp: bool,
}

fn known_hosts_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_default()
        .join(".ssh")
        .join("known_hosts")
}

fn host_key_type_str(t: HostKeyType) -> String {
    match t {
        HostKeyType::Rsa => "ssh-rsa",
        HostKeyType::Dss => "ssh-dss",
        HostKeyType::Ecdsa256 => "ecdsa-sha2-nistp256",
        HostKeyType::Ecdsa384 => "ecdsa-sha2-nistp384",
        HostKeyType::Ecdsa521 => "ecdsa-sha2-nistp521",
        HostKeyType::Ed25519 => "ssh-ed25519",
        _ => "unknown",
    }
    .to_string()
}

fn fingerprint(session: &Session) -> String {
    if let Some(hash) = session.host_key_hash(HashType::Sha256) {
        let b64 = base64::engine::general_purpose::STANDARD_NO_PAD.encode(hash);
        format!("SHA256:{b64}")
    } else if let Some(hash) = session.host_key_hash(HashType::Md5) {
        let hex: Vec<String> = hash.iter().map(|b| format!("{b:02x}")).collect();
        format!("MD5:{}", hex.join(":"))
    } else {
        "unknown".to_string()
    }
}

impl SftpProvider {
    pub fn connect(params: ConnectParams) -> Result<Self> {
        let addr = (params.host.as_str(), params.port)
            .to_socket_addrs()?
            .next()
            .ok_or_else(|| AppError::other("could not resolve host"))?;
        let tcp = TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT)?;
        tcp.set_nodelay(true).ok();

        let mut session = Session::new()?;
        session.set_tcp_stream(tcp);
        session.set_timeout(OP_TIMEOUT_MS);
        session.handshake()?;

        verify_host_key(&session, &params.host, params.port, params.trust_host_key)?;
        authenticate(&session, &params.username, &params.auth)?;

        if !session.authenticated() {
            return Err(AppError::AuthFailed);
        }
        Ok(SftpProvider {
            session,
            use_scp: params.use_scp,
        })
    }

    fn sftp(&self) -> Result<ssh2::Sftp> {
        Ok(self.session.sftp()?)
    }

    fn scp_download(
        &self,
        remote: &str,
        dst: &mut dyn Write,
        progress: &mut ProgressFn,
        cancel: &CancellationToken,
    ) -> Result<()> {
        let (mut chan, _stat) = self.session.scp_recv(Path::new(remote))?;
        let mut buf = vec![0u8; CHUNK];
        loop {
            if cancel.is_cancelled() {
                return Err(AppError::Cancelled);
            }
            let n = chan.read(&mut buf)?;
            if n == 0 {
                break;
            }
            dst.write_all(&buf[..n])?;
            progress(n as u64);
        }
        dst.flush()?;
        chan.send_eof()?;
        chan.wait_eof()?;
        chan.close()?;
        chan.wait_close()?;
        Ok(())
    }

    fn scp_upload(
        &self,
        src: &mut dyn Read,
        remote: &str,
        size: u64,
        progress: &mut ProgressFn,
        cancel: &CancellationToken,
    ) -> Result<()> {
        // SCP requires the exact size up front and streams the whole file.
        let mut chan = self
            .session
            .scp_send(Path::new(remote), 0o644, size, None)?;
        let mut buf = vec![0u8; CHUNK];
        let mut remaining = size;
        while remaining > 0 {
            if cancel.is_cancelled() {
                return Err(AppError::Cancelled);
            }
            let want = remaining.min(CHUNK as u64) as usize;
            let n = src.read(&mut buf[..want])?;
            if n == 0 {
                break;
            }
            chan.write_all(&buf[..n])?;
            progress(n as u64);
            remaining -= n as u64;
        }
        chan.send_eof()?;
        chan.wait_eof()?;
        chan.close()?;
        chan.wait_close()?;
        Ok(())
    }
}

fn verify_host_key(session: &Session, host: &str, port: u16, trust: bool) -> Result<()> {
    let (key, key_type) = session
        .host_key()
        .ok_or_else(|| AppError::other("server presented no host key"))?;
    let key_type_str = host_key_type_str(key_type);

    let mut known = session.known_hosts()?;
    // Missing known_hosts is fine (treated as "not found").
    let _ = known.read_file(&known_hosts_path(), KnownHostFileKind::OpenSSH);

    match known.check_port(host, port, key) {
        CheckResult::Match => Ok(()),
        CheckResult::NotFound | CheckResult::Mismatch => {
            let changed = matches!(known.check_port(host, port, key), CheckResult::Mismatch);
            if trust {
                // The key format must match the host key's algorithm; ssh2
                // provides the conversion from HostKeyType (which is Copy).
                known.add(
                    host,
                    key,
                    "added by LinuxSCP Client",
                    KnownHostKeyFormat::from(key_type),
                )?;
                known.write_file(&known_hosts_path(), KnownHostFileKind::OpenSSH)?;
                Ok(())
            } else {
                Err(AppError::HostKeyVerification {
                    fingerprint: fingerprint(session),
                    key_type: key_type_str,
                    changed,
                })
            }
        }
        CheckResult::Failure => Err(AppError::other("known_hosts check failed")),
    }
}

fn authenticate(session: &Session, user: &str, auth: &AuthMethod) -> Result<()> {
    match auth {
        AuthMethod::Password(pw) => {
            session.userauth_password(user, pw)?;
        }
        AuthMethod::KeyFile { path, passphrase } => {
            session.userauth_pubkey_file(user, None, Path::new(path), passphrase.as_deref())?;
        }
        AuthMethod::Agent => {
            let mut agent = session.agent()?;
            agent.connect()?;
            agent.list_identities()?;
            let mut ok = false;
            for identity in agent.identities()? {
                if agent.userauth(user, &identity).is_ok() {
                    ok = true;
                    break;
                }
            }
            agent.disconnect().ok();
            if !ok {
                return Err(AppError::AuthFailed);
            }
        }
    }
    Ok(())
}

fn entry_from_stat(name: String, stat: &FileStat) -> FileEntry {
    let perm = stat.perm.unwrap_or(0);
    let is_symlink = perm & S_IFMT == S_IFLNK;
    let is_dir = stat.is_dir() || (perm & S_IFMT == S_IFDIR);
    FileEntry {
        name,
        is_dir,
        is_symlink,
        size: stat.size.unwrap_or(0),
        modified: stat.mtime.map(|m| m as i64),
        permissions: stat.perm.map(|p| p & 0o7777),
        owner: stat.uid.map(|u| u.to_string()),
        group: stat.gid.map(|g| g.to_string()),
    }
}

impl FileProvider for SftpProvider {
    fn kind(&self) -> &'static str {
        "sftp"
    }

    fn home_dir(&mut self) -> Result<String> {
        let sftp = self.sftp()?;
        let path = sftp.realpath(Path::new("."))?;
        Ok(path.to_string_lossy().into_owned())
    }

    fn list_dir(&mut self, path: &str) -> Result<Vec<FileEntry>> {
        let sftp = self.sftp()?;
        let mut out = Vec::new();
        for (child, stat) in sftp.readdir(Path::new(path))? {
            let name = child
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            if name.is_empty() || name == "." || name == ".." {
                continue;
            }
            let mut entry = entry_from_stat(name, &stat);
            // Resolve symlinks so link-to-directory entries stay navigable.
            if entry.is_symlink {
                if let Ok(target) = sftp.stat(&child) {
                    entry.is_dir = target.is_dir();
                }
            }
            out.push(entry);
        }
        Ok(out)
    }

    fn stat(&mut self, path: &str) -> Result<FileEntry> {
        let sftp = self.sftp()?;
        let stat = sftp.stat(Path::new(path))?;
        let name = Path::new(path)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_string());
        Ok(entry_from_stat(name, &stat))
    }

    fn make_dir(&mut self, path: &str) -> Result<()> {
        let sftp = self.sftp()?;
        sftp.mkdir(Path::new(path), 0o755)?;
        Ok(())
    }

    fn remove_file(&mut self, path: &str) -> Result<()> {
        let sftp = self.sftp()?;
        sftp.unlink(Path::new(path))?;
        Ok(())
    }

    fn remove_dir(&mut self, path: &str) -> Result<()> {
        let sftp = self.sftp()?;
        remove_dir_recursive(&sftp, Path::new(path))
    }

    fn rename(&mut self, from: &str, to: &str) -> Result<()> {
        let sftp = self.sftp()?;
        sftp.rename(Path::new(from), Path::new(to), None)?;
        Ok(())
    }

    fn set_permissions(&mut self, path: &str, mode: u32) -> Result<()> {
        let sftp = self.sftp()?;
        let stat = FileStat {
            size: None,
            uid: None,
            gid: None,
            perm: Some(mode),
            atime: None,
            mtime: None,
        };
        sftp.setstat(Path::new(path), stat)?;
        Ok(())
    }

    fn checksum_sha256(&mut self, path: &str) -> Result<Option<String>> {
        // Have the server compute it via `sha256sum` over an exec channel.
        let quoted = format!("'{}'", path.replace('\'', "'\\''"));
        let mut channel = self.session.channel_session()?;
        channel.exec(&format!("sha256sum {quoted}"))?;
        let mut out = String::new();
        channel.read_to_string(&mut out)?;
        channel.wait_close().ok();
        let hash = out.split_whitespace().next().map(str::to_string);
        Ok(hash.filter(|h| h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit())))
    }

    fn download_to(
        &mut self,
        remote: &str,
        dst: &mut dyn Write,
        offset: u64,
        progress: &mut ProgressFn,
        cancel: &CancellationToken,
    ) -> Result<()> {
        if self.use_scp {
            if offset > 0 {
                return Err(AppError::other("SCP unterstützt kein Fortsetzen (Resume)"));
            }
            return self.scp_download(remote, dst, progress, cancel);
        }
        let sftp = self.sftp()?;
        let mut remote_file = sftp.open(Path::new(remote))?;
        if offset > 0 {
            remote_file.seek(SeekFrom::Start(offset))?;
        }
        let mut buf = vec![0u8; CHUNK];
        loop {
            if cancel.is_cancelled() {
                return Err(AppError::Cancelled);
            }
            let n = remote_file.read(&mut buf)?;
            if n == 0 {
                break;
            }
            dst.write_all(&buf[..n])?;
            progress(n as u64);
        }
        dst.flush()?;
        Ok(())
    }

    fn upload_from(
        &mut self,
        src: &mut dyn Read,
        remote: &str,
        size: u64,
        offset: u64,
        progress: &mut ProgressFn,
        cancel: &CancellationToken,
    ) -> Result<()> {
        if self.use_scp {
            if offset > 0 {
                return Err(AppError::other("SCP unterstützt kein Fortsetzen (Resume)"));
            }
            return self.scp_upload(src, remote, size, progress, cancel);
        }
        let sftp = self.sftp()?;
        let mut remote_file = if offset > 0 {
            // Resume: append at the current end (which equals `offset`).
            sftp.open_mode(
                Path::new(remote),
                OpenFlags::WRITE | OpenFlags::APPEND,
                0o644,
                OpenType::File,
            )?
        } else {
            sftp.create(Path::new(remote))?
        };
        let mut buf = vec![0u8; CHUNK];
        loop {
            if cancel.is_cancelled() {
                return Err(AppError::Cancelled);
            }
            let n = src.read(&mut buf)?;
            if n == 0 {
                break;
            }
            remote_file.write_all(&buf[..n])?;
            progress(n as u64);
        }
        remote_file.flush()?;
        Ok(())
    }
}

fn remove_dir_recursive(sftp: &ssh2::Sftp, path: &Path) -> Result<()> {
    for (child, stat) in sftp.readdir(path)? {
        let name = child.file_name().map(|n| n.to_string_lossy().into_owned());
        if matches!(name.as_deref(), Some(".") | Some("..") | None) {
            continue;
        }
        let perm = stat.perm.unwrap_or(0);
        let is_symlink = perm & S_IFMT == S_IFLNK;
        if stat.is_dir() && !is_symlink {
            remove_dir_recursive(sftp, &child)?;
        } else {
            sftp.unlink(&child)?;
        }
    }
    sftp.rmdir(path)?;
    Ok(())
}
