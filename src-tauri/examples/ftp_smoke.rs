//! End-to-end smoke test of the async FtpProvider against a real FTP(S) server.
//!   cargo run --example ftp_smoke -- 127.0.0.1 21 tester testpass /ftp/tester [tls]

use linuxscp_lib::protocol::ftp::FtpProvider;
use linuxscp_lib::protocol::FileProvider;
use tokio_util::sync::CancellationToken;

#[tokio::main]
async fn main() {
    let a: Vec<String> = std::env::args().collect();
    let host = a.get(1).cloned().unwrap_or_else(|| "127.0.0.1".into());
    let port: u16 = a.get(2).and_then(|s| s.parse().ok()).unwrap_or(21);
    let user = a.get(3).cloned().unwrap_or_else(|| "tester".into());
    let pass = a.get(4).cloned().unwrap_or_else(|| "testpass".into());
    let dir = a.get(5).cloned().unwrap_or_else(|| "/".into());
    let tls = a.get(6).map(|s| s == "tls").unwrap_or(false);

    let p = FtpProvider::connect(&host, port, &user, &pass, tls)
        .await
        .expect("connect");
    println!("✓ connect (tls={tls})");
    println!("  home      = {}", p.home_dir().await.expect("home"));
    println!(
        "  list {dir} = {} entries",
        p.list_dir(&dir).await.expect("list").len()
    );

    let remote = format!("{}/boon-ftp.bin", dir.trim_end_matches('/'));
    let data: Vec<u8> = (0..500_000u32).map(|i| i as u8).collect();
    let cancel = CancellationToken::new();
    let mut noop = |_: u64| {};

    let mut src = std::io::Cursor::new(data.clone());
    p.upload_from(&mut src, &remote, data.len() as u64, 0, &mut noop, &cancel)
        .await
        .expect("upload");
    println!("✓ upload   {} bytes", data.len());

    let mut buf: Vec<u8> = Vec::new();
    p.download_to(&remote, &mut buf, 0, &mut noop, &cancel)
        .await
        .expect("download");
    println!(
        "✓ download {} bytes — content match = {}",
        buf.len(),
        buf == data
    );

    p.remove_file(&remote).await.expect("remove");
    println!("✓ remove — ALL OK");
}
