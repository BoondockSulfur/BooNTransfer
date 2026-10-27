//! End-to-end smoke test of WebdavProvider against a real WebDAV server.
//!   cargo run --example webdav_smoke -- 127.0.0.1 8088 tester testpass /

use boon_transfer_lib::protocol::webdav::WebdavProvider;
use boon_transfer_lib::protocol::FileProvider;
use tokio_util::sync::CancellationToken;

#[tokio::main]
async fn main() {
    let a: Vec<String> = std::env::args().collect();
    let host = a.get(1).cloned().unwrap_or_else(|| "127.0.0.1".into());
    let port: u16 = a.get(2).and_then(|s| s.parse().ok()).unwrap_or(8088);
    let user = a.get(3).cloned().unwrap_or_else(|| "tester".into());
    let pass = a.get(4).cloned().unwrap_or_else(|| "testpass".into());
    let root = a.get(5).cloned().unwrap_or_else(|| "/".into());
    let tls = a.get(6).map(|s| s == "tls").unwrap_or(false);

    let p = WebdavProvider::connect(&host, port, &user, &pass, tls, &root)
        .await
        .expect("connect");
    println!("✓ connect (tls={tls})");
    println!("  home = {}", p.home_dir().await.expect("home"));
    println!(
        "  list / = {} entries",
        p.list_dir("/").await.expect("list").len()
    );

    let dir = "/boon-dav-test";
    let _ = p.remove_dir(dir).await; // clean slate
    p.make_dir(dir).await.expect("mkdir");
    println!("✓ mkdir {dir}");

    let remote = format!("{dir}/data.bin");
    let data: Vec<u8> = (0..300_000u32).map(|i| i as u8).collect();
    let cancel = CancellationToken::new();
    let mut noop = |_: u64| {};

    let mut src = std::io::Cursor::new(data.clone());
    p.upload_from(&mut src, &remote, data.len() as u64, 0, &mut noop, &cancel)
        .await
        .expect("upload");
    println!("✓ upload {} bytes", data.len());

    let st = p.stat(&remote).await.expect("stat");
    println!(
        "✓ stat: name={} size={} dir={}",
        st.name, st.size, st.is_dir
    );

    let mut buf: Vec<u8> = Vec::new();
    p.download_to(&remote, &mut buf, 0, &mut noop, &cancel)
        .await
        .expect("download");
    println!(
        "✓ download {} bytes — content match = {}",
        buf.len(),
        buf == data
    );

    let renamed = format!("{dir}/renamed.bin");
    p.rename(&remote, &renamed).await.expect("rename");
    println!("✓ rename");

    p.remove_file(&renamed).await.expect("remove file");
    p.remove_dir(dir).await.expect("remove dir");
    println!("✓ remove — ALL OK");
}
