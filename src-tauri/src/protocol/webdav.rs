//! WebDAV / WebDAVS provider over HTTP(S) using reqwest (rustls) + quick-xml.
//!
//! Directory listings use `PROPFIND Depth: 1`; the rest map to WebDAV verbs
//! (MKCOL, DELETE, MOVE, GET/PUT). Auth is HTTP Basic. Uploads stream with a
//! fixed length (WebDAV has no standard resume); downloads stream with Range.

use super::{http, FileEntry, FileProvider, ProgressFn};
use crate::error::{AppError, Result};
use percent_encoding::percent_decode_str;
use quick_xml::events::Event;
use reqwest::{Client, Method, Url};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_util::sync::CancellationToken;

pub struct WebdavProvider {
    client: Client,
    base: Url,
    user: String,
    pass: String,
}

struct RawEntry {
    href: String,
    is_dir: bool,
    size: u64,
}

fn werr(e: impl std::fmt::Display) -> AppError {
    AppError::other(format!("WebDAV: {e}"))
}

fn propfind_method() -> Method {
    Method::from_bytes(b"PROPFIND").expect("valid method")
}

/// Parses a WebDAV multistatus body into raw entries (href/is_dir/size).
fn parse_propfind(xml: &str) -> Result<Vec<RawEntry>> {
    let mut reader = quick_xml::Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut out = Vec::new();
    let (mut href, mut is_dir, mut size) = (String::new(), false, 0u64);
    let mut capture: Option<&'static str> = None;
    loop {
        match reader.read_event().map_err(werr)? {
            Event::Start(e) => match e.local_name().as_ref() {
                b"response" => {
                    href.clear();
                    is_dir = false;
                    size = 0;
                }
                b"href" => capture = Some("href"),
                b"getcontentlength" => capture = Some("size"),
                b"collection" => is_dir = true,
                _ => {}
            },
            Event::Empty(e) => {
                if e.local_name().as_ref() == b"collection" {
                    is_dir = true;
                }
            }
            Event::Text(e) => {
                if let Some(field) = capture.take() {
                    let txt = e.decode().map_err(werr)?.into_owned();
                    match field {
                        "href" => href = txt,
                        "size" => size = txt.trim().parse().unwrap_or(0),
                        _ => {}
                    }
                }
            }
            Event::End(e) => match e.local_name().as_ref() {
                b"href" | b"getcontentlength" => capture = None,
                b"response" => out.push(RawEntry {
                    href: href.clone(),
                    is_dir,
                    size,
                }),
                _ => {}
            },
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(out)
}

/// The path portion of an href (handles both absolute-URL and path-only hrefs),
/// with any trailing slash removed.
fn href_path(href: &str) -> String {
    let path = match Url::parse(href) {
        Ok(u) => u.path().to_string(),
        Err(_) => href.to_string(),
    };
    path.trim_end_matches('/').to_string()
}

fn decode(seg: &str) -> String {
    percent_decode_str(seg).decode_utf8_lossy().into_owned()
}

fn basename(path: &str) -> String {
    decode(path.trim_end_matches('/').rsplit('/').next().unwrap_or(""))
}

impl WebdavProvider {
    pub async fn connect(
        host: &str,
        port: u16,
        user: &str,
        pass: &str,
        tls: bool,
        root: &str,
    ) -> Result<Self> {
        let scheme = if tls { "https" } else { "http" };
        let root = if root.is_empty() { "/" } else { root };
        let root = if root.ends_with('/') {
            root.to_string()
        } else {
            format!("{root}/")
        };
        let base = Url::parse(&format!("{scheme}://{host}:{port}{root}")).map_err(werr)?;
        let client = http::client().map_err(werr)?;
        let p = WebdavProvider {
            client,
            base,
            user: user.to_string(),
            pass: pass.to_string(),
        };
        // Validate connection + credentials up front.
        p.propfind("/", "0").await?;
        Ok(p)
    }

    fn url_for(&self, path: &str) -> Result<Url> {
        let mut url = self.base.clone();
        {
            let mut segs = url
                .path_segments_mut()
                .map_err(|_| AppError::other("WebDAV: ungültige Basis-URL"))?;
            segs.pop_if_empty();
            for seg in path.split('/').filter(|s| !s.is_empty()) {
                segs.push(seg);
            }
        }
        Ok(url)
    }

    fn req(&self, method: Method, url: Url) -> reqwest::RequestBuilder {
        self.client
            .request(method, url)
            .basic_auth(&self.user, Some(&self.pass))
    }

    async fn propfind(&self, path: &str, depth: &str) -> Result<Vec<RawEntry>> {
        let url = self.url_for(path)?;
        let body = r#"<?xml version="1.0"?><propfind xmlns="DAV:"><prop><resourcetype/><getcontentlength/><getlastmodified/></prop></propfind>"#;
        let resp = self
            .req(propfind_method(), url)
            .header("Depth", depth)
            .header("Content-Type", "application/xml")
            .body(body)
            .send()
            .await
            .map_err(werr)?;
        let resp = resp.error_for_status().map_err(werr)?;
        let text = resp.text().await.map_err(werr)?;
        parse_propfind(&text)
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
impl FileProvider for WebdavProvider {
    fn kind(&self) -> &'static str {
        "webdav"
    }

    fn supports_upload_resume(&self) -> bool {
        false
    }

    async fn home_dir(&self) -> Result<String> {
        Ok("/".to_string())
    }

    async fn list_dir(&self, path: &str) -> Result<Vec<FileEntry>> {
        let self_path = href_path(self.url_for(path)?.path());
        let raw = self.propfind(path, "1").await?;
        let mut out = Vec::new();
        for r in raw {
            if href_path(&r.href) == self_path {
                continue; // the collection itself
            }
            let name = basename(&r.href);
            if !name.is_empty() {
                out.push(entry(name, r.is_dir, r.size));
            }
        }
        Ok(out)
    }

    async fn stat(&self, path: &str) -> Result<FileEntry> {
        let raw = self.propfind(path, "0").await?;
        let r = raw
            .into_iter()
            .next()
            .ok_or_else(|| AppError::other(format!("WebDAV: nicht gefunden: {path}")))?;
        Ok(entry(basename(path), r.is_dir, r.size))
    }

    async fn make_dir(&self, path: &str) -> Result<()> {
        let url = self.url_for(path)?;
        self.req(Method::from_bytes(b"MKCOL").unwrap(), url)
            .send()
            .await
            .map_err(werr)?
            .error_for_status()
            .map_err(werr)?;
        Ok(())
    }

    async fn remove_file(&self, path: &str) -> Result<()> {
        let url = self.url_for(path)?;
        self.req(Method::DELETE, url)
            .send()
            .await
            .map_err(werr)?
            .error_for_status()
            .map_err(werr)?;
        Ok(())
    }

    async fn remove_dir(&self, path: &str) -> Result<()> {
        // WebDAV DELETE on a collection removes it recursively.
        self.remove_file(path).await
    }

    async fn rename(&self, from: &str, to: &str) -> Result<()> {
        let url = self.url_for(from)?;
        let dest = self.url_for(to)?;
        self.req(Method::from_bytes(b"MOVE").unwrap(), url)
            .header("Destination", dest.as_str())
            // Never replace an existing target silently (same as SFTP rename).
            .header("Overwrite", "F")
            .send()
            .await
            .map_err(werr)?
            .error_for_status()
            .map_err(werr)?;
        Ok(())
    }

    async fn set_permissions(&self, _path: &str, _mode: u32) -> Result<()> {
        Err(AppError::other(
            "WebDAV kennt keine Unix-Rechte (chmod nicht unterstützt)",
        ))
    }

    async fn download_to(
        &self,
        remote: &str,
        dst: &mut (dyn AsyncWrite + Unpin + Send),
        offset: u64,
        progress: &mut ProgressFn,
        cancel: &CancellationToken,
    ) -> Result<()> {
        let url = self.url_for(remote)?;
        let mut req = self.req(Method::GET, url);
        if offset > 0 {
            req = req.header("Range", format!("bytes={offset}-"));
        }
        let resp = req
            .send()
            .await
            .map_err(werr)?
            .error_for_status()
            .map_err(werr)?;
        http::receive(resp, dst, offset, progress, cancel).await
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
            return Err(AppError::other("WebDAV unterstützt kein Resume"));
        }
        let url = self.url_for(remote)?;
        http::send_streaming(self.req(Method::PUT, url), src, size, progress, cancel)
            .await?
            .error_for_status()
            .map_err(werr)?;
        Ok(())
    }
}
