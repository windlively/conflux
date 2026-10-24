//! 状态栏图标：macOS 菜单栏 / Windows 通知区域。
//!
//! 菜单列出正在进行的任务（排队、解析、下载中），点条目回主窗口定位到它；
//! 另带「新建任务…」「打开川流」「退出川流」。是否显示、关窗是否驻留
//! 都由前端设置决定，默认隐藏，等前端把本机保存的开关推过来。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use serde::Serialize;
use tauri::image::Image;
use tauri::menu::{Menu, MenuBuilder, MenuEvent, MenuItem};
use tauri::tray::TrayIcon;
use tauri::{AppHandle, Emitter, Manager};

use crate::download::{ManagerEvent, TaskStatus, TaskView};

const TRAY_ID: &str = "conflux-tray";
const MENU_EMPTY: &str = "tray-empty";
const MENU_OPEN: &str = "tray-open";
const MENU_NEW: &str = "tray-new";
const MENU_QUIT: &str = "tray-quit";
const TASK_PREFIX: &str = "tray-task:";

/// 菜单最多列这么多任务，再多就折叠成一行计数，免得菜单比屏幕还长
const MAX_MENU_TASKS: usize = 6;
/// 文件名太长会把菜单撑得难看，按字符截断
const MAX_LABEL_CHARS: usize = 28;

#[cfg(target_os = "macos")]
const TRAY_ICON: &[u8] = include_bytes!("../icons/tray/tray-template.png");
#[cfg(not(target_os = "macos"))]
const TRAY_ICON: &[u8] = include_bytes!("../icons/tray/tray-windows.png");

/// 点托盘菜单后发给前端的事件；`id` 只在 action 为 task 时存在
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrayAction {
    pub action: &'static str,
    pub id: Option<String>,
}

/// 菜单里的一项：任务 id + 显示文本。保持进入顺序，状态变化时原地替换。
type ActiveEntry = (String, String);

pub struct TrayState {
    icon: TrayIcon,
    enabled: AtomicBool,
    /// 关闭主窗口时是缩到状态栏，还是直接退出
    close_to_tray: AtomicBool,
    active: Mutex<Vec<ActiveEntry>>,
}

impl TrayState {
    pub fn new(app: &AppHandle, tasks: &[TaskView]) -> tauri::Result<Self> {
        let active: Vec<ActiveEntry> = tasks
            .iter()
            .filter(|task| is_active(task.status))
            .map(|task| (task.id.clone(), menu_label(task)))
            .collect();

        let icon = Image::from_bytes(TRAY_ICON)?;
        let mut builder = tauri::tray::TrayIconBuilder::with_id(TRAY_ID).icon(icon);
        // macOS 菜单栏用模板图，跟着系统深浅色自动反色
        #[cfg(target_os = "macos")]
        {
            builder = builder.icon_as_template(true);
        }
        let icon = builder.build(app)?;
        icon.set_tooltip(Some(tooltip(active.len())))?;
        icon.set_menu(Some(build_menu(app, &active)?))?;
        icon.set_visible(false)?;

        let tray = Self {
            icon,
            enabled: AtomicBool::new(false),
            close_to_tray: AtomicBool::new(true),
            active: Mutex::new(active),
        };
        // on_menu_event 要注册在创建后：构建时拿不到 TrayState 自身
        tray.icon
            .on_menu_event(|app, event| handle_menu(app, event));
        Ok(tray)
    }

    /// 前端设置的开关；开启时按当前任务列表重建菜单，关掉就整颗隐藏
    pub fn set_enabled(&self, enabled: bool) -> Result<(), String> {
        self.enabled.store(enabled, Ordering::Relaxed);
        self.icon
            .set_visible(enabled)
            .map_err(|err| err.to_string())?;
        if enabled {
            self.rebuild();
        }
        Ok(())
    }

    pub fn set_close_to_tray(&self, enabled: bool) {
        self.close_to_tray.store(enabled, Ordering::Relaxed);
    }

    /// 关窗时是否缩到状态栏。图标没开时没有可恢复的入口，一律按退出处理。
    pub fn hide_on_close(&self) -> bool {
        self.enabled.load(Ordering::Relaxed) && self.close_to_tray.load(Ordering::Relaxed)
    }

    /// 跟着任务事件维护进行中的列表。只在「哪些任务/状态/名字变了」时重建菜单，
    /// 进度每 250ms 一跳，不该让原生菜单跟着抖。
    pub fn observe(&self, event: &ManagerEvent) {
        let changed = {
            let mut active = self.active.lock().unwrap();
            match event {
                ManagerEvent::Update(view) => {
                    let position = active.iter().position(|(id, _)| id == &view.id);
                    match (position, is_active(view.status)) {
                        (Some(index), true) => {
                            let label = menu_label(view);
                            if active[index].1 == label {
                                false
                            } else {
                                active[index].1 = label;
                                true
                            }
                        }
                        (Some(index), false) => {
                            active.remove(index);
                            true
                        }
                        (None, true) => {
                            active.push((view.id.clone(), menu_label(view)));
                            true
                        }
                        (None, false) => false,
                    }
                }
                ManagerEvent::Removed(id) => {
                    let before = active.len();
                    active.retain(|(task_id, _)| task_id != id);
                    active.len() != before
                }
            }
        };

        if changed && self.enabled.load(Ordering::Relaxed) {
            self.rebuild();
        }
    }

    fn rebuild(&self) {
        let active = self.active.lock().unwrap().clone();
        match build_menu(self.icon.app_handle(), &active) {
            Ok(menu) => {
                if let Err(err) = self.icon.set_menu(Some(menu)) {
                    eprintln!("更新状态栏菜单失败：{err}");
                }
            }
            Err(err) => eprintln!("构建状态栏菜单失败：{err}"),
        }
        let _ = self.icon.set_tooltip(Some(tooltip(active.len())));
    }
}

fn build_menu(app: &AppHandle, active: &[ActiveEntry]) -> tauri::Result<Menu<tauri::Wry>> {
    let mut builder = MenuBuilder::new(app);

    if active.is_empty() {
        builder = builder.item(&MenuItem::with_id(
            app,
            MENU_EMPTY,
            "没有进行中的任务",
            false,
            None::<&str>,
        )?);
    } else {
        for (id, label) in active.iter().take(MAX_MENU_TASKS) {
            builder = builder.item(&MenuItem::with_id(
                app,
                format!("{TASK_PREFIX}{id}"),
                label,
                true,
                None::<&str>,
            )?);
        }
        if active.len() > MAX_MENU_TASKS {
            builder = builder.item(&MenuItem::with_id(
                app,
                MENU_EMPTY,
                format!("还有 {} 个进行中…", active.len() - MAX_MENU_TASKS),
                false,
                None::<&str>,
            )?);
        }
    }

    builder
        .separator()
        .item(&MenuItem::with_id(
            app,
            MENU_NEW,
            "新建任务…",
            true,
            None::<&str>,
        )?)
        .separator()
        .item(&MenuItem::with_id(
            app,
            MENU_OPEN,
            "打开川流",
            true,
            None::<&str>,
        )?)
        .separator()
        .item(&MenuItem::with_id(
            app,
            MENU_QUIT,
            "退出川流",
            true,
            None::<&str>,
        )?)
        .build()
}

fn handle_menu(app: &AppHandle, event: MenuEvent) {
    let id = event.id().as_ref();
    match id {
        MENU_NEW => {
            show_main_window(app);
            emit(
                app,
                TrayAction {
                    action: "new-task",
                    id: None,
                },
            );
        }
        MENU_OPEN => show_main_window(app),
        MENU_QUIT => app.exit(0),
        _ => {
            if let Some(task_id) = id.strip_prefix(TASK_PREFIX) {
                show_main_window(app);
                emit(
                    app,
                    TrayAction {
                        action: "task",
                        id: Some(task_id.to_string()),
                    },
                );
            }
        }
    }
}

fn emit(app: &AppHandle, action: TrayAction) {
    if let Err(err) = app.emit("tray:action", action) {
        eprintln!("发送状态栏事件失败：{err}");
    }
}

fn show_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

fn is_active(status: TaskStatus) -> bool {
    matches!(
        status,
        TaskStatus::Queued | TaskStatus::Probing | TaskStatus::Downloading
    )
}

fn status_label(status: TaskStatus) -> Option<&'static str> {
    match status {
        TaskStatus::Queued => Some("排队中"),
        TaskStatus::Probing => Some("解析中"),
        TaskStatus::Downloading => Some("下载中"),
        _ => None,
    }
}

fn menu_label(task: &TaskView) -> String {
    let name = truncate(&task.file_name, MAX_LABEL_CHARS);
    match status_label(task.status) {
        Some(status) => format!("{name} · {status}"),
        None => name,
    }
}

fn truncate(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_string();
    }
    let mut shortened: String = text.chars().take(limit.saturating_sub(1)).collect();
    shortened.push('…');
    shortened
}

fn tooltip(active: usize) -> String {
    match active {
        0 => "川流".to_string(),
        count => format!("川流 · {count} 个任务进行中"),
    }
}
