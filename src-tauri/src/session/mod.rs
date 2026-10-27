//! Persistence for saved connections. Stored as JSON in the platform config
//! directory. **No secrets are ever written here** — passwords and passphrases
//! live in the OS keyring (see [`crate::secrets`]).

use crate::error::Result;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    #[default]
    Sftp,
    Scp,
    Ftp,
    Ftps,
    Webdav,
    Webdavs,
    S3,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AuthType {
    Password,
    Key,
    Agent,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionInfo {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub protocol: Protocol,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub auth_type: AuthType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_path: Option<String>,
    /// Optional grouping folder for the connection tree / favourites.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_dir: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_dir: Option<String>,
    /// S3 only: bucket name and region.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bucket: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
    /// S3 only: use https. `None` (older sessions) means https on port 443.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub use_tls: Option<bool>,
    /// Unix timestamp (seconds) of last successful use; drives "recent".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_used: Option<i64>,
}

/// JSON-file backed session store, guarded by a mutex for interior mutability.
pub struct Store {
    path: PathBuf,
    sessions: Mutex<Vec<SessionInfo>>,
}

fn config_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("boon-transfer")
        .join("sessions.json")
}

impl Store {
    pub fn load() -> Self {
        let path = config_path();
        let sessions = match fs::read_to_string(&path) {
            Ok(text) => match serde_json::from_str(&text) {
                Ok(sessions) => sessions,
                Err(e) => {
                    // Keep the unreadable file instead of overwriting it with
                    // an empty list on the next save.
                    let backup = path.with_extension(format!(
                        "json.corrupt-{}",
                        std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_secs())
                            .unwrap_or(0)
                    ));
                    tracing::error!(
                        "sessions.json is unreadable ({e}); moved to {}",
                        backup.display()
                    );
                    let _ = fs::rename(&path, &backup);
                    Vec::new()
                }
            },
            Err(_) => Vec::new(),
        };
        Store {
            path,
            sessions: Mutex::new(sessions),
        }
    }

    /// Writes via a temp file + rename, so a crash never leaves a truncated
    /// sessions.json behind.
    fn persist(&self, sessions: &[SessionInfo]) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(sessions)?;
        let tmp = self.path.with_extension("json.tmp");
        fs::write(&tmp, json)?;
        fs::rename(&tmp, &self.path)?;
        Ok(())
    }

    pub fn list(&self) -> Vec<SessionInfo> {
        self.sessions.lock().unwrap().clone()
    }

    pub fn get(&self, id: &str) -> Option<SessionInfo> {
        self.sessions
            .lock()
            .unwrap()
            .iter()
            .find(|s| s.id == id)
            .cloned()
    }

    /// Inserts a new session or replaces an existing one with the same id.
    pub fn upsert(&self, session: SessionInfo) -> Result<()> {
        let mut guard = self.sessions.lock().unwrap();
        if let Some(existing) = guard.iter_mut().find(|s| s.id == session.id) {
            *existing = session;
        } else {
            guard.push(session);
        }
        self.persist(&guard)
    }

    pub fn delete(&self, id: &str) -> Result<()> {
        let mut guard = self.sessions.lock().unwrap();
        guard.retain(|s| s.id != id);
        self.persist(&guard)
    }

    pub fn touch(&self, id: &str, ts: i64) -> Result<()> {
        let mut guard = self.sessions.lock().unwrap();
        if let Some(s) = guard.iter_mut().find(|s| s.id == id) {
            s.last_used = Some(ts);
        }
        self.persist(&guard)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(id: &str) -> SessionInfo {
        SessionInfo {
            id: id.to_string(),
            name: "test".into(),
            protocol: Protocol::Sftp,
            host: "example.com".into(),
            port: 22,
            username: "user".into(),
            auth_type: AuthType::Password,
            key_path: None,
            folder: None,
            remote_dir: None,
            local_dir: None,
            bucket: None,
            region: None,
            use_tls: None,
            last_used: None,
        }
    }

    #[test]
    fn serialize_has_no_secret_fields() {
        let json = serde_json::to_string(&sample("a")).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        let obj = value.as_object().unwrap();
        // No secret is ever a field of the persisted session (they live in the
        // keyring). "password" only appears as the authType *value*.
        assert!(!obj.contains_key("password"));
        assert!(!obj.contains_key("passphrase"));
        assert_eq!(obj.get("authType").unwrap(), "password");
    }

    #[test]
    fn upsert_replaces_in_place() {
        let store = Store {
            path: std::env::temp_dir().join("boon-transfer-test-sessions.json"),
            sessions: Mutex::new(vec![]),
        };
        store.upsert(sample("a")).unwrap();
        let mut updated = sample("a");
        updated.name = "renamed".into();
        store.upsert(updated).unwrap();
        let all = store.list();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].name, "renamed");
    }
}
