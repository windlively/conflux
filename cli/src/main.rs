//! conflux-cli：川流 Conflux 的命令行客户端。
//!
//! 它不自己下载，而是通过应用内的本地桥接服务，把任务交给正在运行的川流；
//! 任务会出现在界面里，和 GUI 共用队列、连接数上限与断点续传。

use std::env;
use std::fs;
use std::io::{self, BufRead, BufReader, IsTerminal, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::process::ExitCode;
use std::thread::sleep;
use std::time::Duration;

use serde_json::{json, Value};

const USAGE: &str = r#"conflux-cli —— 川流 Conflux 命令行客户端

用法：
  conflux-cli add <链接|种子文件> [--dir 目录] [-c 连接数] [--files 序号,...] [--wait]
  conflux-cli list
  conflux-cli status <任务 id>
  conflux-cli wait <任务 id>
  conflux-cli pause|resume <任务 id>
  conflux-cli rm <任务 id> [--delete-file]
  conflux-cli info

说明：
  支持 http(s)、magnet:、ed2k:// 链接和本地 .torrent 种子文件。
  需要川流正在运行，并在「设置 → 系统」里开启「命令行工具」。
  --json 输出机器可读的 JSON；wait 在任务完成时退出码为 0，失败/取消为 1。
"#;

struct Endpoint {
    port: u16,
    token: String,
}

struct Options {
    command: String,
    positional: Vec<String>,
    dir: Option<String>,
    connections: Option<usize>,
    files: Option<Vec<usize>>,
    wait: bool,
    delete_file: bool,
    json: bool,
}

fn main() -> ExitCode {
    let options = match parse_args() {
        Ok(Some(options)) => options,
        Ok(None) => return ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::from(2);
        }
    };

    match run(&options) {
        Ok(code) => ExitCode::from(code),
        Err(message) => {
            eprintln!("错误：{message}");
            ExitCode::from(2)
        }
    }
}

fn parse_args() -> Result<Option<Options>, String> {
    let mut args = env::args().skip(1);
    let Some(command) = args.next() else {
        print!("{USAGE}");
        return Ok(None);
    };
    if matches!(command.as_str(), "-h" | "--help") {
        print!("{USAGE}");
        return Ok(None);
    }
    if matches!(command.as_str(), "-V" | "--version") {
        println!("conflux-cli {}", env!("CARGO_PKG_VERSION"));
        return Ok(None);
    }

    let mut options = Options {
        command,
        positional: Vec::new(),
        dir: None,
        connections: None,
        files: None,
        wait: false,
        delete_file: false,
        json: false,
    };

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--json" => options.json = true,
            "--wait" => options.wait = true,
            "--delete-file" => options.delete_file = true,
            "--dir" => {
                options.dir = Some(next_value(&mut args, "--dir")?.to_string());
            }
            "-c" | "--connections" => {
                let value = next_value(&mut args, "--connections")?;
                let connections = value
                    .parse::<usize>()
                    .map_err(|_| format!("连接数不是数字：{value}"))?;
                if connections == 0 {
                    return Err("连接数至少为 1".to_string());
                }
                options.connections = Some(connections);
            }
            "--files" => {
                let value = next_value(&mut args, "--files")?;
                let files = value
                    .split(',')
                    .map(|item| item.trim().parse::<usize>())
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|_| format!("文件序号格式不对：{value}"))?;
                options.files = Some(files);
            }
            other if other.starts_with('-') => {
                return Err(format!("未知参数：{other}\n\n{USAGE}"));
            }
            other => options.positional.push(other.to_string()),
        }
    }

    Ok(Some(options))
}

fn next_value(args: &mut impl Iterator<Item = String>, flag: &str) -> Result<String, String> {
    args.next().ok_or_else(|| format!("{flag} 缺少值"))
}

fn run(options: &Options) -> Result<u8, String> {
    match options.command.as_str() {
        "info" => command_info(options),
        "list" | "ls" => command_list(options),
        "status" | "show" => command_status(options),
        "wait" => command_wait(options),
        "add" | "download" => command_add(options),
        "pause" => command_simple(options, "pause", "已暂停"),
        "resume" => command_simple(options, "resume", "已继续"),
        "rm" | "remove" => command_remove(options),
        other => {
            eprintln!("未知命令：{other}\n");
            print!("{USAGE}");
            Ok(2)
        }
    }
}

fn endpoint() -> Result<Endpoint, String> {
    let path = endpoint_path();
    let raw = fs::read_to_string(&path).map_err(|_| {
        format!(
            "找不到命令行服务（{}）。\n请先启动川流，并在「设置 → 系统」里开启「命令行工具」。",
            path.display()
        )
    })?;
    let value: Value =
        serde_json::from_str(&raw).map_err(|error| format!("命令行服务信息损坏：{error}"))?;
    let port = value
        .get("port")
        .and_then(Value::as_u64)
        .ok_or_else(|| "命令行服务信息缺少端口".to_string())? as u16;
    let token = value
        .get("token")
        .and_then(Value::as_str)
        .ok_or_else(|| "命令行服务信息缺少 token".to_string())?
        .to_string();
    Ok(Endpoint { port, token })
}

fn call(endpoint: &Endpoint, op: &str, extra: Value) -> Result<Value, String> {
    let mut request = json!({ "token": endpoint.token, "op": op });
    if let (Some(target), Some(extra)) = (request.as_object_mut(), extra.as_object()) {
        target.extend(extra.clone());
    }

    let mut stream = TcpStream::connect(("127.0.0.1", endpoint.port)).map_err(|_| {
        format!(
            "连不上川流（127.0.0.1:{}）。应用可能已退出，或在设置里关闭了「命令行工具」。",
            endpoint.port
        )
    })?;
    let timeout = Some(Duration::from_secs(10));
    let _ = stream.set_read_timeout(timeout);
    let _ = stream.set_write_timeout(timeout);

    let mut payload = request.to_string();
    payload.push('\n');
    stream
        .write_all(payload.as_bytes())
        .map_err(|error| format!("发送请求失败：{error}"))?;

    let mut line = String::new();
    BufReader::new(stream)
        .read_line(&mut line)
        .map_err(|error| format!("读取响应失败：{error}"))?;
    let response: Value = serde_json::from_str(line.trim())
        .map_err(|error| format!("命令行服务返回了无法解析的响应：{error}"))?;
    if response.get("ok").and_then(Value::as_bool) == Some(true) {
        Ok(response.get("data").cloned().unwrap_or(Value::Null))
    } else {
        Err(response
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("未知错误")
            .to_string())
    }
}

fn command_info(options: &Options) -> Result<u8, String> {
    let info = call(&endpoint()?, "info", Value::Null)?;
    if options.json {
        print_json(&info);
    } else {
        println!(
            "川流 {} · PID {} · {} 个任务进行中",
            text(&info["version"]),
            info["pid"].as_u64().unwrap_or(0),
            info["active"].as_u64().unwrap_or(0)
        );
    }
    Ok(0)
}

fn command_list(options: &Options) -> Result<u8, String> {
    let tasks = call(&endpoint()?, "list", Value::Null)?;
    let tasks = tasks.as_array().cloned().unwrap_or_default();
    if options.json {
        print_json(&Value::Array(tasks));
        return Ok(0);
    }
    if tasks.is_empty() {
        println!("没有任务");
        return Ok(0);
    }
    for task in &tasks {
        println!(
            "{}  {}  {}  {}  {}",
            short_id(task),
            pad_status(task),
            percent_label(task),
            speed_label(task),
            text(&task["fileName"]),
        );
    }
    Ok(0)
}

fn command_status(options: &Options) -> Result<u8, String> {
    let id = one_positional(options, "status")?;
    let task = call(&endpoint()?, "get", json!({ "id": id }))?;
    if options.json {
        print_json(&task);
    } else {
        print_task(&task);
    }
    Ok(0)
}

fn command_add(options: &Options) -> Result<u8, String> {
    let url = one_positional(options, "add")?;
    let mut extra = json!({ "url": url });
    if let Some(dir) = &options.dir {
        extra["dir"] = json!(dir);
    }
    if let Some(connections) = options.connections {
        extra["connections"] = json!(connections);
    }
    if let Some(files) = &options.files {
        extra["files"] = json!(files);
    }

    let data = call(&endpoint()?, "add", extra)?;
    let id = data
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| "命令行服务没有返回任务 id".to_string())?
        .to_string();
    let task = data.get("task").cloned().unwrap_or(Value::Null);

    if options.json && !options.wait {
        print_json(&data);
        return Ok(0);
    }
    if !options.json {
        println!("已加入队列：{}  {}", text(&task["fileName"]), id);
    }

    if options.wait {
        wait_for(&id, options)
    } else {
        Ok(0)
    }
}

fn command_wait(options: &Options) -> Result<u8, String> {
    let id = one_positional(options, "wait")?;
    wait_for(id, options)
}

fn command_simple(options: &Options, op: &str, done: &str) -> Result<u8, String> {
    let id = one_positional(options, op)?;
    let extra = json!({ "id": id });
    call(&endpoint()?, op, extra)?;
    if options.json {
        print_json(&json!({ "ok": true }));
    } else {
        println!("{done}：{id}");
    }
    Ok(0)
}

fn command_remove(options: &Options) -> Result<u8, String> {
    let id = one_positional(options, "rm")?;
    call(
        &endpoint()?,
        "remove",
        json!({ "id": id, "deleteFile": options.delete_file }),
    )?;
    if options.json {
        print_json(&json!({ "ok": true, "id": id, "deleteFile": options.delete_file }));
    } else if options.delete_file {
        println!("已移除并删除文件：{id}");
    } else {
        println!("已移除：{id}");
    }
    Ok(0)
}

fn wait_for(id: &str, options: &Options) -> Result<u8, String> {
    let endpoint = endpoint()?;
    let interactive = io::stderr().is_terminal() && !options.json;
    let mut last_status = String::new();
    loop {
        let task = call(&endpoint, "get", json!({ "id": id }))?;
        let status = text(&task["status"]).to_string();
        if interactive {
            eprint!(
                "\r{}  {}  {}  {}   ",
                text(&task["fileName"]),
                status_text(&status),
                percent_label(&task),
                speed_label(&task),
            );
            let _ = io::stderr().flush();
        } else if status != last_status && !options.json {
            if !last_status.is_empty() {
                eprintln!();
            }
            eprintln!(
                "{}  {}  {}",
                text(&task["fileName"]),
                status_text(&status),
                percent_label(&task)
            );
        }
        last_status = status.clone();

        match status.as_str() {
            "completed" => {
                if interactive {
                    eprintln!();
                }
                if options.json {
                    print_json(&task);
                } else {
                    println!("已完成：{}", text(&task["savePath"]));
                }
                return Ok(0);
            }
            "failed" | "cancelled" => {
                if interactive {
                    eprintln!();
                }
                if options.json {
                    print_json(&task);
                } else {
                    let error = text(&task["error"]);
                    println!("任务{}：{}", status_text(&status), error);
                }
                return Ok(1);
            }
            _ => sleep(Duration::from_millis(500)),
        }
    }
}

fn one_positional<'a>(options: &'a Options, command: &str) -> Result<&'a str, String> {
    match options.positional.as_slice() {
        [value] => Ok(value),
        [] => Err(format!("{command} 缺少参数\n\n{USAGE}")),
        _ => Err(format!("{command} 只接受一个参数\n\n{USAGE}")),
    }
}

fn print_task(task: &Value) {
    println!("{}", text(&task["fileName"]));
    println!("  id      {}", text(&task["id"]));
    println!("  状态    {}", status_text(text(&task["status"])));
    println!("  进度    {}", progress_label(task));
    println!("  速度    {}", speed_label(task));
    println!("  保存到  {}", text(&task["savePath"]));
    let error = text(&task["error"]);
    if !error.is_empty() {
        println!("  错误    {error}");
    }
}

fn print_json(value: &Value) {
    match serde_json::to_string(value) {
        Ok(text) => println!("{text}"),
        Err(error) => eprintln!("输出 JSON 失败：{error}"),
    }
}

fn short_id(task: &Value) -> String {
    let id = text(&task["id"]);
    id.chars().take(8).collect()
}

fn percent(task: &Value) -> Option<f64> {
    let downloaded = task["downloadedBytes"].as_u64()? as f64;
    let total = task["totalBytes"].as_u64()? as f64;
    (total > 0.0).then(|| downloaded / total * 100.0)
}

fn percent_label(task: &Value) -> String {
    match percent(task) {
        Some(value) => format!("{value:5.1}%"),
        None => "  --  ".to_string(),
    }
}

fn progress_label(task: &Value) -> String {
    let downloaded = bytes(task["downloadedBytes"].as_u64().unwrap_or(0) as f64);
    match task["totalBytes"].as_u64().filter(|total| *total > 0) {
        Some(total) => format!(
            "{downloaded} / {}（{}）",
            bytes(total as f64),
            percent_label(task).trim()
        ),
        None => downloaded,
    }
}

fn speed_label(task: &Value) -> String {
    format!("{}/s", bytes(task["speedBps"].as_u64().unwrap_or(0) as f64))
}

fn bytes(value: f64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut size = value.max(0.0);
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{size:.0} {}", UNITS[unit])
    } else {
        format!("{size:.1} {}", UNITS[unit])
    }
}

fn pad_status(task: &Value) -> String {
    format!("{:<4}", status_text(text(&task["status"])))
}

fn status_text(status: &str) -> &str {
    match status {
        "queued" => "排队中",
        "probing" => "解析中",
        "downloading" => "下载中",
        "paused" => "已暂停",
        "completed" => "已完成",
        "failed" => "失败",
        "cancelled" => "已取消",
        other => other,
    }
}

fn text(value: &Value) -> &str {
    value.as_str().unwrap_or("")
}

fn endpoint_path() -> PathBuf {
    if let Some(path) = env::var_os("CONFLUX_CLI_FILE") {
        return PathBuf::from(path);
    }
    #[cfg(target_os = "macos")]
    {
        home().join("Library/Application Support/com.conflux.app/cli.json")
    }
    #[cfg(target_os = "windows")]
    {
        env::var_os("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_default()
            .join("com.conflux.app")
            .join("cli.json")
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let base = env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home().join(".local/share"));
        base.join("com.conflux.app").join("cli.json")
    }
}

#[cfg(unix)]
fn home() -> PathBuf {
    env::var_os("HOME").map(PathBuf::from).unwrap_or_default()
}
