use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::store::Store;

use super::engine::{self, Outcome, ProgressFlush, ProgressSink};
use super::probe;
use super::task::{StopKind, Task};
use super::types::*;
use super::persist;

/// 同时下载的任务数上限，其余任务排队等待
const MAX_CONCURRENT_TASKS: usize = 3;
const DEFAULT_CONNECTIONS: usize = 4;

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

    pub fn add(
        self: &Arc<Self>,
        url: String,
        dir: Option<String>,
        connections: Option<usize>,
    ) -> Result<String, String> {
        let url = url.trim().to_string();
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            return Err("当前版本仅支持 http/https 链接".to_string());
        }
        let dir = match dir {
            Some(dir) if !dir.trim().is_empty() => PathBuf::from(dir.trim()),
            _ => self.default_dir.clone(),
        };
        std::fs::create_dir_all(&dir).map_err(|e| format!("无法创建下载目录：{e}"))?;

        let id = Uuid::new_v4().to_string();
        let task = Arc::new(Task::new(
            id.clone(),
            url,
            dir,
            connections.unwrap_or(DEFAULT_CONNECTIONS),
        ));
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
            if matches!(inner.status, TaskStatus::Completed | TaskStatus::Cancelled) {
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
        let outcome = engine::run(
            task.clone(),
            self.client.clone(),
            engine_sink,
            cancel,
            flush,
        )
        .await;

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
            if delete_file {
                let _ = std::fs::remove_file(super::path::part_path(&path_to_delete));
                if path_to_delete.exists() {
                    let _ = std::fs::remove_file(&path_to_delete);
                }
            }
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
            if !matches!(inner.status, TaskStatus::Paused | TaskStatus::Failed) {
                return Err("只有已暂停或失败的任务可以继续".to_string());
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
        if delete_file {
            let save_path = task.inner.lock().unwrap().save_path.clone();
            let _ = std::fs::remove_file(super::path::part_path(&save_path));
            if save_path.exists() {
                let _ = std::fs::remove_file(&save_path);
            }
        }
        (self.sink)(ManagerEvent::Removed(id.to_string()));
        Ok(())
    }
}
