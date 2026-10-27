//! Logging via `tracing`. Logs go to stderr and to a rolling file in the
//! platform log directory. Secrets are never passed to logging call sites.

use std::path::PathBuf;
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

pub fn log_dir() -> PathBuf {
    dirs::state_dir()
        .or_else(dirs::data_local_dir)
        .unwrap_or_else(std::env::temp_dir)
        .join("boon-transfer")
        .join("logs")
}

pub fn init() {
    let dir = log_dir();
    let _ = std::fs::create_dir_all(&dir);

    let file_appender = tracing_appender::rolling::daily(&dir, "boon-transfer.log");

    let filter = EnvFilter::try_from_env("BOON_TRANSFER_LOG")
        .unwrap_or_else(|_| EnvFilter::new("info,boon_transfer_lib=debug"));

    let stderr_layer = fmt::layer().with_target(true).with_ansi(true);
    let file_layer = fmt::layer()
        .with_target(true)
        .with_ansi(false)
        .with_writer(file_appender);

    let _ = tracing_subscriber::registry()
        .with(filter)
        .with(stderr_layer)
        .with(file_layer)
        .try_init();
}
