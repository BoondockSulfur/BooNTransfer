//! End-to-end smoke test of the russh SftpProvider against a real sshd.
//! Run with a server available, e.g. the benchmark container:
//!   cargo run --example smoke -- 127.0.0.1 2223 tester testpass /config

use boon_transfer_lib::error::AppError;
use boon_transfer_lib::protocol::sftp::{AuthMethod, ConnectParams, SftpProvider};
use boon_transfer_lib::protocol::FileProvider;
use tokio_util::sync::CancellationToken;

#[tokio::main]
async fn main() {
    let a: Vec<String> = std::env::args().collect();
    let host = a.get(1).cloned().unwrap_or_else(|| "127.0.0.1".into());
    let port: u16 = a.get(2).and_then(|s| s.parse().ok()).unwrap_or(2223);
    let user = a.get(3).cloned().unwrap_or_else(|| "tester".into());
    let pass = a.get(4).cloned().unwrap_or_else(|| "testpass".into());
    let dir = a.get(5).cloned().unwrap_or_else(|| "/config".into());

    // pass == "agent" → use the SSH agent (SSH_AUTH_SOCK); otherwise password.
    let params = |trusted_fingerprint: Option<String>| ConnectParams {
        host: host.clone(),
        port,
        username: user.clone(),
        auth: if pass == "agent" {
            AuthMethod::Agent
        } else {
            AuthMethod::Password(pass.clone())
        },
        trusted_fingerprint,
    };

    // Test server: accept its host key after the first (rejected) attempt.
    let p = match SftpProvider::connect(params(None)).await {
        Ok(p) => p,
        Err(AppError::HostKeyVerification { fingerprint, .. }) => {
            println!("  host key {fingerprint} accepted for the test");
            SftpProvider::connect(params(Some(fingerprint)))
                .await
                .expect("connect")
        }
        Err(e) => panic!("connect: {e}"),
    };

    println!("✓ connect + auth");
    println!("  home      = {}", p.home_dir().await.expect("home"));
    println!(
        "  list {dir} = {} entries",
        p.list_dir(&dir).await.expect("list").len()
    );

    let remote = format!("{dir}/boon-smoke.bin");
    let data: Vec<u8> = (0..2_000_000u32).map(|i| i as u8).collect();
    let cancel = CancellationToken::new();
    let mut noop = |_: u64| {};

    let mut src = std::io::Cursor::new(data.clone());
    p.upload_from(&mut src, &remote, data.len() as u64, 0, &mut noop, &cancel)
        .await
        .expect("upload");
    println!("✓ upload   {} bytes (pipelined)", data.len());

    let mut buf: Vec<u8> = Vec::new();
    p.download_to(&remote, &mut buf, 0, &mut noop, &cancel)
        .await
        .expect("download");
    println!(
        "✓ download {} bytes — content match = {}",
        buf.len(),
        buf == data
    );

    println!(
        "  sha256    = {:?}",
        p.checksum_sha256(&remote).await.expect("sha256")
    );

    let st = p.stat(&remote).await.expect("stat");
    println!("  stat size = {}", st.size);

    p.remove_file(&remote).await.expect("remove");
    println!("✓ remove — ALL OK");
}
