//! 状态栏图标：macOS 菜单栏 / Windows 通知区域。
//!
//! 菜单列出正在进行的任务（排队、解析、下载中），点条目回主窗口定位到它；
//! 另带「新建任务…」「打开川流」「退出川流」。是否显示、关窗是否驻留
//! 都由前端设置决定，默认隐藏，等前端把本机保存的开关推过来。
//!
//! 速度也在这里：菜单栏图标右侧显示总速度（macOS 的 tray title），
//! 菜单里每条任务带上自己的速度。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

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

/// 速度是一直流动的数字，而进度事件 250ms 就来一次。原生菜单重建、菜单栏
/// 文字跳动都吃不消这个频率，所以速度最多 1 秒刷一次；增删任务、状态切换
/// 这类结构变化不受限，立刻重画。
const SPEED_REFRESH: Duration = Duration::from_millis(1_000);

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

/// 菜单里的一项：任务 id + 文件名 + 状态 + 当前速度。
/// 名字和状态决定菜单的「结构」，速度只影响文字，两者分开判断才好节流。
#[derive(Clone)]
struct Entry {
    id: String,
    name: String,
    status: TaskStatus,
    speed: u64,
}

impl Entry {
    fn from_task(task: &TaskView) -> Self {
        Self {
            id: task.id.clone(),
            name: truncate(&task.file_name, MAX_LABEL_CHARS),
            status: task.status,
            speed: task.speed_bps,
        }
    }

    /// 名字、状态都没变，就算同一条任务——只是速度在走，可以晚一拍再画
    fn same_shape(&self, other: &Self) -> bool {
        self.id == other.id && self.name == other.name && self.status == other.status
    }
}

/// 上一次真正画出去的内容：菜单各项的文案 + 菜单栏标题。
/// 两者都没变就不碰原生控件，避免无意义的重建。
struct Painted {
    labels: Vec<String>,
    title: String,
    at: Instant,
}

pub struct TrayState {
    icon: TrayIcon,
    enabled: AtomicBool,
    /// 关闭主窗口时是缩到状态栏，还是直接退出
    close_to_tray: AtomicBool,
    /// 菜单栏图标旁要不要跟着显示总速度
    show_speed: AtomicBool,
    entries: Mutex<Vec<Entry>>,
    painted: Mutex<Painted>,
}

impl TrayState {
    pub fn new(app: &AppHandle, tasks: &[TaskView]) -> tauri::Result<Self> {
        let entries: Vec<Entry> = tasks
            .iter()
            .filter(|task| is_active(task.status))
            .map(Entry::from_task)
            .collect();

        let icon = Image::from_bytes(TRAY_ICON)?;
        let mut builder = tauri::tray::TrayIconBuilder::with_id(TRAY_ID).icon(icon);
        // macOS 菜单栏用模板图，跟着系统深浅色自动反色
        #[cfg(target_os = "macos")]
        {
            builder = builder.icon_as_template(true);
        }
        let icon = builder.build(app)?;
        icon.set_tooltip(Some(tooltip(&entries, true)))?;
        icon.set_menu(Some(build_menu(app, &entries)?))?;
        icon.set_visible(false)?;

        let tray = Self {
            icon,
            enabled: AtomicBool::new(false),
            close_to_tray: AtomicBool::new(true),
            show_speed: AtomicBool::new(true),
            entries: Mutex::new(entries),
            painted: Mutex::new(Painted {
                labels: Vec::new(),
                title: String::new(),
                at: Instant::now(),
            }),
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
            self.paint(true);
        } else {
            // 图标都收起来了，速度文字也别留在菜单栏上；同时清掉绘制快照，
            // 否则重新打开开关时会被判成「没变化」而不画
            let _ = self.icon.set_title(None::<&str>);
            let mut painted = self.painted.lock().unwrap();
            painted.labels.clear();
            painted.title.clear();
        }
        Ok(())
    }

    pub fn set_close_to_tray(&self, enabled: bool) {
        self.close_to_tray.store(enabled, Ordering::Relaxed);
    }

    /// 菜单栏图标旁的总速度开关。关掉只是不显示那串文字，菜单里每条任务的
    /// 速度照旧——那是点开才看得到的，不占常驻位置。
    pub fn set_show_speed(&self, enabled: bool) {
        self.show_speed.store(enabled, Ordering::Relaxed);
        self.paint(true);
    }

    /// 关窗时是否缩到状态栏。图标没开时没有可恢复的入口，一律按退出处理。
    pub fn hide_on_close(&self) -> bool {
        self.enabled.load(Ordering::Relaxed) && self.close_to_tray.load(Ordering::Relaxed)
    }

    /// 跟着任务事件维护进行中的列表，再把速度画到菜单栏与菜单上。
    pub fn observe(&self, event: &ManagerEvent) {
        let structural = {
            let mut entries = self.entries.lock().unwrap();
            match event {
                ManagerEvent::Update(view) => apply(&mut entries, view),
                ManagerEvent::Removed(id) => {
                    let before = entries.len();
                    entries.retain(|entry| &entry.id != id);
                    entries.len() != before
                }
            }
        };

        self.paint(structural);
    }

    /// 把当前列表画到原生控件上。`structural` 为真（增删、改名、换状态）时
    /// 立刻重画；只有速度在变的，等 `SPEED_REFRESH` 那一拍。
    fn paint(&self, structural: bool) {
        if !self.enabled.load(Ordering::Relaxed) {
            return;
        }

        let entries = self.entries.lock().unwrap().clone();
        let labels: Vec<String> = entries.iter().map(menu_label).collect();
        let show_speed = self.show_speed.load(Ordering::Relaxed);
        let title = tray_title(&entries, show_speed);

        {
            let mut painted = self.painted.lock().unwrap();
            if painted.labels == labels && painted.title == title {
                return;
            }
            if !structural && painted.at.elapsed() < SPEED_REFRESH {
                return;
            }
            painted.labels = labels;
            painted.title = title.clone();
            painted.at = Instant::now();
        }

        match build_menu(self.icon.app_handle(), &entries) {
            Ok(menu) => {
                if let Err(err) = self.icon.set_menu(Some(menu)) {
                    eprintln!("更新状态栏菜单失败：{err}");
                }
            }
            Err(err) => eprintln!("构建状态栏菜单失败：{err}"),
        }

        // 菜单栏标题只有 macOS 有；Windows 的通知区域放不下文字，靠 tooltip
        #[cfg(target_os = "macos")]
        {
            let title = if title.is_empty() { None } else { Some(title) };
            if let Err(err) = self.icon.set_title(title) {
                eprintln!("更新菜单栏速度失败：{err}");
            }
        }

        let _ = self.icon.set_tooltip(Some(tooltip(&entries, show_speed)));
    }
}

/// 把一条任务更新揉进列表；返回菜单结构是否变了（增删、改名、换状态）。
fn apply(entries: &mut Vec<Entry>, view: &TaskView) -> bool {
    let next = Entry::from_task(view);
    match entries.iter().position(|entry| entry.id == next.id) {
        // 下完了、暂停了、失败了：从「进行中」里摘掉
        Some(index) if !is_active(next.status) => {
            entries.remove(index);
            true
        }
        Some(index) => {
            let changed = !entries[index].same_shape(&next);
            entries[index] = next;
            changed
        }
        None if is_active(next.status) => {
            entries.push(next);
            true
        }
        None => false,
    }
}

fn build_menu(app: &AppHandle, active: &[Entry]) -> tauri::Result<Menu<tauri::Wry>> {
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
        for entry in active.iter().take(MAX_MENU_TASKS) {
            builder = builder.item(&MenuItem::with_id(
                app,
                format!("{TASK_PREFIX}{}", entry.id),
                menu_label(entry),
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

/// 菜单项的文案：下载中带速度，其余带状态。速度刚好是 0 时写「下载中」
/// 比写「0 B/s」实在——连接刚起来、或者卡住了，都是这个数。
fn menu_label(entry: &Entry) -> String {
    match entry.status {
        TaskStatus::Downloading if entry.speed > 0 => {
            format!("{} · {}", entry.name, speed_text(entry.speed))
        }
        status => match status_label(status) {
            Some(label) => format!("{} · {label}", entry.name),
            None => entry.name.clone(),
        },
    }
}

/// 菜单栏上的总速度：开关关掉、或者没有流量，都留空只显示图标
fn tray_title(entries: &[Entry], show_speed: bool) -> String {
    if !show_speed {
        return String::new();
    }
    let total = total_speed(entries);
    if total == 0 {
        String::new()
    } else {
        speed_text(total)
    }
}

/// 只算真正在传的任务：排队和解析阶段速度是 0，加进来也没意义
fn total_speed(entries: &[Entry]) -> u64 {
    entries
        .iter()
        .filter(|entry| entry.status == TaskStatus::Downloading)
        .map(|entry| entry.speed)
        .sum()
}

/// 与前端 `formatSpeed` 同一套十进制单位；菜单栏位置窄，写成 `6.8M/s` 这种
/// 不带空格、不带 B 的紧凑形式：B 是每个单位都有的信息，省掉不损失可读性
fn speed_text(bps: u64) -> String {
    const UNITS: [&str; 4] = ["B", "K", "M", "G"];

    if bps < 1000 {
        return format!("{bps}B/s");
    }
    let mut value = bps as f64;
    let mut unit = 0;
    while value >= 1000.0 && unit < UNITS.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    // 一位小数只在小数值上有意义：6.8M/s 比 6.84M/s 好扫，12M/s 又比 12.3M/s 干净
    let number = if value < 10.0 {
        format!("{value:.1}")
    } else {
        format!("{value:.0}")
    };
    format!("{number}{}/s", UNITS[unit])
}

fn truncate(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_string();
    }
    let mut shortened: String = text.chars().take(limit.saturating_sub(1)).collect();
    shortened.push('…');
    shortened
}

fn tooltip(entries: &[Entry], show_speed: bool) -> String {
    let count = entries.len();
    let speed = if show_speed { total_speed(entries) } else { 0 };
    match (count, speed) {
        (0, _) => "川流".to_string(),
        (count, 0) => format!("川流 · {count} 个任务进行中"),
        (count, speed) => format!("川流 · {count} 个任务进行中 · {}", speed_text(speed)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, status: TaskStatus, speed: u64) -> Entry {
        Entry {
            id: name.to_string(),
            name: name.to_string(),
            status,
            speed,
        }
    }

    #[test]
    fn speed_text_is_compact() {
        assert_eq!(speed_text(0), "0B/s");
        assert_eq!(speed_text(999), "999B/s");
        assert_eq!(speed_text(1_000), "1.0K/s");
        assert_eq!(speed_text(128_400), "128K/s");
        assert_eq!(speed_text(6_840_000), "6.8M/s");
        assert_eq!(speed_text(1_200_000_000), "1.2G/s");
    }

    #[test]
    fn title_sums_only_downloading_tasks() {
        let running = vec![
            entry("a", TaskStatus::Downloading, 3_000_000),
            entry("b", TaskStatus::Downloading, 1_000_000),
            entry("c", TaskStatus::Queued, 0),
        ];
        assert_eq!(total_speed(&running), 4_000_000);
        assert_eq!(tray_title(&running, true), "4.0M/s");
    }

    #[test]
    fn title_is_empty_when_nothing_moves_or_switch_is_off() {
        assert_eq!(tray_title(&[], true), "");
        assert_eq!(tray_title(&[entry("a", TaskStatus::Probing, 0)], true), "");
        // 设置里关掉「状态栏显示速度」：有流量也不写
        let running = vec![entry("a", TaskStatus::Downloading, 6_840_000)];
        assert_eq!(tray_title(&running, false), "");
    }

    #[test]
    fn menu_label_carries_speed_only_when_downloading() {
        assert_eq!(
            menu_label(&entry("debian.iso", TaskStatus::Downloading, 6_840_000)),
            "debian.iso · 6.8M/s"
        );
        // 刚连上、或者卡住了：写状态比写 0 B/s 实在
        assert_eq!(
            menu_label(&entry("debian.iso", TaskStatus::Downloading, 0)),
            "debian.iso · 下载中"
        );
        assert_eq!(
            menu_label(&entry("debian.iso", TaskStatus::Probing, 0)),
            "debian.iso · 解析中"
        );
    }

    #[test]
    fn speed_only_changes_keep_the_same_shape() {
        let first = entry("a", TaskStatus::Downloading, 1_000);
        let faster = entry("a", TaskStatus::Downloading, 2_000);
        // 只有速度变了：可以按 SPEED_REFRESH 节流，不必立刻重建菜单
        assert!(first.same_shape(&faster));
        // 状态一变就要立刻重画
        assert!(!first.same_shape(&entry("a", TaskStatus::Paused, 1_000)));
    }
}
