//! Integration tests for the transfer primitives, exercised through the local
//! provider (no network required). These cover the read/write/progress loop and
//! cancellation that the SFTP provider and the transfer queue rely on.

use linuxscp_lib::error::AppError;
use linuxscp_lib::protocol::local::{copy_with_progress, LocalProvider};
use linuxscp_lib::protocol::FileProvider;
use tokio_util::sync::CancellationToken;

fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("linuxscp-it-{}-{}", std::process::id(), name));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn roundtrip_reports_exact_progress() {
    let dir = scratch("roundtrip");
    let src = dir.join("src.bin");
    let dst = dir.join("dst.bin");
    let data = vec![0x5Au8; 200_000];
    std::fs::write(&src, &data).unwrap();

    let mut provider = LocalProvider::new();
    let mut out = std::fs::File::create(&dst).unwrap();
    let mut total: u64 = 0;
    let cancel = CancellationToken::new();
    {
        let mut progress = |delta: u64| total += delta;
        provider
            .download_to(src.to_str().unwrap(), &mut out, 0, &mut progress, &cancel)
            .unwrap();
    }
    drop(out);

    assert_eq!(total, data.len() as u64);
    assert_eq!(std::fs::read(&dst).unwrap(), data);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn resume_from_offset_completes_file() {
    let dir = scratch("resume");
    let src = dir.join("src.bin");
    let dst = dir.join("dst.part");
    let data: Vec<u8> = (0..150_000u32).map(|i| i as u8).collect();
    std::fs::write(&src, &data).unwrap();

    // Simulate an interrupted download: first 40_000 bytes already present.
    let offset = 40_000u64;
    std::fs::write(&dst, &data[..offset as usize]).unwrap();

    let mut provider = LocalProvider::new();
    let mut out = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&dst)
        .unwrap();
    let cancel = CancellationToken::new();
    let mut copied: u64 = 0;
    {
        let mut progress = |delta: u64| copied += delta;
        provider
            .download_to(
                src.to_str().unwrap(),
                &mut out,
                offset,
                &mut progress,
                &cancel,
            )
            .unwrap();
    }
    drop(out);

    assert_eq!(copied, data.len() as u64 - offset); // only the remainder transferred
    assert_eq!(std::fs::read(&dst).unwrap(), data); // file is complete & correct
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn cancelled_before_start_yields_cancelled_error() {
    let cancel = CancellationToken::new();
    cancel.cancel();

    let mut src: &[u8] = b"some payload that should never be copied";
    let mut dst: Vec<u8> = Vec::new();
    let mut progress = |_delta: u64| {};

    let result = copy_with_progress(&mut src, &mut dst, &mut progress, &cancel);
    assert!(matches!(result, Err(AppError::Cancelled)));
    assert!(dst.is_empty());
}
