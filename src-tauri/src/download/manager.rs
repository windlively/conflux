use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::store::Store;

use super::bt::{self, BtRuntime};
use super::ed2k::{self, Ed2kConfig, Ed2kRuntime, Ed2kSearchResult};
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
const DEFAULT_CONNECTIONS: usize = 8;
/// 预览磁力时的等待上限：磁力本身不含文件清单，得先从 peer 拿到元数据
const PREVIEW_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);
/// 搜 ed2k 的等待上限：要等服务器与 Kad 两边都回一轮
const ED2K_SEARCH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(25);

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
    /// BT 的 SOCKS5 代理；会话建好之后改不了，改动要等下次建会话（重启应用）
    bt_proxy: Mutex<Option<String>>,
    /// ed2k 侧车同样按需拉起：没有 ed2k 任务时不必起这个进程
    ed2k: tokio::sync::OnceCell<Arc<Ed2kRuntime>>,
    /// ed2k 侧车的状态文件放这儿（应用数据目录），不设时用下载目录
    data_dir: Mutex<Option<PathBuf>>,
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
            bt_proxy: Mutex::new(None),
            ed2k: tokio::sync::OnceCell::new(),
            data_dir: Mutex::new(None),
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
        let proxy = self.bt_proxy.lock().unwrap().clone();
        self.bt
            .get_or_try_init(|| BtRuntime::new_with_proxy(self.default_dir.clone(), proxy))
            .await
            .cloned()
    }

    /// 设置 BT 的 SOCKS5 代理；传空串表示不用代理。
    /// 代理是会话级设置，会话已经建好时这次改动不会立刻生效（界面据此提示重启）。
    pub fn set_bt_proxy(&self, raw: &str) -> Result<Option<String>, String> {
        let value = bt::normalize_proxy(raw)?;
        *self.bt_proxy.lock().unwrap() = value.clone();
        Ok(value)
    }

    /// BT 会话是否已经建好：是的话，改代理要重启应用才生效
    pub fn bt_session_started(&self) -> bool {
        self.bt.get().is_some()
    }

    /// ed2k 侧车的状态文件（断点数据与 Kad 路由表）放在这里
    pub fn set_data_dir(&self, dir: PathBuf) {
        *self.data_dir.lock().unwrap() = Some(dir);
    }

    /// ed2k 侧车：第一次用到才拉起进程
    pub async fn ed2k_runtime(&self) -> Result<Arc<Ed2kRuntime>, String> {
        self.ed2k
            .get_or_try_init(|| async {
                let data_dir = self
                    .data_dir
                    .lock()
                    .unwrap()
                    .clone()
                    .unwrap_or_else(|| self.default_dir.clone());
                Ed2kRuntime::new(Ed2kConfig {
                    state_path: ed2k::state_path(&data_dir),
                    // 用 eMule 的标准端口，别人（与自己的下次启动）都能回连
                    tcp_port: 4661,
                    udp_port: 4662,
                    kad: true,
                    upnp: false,
                })
                .await
            })
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
        let task = if url.starts_with("ed2k://") {
            // ed2k 链接自带文件名、大小与哈希，入队时就能把任务卡填满
            let info = ed2k::parse_link(&url)?;
            Task::new_ed2k(
                id.clone(),
                url,
                dir,
                connections,
                ed2k::safe_file_name(&info.name),
                info.size,
                info.hash,
            )
        } else if url.starts_with("magnet:") {
            let info = bt::parse_magnet(&url)?;
            Task::new_bt(id.clone(), url, dir, connections, info.info_hash, info.name)
        } else if url.starts_with("http://") || url.starts_with("https://") {
            Task::new(id.clone(), url, dir, connections)
        } else {
            return Err(
                "当前版本支持 http(s) 链接、磁力链接（magnet:）、.torrent 种子文件与 ed2k 链接，其它协议还在路上"
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

    /// 在 eD2k 服务器与 Kad 上搜关键词，返回结果给界面挑。
    /// 侧车会按哈希去重并带上源数，界面据它决定下哪个。
    pub async fn search_ed2k(&self, query: String) -> Result<Vec<Ed2kSearchResult>, String> {
        let query = query.trim().to_string();
        if query.is_empty() {
            return Err("请先输入搜索关键词".to_string());
        }
        if ed2k::search_query_rejected(&query) {
            return Err("这类内容不在搜索范围内".to_string());
        }
        let runtime = self.ed2k_runtime().await?;
        let state = runtime.search(&query, ED2K_SEARCH_TIMEOUT).await?;
        Ok(ed2k::search_results(&state))
    }

    /// 改任务勾选的文件：正在跑的任务由会话在下一拍应用，暂停中的等继续时生效，
    /// 已完成的（勾回了没下过的文件时）会自动重新挂上接着下。
    pub fn set_files(self: &Arc<Self>, id: &str, indices: Vec<usize>) -> Result<(), String> {
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
        // 比的是归一化之后的结果：把 [0, 1] 换成「全选」不算改动
        let before = task.only_files();
        task.set_only_files(indices);
        let after = task.only_files();
        // 只有新勾上了文件才值得重挂；取消勾选时文件早就下好了
        let added = match (&before, &after) {
            (_, None) => before.is_some(),
            (None, Some(_)) => false,
            (Some(old), Some(new)) => new.iter().any(|index| !old.contains(index)),
        };
        self.emit(&task);
        self.persist(&task);
        // 已完成的任务里把文件勾回来：自动接着下。少了这一步，界面与库里的勾选变了，
        // 文件却永远不补（完成后会话仍把句柄留着，重新挂上就能续）。
        let restart = added && {
            let inner = task.inner.lock().unwrap();
            matches!(inner.status, TaskStatus::Completed) && !inner.running
        };
        if restart {
            self.start(id)?;
        }
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
        self.reject_duplicate_torrent(&task)?;
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

    /// 同一个种子同时只留一个任务：会话按 info_hash 管句柄，第二个任务接上去会用
    /// 别人的保存目录与勾选，还可能「秒完成」而自己的目录是空的（见方案文档 §16.4）。
    fn reject_duplicate_torrent(&self, task: &Task) -> Result<(), String> {
        // BT 与 ed2k 都是按内容哈希认文件，同一个哈希只留一个任务
        if !matches!(task.kind, TaskKind::Bt | TaskKind::Ed2k) {
            return Ok(());
        }
        let Some(hash) = torrent_hash_of(task) else {
            // 链接不对到那种程度，等真正挂载时的解析报错去说
            return Ok(());
        };
        // 先克隆一份任务表再逐个看：不长时间占着列表锁，也不与 inner 锁套在一起
        let others: Vec<Arc<Task>> = self.tasks.lock().unwrap().clone();
        let hit = others.into_iter().find(|other| {
            other.id != task.id
                && matches!(other.kind, TaskKind::Bt | TaskKind::Ed2k)
                && torrent_hash_of(other).as_deref() == Some(hash.as_str())
        });
        match hit {
            Some(other) => Err(format!(
                "这个文件已经在任务「{}」里了：先移除那个任务，或者继续用它",
                other.file_name()
            )),
            None => Ok(()),
        }
    }

    /// 任务被删掉后，会话里那份句柄也该跟着消失：否则下次为同一个种子建任务时，
    /// librqbit 会把旧句柄原样交回来，新任务的目录与勾选都会被忽略。
    fn drop_bt_handle(&self, task: &Task) {
        if task.kind != TaskKind::Bt {
            return;
        }
        let (Some(runtime), Some(hash)) = (self.bt.get().cloned(), task.info_hash()) else {
            return;
        };
        tokio::spawn(async move { bt::drop_torrent(&runtime, &hash).await });
    }

    /// 任务被删掉后，侧车里那条记录也要清掉：否则侧车下次启动会把它恢复回来。
    /// 文件由 `delete_task_files` 处理，所以这里不让侧车删。
    fn drop_ed2k_transfer(&self, task: &Task) {
        if task.kind != TaskKind::Ed2k {
            return;
        }
        let (Some(runtime), Some(hash)) = (self.ed2k.get().cloned(), task.info_hash()) else {
            return;
        };
        tokio::spawn(async move {
            let _ = runtime
                .call(
                    "remove",
                    Some(serde_json::json!({ "hash": hash, "deleteFile": false })),
                )
                .await;
        });
    }

    fn start(self: &Arc<Self>, id: &str) -> Result<(), String> {
        let task = self.find(id).ok_or_else(|| "任务不存在".to_string())?;
        {
            let inner = task.inner.lock().unwrap();
            if inner.running {
                return Ok(());
            }
        }
        // 「已完成」不挡在这里：BT 的完成只是停下做种，勾选改回去要能接着下
        // （`set_files`），取消也只是「停在这儿」，两者的临时文件与进度都在。

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
        } else if task.kind == TaskKind::Ed2k {
            match self.ed2k_runtime().await {
                Ok(runtime) => ed2k::run(task.clone(), runtime, engine_sink, cancel, flush).await,
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
            // 句柄由 bt::run 的取消分支顺手摘掉了（它拿到的是 Id，不会再找一遍）
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
        self.drop_bt_handle(&task);
        self.drop_ed2k_transfer(&task);
        delete_task_files(&task, delete_file);
        (self.sink)(ManagerEvent::Removed(id.to_string()));
        Ok(())
    }
}

/// 看起来是本地种子文件路径（而不是链接）时，按 .torrent 处理。
fn looks_like_torrent_path(input: &str) -> bool {
    !input.contains("://") && input.to_ascii_lowercase().ends_with(".torrent")
}

/// 任务的磁力原文（种子文件任务是 None）。
fn magnet_of(task: &Task) -> Option<String> {
    let inner = task.inner.lock().unwrap();
    inner.bt.as_ref()?.magnet.clone()
}

/// 任务对应哪个种子：种子文件的 info hash 入队就有，磁力的直接写在链接里。
/// 拿不到（链接本身就是坏的）时返回 None。
fn torrent_hash_of(task: &Task) -> Option<String> {
    if let Some(hash) = task.info_hash() {
        return Some(hash);
    }
    bt::parse_magnet(&magnet_of(task)?)
        .ok()
        .map(|info| info.info_hash)
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
        // ed2k 与 http 一样是单文件，直接落在任务目录下
        TaskKind::Http | TaskKind::Ed2k => {
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
