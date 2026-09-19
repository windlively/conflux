mod commands;
pub mod download;
pub mod store;

use std::sync::Arc;

use tauri::{Emitter, Manager};

use commands::AppState;
use download::{manager::EventSink, DownloadManager, ManagerEvent};
use store::Store;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
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
                .unwrap_or_else(|_| std::env::temp_dir());

            // 窗口材质拿不到不影响功能，前端拿到 "none" 就用纯色侧边栏
            let material = apply_window_material(app).to_string();

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

            let manager = DownloadManager::new(sink, default_dir, store.clone());
            app.manage(AppState {
                manager,
                store,
                material,
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
            commands::storage_info,
            commands::window_material,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

/// 窗口材质：macOS 用系统侧边栏磨砂，Windows 依次尝试 Mica → Acrylic → Blur，
/// 都不支持时返回 "none"，由前端退回纯色侧边栏。
fn apply_window_material(app: &tauri::App) -> &'static str {
    let Some(window) = app.get_webview_window("main") else {
        return "none";
    };

    #[cfg(target_os = "macos")]
    {
        use window_vibrancy::{apply_vibrancy, NSVisualEffectMaterial, NSVisualEffectState};
        match apply_vibrancy(
            &window,
            NSVisualEffectMaterial::Sidebar,
            Some(NSVisualEffectState::FollowsWindowActiveState),
            None,
        ) {
            Ok(()) => "vibrancy",
            Err(err) => {
                eprintln!("窗口磨砂不可用，退回纯色侧边栏：{err}");
                "none"
            }
        }
    }

    #[cfg(target_os = "windows")]
    {
        // Windows 11 22H2 起有 Mica；更老的版本退回 Acrylic / Blur
        if window_vibrancy::apply_mica(&window, None).is_ok() {
            "mica"
        } else if window_vibrancy::apply_acrylic(&window, None).is_ok() {
            "acrylic"
        } else if window_vibrancy::apply_blur(&window, None).is_ok() {
            "blur"
        } else {
            eprintln!("系统不支持窗口材质，退回纯色侧边栏");
            "none"
        }
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let _ = window;
        "none"
    }
}
