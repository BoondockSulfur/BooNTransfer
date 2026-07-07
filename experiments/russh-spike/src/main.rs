//! russh-spike — Phase-1-Prototyp/Benchmark für den v2-Umbau.
//!
//! Fährt denselben Ablauf (verbinden, auflisten, hoch-/runterladen mit
//! Durchsatzmessung) einmal über **russh + russh-sftp** und einmal über das
//! aktuelle **ssh2 (libssh2)** — zum direkten Vergleich, optional mit künstlicher
//! Latenz (netem). Berührt den App-Code NICHT.
//!
//! Aufruf:
//!   russh-spike <russh|ssh2|both> <host> <port> <user> <pass> <remote_dir> <size_mb>

use anyhow::{bail, Context, Result};
use std::io::Read;
use std::sync::Arc;
use std::time::{Duration, Instant};

fn mbps(bytes: usize, dt: Duration) -> f64 {
    (bytes as f64 / (1024.0 * 1024.0)) / dt.as_secs_f64()
}

fn random_data(len: usize) -> Result<Vec<u8>> {
    // Incompressible payload so SSH-Kompression den Benchmark nicht verfälscht.
    let mut buf = vec![0u8; len];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut buf)?;
    Ok(buf)
}

// ---------------------------------------------------------------------------
// russh backend
// ---------------------------------------------------------------------------

struct AcceptAll;

#[async_trait::async_trait]
impl russh::client::Handler for AcceptAll {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        _server_public_key: &russh::keys::key::PublicKey,
    ) -> Result<bool, Self::Error> {
        Ok(true) // Prototyp: Host-Key akzeptieren (in v2 gegen known_hosts prüfen)
    }
}

async fn bench_russh(
    host: &str,
    port: u16,
    user: &str,
    pass: &str,
    remote_dir: &str,
    size_mb: usize,
) -> Result<()> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let config = Arc::new(russh::client::Config::default());
    let mut handle = russh::client::connect(config, (host, port), AcceptAll)
        .await
        .context("russh connect")?;
    let authenticated = handle
        .authenticate_password(user, pass)
        .await
        .context("russh auth")?;
    if !authenticated {
        bail!("russh: Authentifizierung fehlgeschlagen");
    }

    let channel = handle.channel_open_session().await?;
    channel.request_subsystem(true, "sftp").await?;
    let sftp = russh_sftp::client::SftpSession::new(channel.into_stream())
        .await
        .context("russh-sftp session")?;

    let entries = sftp.read_dir(remote_dir).await?;
    let count = entries.count();

    let data = random_data(size_mb * 1024 * 1024)?;
    let remote_path = format!("{}/spike-bench.bin", remote_dir.trim_end_matches('/'));

    let t = Instant::now();
    let mut f = sftp.create(&remote_path).await?;
    f.write_all(&data).await?;
    f.flush().await?;
    f.shutdown().await?;
    let up = t.elapsed();

    let t = Instant::now();
    let mut f = sftp.open(&remote_path).await?;
    let mut buf = Vec::with_capacity(data.len());
    f.read_to_end(&mut buf).await?;
    let down = t.elapsed();

    if buf.len() != data.len() {
        bail!("russh: Größe stimmt nicht ({} != {})", buf.len(), data.len());
    }
    let _ = sftp.remove_file(&remote_path).await;

    println!(
        "russh : readdir={count:>3}  upload={:>6.1} MB/s  download={:>6.1} MB/s",
        mbps(data.len(), up),
        mbps(data.len(), down),
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// russh backend MIT selbst gebautem Pipelining (viele parallele Requests)
// ---------------------------------------------------------------------------

async fn bench_russh_pipelined(
    host: &str,
    port: u16,
    user: &str,
    pass: &str,
    remote_dir: &str,
    size_mb: usize,
) -> Result<()> {
    use futures::stream::{self, StreamExt};
    use russh_sftp::protocol::{FileAttributes, OpenFlags};

    const CHUNK: usize = 32 * 1024; // SFTP-Paketgröße, überall akzeptiert
    const CONC: usize = 64; // gleichzeitig offene Requests

    // Größeres Kanal-Fenster als der 2-MB-Default, damit unter Latenz mehr fliegt.
    let config = Arc::new(russh::client::Config {
        window_size: 64 * 1024 * 1024,
        ..Default::default()
    });
    let mut handle = russh::client::connect(config, (host, port), AcceptAll).await?;
    if !handle.authenticate_password(user, pass).await? {
        bail!("russhp: Authentifizierung fehlgeschlagen");
    }
    let channel = handle.channel_open_session().await?;
    channel.request_subsystem(true, "sftp").await?;
    let raw = Arc::new(russh_sftp::client::RawSftpSession::new(channel.into_stream()));
    raw.init().await?;

    let data = random_data(size_mb * 1024 * 1024)?;
    let total = data.len();
    let data = Arc::new(data);
    let remote_path = format!("{}/spike-bench.bin", remote_dir.trim_end_matches('/'));
    let n_chunks = total.div_ceil(CHUNK);

    // --- Upload: viele write(offset) gleichzeitig ---
    let t = Instant::now();
    let wh = raw
        .open(
            &remote_path,
            OpenFlags::CREATE | OpenFlags::WRITE | OpenFlags::TRUNCATE,
            FileAttributes::default(),
        )
        .await?
        .handle;
    let up_results: Vec<_> = stream::iter((0..n_chunks).map(|i| {
        let raw = raw.clone();
        let data = data.clone();
        let h = wh.clone();
        let off = i * CHUNK;
        let end = (off + CHUNK).min(total);
        async move { raw.write(h, off as u64, data[off..end].to_vec()).await }
    }))
    .buffer_unordered(CONC)
    .collect()
    .await;
    for r in up_results {
        r?;
    }
    raw.close(wh).await?;
    let up = t.elapsed();

    // --- Download: viele read(offset) gleichzeitig ---
    let t = Instant::now();
    let rh = raw
        .open(&remote_path, OpenFlags::READ, FileAttributes::default())
        .await?
        .handle;
    let read_results: Vec<_> = stream::iter((0..n_chunks).map(|i| {
        let raw = raw.clone();
        let h = rh.clone();
        let off = i * CHUNK;
        let len = (total - off).min(CHUNK) as u32;
        async move { raw.read(h, off as u64, len).await.map(|d| (off, d.data)) }
    }))
    .buffer_unordered(CONC)
    .collect()
    .await;
    let mut buf = vec![0u8; total];
    for r in read_results {
        let (off, bytes) = r?;
        buf[off..off + bytes.len()].copy_from_slice(&bytes);
    }
    raw.close(rh).await?;
    let down = t.elapsed();

    if buf != *data {
        bail!("russhp: Inhalt weicht ab");
    }
    let _ = raw.remove(&remote_path).await;

    println!(
        "russhp: readdir=  -  upload={:>6.1} MB/s  download={:>6.1} MB/s  (pipelined, {CONC}×{}KB)",
        mbps(total, up),
        mbps(total, down),
        CHUNK / 1024,
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// ssh2 backend (aktueller Stack)
// ---------------------------------------------------------------------------

fn bench_ssh2(
    host: &str,
    port: u16,
    user: &str,
    pass: &str,
    remote_dir: &str,
    size_mb: usize,
) -> Result<()> {
    use std::io::Write;
    use std::net::TcpStream;

    let tcp = TcpStream::connect((host, port))?;
    let mut sess = ssh2::Session::new()?;
    sess.set_tcp_stream(tcp);
    sess.handshake()?;
    sess.userauth_password(user, pass)?;
    if !sess.authenticated() {
        bail!("ssh2: Authentifizierung fehlgeschlagen");
    }
    let sftp = sess.sftp()?;

    let count = sftp.readdir(std::path::Path::new(remote_dir))?.len();

    let data = random_data(size_mb * 1024 * 1024)?;
    let remote_path = format!("{}/spike-bench.bin", remote_dir.trim_end_matches('/'));
    let rp = std::path::Path::new(&remote_path);

    let t = Instant::now();
    let mut f = sftp.create(rp)?;
    f.write_all(&data)?;
    drop(f);
    let up = t.elapsed();

    let t = Instant::now();
    let mut f = sftp.open(rp)?;
    let mut buf = Vec::with_capacity(data.len());
    f.read_to_end(&mut buf)?;
    let down = t.elapsed();

    if buf.len() != data.len() {
        bail!("ssh2: Größe stimmt nicht");
    }
    let _ = sftp.unlink(rp);

    println!(
        "ssh2  : readdir={count:>3}  upload={:>6.1} MB/s  download={:>6.1} MB/s",
        mbps(data.len(), up),
        mbps(data.len(), down),
    );
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let a: Vec<String> = std::env::args().collect();
    let backend = a.get(1).map(String::as_str).unwrap_or("both");
    let host = a.get(2).map(String::as_str).unwrap_or("127.0.0.1");
    let port: u16 = a.get(3).and_then(|s| s.parse().ok()).unwrap_or(2222);
    let user = a.get(4).map(String::as_str).unwrap_or("tester");
    let pass = a.get(5).map(String::as_str).unwrap_or("testpass");
    let dir = a.get(6).map(String::as_str).unwrap_or("/config");
    let size_mb: usize = a.get(7).and_then(|s| s.parse().ok()).unwrap_or(32);

    println!("== russh-spike: {host}:{port} als {user}, {size_mb} MB, dir={dir} ==");
    let all = backend == "all" || backend == "both";
    if backend == "ssh2" || all {
        bench_ssh2(host, port, user, pass, dir, size_mb)?;
    }
    if backend == "russh" || all {
        bench_russh(host, port, user, pass, dir, size_mb).await?;
    }
    if backend == "russhp" || all {
        bench_russh_pipelined(host, port, user, pass, dir, size_mb).await?;
    }
    Ok(())
}
