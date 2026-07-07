//! End-to-end smoke test of S3Provider against a real S3 server (MinIO).
//!   cargo run --example s3_smoke -- 127.0.0.1 9000 minioadmin minioadmin boon-test us-east-1

use linuxscp_lib::protocol::s3::S3Provider;
use linuxscp_lib::protocol::FileProvider;
use tokio_util::sync::CancellationToken;

#[tokio::main]
async fn main() {
    let a: Vec<String> = std::env::args().collect();
    let host = a.get(1).cloned().unwrap_or_else(|| "127.0.0.1".into());
    let port: u16 = a.get(2).and_then(|s| s.parse().ok()).unwrap_or(9000);
    let access = a.get(3).cloned().unwrap_or_else(|| "minioadmin".into());
    let secret = a.get(4).cloned().unwrap_or_else(|| "minioadmin".into());
    let bucket = a.get(5).cloned().unwrap_or_else(|| "boon-test".into());
    let region = a.get(6).cloned().unwrap_or_else(|| "us-east-1".into());

    let p = S3Provider::connect(&host, port, &access, &secret, false, &bucket, &region)
        .await
        .expect("connect");
    println!("✓ connect (bucket={bucket})");
    println!(
        "  list / = {} entries",
        p.list_dir("/").await.expect("list").len()
    );

    p.make_dir("/boon-dir").await.expect("mkdir");
    println!("✓ mkdir /boon-dir");

    let remote = "/boon-dir/data.bin";
    let data: Vec<u8> = (0..300_000u32).map(|i| i as u8).collect();
    let cancel = CancellationToken::new();
    let mut noop = |_: u64| {};

    let mut src = std::io::Cursor::new(data.clone());
    p.upload_from(&mut src, remote, data.len() as u64, 0, &mut noop, &cancel)
        .await
        .expect("upload");
    println!("✓ upload {} bytes", data.len());

    let st = p.stat(remote).await.expect("stat");
    println!(
        "✓ stat: name={} size={} dir={}",
        st.name, st.size, st.is_dir
    );
    println!(
        "  list /boon-dir = {} entries",
        p.list_dir("/boon-dir").await.expect("list").len()
    );

    let mut buf: Vec<u8> = Vec::new();
    p.download_to(remote, &mut buf, 0, &mut noop, &cancel)
        .await
        .expect("download");
    println!(
        "✓ download {} bytes — content match = {}",
        buf.len(),
        buf == data
    );

    let renamed = "/boon-dir/renamed.bin";
    p.rename(remote, renamed).await.expect("rename");
    println!("✓ rename (copy+delete)");

    p.remove_file(renamed).await.expect("remove file");
    p.remove_dir("/boon-dir").await.expect("remove dir");
    println!("✓ remove — ALL OK");
}
