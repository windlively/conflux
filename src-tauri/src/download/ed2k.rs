//! ed2k：把 goed2k 包成的侧车进程托管起来。
//!
//! Rust 侧没有能用的 ed2k 客户端实现（crates.io 上只有那个 MD4 哈希函数），
//! 所以进程内没有协议栈可写：这里起一个 Go 侧车（`sidecar/ed2k`），按行式 JSON
//! 通信——stdin 发命令、stdout 收应答与事件，进度与状态都从事件里来。
//!
//! 与 BT 一样，断点数据不在我们手里：侧车自己把分片位图与 Kad 路由表写进状态文件，
//! 我们只负责把「这个任务在侧车里的那个文件」映射成任务卡上的进度。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use super::engine::{Outcome, ProgressFlush, ProgressSink};
use super::path::sanitize_file_name;
use super::task::{StopKind, Task};
use super::types::{Ed2kStats, TaskStatus};

/// 状态事件的节拍（侧车每秒推一次），跟着它刷新界面就够了
const TICK: Duration = Duration::from_millis(500);
/// 单向命令的等待上限：侧车是本机进程，正常都是毫秒级
const CALL_TIMEOUT: Duration = Duration::from_secs(20);
/// 等侧车握手 ready 的上限：第一次启动要建 Kad 路由表
const READY_TIMEOUT: Duration = Duration::from_secs(20);
/// 挂上之后连续多少拍查不到这个任务就当它丢了
const MISSING_TICKS: u32 = 10;

/// ed2k 链接里的信息，入队时就要用（文件名、大小、哈希）
#[derive(Debug, Clone)]
pub struct Ed2kLink {
    pub name: String,
    pub size: u64,
    pub hash: String,
}

/// 解析 `ed2k://|file|<名字>|<字节数>|<哈希>|/`。
/// 链接里的名字是 URL 编码过的，这里解一次；解不出来就按原文用。
pub fn parse_link(link: &str) -> Result<Ed2kLink, String> {
    let trimmed = link.trim();
    let rest = trimmed
        .strip_prefix("ed2k://|file|")
        .ok_or_else(|| "不是 ed2k 文件链接（要 ed2k://|file|…）".to_string())?;
    let parts: Vec<&str> = rest.split('|').collect();
    if parts.len() < 4 {
        return Err("ed2k 链接缺少字段".to_string());
    }
    let size: u64 = parts[1]
        .trim()
        .parse()
        .map_err(|_| format!("ed2k 链接里的大小不是数字：{}", parts[1]))?;
    let hash = parts[2].trim().to_ascii_uppercase();
    if hash.len() != 32 || !hash.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(format!("ed2k 哈希应是 32 位十六进制：{hash}"));
    }
    let name = percent_decode(parts[0]);
    Ok(Ed2kLink { name, size, hash })
}

/// 只处理 %XX 与 %20 这类转义，够 ed2k 链接用了（不引依赖）
fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[index + 1..index + 3]).unwrap_or("");
            if let Ok(byte) = u8::from_str_radix(hex, 16) {
                out.push(byte);
                index += 3;
                continue;
            }
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

/// 侧车里的一个任务，字段与 `sidecar/ed2k` 推来的 JSON 对应
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct TransferSnapshot {
    pub hash: String,
    pub name: String,
    pub path: String,
    pub link: String,
    pub size: i64,
    /// downloading / paused / completed / verifying / queued
    pub state: String,
    pub done: i64,
    pub wanted: i64,
    pub received: i64,
    pub rate_bps: u64,
    pub peers: u32,
    pub active_peers: u32,
    pub uploaded: u64,
    pub upload_rate: u64,
    pub eta_seconds: i64,
    pub removed: bool,
}

impl TransferSnapshot {
    fn completed(&self) -> bool {
        self.state == "completed"
    }

    fn status(&self) -> TaskStatus {
        match self.state.as_str() {
            "paused" => TaskStatus::Paused,
            "completed" => TaskStatus::Completed,
            "verifying" => TaskStatus::Probing,
            _ => TaskStatus::Downloading,
        }
    }
}

/// 侧车的启动参数
#[derive(Debug, Clone)]
pub struct Ed2kConfig {
    /// 状态文件：断点数据与 Kad 路由表都在这儿
    pub state_path: PathBuf,
    pub tcp_port: u16,
    pub udp_port: u16,
    pub kad: bool,
    pub upnp: bool,
}

struct Pending {
    sender: oneshot::Sender<Result<Value, String>>,
}

/// 侧车进程与它的协议状态
pub struct Ed2kRuntime {
    child: Mutex<Option<Child>>,
    stdin: tokio::sync::Mutex<Option<tokio::process::ChildStdin>>,
    pending: Mutex<HashMap<u64, Pending>>,
    next_id: AtomicU64,
    /// 侧车推来的最新任务快照，按哈希索引
    transfers: Mutex<HashMap<String, TransferSnapshot>>,
    servers: AtomicU64,
    kad_nodes: AtomicU64,
    ready: AtomicBool,
    alive: AtomicBool,
    config: Ed2kConfig,
}

impl Ed2kRuntime {
    /// 拉起侧车并等它握手：二进制找不到、端口占用都会在这里报出来。
    pub async fn new(config: Ed2kConfig) -> Result<Arc<Self>, String> {
        let binary = sidecar_path()?;
        let mut child = Command::new(&binary)
            .arg("-state")
            .arg(&config.state_path)
            .arg("-tcp-port")
            .arg(config.tcp_port.to_string())
            .arg("-udp-port")
            .arg(config.udp_port.to_string())
            .arg("-kad")
            .arg(if config.kad { "true" } else { "false" })
            .arg("-upnp")
            .arg(if config.upnp { "true" } else { "false" })
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|err| format!("无法启动 ed2k 侧车（{}）：{err}", binary.display()))?;

        let stdin = child.stdin.take().ok_or_else(|| "侧车没有 stdin".to_string())?;
        let stdout = child.stdout.take().ok_or_else(|| "侧车没有 stdout".to_string())?;
        let stderr = child.stderr.take();

        let runtime = Arc::new(Ed2kRuntime {
            child: Mutex::new(Some(child)),
            stdin: tokio::sync::Mutex::new(Some(stdin)),
            pending: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
            transfers: Mutex::new(HashMap::new()),
            servers: AtomicU64::new(0),
            kad_nodes: AtomicU64::new(0),
            ready: AtomicBool::new(false),
            alive: AtomicBool::new(true),
            config,
        });

        spawn_reader(Arc::clone(&runtime), stdout);
        if let Some(stderr) = stderr {
            spawn_stderr(stderr);
        }

        // 等 ready：侧车要把监听端口绑上、Kad 表建起来才会报
        let started = Instant::now();
        while started.elapsed() < READY_TIMEOUT {
            if runtime.ready.load(Ordering::SeqCst) {
                return Ok(runtime);
            }
            if !runtime.alive.load(Ordering::SeqCst) {
                return Err("ed2k 侧车启动失败，详情见日志".to_string());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        Err("等 ed2k 侧车就绪超时".to_string())
    }

    pub fn config(&self) -> &Ed2kConfig {
        &self.config
    }

    /// 发一条命令等应答，`params` 为 None 时表示无参数
    pub async fn call(&self, method: &str, params: Option<Value>) -> Result<Value, String> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (sender, receiver) = oneshot::channel();
        {
            let mut pending = self.pending.lock().unwrap();
            pending.insert(id, Pending { sender });
        }
        let request = match params {
            Some(params) => json!({ "id": id, "method": method, "params": params }),
            None => json!({ "id": id, "method": method }),
        };
        let line = format!("{request}\n");
        {
            let mut stdin = self.stdin.lock().await;
            let Some(stdin) = stdin.as_mut() else {
                return Err("ed2k 侧车已经退出".to_string());
            };
            stdin
                .write_all(line.as_bytes())
                .await
                .map_err(|err| format!("给侧车发命令失败：{err}"))?;
            stdin
                .flush()
                .await
                .map_err(|err| format!("给侧车发命令失败：{err}"))?;
        }
        match tokio::time::timeout(CALL_TIMEOUT, receiver).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err("ed2k 侧车没有回话".to_string()),
            Err(_) => {
                self.pending.lock().unwrap().remove(&id);
                Err(format!("ed2k 侧车对 {method} 没有响应"))
            }
        }
    }

    pub fn snapshot_of(&self, hash: &str) -> Option<TransferSnapshot> {
        self.transfers.lock().unwrap().get(hash).cloned()
    }

    /// 已连上的 eD2k 服务器数（侧车每秒推的 status 事件里来）
    pub fn servers(&self) -> u32 {
        self.servers.load(Ordering::SeqCst) as u32
    }

    /// Kad 路由表里活着的节点数
    pub fn kad_nodes(&self) -> u32 {
        self.kad_nodes.load(Ordering::SeqCst) as u32
    }

    /// 挂上一个 ed2k 链接；已经在侧车里挂着的（重启后恢复或重复入队）也算成功
    pub async fn ensure_transfer(&self, link: &str, dir: &Path) -> Result<TransferSnapshot, String> {
        match self
            .call(
                "add",
                Some(json!({ "link": link, "dir": dir.to_string_lossy() })),
            )
            .await
        {
            Ok(value) => serde_json::from_value::<TransferSnapshot>(value)
                .map_err(|err| format!("侧车返回的任务信息读不出来：{err}")),
            Err(err) if err.contains("已经在任务里") => {
                let parsed = parse_link(link)?;
                self.snapshot_of(&parsed.hash)
                    .ok_or_else(|| "侧车里已有这个任务，但状态还没到".to_string())
            }
            Err(err) => Err(err),
        }
    }

    /// 搜索：起一次搜索，等到它自己结束或者等够 `timeout`。
    /// goed2k 的搜索会一直挂着收结果（服务器一轮、Kad 一轮），等到超时就把
    /// 已经收到的结果交出去——搜索本来就该边收边看，不该因为没「结束」而失败。
    pub async fn search(&self, query: &str, timeout: Duration) -> Result<Value, String> {
        self.call(
            "search-start",
            Some(json!({ "query": query, "minSources": 1 })),
        )
        .await?;
        let started = Instant::now();
        loop {
            let state = self.call("search-state", None).await?;
            if let Some(error) = state.get("error").and_then(|value| value.as_str()) {
                if !error.is_empty() {
                    return Err(format!("搜索失败：{error}"));
                }
            }
            let running = state
                .get("state")
                .and_then(|value| value.as_str())
                .is_some_and(|state| state == "RUNNING");
            if !running {
                return Ok(state);
            }
            if started.elapsed() >= timeout {
                // 停掉之后侧车就不认这份结果了，把刚拿到的那份交出去
                let _ = self.call("search-stop", None).await;
                return Ok(state);
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }

    /// 让侧车落盘退出（应用退出、测试收尾时用）
    pub async fn shutdown(&self) {
        let _ = self.call("shutdown", None).await;
        let mut stdin = self.stdin.lock().await;
        *stdin = None;
        if let Some(mut child) = self.child.lock().unwrap().take() {
            let _ = child.start_kill();
            let _ = child.wait().await;
        }
        self.alive.store(false, Ordering::SeqCst);
    }

    fn resolve(&self, id: u64, result: Result<Value, String>) {
        let pending = self.pending.lock().unwrap().remove(&id);
        if let Some(pending) = pending {
            let _ = pending.sender.send(result);
        }
    }

    fn handle_event(&self, event: &Value) {
        match event.get("event").and_then(|value| value.as_str()) {
            Some("ready") => {
                self.ready.store(true, Ordering::SeqCst);
                println!(
                    "ed2k 侧车已就绪：{}",
                    event.get("build").and_then(|v| v.as_str()).unwrap_or("?")
                );
            }
            Some("status") | Some("progress") => {
                if let Some(servers) = event.get("servers").and_then(|value| value.as_u64()) {
                    self.servers.store(servers, Ordering::SeqCst);
                }
                if let Some(nodes) = event.get("kadLive").and_then(|value| value.as_u64()) {
                    self.kad_nodes.store(nodes, Ordering::SeqCst);
                }
                let Some(list) = event.get("transfers").and_then(|value| value.as_array()) else {
                    return;
                };
                let mut transfers = self.transfers.lock().unwrap();
                for item in list {
                    let Ok(snapshot) = serde_json::from_value::<TransferSnapshot>(item.clone()) else {
                        continue;
                    };
                    if snapshot.removed {
                        transfers.remove(&snapshot.hash);
                    } else {
                        // 进度事件字段较少，用「有值就覆盖」的方式合并
                        let entry = transfers.entry(snapshot.hash.clone()).or_default();
                        merge(entry, snapshot);
                    }
                }
            }
            Some("bye") | Some("fatal") => {
                if let Some(error) = event.get("error").and_then(|value| value.as_str()) {
                    eprintln!("ed2k 侧车退出：{error}");
                }
                self.alive.store(false, Ordering::SeqCst);
                let mut transfers = self.transfers.lock().unwrap();
                transfers.clear();
            }
            _ => {}
        }
    }
}

/// 进度事件只带少数字段：空值不覆盖已有状态
fn merge(target: &mut TransferSnapshot, incoming: TransferSnapshot) {
    if !incoming.hash.is_empty() {
        target.hash = incoming.hash;
    }
    if !incoming.name.is_empty() {
        target.name = incoming.name;
    }
    if !incoming.path.is_empty() {
        target.path = incoming.path;
    }
    if !incoming.link.is_empty() {
        target.link = incoming.link;
    }
    if !incoming.state.is_empty() {
        target.state = incoming.state;
    }
    target.size = incoming.size.max(target.size);
    target.done = incoming.done.max(target.done);
    target.wanted = incoming.wanted.max(target.wanted);
    target.received = incoming.received.max(target.received);
    target.peers = incoming.peers.max(target.peers);
    target.active_peers = incoming.active_peers.max(target.active_peers);
    target.uploaded = incoming.uploaded.max(target.uploaded);
    // 速率与 ETA 会回落，按整段覆盖
    target.rate_bps = incoming.rate_bps;
    target.upload_rate = incoming.upload_rate;
    target.eta_seconds = incoming.eta_seconds;
}

fn spawn_reader(runtime: Arc<Ed2kRuntime>, stdout: tokio::process::ChildStdout) {
    tokio::spawn(async move {
        let mut lines = BufReader::new(stdout).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let Ok(value) = serde_json::from_str::<Value>(trimmed) else {
                eprintln!("ed2k 侧车输出不是 JSON：{trimmed}");
                continue;
            };
            match value.get("id").and_then(|id| id.as_u64()) {
                Some(id) => {
                    let result = if value.get("ok").and_then(|ok| ok.as_bool()).unwrap_or(false) {
                        Ok(value.get("result").cloned().unwrap_or(Value::Null))
                    } else {
                        Err(value
                            .get("error")
                            .and_then(|error| error.as_str())
                            .unwrap_or("侧车报了个没说明的错误")
                            .to_string())
                    };
                    runtime.resolve(id, result);
                }
                None => runtime.handle_event(&value),
            }
        }
        runtime.alive.store(false, Ordering::SeqCst);
    });
}

fn spawn_stderr(stderr: tokio::process::ChildStderr) {
    tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            eprintln!("[ed2k] {line}");
        }
    });
}

/// 侧车二进制的位置：先看环境变量（测试用），再找主程序旁边，
/// 最后回落到开发树里的 `src-tauri/binaries/`。
fn sidecar_path() -> Result<PathBuf, String> {
    let name = if cfg!(windows) {
        "ed2k-sidecar.exe"
    } else {
        "ed2k-sidecar"
    };
    if let Ok(path) = std::env::var("CONFLUX_ED2K_SIDECAR") {
        let path = PathBuf::from(path);
        if path.exists() {
            return Ok(path);
        }
        return Err(format!("CONFLUX_ED2K_SIDECAR 指向的文件不存在：{}", path.display()));
    }
    if let Some(dir) = std::env::current_exe().ok().and_then(|exe| exe.parent().map(Path::to_path_buf))
    {
        for candidate in [
            dir.join(name),
            // 打包时 tauri 会把 externalBin 连同目标三元组一起放进来
            dir.join(format!("{name}-{}", target_triple())),
            dir.join(format!("{}-{}", "ed2k-sidecar", target_triple())),
        ] {
            if candidate.exists() {
                return Ok(candidate);
            }
        }
    }
    for candidate in [
        dev_sidecar_path(&format!("{name}-{}", target_triple())),
        dev_sidecar_path(name),
    ] {
        if candidate.exists() {
            return Ok(candidate);
        }
    }
    Err(format!(
        "找不到 ed2k 侧车（{name}）：先在 sidecar/ed2k 里跑一次构建脚本"
    ))
}

fn dev_sidecar_path(file_name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("binaries")
        .join(file_name)
}

fn target_triple() -> &'static str {
    // 只需要覆盖我们实际发布的两条链路
    if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        "aarch64-apple-darwin"
    } else if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        "x86_64-pc-windows-msvc"
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        "x86_64-apple-darwin"
    } else {
        "unknown"
    }
}

fn set_status(task: &Task, status: TaskStatus) {
    task.inner.lock().unwrap().status = status;
}

/// 侧车停了就不会再有 peer，别把上一拍的数字留在界面上
fn clear_stats(task: &Task) {
    let mut inner = task.inner.lock().unwrap();
    if let Some(stats) = inner.ed2k_stats.as_mut() {
        stats.peers = 0;
        stats.active_peers = 0;
        stats.uploaded_bytes = 0;
    }
}

fn fail(task: &Task, sink: &ProgressSink, message: String) -> Outcome {
    {
        let mut inner = task.inner.lock().unwrap();
        inner.error = Some(message.clone());
        inner.status = TaskStatus::Failed;
    }
    task.zero_speeds();
    clear_stats(task);
    sink(task.snapshot());
    Outcome::Failed(message)
}

/// 把一个 ed2k 任务跟到结束：挂上侧车，按侧车推来的状态刷新任务卡，
/// 取消时让它停下（移除时连侧面里的任务一起删）。
pub async fn run(
    task: Arc<Task>,
    runtime: Arc<Ed2kRuntime>,
    sink: ProgressSink,
    cancel: CancellationToken,
    flush: Option<ProgressFlush>,
) -> Outcome {
    let Some(link) = task.ed2k_link() else {
        return fail(&task, &sink, "这个任务没有 ed2k 链接".to_string());
    };
    let dir = task.inner.lock().unwrap().dir.clone();
    let hash = match parse_link(&link) {
        Ok(parsed) => parsed.hash,
        Err(message) => return fail(&task, &sink, message),
    };

    set_status(&task, TaskStatus::Probing);
    sink(task.snapshot());

    let mounted = tokio::select! {
        mounted = runtime.ensure_transfer(&link, &dir) => mounted,
        _ = cancel.cancelled() => {
            task.zero_speeds();
            clear_stats(&task);
            sink(task.snapshot());
            return Outcome::Cancelled;
        }
    };
    let snapshot = match mounted {
        Ok(snapshot) => snapshot,
        Err(message) => return fail(&task, &sink, message),
    };
    // 侧车才是权威：文件名与总大小以它为准（链接里的大小可能与实际不符）
    task.apply_ed2k_meta(&snapshot.name, snapshot.size.max(0) as u64, &hash);
    sink(task.snapshot());

    let mut missing = 0u32;
    let mut last_flush = Instant::now();
    loop {
        if cancel.is_cancelled() {
            let (removed, delete_file) = {
                let inner = task.inner.lock().unwrap();
                (
                    inner.removed,
                    matches!(inner.stop, Some(StopKind::Cancel { delete_file: true })),
                )
            };
            if removed {
                // 侧车知道这个文件落在哪，删除交给它
                let _ = runtime
                    .call(
                        "remove",
                        Some(json!({ "hash": hash, "deleteFile": delete_file })),
                    )
                    .await;
            } else {
                let _ = runtime
                    .call("pause", Some(json!({ "hash": hash })))
                    .await;
            }
            task.zero_speeds();
            clear_stats(&task);
            sink(task.snapshot());
            if let Some(flush) = &flush {
                flush();
            }
            return Outcome::Cancelled;
        }

        let Some(snapshot) = runtime.snapshot_of(&hash) else {
            missing += 1;
            if missing >= MISSING_TICKS {
                return fail(&task, &sink, "这个任务在 ed2k 侧车里找不到了".to_string());
            }
            tokio::time::sleep(TICK).await;
            continue;
        };
        missing = 0;

        {
            let mut inner = task.inner.lock().unwrap();
            inner.status = snapshot.status();
            inner.error = None;
            inner.total = if snapshot.wanted > 0 {
                snapshot.wanted as u64
            } else {
                inner.total
            };
            inner.downloaded = snapshot.done.max(0) as u64;
        }
        task.set_speed(snapshot.rate_bps);
        task.set_ed2k_stats(Ed2kStats {
            peers: snapshot.peers,
            active_peers: snapshot.active_peers,
            servers: runtime.servers(),
            kad_nodes: runtime.kad_nodes(),
            uploaded_bytes: snapshot.uploaded.max(0) as u64,
        });
        sink(task.snapshot());

        if snapshot.completed() {
            {
                let mut inner = task.inner.lock().unwrap();
                inner.status = TaskStatus::Completed;
                inner.downloaded = inner.total;
            }
            task.zero_speeds();
            clear_stats(&task);
            sink(task.snapshot());
            if let Some(flush) = &flush {
                flush();
            }
            return Outcome::Completed;
        }

        if last_flush.elapsed() >= Duration::from_secs(2) {
            if let Some(flush) = &flush {
                flush();
            }
            last_flush = Instant::now();
        }
        tokio::time::sleep(TICK).await;
    }
}

/// 给界面用的搜索结果
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Ed2kSearchResult {
    pub hash: String,
    pub name: String,
    pub size: u64,
    pub sources: u32,
    pub complete_sources: u32,
    pub link: String,
}

/// 界面只列源最多的这一批：侧车按源数排好了序，一次能搜出上千条，全推给前端没意义
const SEARCH_RESULT_LIMIT: usize = 50;

/// 明显违法的关键词：搜索请求里出现就拒绝，结果名里出现就丢掉。
/// 这是最低限度的一道闸门——公开 eD2k 索引里混着盗版与 CSAM，换个拼写就能绕开，
/// 挡不住有心的人，作用只是不让产品主动把这类东西摆到用户面前。
const BLOCKED_TERMS: &[&str] = &[
    "pthc",
    "pedo",
    "paedo",
    "pedophil",
    "childlover",
    "child porn",
    "childporn",
    "underage",
];

fn blocked_in(text: &str) -> bool {
    let lowered = text.to_lowercase();
    BLOCKED_TERMS.iter().any(|term| lowered.contains(term))
}

/// 关键词自带违规词就不搜了
pub fn search_query_rejected(query: &str) -> bool {
    blocked_in(query)
}

/// 把侧车返回的搜索快照转换成界面要的形状
pub fn search_results(state: &Value) -> Vec<Ed2kSearchResult> {
    let Some(results) = state.get("results").and_then(|value| value.as_array()) else {
        return Vec::new();
    };
    results
        .iter()
        .filter(|item| match item.get("name").and_then(|value| value.as_str()) {
            Some(name) => !blocked_in(name),
            None => false,
        })
        .take(SEARCH_RESULT_LIMIT)
        .filter_map(|item| {
            let hash = item.get("hash")?.as_str()?.to_string();
            let name = item.get("name")?.as_str()?.to_string();
            let link = item.get("link")?.as_str()?.to_string();
            if link.is_empty() {
                return None;
            }
            Some(Ed2kSearchResult {
                hash,
                name,
                size: item.get("size").and_then(|value| value.as_u64()).unwrap_or(0),
                sources: item.get("sources").and_then(|value| value.as_u64()).unwrap_or(0) as u32,
                complete_sources: item
                    .get("completeSources")
                    .and_then(|value| value.as_u64())
                    .unwrap_or(0) as u32,
                link,
            })
        })
        .collect()
}

/// 侧车状态文件放在应用数据目录下，名字固定
pub fn state_path(data_dir: &Path) -> PathBuf {
    data_dir.join("ed2k-state.json")
}

/// 文件名清理：与 BT 用同一套规则（去掉路径分隔符等危险字符）
pub fn safe_file_name(name: &str) -> String {
    sanitize_file_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result(hash: &str, name: &str, sources: u64) -> Value {
        serde_json::json!({
            "hash": hash,
            "name": name,
            "size": 1024,
            "sources": sources,
            "completeSources": 0,
            "link": format!("ed2k://|file|{name}|1024|{hash}|/"),
        })
    }

    #[test]
    fn blocked_terms_are_dropped_from_results() {
        let state = serde_json::json!({ "results": [
            result("AAAA", "ubuntu-24.04.iso", 10),
            result("BBBB", "PTHC collection", 99),
            result("CCCC", "Child Porn vol.2", 5),
            result("DDDD", "underage stuff", 1),
        ]});
        let found = search_results(&state);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "ubuntu-24.04.iso");
    }

    #[test]
    fn filter_runs_before_the_limit() {
        // 被删掉的条目不该占 50 个名额，后面的条目要能补进来
        let mut results: Vec<Value> = (0..SEARCH_RESULT_LIMIT - 1)
            .map(|index| result(&format!("H{index}"), &format!("file-{index}"), 1))
            .collect();
        results.insert(0, result("BLOCKED", "pthc pack", 999));
        results.push(result("TAIL", "tail-file", 1));
        let found = search_results(&serde_json::json!({ "results": results }));
        assert_eq!(found.len(), SEARCH_RESULT_LIMIT);
        assert_eq!(found.last().unwrap().name, "tail-file");
        assert!(found.iter().all(|item| item.name != "pthc pack"));
    }

    #[test]
    fn blocked_query_is_rejected() {
        assert!(search_query_rejected("PTHC 合集"));
        assert!(!search_query_rejected("ubuntu 24.04"));
    }
}
