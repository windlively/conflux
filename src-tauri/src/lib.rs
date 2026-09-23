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

            // 右键菜单里的「检查元素」要挡掉（见 disable_webview_inspector）；
            // 配置里显式写 "devtools": true 时留着，方便自己调试
            let devtools_wanted = app
                .config()
                .app
                .windows
                .iter()
                .find(|window| window.label == "main")
                .and_then(|window| window.devtools)
                .unwrap_or(false);
            if !devtools_wanted {
                if let Some(window) = app.get_webview_window("main") {
                    if let Err(err) = disable_webview_inspector(&window) {
                        eprintln!("关闭 webview 检查器失败：{err}");
                    }
                }
            }

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
            // ed2k 侧车的断点数据与 Kad 路由表跟数据库放一起
            if let Ok(data_dir) = app.path().app_data_dir() {
                manager.set_data_dir(data_dir);
            }
            app.manage(AppState {
                manager,
                store,
                material,
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::add_task,
            commands::add_torrent_file,
            commands::preview_magnet,
            commands::preview_torrent_file,
            commands::search_ed2k,
            commands::set_task_files,
            commands::pause_task,
            commands::resume_task,
            commands::cancel_task,
            commands::remove_task,
            commands::list_tasks,
            commands::default_download_dir,
            commands::storage_info,
            commands::window_material,
            commands::set_max_connections,
            commands::set_bt_proxy,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

/// 窗口材质：macOS 优先液态玻璃（macOS 26 起），拿不到再退回系统侧边栏磨砂；
/// Windows 依次尝试 Mica → Acrylic → Blur，都不支持时返回 "none"，
/// 由前端退回纯色侧边栏。
fn apply_window_material(app: &tauri::App) -> &'static str {
    let Some(window) = app.get_webview_window("main") else {
        return "none";
    };

    #[cfg(target_os = "macos")]
    {
        if let Err(err) = apply_liquid_glass(&window) {
            eprintln!("液态玻璃不可用（{err}），改用系统侧边栏磨砂");

            use window_vibrancy::{apply_vibrancy, NSVisualEffectMaterial, NSVisualEffectState};
            return match apply_vibrancy(
                &window,
                NSVisualEffectMaterial::Sidebar,
                Some(NSVisualEffectState::FollowsWindowActiveState),
                None,
            ) {
                Ok(()) => "vibrancy",
                Err(err) => {
                    eprintln!("窗口磨砂也不可用，退回纯色侧边栏：{err}");
                    "none"
                }
            };
        }
        "glass"
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

/// 右键菜单里的「检查元素 / Reload」是 WebKit 自带的。配置里的 `devtools: false` 只管到 Windows
/// （wry 在 WebView2 上会调 `SetAreDevToolsEnabled(false)`）；macOS 上 wry 只在**开启**时才
/// `setInspectable(true)`，关掉时不去设 false，于是系统默认的可检查状态还在。这里找到
/// WKWebView 显式关掉——它挡得住非编辑区的菜单与外部检查器挂载，但输入框上的编辑菜单
/// 仍会带出「检查元素」（那条路径认的是 WKWebView 构造时的私有偏好，运行时改不到），
/// 所以前端还压了一道 contextmenu 的 preventDefault，见 web/src/app/app.ts。
/// 注意别在这里改 `configuration.preferences`：macOS 上 `WKWebView.configuration` 返回的是副本。
#[cfg(target_os = "macos")]
fn disable_webview_inspector(window: &tauri::WebviewWindow) -> Result<(), String> {
    use objc2::msg_send;
    use objc2::rc::Retained;
    use objc2::runtime::{AnyObject, Bool};
    use objc2_app_kit::NSView;

    // webview 是宿主视图的直接子视图，但材质那层也可能插在中间，所以逐层找
    fn find_webview(view: &NSView, depth: u8) -> Option<Retained<NSView>> {
        if depth > 8 {
            return None;
        }
        for sub in view.subviews().iter() {
            let object: &AnyObject = unsafe { &*(Retained::as_ptr(&sub).cast()) };
            if object.class().name().to_string_lossy().contains("WKWebView") {
                return Some(sub);
            }
            if let Some(found) = find_webview(&sub, depth + 1) {
                return Some(found);
            }
        }
        None
    }

    let ns_view = window.ns_view().map_err(|err| err.to_string())?;
    let host: &NSView = unsafe { &*ns_view.cast() };
    let webview = find_webview(host, 0).ok_or("窗口里没找到 WKWebView")?;
    let object: &AnyObject = unsafe { &*(Retained::as_ptr(&webview).cast()) };

    // setInspectable: 是 macOS 13.3 起才有的，老系统没这个方法（那儿本来也关不掉）
    let inspectable_available: bool =
        unsafe { msg_send![object, respondsToSelector: objc2::sel!(setInspectable:)] };
    if inspectable_available {
        let _: () = unsafe { msg_send![object, setInspectable: Bool::NO] };
    }

    Ok(())
}

/// macOS 26 起系统提供 NSGlassEffectView（液态玻璃）：垫在 webview 底下，
/// 前端把侧边栏区域留透明就能透出玻璃。系统没有这个类说明版本更低，返回错误交给调用方降级。
#[cfg(target_os = "macos")]
fn apply_liquid_glass(window: &tauri::WebviewWindow) -> Result<(), String> {
    use objc2::runtime::AnyClass;
    use objc2::MainThreadMarker;
    use objc2_app_kit::{
        NSAutoresizingMaskOptions, NSGlassEffectView, NSGlassEffectViewStyle, NSView,
        NSWindowOrderingMode,
    };

    if AnyClass::get(c"NSGlassEffectView").is_none() {
        return Err("当前系统没有 NSGlassEffectView".into());
    }

    let mtm = MainThreadMarker::new().ok_or("窗口材质必须在主线程设置")?;
    let ns_view = window.ns_view().map_err(|err| err.to_string())?;
    // 与 window-vibrancy 同一套拿法：直接取 tao 窗口的 NSView 当宿主
    let host: &NSView = unsafe { &*ns_view.cast() };

    let glass = NSGlassEffectView::initWithFrame(mtm.alloc(), host.bounds());
    glass.setStyle(NSGlassEffectViewStyle::Regular);
    glass.setAutoresizingMask(
        NSAutoresizingMaskOptions::ViewWidthSizable
            | NSAutoresizingMaskOptions::ViewHeightSizable,
    );
    host.addSubview_positioned_relativeTo(&glass, NSWindowOrderingMode::Below, None);

    Ok(())
}
