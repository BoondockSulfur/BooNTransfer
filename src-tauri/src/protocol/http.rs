//! Shared HTTP plumbing for the WebDAV and S3 providers: a client with sane
//! timeouts, streaming uploads (no whole-file buffering) and range downloads.

use super::ProgressFn;
use crate::error::{AppError, Result};
use futures::{SinkExt, StreamExt};
use reqwest::{Client, RequestBuilder, Response, StatusCode};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio_util::sync::CancellationToken;

const CHUNK: usize = 128 * 1024;

/// Client for file traffic: bounded connect time and a read timeout that
/// catches stalled servers without limiting the duration of large transfers.
pub fn client() -> reqwest::Result<Client> {
    Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .read_timeout(Duration::from_secs(60))
        .build()
}

/// Sends `req` with `src` as a streamed body of exactly `size` bytes. Progress
/// is reported as data is handed to the connection; cancellation aborts the
/// request.
pub async fn send_streaming(
    req: RequestBuilder,
    src: &mut (dyn AsyncRead + Unpin + Send),
    size: u64,
    progress: &mut ProgressFn<'_>,
    cancel: &CancellationToken,
) -> Result<Response> {
    let (tx, rx) = futures::channel::mpsc::channel::<std::io::Result<Vec<u8>>>(4);
    let send = req
        .header(reqwest::header::CONTENT_LENGTH, size)
        .body(reqwest::Body::wrap_stream(rx))
        .send();
    // The pump owns the sender: when it returns, the body ends (early on
    // cancel/error, which makes the request fail instead of hanging).
    let pump = async move {
        let mut tx = tx;
        let mut buf = vec![0u8; CHUNK];
        loop {
            if cancel.is_cancelled() {
                let _ = tx.send(Err(std::io::Error::other("cancelled"))).await;
                return Err(AppError::Cancelled);
            }
            let n = match src.read(&mut buf).await {
                Ok(n) => n,
                Err(e) => {
                    let _ = tx.send(Err(std::io::Error::other(e.to_string()))).await;
                    return Err(e.into());
                }
            };
            if n == 0 {
                return Ok(());
            }
            if tx.send(Ok(buf[..n].to_vec())).await.is_err() {
                return Ok(()); // request already finished; its response tells why
            }
            progress(n as u64);
        }
    };
    let (resp, pumped) = futures::join!(send, pump);
    pumped?;
    resp.map_err(|e| AppError::other(e.to_string()))
}

/// Streams a GET response into `dst`. With `offset > 0` a `Range` request was
/// sent; a server that ignores it answers 200 with the whole body, so the
/// already present prefix is skipped instead of being appended twice.
pub async fn receive(
    resp: Response,
    dst: &mut (dyn AsyncWrite + Unpin + Send),
    offset: u64,
    progress: &mut ProgressFn<'_>,
    cancel: &CancellationToken,
) -> Result<()> {
    let mut skip = if offset > 0 && resp.status() != StatusCode::PARTIAL_CONTENT {
        offset
    } else {
        0
    };
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        if cancel.is_cancelled() {
            return Err(AppError::Cancelled);
        }
        let chunk = chunk.map_err(|e| AppError::other(e.to_string()))?;
        let mut data = &chunk[..];
        if skip > 0 {
            let n = skip.min(data.len() as u64) as usize;
            data = &data[n..];
            skip -= n as u64;
        }
        if data.is_empty() {
            continue;
        }
        dst.write_all(data).await?;
        progress(data.len() as u64);
    }
    dst.flush().await?;
    Ok(())
}
