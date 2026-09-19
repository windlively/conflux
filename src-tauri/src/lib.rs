mod commands;
pub mod download;
pub mod store;

use std::path::PathBuf;
use std::sync::Arc;

use tauri::{Emitter, Manager};

use commands::AppState;
use download::{manager::EventSink, DownloadManager, ManagerEvent};
use store::Store;

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
            let manager = DownloadManager::new(sink, default_dir);

            // 持久化不可用不应连带下载功能一起不可用，因此这里降级提示而不是中断启动
            let store = match app.path().app_data_dir() {
                Ok(dir) => {
                    let db_path = dir.join("conflux.db");
                    match Store::open(&db_path) {
                        Ok(store) => {
                            println!("数据库已就绪：{}", db_path.display());
                            Some(Arc::new(store))
                        }
                        Err(err) => {
                            eprintln!("数据库初始化失败，本次运行不会保存任务：{err}");
                            None
                        }
                    }
                }
                Err(err) => {
                    eprintln!("无法确定应用数据目录，本次运行不会保存任务：{err}");
                    None
                }
            };

            app.manage(AppState { manager, store });
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
            commands::storage_info,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
