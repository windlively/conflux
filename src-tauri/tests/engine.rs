//! 引擎端到端测试：使用内置的最小 HTTP 服务器验证分段下载、
//! 无 Range 降级以及暂停/继续后的文件正确性。

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use conflux_lib::download::engine::{self, Outcome, ProgressSink};
use conflux_lib::download::manager::{DownloadManager, EventSink, ManagerEvent};
use conflux_lib::download::probe;
use conflux_lib::download::task::Task;
use conflux_lib::download::types::{ChannelState, SegmentState, TaskStatus, TaskView};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::sync::CancellationToken;

/// 每连接限速的最小测试服务器，bytes_per_sec 为 0 表示不限速。
struct TestServer {
    addr: SocketAddr,
}

async fn start_server(body: Vec<u8>, support_range: bool, bytes_per_sec: usize) -> TestServer {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("绑定端口失败");
    let addr = listener.local_addr().unwrap();
    let body = Arc::new(body);
    tokio::spawn(async move {
        loop {
            let Ok((socket, _)) = listener.accept().await else {
                break;
            };
            let body = body.clone();
            tokio::spawn(async move {
                let _ = handle_connection(socket, body, support_range, bytes_per_sec).await;
            });
        }
    });
    TestServer { addr }
}

fn parse_range(line: &str) -> Option<(u64, u64)> {
    let value = line.split_once(':')?.1.trim();
    let spec = value.strip_prefix("bytes=")?;
    let (start, end) = spec.split_once('-')?;
    let start: u64 = start.trim().parse().ok()?;
    let end = if end.trim().is_empty() {
        u64::MAX
    } else {
        end.trim().parse().ok()?
    };
    Some((start, end))
}

async fn handle_connection(
    mut socket: TcpStream,
    body: Arc<Vec<u8>>,
    support_range: bool,
    bytes_per_sec: usize,
) -> std::io::Result<()> {
    let mut buffer = vec![0u8; 16 * 1024];
    let read = socket.read(&mut buffer).await?;
    if read == 0 {
        return Ok(());
    }
    let request = String::from_utf8_lossy(&buffer[..read]).to_string();
    let is_head = request.starts_with("HEAD");
    let total = body.len() as u64;

    let range = request
        .lines()
        .find(|line| line.to_ascii_lowercase().starts_with("range:"))
        .and_then(parse_range);

    let (start, end, partial) = match range {
        Some((start, end)) if support_range => (start, end.min(total.saturating_sub(1)), true),
        _ => (0, total.saturating_sub(1), false),
    };
    let length = end.saturating_sub(start) + 1;

    let header = if partial {
        format!(
            "HTTP/1.1 206 Partial Content\r\ncontent-length: {length}\r\ncontent-range: bytes {start}-{end}/{total}\r\naccept-ranges: bytes\r\ncontent-type: application/octet-stream\r\nconnection: close\r\n\r\n"
        )
    } else {
        format!(
            "HTTP/1.1 200 OK\r\ncontent-length: {total}\r\naccept-ranges: bytes\r\ncontent-type: application/octet-stream\r\nconnection: close\r\n\r\n"
        )
    };
    socket.write_all(header.as_bytes()).await?;

    if !is_head && total > 0 {
        let slice = &body[start as usize..=end as usize];
        for chunk in slice.chunks(32 * 1024) {
            if socket.write_all(chunk).await.is_err() {
                break;
            }
            if bytes_per_sec > 0 {
                let delay = chunk.len() as f64 / bytes_per_sec as f64;
                tokio::time::sleep(Duration::from_secs_f64(delay)).await;
            }
        }
    }
    let _ = socket.shutdown().await;
    Ok(())
}

fn temp_dir(tag: &str) -> PathBuf {
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let dir = std::env::temp_dir().join(format!("conflux-test-{tag}-{nanos}"));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn pattern(len: usize, modulo: usize) -> Vec<u8> {
    (0..len).map(|i| (i % modulo) as u8).collect()
}

fn collect_views() -> (ProgressSink, Arc<Mutex<Vec<TaskView>>>) {
    let views = Arc::new(Mutex::new(Vec::new()));
    let sink: ProgressSink = {
        let views = views.clone();
        Arc::new(move |view: TaskView| views.lock().unwrap().push(view))
    };
    (sink, views)
}

fn noop_sink() -> EventSink {
    Arc::new(|_event: ManagerEvent| {})
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn segmented_download_produces_correct_file_and_channel_progress() {
    let size = 24 * 1024 * 1024usize;
    let body = pattern(size, 251);
    // 限速到每连接 2 MiB/s：本机回环太快，不限速时下载会在一次进度采样内结束，
    // 无法观察到多通道并行与中途进度。
    let server = start_server(body.clone(), true, 2 * 1024 * 1024).await;
    let dir = temp_dir("segmented");

    let task = Arc::new(Task::new(
        "t1".to_string(),
        format!("http://{}/test.bin", server.addr),
        dir.clone(),
        4,
    ));
    let (sink, views) = collect_views();
    let outcome = engine::run(task.clone(), probe::client(), sink, CancellationToken::new()).await;
    assert!(matches!(outcome, Outcome::Completed), "任务应完成");

    let snapshot = task.snapshot();
    assert!(snapshot.supports_range, "应判定服务器支持分段");
    assert_eq!(snapshot.channels.len(), 4, "应使用 4 个下载通道");
    assert!(snapshot.segments.len() > 1, "应划分出多个分片");
    assert_eq!(snapshot.total_bytes, size as u64);
    assert_eq!(snapshot.downloaded_bytes, size as u64, "总进度应等于文件大小");
    assert_eq!(snapshot.status, TaskStatus::Completed);

    let written = std::fs::read(&snapshot.save_path).expect("最终文件应存在");
    assert_eq!(written.len(), size);
    assert_eq!(written, body, "落盘内容必须与源文件一致");
    assert!(!dir.join("test.bin.part").exists(), "临时文件应已改名");
    assert!(
        snapshot
            .segments
            .iter()
            .all(|segment| segment.state == SegmentState::Completed),
        "完成后所有分片都应标记为已完成"
    );

    let views = views.lock().unwrap();

    // 并发确实发生在多个通道上
    assert!(
        views
            .iter()
            .any(|view| view
                .channels
                .iter()
                .filter(|channel| channel.state == ChannelState::Downloading)
                .count()
                >= 2),
        "应观察到多个通道同时处于下载中"
    );

    // 每个分片都被某个通道真实下载过
    let mut seen: Vec<usize> = views
        .iter()
        .flat_map(|view| view.channels.iter().filter_map(|channel| channel.segment_index))
        .collect();
    seen.sort_unstable();
    seen.dedup();
    assert_eq!(
        seen,
        (0..snapshot.segments.len()).collect::<Vec<_>>(),
        "每个分片都应被通道领取并下载"
    );

    // 必须有多个通道真的承担了下载工作，而不是名义上开了多连接
    let workers: std::collections::BTreeSet<usize> = views
        .iter()
        .flat_map(|view| {
            view.channels
                .iter()
                .filter(|channel| channel.segment_index.is_some())
                .map(|channel| channel.id)
        })
        .collect();
    assert!(
        workers.len() >= 2,
        "应有至少两个通道实际参与下载，实际 {workers:?}"
    );

    // 通道视图必须带自己的分片进度与速度，而不是由总进度推算
    assert!(
        views.iter().any(|view| view.channels.iter().any(|channel| {
            channel.segment_downloaded > 0
                && channel.segment_total > 0
                && channel.segment_downloaded <= channel.segment_total
        })),
        "应上报通道级分片进度"
    );
    assert!(
        views
            .iter()
            .any(|view| view.channels.iter().any(|channel| channel.speed_bps > 0)),
        "应上报通道速度"
    );
    assert!(
        views
            .iter()
            .any(|view| view.segments.iter().any(|segment| segment.downloaded > 0)),
        "应上报分片地图数据"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn falls_back_to_single_connection_when_server_ignores_range() {
    let size = 8 * 1024 * 1024usize;
    let body = pattern(size, 7);
    let server = start_server(body.clone(), false, 0).await;
    let dir = temp_dir("fallback");

    let task = Arc::new(Task::new(
        "t2".to_string(),
        format!("http://{}/test.bin", server.addr),
        dir,
        4,
    ));
    let (sink, _views) = collect_views();
    let outcome = engine::run(task.clone(), probe::client(), sink, CancellationToken::new()).await;
    assert!(matches!(outcome, Outcome::Completed));

    let snapshot = task.snapshot();
    assert!(!snapshot.supports_range, "服务器忽略 Range 时应判定为不支持分段");
    assert_eq!(snapshot.channels.len(), 1, "不支持分段时应退化为单连接");
    assert!(snapshot.segments.is_empty(), "单连接模式不产生分片地图");
    assert_eq!(snapshot.downloaded_bytes, size as u64);

    let written = std::fs::read(&snapshot.save_path).expect("最终文件应存在");
    assert_eq!(written, body, "降级模式的落盘内容也必须一致");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pause_then_resume_keeps_file_correct() {
    let size = 16 * 1024 * 1024usize;
    let body = pattern(size, 97);
    // 每连接 1 MiB/s，四个分片约 4 MiB/s，留出足够的暂停窗口
    let server = start_server(body.clone(), true, 1024 * 1024).await;
    let dir = temp_dir("resume");

    let sink = noop_sink();
    let manager = DownloadManager::new(sink, dir.clone());
    let id = manager
        .add(format!("http://{}/test.bin", server.addr), Some(dir.to_string_lossy().to_string()), Some(4))
        .expect("添加任务应成功");

    tokio::time::sleep(Duration::from_millis(1500)).await;
    manager.pause(&id).expect("暂停应成功");
    wait_until_stopped(&manager, &id).await;

    let paused = manager
        .list()
        .into_iter()
        .find(|task| task.id == id)
        .expect("任务应存在");
    assert_eq!(paused.status, TaskStatus::Paused);
    assert!(
        paused.downloaded_bytes > 0 && paused.downloaded_bytes < size as u64,
        "暂停时应已完成部分分片，实际 {}",
        paused.downloaded_bytes
    );
    assert_eq!(paused.speed_bps, 0, "暂停后速度应归零");

    manager.resume(&id).expect("继续应成功");
    let done = wait_for(&manager, &id, TaskStatus::Completed).await;

    let written = std::fs::read(&done.save_path).expect("最终文件应存在");
    assert_eq!(written.len(), size);
    assert_eq!(written, body, "续传后的文件必须与源文件一致");

    manager.remove(&id, true).ok();
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn resume_while_pausing_is_not_lost() {
    let size = 16 * 1024 * 1024usize;
    let body = pattern(size, 131);
    let server = start_server(body.clone(), true, 1024 * 1024).await;
    let dir = temp_dir("resume-race");

    let sink = noop_sink();
    let manager = DownloadManager::new(sink, dir.clone());
    let id = manager
        .add(format!("http://{}/test.bin", server.addr), Some(dir.to_string_lossy().to_string()), Some(4))
        .expect("添加任务应成功");

    tokio::time::sleep(Duration::from_millis(1200)).await;
    manager.pause(&id).expect("暂停应成功");
    // 暂停还没收尾就立刻点继续，任务不应被遗忘
    manager.resume(&id).expect("继续应成功");

    let done = wait_for(&manager, &id, TaskStatus::Completed).await;
    let written = std::fs::read(&done.save_path).expect("最终文件应存在");
    assert_eq!(written, body, "竞态场景下文件仍须完整");

    manager.remove(&id, true).ok();
    std::fs::remove_dir_all(&dir).ok();
}

async fn wait_until_stopped(manager: &Arc<DownloadManager>, id: &str) {
    for _ in 0..200 {
        if !manager.is_running(id) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("等待暂停收尾超时");
}

async fn wait_for(manager: &Arc<DownloadManager>, id: &str, expected: TaskStatus) -> TaskView {
    for _ in 0..300 {
        let view = manager
            .list()
            .into_iter()
            .find(|task| task.id == id)
            .expect("任务应存在");
        if view.status == expected {
            return view;
        }
        assert!(
            matches!(view.status, TaskStatus::Downloading | TaskStatus::Paused | TaskStatus::Completed | TaskStatus::Probing | TaskStatus::Queued),
            "任务进入意外状态 {:?}：{}",
            view.status,
            view.error.clone().unwrap_or_default()
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("等待状态 {expected:?} 超时");
}
