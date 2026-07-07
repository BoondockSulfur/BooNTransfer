pub mod error;
pub mod logging;
pub mod protocol;
pub mod secrets;
pub mod session;
pub mod transfer;

mod commands;

use protocol::Terminal;
use std::collections::HashMap;
use std::sync::{Arc, Mutex as StdMutex};
use tauri::Manager;
use transfer::{ConnMap, Queue};

/// Active interactive shells, keyed by a frontend-generated terminal id.
pub type TermMap = Arc<StdMutex<HashMap<String, Arc<dyn Terminal>>>>;

/// Application-wide state managed by Tauri and shared with every command.
pub struct AppState {
    pub connections: ConnMap,
    pub queue: Queue,
    pub sessions: session::Store,
    pub terminals: TermMap,
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    logging::init();

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .setup(|app| {
            let connections: ConnMap = Arc::new(StdMutex::new(HashMap::new()));
            let queue = Queue::new(connections.clone(), app.handle().clone());
            let sessions = session::Store::load();
            app.manage(AppState {
                connections,
                queue,
                sessions,
                terminals: Arc::new(StdMutex::new(HashMap::new())),
            });
            tracing::info!("BooN Transfer started");
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::list_sessions,
            commands::save_session,
            commands::delete_session,
            commands::connect,
            commands::disconnect,
            commands::local_home,
            commands::list_local,
            commands::make_dir_local,
            commands::delete_local,
            commands::rename_local,
            commands::set_permissions_local,
            commands::list_remote,
            commands::make_dir_remote,
            commands::delete_remote,
            commands::rename_remote,
            commands::set_permissions_remote,
            commands::plan_transfers,
            commands::enqueue_transfers,
            commands::list_transfers,
            commands::verify_transfer,
            commands::plan_sync,
            commands::read_text,
            commands::write_text,
            commands::transfer_pause,
            commands::transfer_resume,
            commands::transfer_cancel,
            commands::transfer_retry,
            commands::clear_finished,
            commands::terminal_open,
            commands::terminal_write,
            commands::terminal_resize,
            commands::terminal_close,
        ])
        .run(tauri::generate_context!())
        .expect("error while running BooN Transfer");
}
