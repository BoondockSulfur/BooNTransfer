//! S3 (and S3-compatible: MinIO, Backblaze B2, …) provider.
//!
//! Uses `rusty-s3` to presign SigV4 requests, executed with reqwest (rustls).
//! One connection maps to one bucket; POSIX paths become object keys. S3 has no
//! real directories — listings use `prefix` + `delimiter=/`, and "folders" are
//! zero-byte `name/` marker objects. No rename (copy + delete); no chmod.

use super::{FileEntry, FileProvider, ProgressFn};
use crate::error::{AppError, Result};
use futures::StreamExt;
use reqwest::Client;
use rusty_s3::actions::ListObjectsV2;
use rusty_s3::{Bucket, Credentials, S3Action, UrlStyle};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio_util::sync::CancellationToken;

const CHUNK: usize = 128 * 1024;
const SIGN_TTL: Duration = Duration::from_secs(300);

pub struct S3Provider {
    bucket: Bucket,
    creds: Credentials,
    client: Client,
}

fn serr(e: impl std::fmt::Display) -> AppError {
    AppError::other(format!("S3: {e}"))
}

/// POSIX path -> object key (no leading slash). Directories keep no trailing
/// slash here; callers add one where the S3 API expects it.
fn key_of(path: &str) -> String {
    path.trim_start_matches('/').to_string()
}

fn basename(key: &str) -> String {
    key.trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or("")
        .to_string()
}

impl S3Provider {
    pub async fn connect(
        host: &str,
        port: u16,
        access_key: &str,
        secret_key: &str,
        tls: bool,
        bucket: &str,
        region: &str,
    ) -> Result<Self> {
        if bucket.is_empty() {
            return Err(AppError::other("S3: Bucket-Name fehlt"));
        }
        let scheme = if tls { "https" } else { "http" };
        let endpoint = reqwest::Url::parse(&format!("{scheme}://{host}:{port}")).map_err(serr)?;
        let region = if region.is_empty() {
            "us-east-1"
        } else {
            region
        };
        // Path-style addressing works for both AWS and self-hosted (MinIO).
        let bucket = Bucket::new(
            endpoint,
            UrlStyle::Path,
            bucket.to_string(),
            region.to_string(),
        )
        .map_err(serr)?;
        let creds = Credentials::new(access_key, secret_key);
        let p = S3Provider {
            bucket,
            creds,
            client: Client::builder().build().map_err(serr)?,
        };
        // Validate credentials/bucket up front.
        p.list_dir("/").await?;
        Ok(p)
    }

    fn prefix_of(path: &str) -> String {
        let k = key_of(path);
        if k.is_empty() || k.ends_with('/') {
            k
        } else {
            format!("{k}/")
        }
    }

    /// Lists every object key under `prefix` (recursive, paginated).
    async fn list_keys(&self, prefix: &str) -> Result<Vec<String>> {
        let mut keys = Vec::new();
        let mut token: Option<String> = None;
        loop {
            let mut action = self.bucket.list_objects_v2(Some(&self.creds));
            action.with_prefix(prefix.to_string());
            if let Some(t) = &token {
                action.with_continuation_token(t.clone());
            }
            let url = action.sign(SIGN_TTL);
            let text = self
                .client
                .get(url)
                .send()
                .await
                .map_err(serr)?
                .error_for_status()
                .map_err(serr)?
                .text()
                .await
                .map_err(serr)?;
            let resp = ListObjectsV2::parse_response(&text).map_err(serr)?;
            for c in resp.contents {
                keys.push(c.key);
            }
            match resp.next_continuation_token {
                Some(t) => token = Some(t),
                None => break,
            }
        }
        Ok(keys)
    }
}

fn entry(name: String, is_dir: bool, size: u64) -> FileEntry {
    FileEntry {
        name,
        is_dir,
        is_symlink: false,
        size,
        modified: None,
        permissions: None,
        owner: None,
        group: None,
    }
}

#[async_trait::async_trait]
impl FileProvider for S3Provider {
    fn kind(&self) -> &'static str {
        "s3"
    }

    async fn home_dir(&self) -> Result<String> {
        Ok("/".to_string())
    }

    async fn list_dir(&self, path: &str) -> Result<Vec<FileEntry>> {
        let prefix = Self::prefix_of(path);
        let mut out = Vec::new();
        let mut token: Option<String> = None;
        loop {
            let mut action = self.bucket.list_objects_v2(Some(&self.creds));
            if !prefix.is_empty() {
                action.with_prefix(prefix.clone());
            }
            action.with_delimiter("/");
            if let Some(t) = &token {
                action.with_continuation_token(t.clone());
            }
            let url = action.sign(SIGN_TTL);
            let text = self
                .client
                .get(url)
                .send()
                .await
                .map_err(serr)?
                .error_for_status()
                .map_err(serr)?
                .text()
                .await
                .map_err(serr)?;
            let resp = ListObjectsV2::parse_response(&text).map_err(serr)?;
            for cp in resp.common_prefixes {
                let name = basename(&cp.prefix);
                if !name.is_empty() {
                    out.push(entry(name, true, 0));
                }
            }
            for c in resp.contents {
                if c.key == prefix || c.key.ends_with('/') {
                    continue; // the folder marker itself
                }
                out.push(entry(basename(&c.key), false, c.size));
            }
            match resp.next_continuation_token {
                Some(t) => token = Some(t),
                None => break,
            }
        }
        Ok(out)
    }

    async fn stat(&self, path: &str) -> Result<FileEntry> {
        let key = key_of(path);
        let url = self
            .bucket
            .head_object(Some(&self.creds), &key)
            .sign(SIGN_TTL);
        let resp = self.client.head(url).send().await.map_err(serr)?;
        if resp.status().is_success() {
            let size = resp
                .headers()
                .get("content-length")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            return Ok(entry(basename(&key), false, size));
        }
        // Not an object — treat as a directory if anything lives under it.
        let prefix = Self::prefix_of(path);
        let keys = self.list_keys(&prefix).await?;
        if keys.is_empty() {
            return Err(AppError::other(format!("S3: nicht gefunden: {path}")));
        }
        Ok(entry(basename(&key), true, 0))
    }

    async fn make_dir(&self, path: &str) -> Result<()> {
        let marker = Self::prefix_of(path); // "foo/"
        let url = self
            .bucket
            .put_object(Some(&self.creds), &marker)
            .sign(SIGN_TTL);
        self.client
            .put(url)
            .body(Vec::new())
            .send()
            .await
            .map_err(serr)?
            .error_for_status()
            .map_err(serr)?;
        Ok(())
    }

    async fn remove_file(&self, path: &str) -> Result<()> {
        let key = key_of(path);
        let url = self
            .bucket
            .delete_object(Some(&self.creds), &key)
            .sign(SIGN_TTL);
        self.client
            .delete(url)
            .send()
            .await
            .map_err(serr)?
            .error_for_status()
            .map_err(serr)?;
        Ok(())
    }

    async fn remove_dir(&self, path: &str) -> Result<()> {
        let prefix = Self::prefix_of(path);
        let mut keys = self.list_keys(&prefix).await?;
        // Ensure the marker object itself is removed too.
        if !keys.iter().any(|k| k == &prefix) {
            keys.push(prefix);
        }
        for key in keys {
            let url = self
                .bucket
                .delete_object(Some(&self.creds), &key)
                .sign(SIGN_TTL);
            self.client
                .delete(url)
                .send()
                .await
                .map_err(serr)?
                .error_for_status()
                .map_err(serr)?;
        }
        Ok(())
    }

    async fn rename(&self, from: &str, to: &str) -> Result<()> {
        // No native rename: server-side copy, then delete the source.
        let src_key = key_of(from);
        let dst_key = key_of(to);
        let copy_source = format!("/{}/{}", self.bucket.name(), src_key);
        let url = self
            .bucket
            .put_object(Some(&self.creds), &dst_key)
            .sign(SIGN_TTL);
        self.client
            .put(url)
            .header("x-amz-copy-source", copy_source)
            .send()
            .await
            .map_err(serr)?
            .error_for_status()
            .map_err(serr)?;
        self.remove_file(from).await
    }

    async fn set_permissions(&self, _path: &str, _mode: u32) -> Result<()> {
        Err(AppError::other("S3 kennt keine Unix-Rechte (chmod)"))
    }

    async fn download_to(
        &self,
        remote: &str,
        dst: &mut (dyn AsyncWrite + Unpin + Send),
        offset: u64,
        progress: &mut ProgressFn,
        cancel: &CancellationToken,
    ) -> Result<()> {
        let key = key_of(remote);
        let url = self
            .bucket
            .get_object(Some(&self.creds), &key)
            .sign(SIGN_TTL);
        let mut req = self.client.get(url);
        if offset > 0 {
            req = req.header("Range", format!("bytes={offset}-"));
        }
        let resp = req
            .send()
            .await
            .map_err(serr)?
            .error_for_status()
            .map_err(serr)?;
        let mut stream = resp.bytes_stream();
        while let Some(chunk) = stream.next().await {
            if cancel.is_cancelled() {
                return Err(AppError::Cancelled);
            }
            let chunk = chunk.map_err(serr)?;
            dst.write_all(&chunk).await?;
            progress(chunk.len() as u64);
        }
        Ok(())
    }

    async fn upload_from(
        &self,
        src: &mut (dyn AsyncRead + Unpin + Send),
        remote: &str,
        size: u64,
        offset: u64,
        progress: &mut ProgressFn,
        cancel: &CancellationToken,
    ) -> Result<()> {
        if offset > 0 {
            return Err(AppError::other("S3-Upload unterstützt hier kein Resume"));
        }
        let mut buf: Vec<u8> = Vec::with_capacity(size as usize);
        let mut chunk = vec![0u8; CHUNK];
        loop {
            if cancel.is_cancelled() {
                return Err(AppError::Cancelled);
            }
            let n = src.read(&mut chunk).await?;
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..n]);
            progress(n as u64);
        }
        let key = key_of(remote);
        let url = self
            .bucket
            .put_object(Some(&self.creds), &key)
            .sign(SIGN_TTL);
        self.client
            .put(url)
            .body(buf)
            .send()
            .await
            .map_err(serr)?
            .error_for_status()
            .map_err(serr)?;
        Ok(())
    }
}
