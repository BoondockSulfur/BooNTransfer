//! Secure credential storage via the freedesktop Secret Service (GNOME Keyring /
//! KWallet through libsecret). Secrets are keyed by `<session-id>:<kind>` and
//! never touch the sessions JSON or any log line.

use crate::error::Result;
use keyring::Entry;

const SERVICE: &str = "linuxscp-client";

/// Secret categories stored per session.
pub const KIND_PASSWORD: &str = "password";
pub const KIND_PASSPHRASE: &str = "passphrase";

fn entry(session_id: &str, kind: &str) -> Result<Entry> {
    Ok(Entry::new(SERVICE, &format!("{session_id}:{kind}"))?)
}

pub fn set(session_id: &str, kind: &str, secret: &str) -> Result<()> {
    entry(session_id, kind)?.set_password(secret)?;
    Ok(())
}

pub fn get(session_id: &str, kind: &str) -> Result<Option<String>> {
    match entry(session_id, kind)?.get_password() {
        Ok(secret) => Ok(Some(secret)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

pub fn delete(session_id: &str, kind: &str) -> Result<()> {
    match entry(session_id, kind)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(e.into()),
    }
}

/// Removes every secret associated with a session.
pub fn delete_all(session_id: &str) -> Result<()> {
    delete(session_id, KIND_PASSWORD)?;
    delete(session_id, KIND_PASSPHRASE)?;
    Ok(())
}
