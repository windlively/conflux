//! 命令行桥接：在本地回环地址上开一个只认临时 token 的 JSON 服务，
//! 让 conflux-cli 把任务交给同一个 DownloadManager（任务会出现在界面里）。
//!
//! 发现文件放在应用数据目录的 `cli.json`（Unix 权限 0600），里面只有端口、
//! token 和进程号；服务默认关闭，由设置里的「命令行工具」开关启动。

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::download::DownloadManager;

const DISCOVERY_FILE: &str = "cli.json";
const MAX_REQUEST_BYTES: u64 = 64 * 1024;
const IO_TIMEOUT: Duration = Duration::from_secs(5);

pub struct CliService {
    manager: Arc<DownloadManager>,
    discovery_path: PathBuf,
    running: Mutex<Option<Running>>,
}

struct Running {
    stop: Arc<AtomicBool>,
}

#[derive(Deserialize)]
struct Request {
    token: String,
    op: String,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    dir: Option<String>,
    #[serde(default)]
    connections: Option<usize>,
    #[serde(default)]
    files: Option<Vec<usize>>,
    #[serde(default)]
    id: Option<String>,
    #[serde(default, rename = "deleteFile")]
    delete_file: bool,
}

impl CliService {
    pub fn new(manager: Arc<DownloadManager>, data_dir: PathBuf) -> Self {
        Self {
            manager,
            discovery_path: data_dir.join(DISCOVERY_FILE),
            running: Mutex::new(None),
        }
    }

    /// 开/关轮询桥接。重复开启是幂等的；关闭时先停线程再删发现文件。
    pub fn set_enabled(&self, enabled: bool) -> Result<(), String> {
        let mut running = self.running.lock().unwrap();
        if !enabled {
            if let Some(service) = running.take() {
                service.stop.store(true, Ordering::Relaxed);
                remove_discovery(&self.discovery_path);
            }
            return Ok(());
        }
        if running.is_some() {
            return Ok(());
        }

        let listener = TcpListener::bind("127.0.0.1:0")
            .map_err(|err| format!("启动命令行服务失败：{err}"))?;
        listener
            .set_nonblocking(true)
            .map_err(|err| format!("启动命令行服务失败：{err}"))?;
        let port = listener
            .local_addr()
            .map_err(|err| format!("启动命令行服务失败：{err}"))?
            .port();
        let token = Uuid::new_v4().to_string();
        write_discovery(&self.discovery_path, port, &token)?;

        let stop = Arc::new(AtomicBool::new(false));
        let manager = self.manager.clone();
        let server_token = token;
        let server_stop = stop.clone();
        // 下载引擎用 tokio::spawn 起任务，桥接线程得先进入 Tauri 的运行时
        let runtime = tauri::async_runtime::handle();
        thread::Builder::new()
            .name("conflux-cli-bridge".to_string())
            .spawn(move || serve(listener, manager, server_token, server_stop, runtime))
            .map_err(|err| format!("启动命令行服务失败：{err}"))?;

        *running = Some(Running { stop });
        println!("命令行服务已启动：127.0.0.1:{port}");
        Ok(())
    }
}

fn serve(
    listener: TcpListener,
    manager: Arc<DownloadManager>,
    token: String,
    stop: Arc<AtomicBool>,
    runtime: tauri::async_runtime::RuntimeHandle,
) {
    while !stop.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, _)) => handle(stream, &manager, &token, &runtime),
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(100));
            }
            Err(err) => {
                eprintln!("命令行服务接受连接失败：{err}");
                break;
            }
        }
    }
}

fn handle(
    mut stream: TcpStream,
    manager: &Arc<DownloadManager>,
    token: &str,
    runtime: &tauri::async_runtime::RuntimeHandle,
) {
    let _ = stream.set_read_timeout(Some(IO_TIMEOUT));
    let _ = stream.set_write_timeout(Some(IO_TIMEOUT));

    let mut line = String::new();
    let read = BufReader::new(&stream)
        .take(MAX_REQUEST_BYTES)
        .read_line(&mut line);
    let response = match read {
        Ok(0) => err("空请求"),
        Ok(_) => match serde_json::from_str::<Request>(line.trim()) {
            Ok(request) => {
                if request.token != token {
                    err("鉴权失败：请重新开启一次命令行工具")
                } else {
                    // 单次请求出问题不该把整个桥接线程带走
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        let _guard = runtime.inner().enter();
                        execute(manager, request)
                    }))
                    .unwrap_or_else(|_| err("命令行服务内部错误"))
                }
            }
            Err(error) => err(format!("请求格式错误：{error}")),
        },
        Err(error) => err(format!("读取请求失败：{error}")),
    };
    let mut payload = response.to_string();
    payload.push('\n');
    let _ = stream.write_all(payload.as_bytes());
    let _ = stream.flush();
}

fn execute(manager: &Arc<DownloadManager>, request: Request) -> Value {
    match request.op.as_str() {
        "info" => ok(json!({
            "app": "conflux",
            "version": env!("CARGO_PKG_VERSION"),
            "pid": std::process::id(),
            "active": manager.list().iter().filter(|task| {
                use crate::download::TaskStatus;
                matches!(
                    task.status,
                    TaskStatus::Queued | TaskStatus::Probing | TaskStatus::Downloading
                )
            }).count(),
        })),
        "add" => {
            let Some(url) = request.url else {
                return err("缺少下载地址");
            };
            match manager.add(url, request.dir, request.connections, request.files) {
                Ok(id) => {
                    let task = manager.list().into_iter().find(|task| task.id == id);
                    ok(json!({ "id": id, "task": task }))
                }
                Err(error) => err(error),
            }
        }
        "list" => ok(json!(manager.list())),
        "get" => {
            let Some(id) = request.id else {
                return err("缺少任务 id");
            };
            match manager.list().into_iter().find(|task| task.id == id) {
                Some(task) => ok(json!(task)),
                None => err("任务不存在"),
            }
        }
        "pause" => run(|| {
            manager.pause(&required_id(&request.id)?)?;
            Ok(Value::Null)
        }),
        "resume" => run(|| {
            manager.resume(&required_id(&request.id)?)?;
            Ok(Value::Null)
        }),
        "remove" => run(|| {
            manager.remove(&required_id(&request.id)?, request.delete_file)?;
            Ok(Value::Null)
        }),
        other => err(format!("未知操作：{other}")),
    }
}

fn required_id(id: &Option<String>) -> Result<String, String> {
    id.clone().filter(|id| !id.trim().is_empty()).ok_or_else(|| "缺少任务 id".to_string())
}

fn run(action: impl FnOnce() -> Result<Value, String>) -> Value {
    match action() {
        Ok(data) => ok(data),
        Err(error) => err(error),
    }
}

fn ok(data: Value) -> Value {
    json!({ "ok": true, "data": data })
}

fn err(message: impl Into<String>) -> Value {
    json!({ "ok": false, "error": message.into() })
}

fn write_discovery(path: &Path, port: u16, token: &str) -> Result<(), String> {
    let payload = json!({
        "port": port,
        "token": token,
        "pid": std::process::id(),
    })
    .to_string();
    std::fs::write(path, payload).map_err(|err| format!("写入命令行服务信息失败：{err}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .map_err(|err| format!("设置命令行服务信息权限失败：{err}"))?;
    }
    Ok(())
}

/// 只删自己写的文件：两个实例同时开着时，别把后来者的发现文件删掉
fn remove_discovery(path: &Path) {
    let ours = std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .and_then(|value| value.get("pid").and_then(Value::as_u64))
        .is_some_and(|pid| pid == u64::from(std::process::id()));
    if ours {
        let _ = std::fs::remove_file(path);
    }
}
