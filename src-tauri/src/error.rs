//! Central error type. It is serialized to the frontend as a tagged object so
//! the UI can react specially to host-key verification requests.

use serde::ser::{Serialize, SerializeMap, Serializer};

pub type Result<T> = std::result::Result<T, AppError>;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("SSH error: {0}")]
    Ssh(#[from] russh::Error),

    #[error("SSH key error: {0}")]
    SshKey(#[from] russh::keys::Error),

    #[error("SFTP error: {0}")]
    Sftp(#[from] russh_sftp::client::error::Error),

    #[error("keyring error: {0}")]
    Keyring(#[from] keyring::Error),

    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),

    /// The remote host key is unknown or has changed. The UI must show a
    /// confirmation dialog and retry the connection with `trust_host_key = true`.
    #[error("host key verification required")]
    HostKeyVerification {
        fingerprint: String,
        key_type: String,
        changed: bool,
    },

    #[error("not connected: {0}")]
    NotConnected(String),

    #[error("authentication failed")]
    AuthFailed,

    #[error("transfer cancelled")]
    Cancelled,

    #[error("{0}")]
    Other(String),
}

impl AppError {
    pub fn other(msg: impl Into<String>) -> Self {
        AppError::Other(msg.into())
    }

    /// Machine-readable discriminator used by the frontend.
    pub fn kind(&self) -> &'static str {
        match self {
            AppError::HostKeyVerification { .. } => "host_key",
            AppError::NotConnected(_) => "not_connected",
            AppError::AuthFailed => "auth_failed",
            AppError::Cancelled => "cancelled",
            _ => "error",
        }
    }
}

impl Serialize for AppError {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        let mut m = s.serialize_map(None)?;
        m.serialize_entry("kind", self.kind())?;
        m.serialize_entry("message", &self.to_string())?;
        if let AppError::HostKeyVerification {
            fingerprint,
            key_type,
            changed,
        } = self
        {
            m.serialize_entry("fingerprint", fingerprint)?;
            m.serialize_entry("keyType", key_type)?;
            m.serialize_entry("changed", changed)?;
        }
        m.end()
    }
}
