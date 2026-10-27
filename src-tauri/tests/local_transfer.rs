//! Integration tests for the async transfer primitives via the local provider
//! (no network). Cover the read/write/progress loop, resume, and cancellation
//! that the SFTP provider and the transfer queue rely on.

use boon_transfer_lib::error::AppError;
use boon_transfer_lib::protocol::local::{copy_with_progress, LocalProvider};
use boon_transfer_lib::protocol::FileProvider;
use tokio_util::sync::CancellationToken;

fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("boon-it-{}-{}", std::process::id(), name));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[tokio::test]
async fn roundtrip_reports_exact_progress() {
    let dir = scratch("roundtrip");
    let src = dir.join("src.bin");
    let dst = dir.join("dst.bin");
    let data = vec![0x5Au8; 200_000];
    tokio::fs::write(&src, &data).await.unwrap();

    let provider = LocalProvider::new();
    let mut out = tokio::fs::File::create(&dst).await.unwrap();
    let mut total: u64 = 0;
    let cancel = CancellationToken::new();
    {
        let mut progress = |delta: u64| total += delta;
        provider
            .download_to(src.to_str().unwrap(), &mut out, 0, &mut progress, &cancel)
            .await
            .unwrap();
    }

    assert_eq!(total, data.len() as u64);
    assert_eq!(tokio::fs::read(&dst).await.unwrap(), data);
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn resume_from_offset_completes_file() {
    let dir = scratch("resume");
    let src = dir.join("src.bin");
    let dst = dir.join("dst.part");
    let data: Vec<u8> = (0..150_000u32).map(|i| i as u8).collect();
    tokio::fs::write(&src, &data).await.unwrap();

    let offset = 40_000u64;
    tokio::fs::write(&dst, &data[..offset as usize])
        .await
        .unwrap();

    let provider = LocalProvider::new();
    let mut out = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&dst)
        .await
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
            .await
            .unwrap();
    }

    assert_eq!(copied, data.len() as u64 - offset);
    assert_eq!(tokio::fs::read(&dst).await.unwrap(), data);
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn cancelled_before_start_yields_cancelled_error() {
    let cancel = CancellationToken::new();
    cancel.cancel();

    let mut src: &[u8] = b"some payload that should never be copied";
    let mut dst: Vec<u8> = Vec::new();
    let mut progress = |_delta: u64| {};

    let result = copy_with_progress(&mut src, &mut dst, &mut progress, &cancel).await;
    assert!(matches!(result, Err(AppError::Cancelled)));
    assert!(dst.is_empty());
}
