mod commands;
pub mod download;

use std::path::PathBuf;
use std::sync::Arc;

use tauri::{Emitter, Manager};

use commands::AppState;
use download::{manager::EventSink, DownloadManager, ManagerEvent};

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let handle = app.handle().clone();
            let sink: EventSink = Arc::new(move |event| match event {
                ManagerEvent::Update(view) => {
                    let _ = handle.emit("task:update", view);
                }
                ManagerEvent::Removed(id) => {
                    let _ = handle.emit("task:removed", id);
                }
            });
            let default_dir = app
                .path()
                .download_dir()
                .unwrap_or_else(|_| PathBuf::from(std::env::temp_dir()));
            app.manage(AppState {
                manager: DownloadManager::new(sink, default_dir),
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::add_task,
            commands::pause_task,
            commands::resume_task,
            commands::cancel_task,
            commands::remove_task,
            commands::list_tasks,
            commands::default_download_dir,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
