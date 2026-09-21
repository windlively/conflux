use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::store::Store;

use super::bt::{self, BtRuntime};
use super::engine::{self, Outcome, ProgressFlush, ProgressSink};
use super::limit::ConnectionGate;
use super::probe;
use super::task::{StopKind, Task, MAX_TASK_CONNECTIONS};
use super::types::*;
use super::persist;

/// 同时下载的任务数上限，其余任务排队等待
const MAX_CONCURRENT_TASKS: usize = 3;
/// 全局连接数上限：默认值与硬上限（三个任务各开满单任务上限就到顶）
pub const DEFAULT_MAX_CONNECTIONS: usize = 16;
pub const MAX_CONNECTIONS_LIMIT: usize = MAX_CONCURRENT_TASKS * MAX_TASK_CONNECTIONS;
const DEFAULT_CONNECTIONS: usize = 4;
/// 预览磁力时的等待上限：磁力本身不含文件清单，得先从 peer 拿到元数据
const PREVIEW_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

pub enum ManagerEvent {
    Update(TaskView),
    Removed(String),
}

pub type EventSink = Arc<dyn Fn(ManagerEvent) + Send + Sync>;

pub struct DownloadManager {
    tasks: Mutex<Vec<Arc<Task>>>,
    waiting: Mutex<VecDeque<String>>,
    active: AtomicUsize,
    client: reqwest::Client,
    sink: EventSink,
    default_dir: PathBuf,
    /// 所有任务合计的连接数闸门，上限由设置决定
    gate: Arc<ConnectionGate>,
    /// BT 会话按需创建：不用磁力就不开 DHT、不占监听端口
    bt: tokio::sync::OnceCell<Arc<BtRuntime>>,
    /// 为 None 时不做任何持久化，下载功能照常
    store: Option<Arc<Store>>,
}

impl DownloadManager {
    pub fn new(
        sink: EventSink,
        default_dir: PathBuf,
        store: Option<Arc<Store>>,
    ) -> Arc<Self> {
        let manager = Arc::new(DownloadManager {
            tasks: Mutex::new(Vec::new()),
            waiting: Mutex::new(VecDeque::new()),
            active: AtomicUsize::new(0),
            client: probe::client(),
            sink,
            default_dir: default_dir.clone(),
            gate: ConnectionGate::new(DEFAULT_MAX_CONNECTIONS),
            bt: tokio::sync::OnceCell::new(),
            store,
        });

        if let Some(store) = &manager.store {
            // 恢复的任务保持原有进度但不自动开跑，由用户决定何时继续
            let restored = persist::restore(store, &default_dir);
            if !restored.is_empty() {
                println!("已恢复 {} 个已保存的任务", restored.len());
                manager.tasks.lock().unwrap().extend(restored);
            }
        }
        manager
    }

    fn persist(&self, task: &Task) {
        let Some(store) = &self.store else {
            return;
        };
        if let Err(err) = persist::save_all(store, task) {
            eprintln!("任务 {} 落库失败：{err}", task.id);
        }
    }

    fn forget(&self, id: &str) {
        let Some(store) = &self.store else {
            return;
        };
        if let Err(err) = store.delete_task(id) {
            eprintln!("任务 {id} 从数据库删除失败：{err}");
        }
    }

    pub fn default_dir(&self) -> String {
        self.default_dir.to_string_lossy().to_string()
    }

    /// 测试夹具（以及将来的启动预热）可以先把会话装进来。会话只建一次。
    pub fn install_bt_runtime(&self, runtime: Arc<BtRuntime>) -> Result<(), String> {
        self.bt
            .set(runtime)
            .map_err(|_| "BT 会话已经创建过了".to_string())
    }

    async fn bt_runtime(&self) -> Result<Arc<BtRuntime>, String> {
        self.bt
            .get_or_try_init(|| BtRuntime::new(self.default_dir.clone()))
            .await
            .cloned()
    }

    /// 所有任务合计的连接数上限，来自设置；调大立刻放行排队的通道，
    /// 调小则等已连上的通道下完当前分片、陆续还位后生效。
    pub fn set_max_connections(&self, limit: usize) {
        self.gate.set_limit(limit.clamp(1, MAX_CONNECTIONS_LIMIT));
    }

    pub fn max_connections(&self) -> usize {
        self.gate.limit()
    }

    /// 当前真正占用的连接数，验证与调试用。
    pub fn active_connections(&self) -> usize {
        self.gate.active()
    }

    pub fn list(&self) -> Vec<TaskView> {
        self.tasks
            .lock()
            .unwrap()
            .iter()
            .map(|task| task.snapshot())
            .collect()
    }

    fn find(&self, id: &str) -> Option<Arc<Task>> {
        self.tasks
            .lock()
            .unwrap()
            .iter()
            .find(|task| task.id == id)
            .cloned()
    }

    /// 是否仍有下载线程在跑；暂停/取消是异步收尾的，界面与测试可用它判断是否真正停稳。
    pub fn is_running(&self, id: &str) -> bool {
        self.find(id).map(|task| task.is_running()).unwrap_or(false)
    }

    fn emit(&self, task: &Task) {
        (self.sink)(ManagerEvent::Update(task.snapshot()));
    }

    fn remove_from_waiting(&self, id: &str) {
        self.waiting.lock().unwrap().retain(|queued| queued != id);
    }

    /// 新建任务：http(s) 链接、磁力链接，或本地 .torrent 种子文件的路径。
    /// `only_files` 是预览后勾选的文件序号，None 表示全都下。
    pub fn add(
        self: &Arc<Self>,
        url: String,
        dir: Option<String>,
        connections: Option<usize>,
        only_files: Option<Vec<usize>>,
    ) -> Result<String, String> {
        let url = url.trim().to_string();
        if looks_like_torrent_path(&url) {
            return self.add_torrent_file(url, dir, connections, only_files);
        }

        let dir = self.resolve_dir(dir)?;
        let connections = connections.unwrap_or(DEFAULT_CONNECTIONS);
        let id = Uuid::new_v4().to_string();
        let task = if url.starts_with("magnet:") {
            let info = bt::parse_magnet(&url)?;
            Task::new_bt(id.clone(), url, dir, connections, info.info_hash, info.name)
        } else if url.starts_with("http://") || url.starts_with("https://") {
            Task::new(id.clone(), url, dir, connections)
        } else {
            return Err(
                "当前版本支持 http(s) 链接、磁力链接（magnet:）与 .torrent 种子文件，其它协议还在路上"
                    .to_string(),
            );
        };
        self.enqueue(Arc::new(task), only_files)
    }

    /// 种子文件任务：元数据就在文件里，不用等 peer，入队时就有真实大小。
    pub fn add_torrent_file(
        self: &Arc<Self>,
        path: String,
        dir: Option<String>,
        connections: Option<usize>,
        only_files: Option<Vec<usize>>,
    ) -> Result<String, String> {
        let path = path.trim().to_string();
        let info = bt::parse_torrent_file(std::path::Path::new(&path))?;
        let dir = self.resolve_dir(dir)?;
        let id = Uuid::new_v4().to_string();
        let task = Task::new_torrent(
            id.clone(),
            path,
            dir,
            connections.unwrap_or(DEFAULT_CONNECTIONS),
            info,
        );
        self.enqueue(Arc::new(task), only_files)
    }

    /// 只取元数据看一眼种子里的文件，不建任务、不落盘。
    pub async fn preview_magnet(&self, magnet: String) -> Result<TorrentPreview, String> {
        let runtime = self.bt_runtime().await?;
        bt::preview_magnet(&runtime, &magnet, PREVIEW_TIMEOUT).await
    }

    /// 预览本地种子文件：元数据随文件自带，纯本地解析。
    pub fn preview_torrent_file(&self, path: String) -> Result<TorrentPreview, String> {
        bt::parse_torrent_file(std::path::Path::new(path.trim()))
    }

    /// 改任务勾选的文件：正在跑的任务由会话在下一拍应用，暂停中的等继续时生效。
    pub fn set_files(&self, id: &str, indices: Vec<usize>) -> Result<(), String> {
        let task = self.find(id).ok_or_else(|| "任务不存在".to_string())?;
        if indices.is_empty() {
            // 一个都不下等于让任务立刻「完成」，没有意义，界面也不该发出来
            return Err("至少要留一个文件".to_string());
        }
        {
            let inner = task.inner.lock().unwrap();
            if inner.bt_files.is_empty() {
                return Err("还没有拿到这个种子的文件清单".to_string());
            }
            let count = inner.bt_files.len();
            if indices.iter().any(|index| *index >= count) {
                return Err(format!("文件序号超出范围（这个种子共 {count} 个文件）"));
            }
        }
        let mut indices = indices;
        indices.sort_unstable();
        indices.dedup();
        task.set_only_files(indices);
        self.emit(&task);
        self.persist(&task);
        Ok(())
    }

    fn resolve_dir(&self, dir: Option<String>) -> Result<PathBuf, String> {
        let dir = match dir {
            Some(dir) if !dir.trim().is_empty() => PathBuf::from(dir.trim()),
            _ => self.default_dir.clone(),
        };
        std::fs::create_dir_all(&dir).map_err(|e| format!("无法创建下载目录：{e}"))?;
        Ok(dir)
    }

    fn enqueue(self: &Arc<Self>, task: Arc<Task>, only_files: Option<Vec<usize>>) -> Result<String, String> {
        let id = task.id.clone();
        // 勾选要在挂载之前写进任务，否则会先下起没勾的文件
        if let Some(indices) = only_files {
            if indices.is_empty() {
                return Err("至少要留一个文件".to_string());
            }
            task.set_only_files(indices);
        }
        self.tasks.lock().unwrap().push(task.clone());
        self.emit(&task);
        self.persist(&task);
        self.start(&id)?;
        Ok(id)
    }

    fn start(self: &Arc<Self>, id: &str) -> Result<(), String> {
        let task = self.find(id).ok_or_else(|| "任务不存在".to_string())?;
        {
            let inner = task.inner.lock().unwrap();
            if inner.running {
                return Ok(());
            }
            // 取消只是「停在这儿」，临时文件与分片进度还在，允许继续
            if matches!(inner.status, TaskStatus::Completed) {
                return Err("任务已结束".to_string());
            }
        }

        if self.active.load(Ordering::SeqCst) >= MAX_CONCURRENT_TASKS {
            task.inner.lock().unwrap().status = TaskStatus::Queued;
            {
                let mut waiting = self.waiting.lock().unwrap();
                if !waiting.iter().any(|queued| queued == id) {
                    waiting.push_back(id.to_string());
                }
            }
            self.emit(&task);
            return Ok(());
        }

        self.active.fetch_add(1, Ordering::SeqCst);
        self.spawn_run(task);
        Ok(())
    }

    fn spawn_run(self: &Arc<Self>, task: Arc<Task>) {
        let manager = Arc::clone(self);
        // 引擎不依赖 Tauri：运行在 Tauri 的 tokio 运行时之上，也便于单独测试
        tokio::spawn(async move {
            manager.run_task(task).await;
        });
    }

    async fn run_task(self: Arc<Self>, task: Arc<Task>) {
        let cancel = CancellationToken::new();
        {
            let mut inner = task.inner.lock().unwrap();
            inner.running = true;
            inner.cancel = Some(cancel.clone());
            inner.stop = None;
        }

        let engine_sink: ProgressSink = {
            let sink = self.sink.clone();
            Arc::new(move |view| sink(ManagerEvent::Update(view)))
        };
        // 引擎负责在 fsync 之后回调它，把任务行与分片进度写进数据库
        let flush: Option<ProgressFlush> = self.store.as_ref().map(|store| {
            let store = Arc::clone(store);
            let task = Arc::clone(&task);
            let flush: ProgressFlush = Arc::new(move || {
                if let Err(err) = persist::save_all(&store, &task) {
                    eprintln!("任务 {} 进度落库失败：{err}", task.id);
                }
            });
            flush
        });
        let outcome = if task.kind == TaskKind::Bt {
            match self.bt_runtime().await {
                Ok(runtime) => bt::run(task.clone(), runtime, engine_sink, cancel, flush).await,
                Err(err) => {
                    task.set_error(err.clone());
                    task.inner.lock().unwrap().status = TaskStatus::Failed;
                    (self.sink)(ManagerEvent::Update(task.snapshot()));
                    Outcome::Failed(err)
                }
            }
        } else {
            engine::run(
                task.clone(),
                self.client.clone(),
                engine_sink,
                cancel,
                flush,
                self.gate.clone(),
            )
            .await
        };

        let (removed, delete_file, path_to_delete) = {
            let mut inner = task.inner.lock().unwrap();
            inner.running = false;
            inner.cancel = None;
            let delete_file = matches!(inner.stop, Some(StopKind::Cancel { delete_file: true }));
            let save_path = inner.save_path.clone();
            let removed = inner.removed;
            // 取消后的重试不应携带上一次的停止意图
            inner.stop = None;
            (removed, delete_file, save_path)
        };

        if removed {
            self.remove_from_waiting(&task.id);
            self.tasks.lock().unwrap().retain(|item| item.id != task.id);
            self.forget(&task.id);
            delete_task_files(&task, delete_file);
            (self.sink)(ManagerEvent::Removed(task.id.clone()));
        } else {
            if let Outcome::Cancelled = outcome {
                if delete_file {
                    let _ = std::fs::remove_file(super::path::part_path(&path_to_delete));
                }
            }
            self.emit(&task);
        }

        self.active.fetch_sub(1, Ordering::SeqCst);
        self.pump();

        let resume_pending = {
            let mut inner = task.inner.lock().unwrap();
            let pending = inner.resume_pending;
            inner.resume_pending = false;
            pending
        };
        if resume_pending && !removed {
            let _ = self.start(&task.id);
        }
    }

    fn pump(self: &Arc<Self>) {
        loop {
            if self.active.load(Ordering::SeqCst) >= MAX_CONCURRENT_TASKS {
                return;
            }
            let next = self.waiting.lock().unwrap().pop_front();
            let Some(id) = next else { return };
            let Some(task) = self.find(&id) else { continue };
            let status = task.inner.lock().unwrap().status;
            if status != TaskStatus::Queued {
                continue;
            }
            self.active.fetch_add(1, Ordering::SeqCst);
            self.spawn_run(task);
        }
    }

    pub fn pause(&self, id: &str) -> Result<(), String> {
        let task = self.find(id).ok_or_else(|| "任务不存在".to_string())?;
        let cancel = {
            let mut inner = task.inner.lock().unwrap();
            if inner.running {
                inner.stop = Some(StopKind::Pause);
                inner.status = TaskStatus::Paused;
                inner.cancel.clone()
            } else {
                None
            }
        };
        match cancel {
            Some(cancel) => cancel.cancel(),
            None => {
                self.remove_from_waiting(id);
                let mut inner = task.inner.lock().unwrap();
                if matches!(inner.status, TaskStatus::Queued | TaskStatus::Probing) {
                    inner.status = TaskStatus::Paused;
                }
            }
        }
        self.emit(&task);
        // 运行中的任务由引擎收尾时落库；排队中的任务在这里直接记下暂停状态
        if !task.is_running() {
            self.persist(&task);
        }
        Ok(())
    }

    pub fn resume(self: &Arc<Self>, id: &str) -> Result<(), String> {
        let task = self.find(id).ok_or_else(|| "任务不存在".to_string())?;
        {
            let mut inner = task.inner.lock().unwrap();
            if inner.running {
                // 暂停还在收尾：记录下来，等本轮结束后自动重启，避免「继续」被静默丢弃
                inner.resume_pending = true;
                return Ok(());
            }
            if !matches!(
                inner.status,
                TaskStatus::Paused | TaskStatus::Failed | TaskStatus::Cancelled
            ) {
                return Err("只有已暂停、失败或已取消的任务可以继续".to_string());
            }
            inner.error = None;
            inner.stop = None;
        }
        self.start(id)
    }

    pub fn cancel(self: &Arc<Self>, id: &str, delete_file: bool) -> Result<(), String> {
        let task = self.find(id).ok_or_else(|| "任务不存在".to_string())?;
        let cancel = {
            let mut inner = task.inner.lock().unwrap();
            if inner.running {
                inner.stop = Some(StopKind::Cancel { delete_file });
                inner.status = TaskStatus::Cancelled;
                inner.cancel.clone()
            } else {
                None
            }
        };
        match cancel {
            Some(cancel) => cancel.cancel(),
            None => {
                self.remove_from_waiting(id);
                {
                    let mut inner = task.inner.lock().unwrap();
                    inner.status = TaskStatus::Cancelled;
                    inner.stop = Some(StopKind::Cancel { delete_file });
                }
                if delete_file {
                    let _ = std::fs::remove_file(task.part_path());
                }
            }
        }
        self.emit(&task);
        if !task.is_running() {
            self.persist(&task);
        }
        Ok(())
    }

    pub fn remove(self: &Arc<Self>, id: &str, delete_file: bool) -> Result<(), String> {
        let task = self.find(id).ok_or_else(|| "任务不存在".to_string())?;
        if task.is_running() {
            {
                let mut inner = task.inner.lock().unwrap();
                inner.removed = true;
                inner.stop = Some(StopKind::Cancel { delete_file });
                if let Some(cancel) = &inner.cancel {
                    cancel.cancel();
                }
            }
            return Ok(());
        }

        self.remove_from_waiting(id);
        self.tasks.lock().unwrap().retain(|item| item.id != id);
        self.forget(id);
        delete_task_files(&task, delete_file);
        (self.sink)(ManagerEvent::Removed(id.to_string()));
        Ok(())
    }
}

/// 看起来是本地种子文件路径（而不是链接）时，按 .torrent 处理。
fn looks_like_torrent_path(input: &str) -> bool {
    !input.contains("://") && input.to_ascii_lowercase().ends_with(".torrent")
}

/// 删除任务留下的文件。BT 的 save_path 可能是多文件种子的目录，
/// 只有它确实位于任务目录之下时才递归删，避免误伤目录里的其它内容。
fn delete_task_files(task: &Task, delete_file: bool) {
    let (dir, save_path) = {
        let inner = task.inner.lock().unwrap();
        (inner.dir.clone(), inner.save_path.clone())
    };
    let _ = std::fs::remove_file(super::path::part_path(&save_path));
    if !delete_file || !save_path.exists() {
        return;
    }
    match task.kind {
        TaskKind::Http => {
            let _ = std::fs::remove_file(&save_path);
        }
        TaskKind::Bt => {
            if save_path.parent() == Some(dir.as_path()) {
                let _ = if save_path.is_dir() {
                    std::fs::remove_dir_all(&save_path)
                } else {
                    std::fs::remove_file(&save_path)
                };
            }
        }
    }
}
