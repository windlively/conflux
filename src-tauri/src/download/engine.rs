use std::fs::{File, OpenOptions};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use reqwest::header::{ACCEPT_ENCODING, CONTENT_RANGE, RANGE};
use reqwest::{Client, StatusCode};
use tokio_util::sync::CancellationToken;

use super::limit::ConnectionGate;
use super::path::{part_path, unique_path, write_all_at};
use super::probe;
use super::task::{
    plan_segment_count, ChannelRuntime, Counters, StopKind, Task, MIN_MULTI_CONNECTION_BYTES,
};
use super::types::*;

pub type ProgressSink = Arc<dyn Fn(TaskView) + Send + Sync>;
/// 周期性落库回调。调用方在调用它之前必须先让临时文件 fsync 完成。
pub type ProgressFlush = Arc<dyn Fn() + Send + Sync>;

const MAX_ATTEMPTS_PER_SEGMENT: u32 = 5;
const RETRY_BASE_MS: u64 = 500;
const TICK_INTERVAL_MS: u64 = 250;
/// 进度落库间隔。崩溃时最多丢失这个时长的进度，重下即可，不会损坏文件。
const FLUSH_INTERVAL: Duration = Duration::from_secs(2);

pub enum Outcome {
    Completed,
    Stopped,
    Cancelled,
    Failed(String),
}

enum FetchError {
    Cancelled,
    Failed(String),
}

fn emit(task: &Task, sink: &ProgressSink) {
    sink(task.snapshot());
}

/// 启动阶段失败时显式落到「失败」状态并通知界面，
/// 否则任务会停留在「解析资源」或「下载中」。
fn abort(task: &Task, sink: &ProgressSink, message: String) -> Outcome {
    task.set_error(message.clone());
    task.inner.lock().unwrap().status = TaskStatus::Failed;
    emit(task, sink);
    Outcome::Failed(message)
}

fn describe_error(err: &reqwest::Error) -> String {
    if err.is_timeout() {
        "请求超时".to_string()
    } else if err.is_connect() {
        "无法建立连接".to_string()
    } else if err.is_body() || err.is_decode() {
        "响应数据中断".to_string()
    } else if err.is_status() {
        format!("服务器返回异常状态：{err}")
    } else {
        format!("网络错误：{err}")
    }
}

fn parse_content_range_start(raw: &str) -> Option<u64> {
    raw.split_whitespace()
        .nth(1)?
        .split('-')
        .next()?
        .parse()
        .ok()
}

async fn sleep_or_cancel(cancel: &CancellationToken, ms: u64) -> bool {
    tokio::select! {
        _ = cancel.cancelled() => true,
        _ = tokio::time::sleep(Duration::from_millis(ms)) => false,
    }
}

/// 下载一个分片；返回 Ok 表示该分片已经完整落盘。
async fn fetch_segment(
    client: &Client,
    task: &Task,
    counters: &Counters,
    idx: usize,
    channel_id: usize,
    cancel: &CancellationToken,
    file: &File,
) -> Result<(), FetchError> {
    let start = counters.seg_starts[idx];
    let len = counters.seg_lens[idx];
    let single = counters.single_stream;

    let mut request = client.get(&task.url).header(ACCEPT_ENCODING, "identity");
    let mut offset;
    if single {
        // 服务器不支持 Range：只能整段重下，进度归零重来
        counters.seg_downloaded[idx].store(0, Ordering::SeqCst);
        offset = 0;
    } else {
        let committed = counters.seg_downloaded[idx].load(Ordering::SeqCst);
        if len > 0 && committed >= len {
            return Ok(());
        }
        let from = start + committed;
        let to = start + len - 1;
        request = request.header(RANGE, format!("bytes={from}-{to}"));
        offset = from;
    }
    let request_offset = offset;

    let response = request.send().await.map_err(|e| FetchError::Failed(describe_error(&e)))?;
    let status = response.status();

    if single {
        if !status.is_success() {
            return Err(FetchError::Failed(format!("服务器返回 HTTP {}", status.as_u16())));
        }
    } else {
        if status != StatusCode::PARTIAL_CONTENT {
            return Err(FetchError::Failed(format!(
                "服务器未按 Range 返回内容（HTTP {}）",
                status.as_u16()
            )));
        }
        if let Some(raw) = response.headers().get(CONTENT_RANGE).and_then(|v| v.to_str().ok()) {
            if let Some(actual) = parse_content_range_start(raw) {
                if actual != request_offset {
                    return Err(FetchError::Failed(format!(
                        "响应区间与请求不一致：期望起点 {request_offset}，实际 {actual}"
                    )));
                }
            }
        }
    }

    let expected_len = response.content_length();
    task.update_channel(channel_id, |c| c.state = ChannelState::Downloading);

    let mut stream = response.bytes_stream();
    let mut completed = false;
    loop {
        let next = tokio::select! {
            _ = cancel.cancelled() => return Err(FetchError::Cancelled),
            item = stream.next() => item,
        };
        let Some(item) = next else { break };
        let chunk = item.map_err(|e| FetchError::Failed(describe_error(&e)))?;
        if chunk.is_empty() {
            continue;
        }
        write_all_at(file, &chunk, offset)
            .map_err(|e| FetchError::Failed(format!("写入文件失败：{e}")))?;
        offset += chunk.len() as u64;
        // 只有整块写完才推进已提交偏移，恢复时从该位置重新请求，
        // 因此半块数据会被完整覆盖而不会污染文件。
        counters.seg_downloaded[idx].store(offset - start, Ordering::SeqCst);
        counters.channel_bytes[channel_id].fetch_add(chunk.len() as u64, Ordering::SeqCst);
        if len > 0 && offset >= start + len {
            completed = true;
            break;
        }
    }

    if let Some(expected) = expected_len {
        if offset - request_offset != expected {
            return Err(FetchError::Failed(format!(
                "响应提前结束：收到 {} 字节，声明 {expected} 字节",
                offset - request_offset
            )));
        }
    }

    if single {
        let done = counters.seg_downloaded[idx].load(Ordering::SeqCst);
        if len == 0 || done >= len {
            counters.stream_completed.store(true, Ordering::SeqCst);
        } else {
            return Err(FetchError::Failed(format!("响应提前结束：已下载 {done} / {len} 字节")));
        }
    } else if !completed {
        let done = counters.seg_downloaded[idx].load(Ordering::SeqCst);
        return Err(FetchError::Failed(format!(
            "分片 {} 未下载完整：{done} / {len} 字节",
            idx + 1
        )));
    }

    Ok(())
}

async fn worker(
    task: Arc<Task>,
    client: Client,
    channel_id: usize,
    counters: Arc<Counters>,
    cancel: CancellationToken,
    file: Arc<File>,
    gate: Arc<ConnectionGate>,
) -> Result<(), String> {
    loop {
        if cancel.is_cancelled() {
            task.update_channel(channel_id, |c| c.state = ChannelState::Stopped);
            return Ok(());
        }
        let next = counters.pending.lock().unwrap().pop_front();
        let Some(idx) = next else {
            // 保留最后领取过的分片：队列空了不代表这一行没干活，
            // 清空会让界面把一个已经下完的分片显示成 0 B
            task.update_channel(channel_id, |c| c.state = ChannelState::Finished);
            return Ok(());
        };

        // 全局连接数上限：领到分片还要等一个空位才真的去连。
        // 凭据活到这次循环结束——分片下完或让回队列时立刻还位，别的任务才有机会。
        let Some(_permit) = gate.acquire(&cancel).await else {
            task.update_channel(channel_id, |c| c.state = ChannelState::Stopped);
            return Ok(());
        };

        task.update_channel(channel_id, |c| {
            c.segment = Some(idx);
            c.state = ChannelState::Connecting;
            c.retries = 0;
        });

        match fetch_segment(&client, &task, &counters, idx, channel_id, &cancel, &file).await {
            Ok(()) => {}
            Err(FetchError::Cancelled) => {
                task.update_channel(channel_id, |c| c.state = ChannelState::Stopped);
                return Ok(());
            }
            Err(FetchError::Failed(msg)) => {
                let attempts = counters.attempts[idx].fetch_add(1, Ordering::SeqCst) + 1;
                task.update_channel(channel_id, |c| {
                    c.state = ChannelState::Retrying;
                    c.retries = attempts;
                    c.last_error = Some(msg.clone());
                });
                if attempts >= MAX_ATTEMPTS_PER_SEGMENT {
                    let message = format!("分片 {} 重试 {attempts} 次后仍失败：{msg}", idx + 1);
                    task.set_error(message.clone());
                    cancel.cancel();
                    return Err(message);
                }
                if sleep_or_cancel(&cancel, RETRY_BASE_MS).await {
                    task.update_channel(channel_id, |c| c.state = ChannelState::Stopped);
                    return Ok(());
                }
                // 放回队首，让任意空闲通道重新领取
                counters.pending.lock().unwrap().push_front(idx);
            }
        }
    }
}

async fn ticker_loop(
    task: Arc<Task>,
    sink: ProgressSink,
    file: Arc<File>,
    flush: Option<ProgressFlush>,
    cancel: CancellationToken,
) {
    let mut interval = tokio::time::interval(Duration::from_millis(TICK_INTERVAL_MS));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut last_flush = Instant::now();
    loop {
        tokio::select! {
            _ = cancel.cancelled() => return,
            _ = interval.tick() => {
                task.refresh_speeds();
                emit(&task, &sink);
                if let Some(flush) = &flush {
                    if last_flush.elapsed() >= FLUSH_INTERVAL {
                        // 顺序不能反：先把数据写进磁盘，再记录已提交偏移。
                        // 否则掉电后库里的进度会超过文件里真实存在的内容，续传就会留空洞。
                        if file.sync_data().is_ok() {
                            flush();
                        }
                        last_flush = Instant::now();
                    }
                }
            }
        }
    }
}

/// 恢复出来的进度必须和临时文件对得上，对不上就丢弃进度重下。
fn part_file_consistent(file_len: u64, counters: &Counters, total: u64) -> bool {
    if counters.single_stream {
        // 单连接模式本来就从 0 重来，只要记录没超过文件实际长度即可
        counters.total_downloaded() <= file_len
    } else {
        total > 0 && file_len == total && counters.total_downloaded() <= total
    }
}

pub async fn run(
    task: Arc<Task>,
    client: Client,
    sink: ProgressSink,
    cancel: CancellationToken,
    flush: Option<ProgressFlush>,
    gate: Arc<ConnectionGate>,
) -> Outcome {
    let needs_probe = {
        let mut inner = task.inner.lock().unwrap();
        if !inner.probed {
            inner.status = TaskStatus::Probing;
            inner.error = None;
            true
        } else {
            false
        }
    };
    emit(&task, &sink);

    if needs_probe {
        match probe::probe(&client, &task.url).await {
            Ok(result) => {
                let mut inner = task.inner.lock().unwrap();
                let probed_total = result.total.unwrap_or(0);
                let previous_total = inner.total;
                let resuming = inner.counters.is_some();

                // 恢复出来的任务会重新探测：远端变了就绝不能接着拼，否则会得到损坏的文件
                let size_changed =
                    previous_total > 0 && probed_total > 0 && probed_total != previous_total;
                let range_lost = inner
                    .counters
                    .as_ref()
                    .map(|counters| !counters.single_stream && !result.supports_range)
                    .unwrap_or(false);
                if resuming && (size_changed || range_lost) {
                    let message = if size_changed {
                        format!(
                            "远端文件大小已变化（原 {previous_total} 字节，现 {probed_total} 字节），已停止续传，请重新下载"
                        )
                    } else {
                        "服务器已不再支持分段下载，无法沿用原有分片，请重新下载".to_string()
                    };
                    drop(inner);
                    return abort(&task, &sink, message);
                }

                if probed_total > 0 {
                    inner.total = probed_total;
                }
                inner.supports_range = result.supports_range;
                // 已有进度时不能改文件名：换了名字就换了 .part 文件，旧进度会全部丢失
                if !resuming {
                    if let Some(name) = result.file_name {
                        if !name.is_empty() {
                            let dir = inner.dir.clone();
                            inner.file_name = name.clone();
                            inner.save_path = unique_path(&dir, &name);
                        }
                    }
                }
                inner.probed = true;
            }
            Err(message) => {
                return abort(&task, &sink, message);
            }
        }
    }

    let counters = {
        let mut inner = task.inner.lock().unwrap();
        if let Some(existing) = inner.counters.clone() {
            existing.reset_pending();
            inner.channels = (0..existing.channel_bytes.len()).map(|_| ChannelRuntime::new()).collect();
            existing
        } else {
            let use_segments = inner.supports_range
                && inner.total >= MIN_MULTI_CONNECTION_BYTES
                && task.connections > 1;
            let counters = if use_segments {
                let channel_count = plan_segment_count(inner.total, task.connections);
                Arc::new(Counters::segmented(inner.total, task.connections, channel_count))
            } else {
                let total = if inner.total > 0 { Some(inner.total) } else { None };
                Arc::new(Counters::single_stream(total, 1))
            };
            inner.channels = (0..counters.channel_bytes.len()).map(|_| ChannelRuntime::new()).collect();
            inner.counters = Some(counters.clone());
            counters
        }
    };

    let part = part_path(&task.inner.lock().unwrap().save_path);
    if let Some(parent) = part.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            return abort(&task, &sink, format!("无法创建下载目录：{e}"));
        }
    }
    // truncate(false)：已有临时文件代表上次的进度，截断等于从头再来
    let file = match OpenOptions::new()
        .create(true)
        .write(true)
        .read(true)
        .truncate(false)
        .open(&part)
    {
        Ok(file) => file,
        Err(e) => {
            return abort(&task, &sink, format!("无法创建临时文件 {}：{e}", part.display()));
        }
    };

    {
        let total = task.inner.lock().unwrap().total;
        let file_len = file.metadata().map(|meta| meta.len()).unwrap_or(0);
        if counters.total_downloaded() > 0 && !part_file_consistent(file_len, &counters, total) {
            eprintln!("临时文件与已记录的进度对不上，丢弃进度重新下载");
            counters.reset_progress();
            let _ = file.set_len(0);
        }
    }

    {
        let mut inner = task.inner.lock().unwrap();
        let total = inner.total;
        let supports_range = inner.supports_range;
        inner.status = TaskStatus::Downloading;
        inner.error = None;
        inner.stop = None;
        inner.last_total_bytes = counters.total_downloaded();
        inner.last_tick = None;
        drop(inner);
        if supports_range && total > 0 {
            if let Err(e) = file.set_len(total) {
                return abort(&task, &sink, format!("无法预分配文件空间：{e}"));
            }
        }
    }

    let file = Arc::new(file);
    emit(&task, &sink);
    // 先把结构（任务行与分片）落库，之后由 ticker 按周期更新进度
    if let Some(flush) = &flush {
        flush();
    }

    let ticker_cancel = CancellationToken::new();
    let ticker = tokio::spawn(ticker_loop(
        task.clone(),
        sink.clone(),
        file.clone(),
        flush.clone(),
        ticker_cancel.clone(),
    ));

    let mut handles = Vec::new();
    for channel_id in 0..counters.channel_bytes.len() {
        handles.push(tokio::spawn(worker(
            task.clone(),
            client.clone(),
            channel_id,
            counters.clone(),
            cancel.clone(),
            file.clone(),
            gate.clone(),
        )));
    }

    let mut worker_error: Option<String> = None;
    for handle in handles {
        match handle.await {
            Ok(Ok(())) => {}
            Ok(Err(message)) => {
                if worker_error.is_none() {
                    worker_error = Some(message);
                }
            }
            Err(join_error) => {
                if worker_error.is_none() {
                    worker_error = Some(format!("下载线程异常终止：{join_error}"));
                }
            }
        }
    }

    ticker_cancel.cancel();
    let _ = ticker.await;

    let outcome = {
        let mut inner = task.inner.lock().unwrap();
        if let Some(err) = inner.error.clone().or(worker_error) {
            inner.status = TaskStatus::Failed;
            inner.error = Some(err.clone());
            Outcome::Failed(err)
        } else if let Some(stop) = inner.stop {
            match stop {
                StopKind::Pause => {
                    inner.status = TaskStatus::Paused;
                    Outcome::Stopped
                }
                StopKind::Cancel { .. } => {
                    inner.status = TaskStatus::Cancelled;
                    Outcome::Cancelled
                }
            }
        } else if counters.is_complete() {
            // 状态留到重命名成功后再置「已完成」，否则界面可能拿到已消失的临时文件路径
            Outcome::Completed
        } else {
            let message = "下载中断：存在未完成的分片".to_string();
            inner.status = TaskStatus::Failed;
            inner.error = Some(message.clone());
            Outcome::Failed(message)
        }
    };

    match &outcome {
        Outcome::Completed => {
            // 先关掉句柄：Windows 上被占用的文件无法重命名
            let _ = file.sync_all();
            drop(file);
            // 下载结束就没有速度可言，否则界面上会停着最后一刻的瞬时速度
            task.zero_speeds();
            let (dir, file_name) = {
                let inner = task.inner.lock().unwrap();
                (inner.dir.clone(), inner.file_name.clone())
            };
            let final_path = unique_path(&dir, &file_name);
            if let Err(e) = std::fs::rename(&part, &final_path) {
                let message = format!("临时文件重命名失败：{e}");
                let mut inner = task.inner.lock().unwrap();
                inner.status = TaskStatus::Failed;
                inner.error = Some(message);
            } else {
                let mut inner = task.inner.lock().unwrap();
                inner.save_path = final_path;
                inner.status = TaskStatus::Completed;
            }
            if let Some(flush) = &flush {
                flush();
            }
        }
        Outcome::Stopped | Outcome::Cancelled | Outcome::Failed(_) => {
            // 停止前把已写入的数据刷盘，再记录进度，保证下次续传的起点是可靠的
            let _ = file.sync_data();
            if let Some(flush) = &flush {
                flush();
            }
            task.zero_speeds();
            let mut inner = task.inner.lock().unwrap();
            for channel in inner.channels.iter_mut() {
                if matches!(channel.state, ChannelState::Connecting | ChannelState::Downloading | ChannelState::Retrying) {
                    channel.state = ChannelState::Stopped;
                }
            }
        }
    }

    emit(&task, &sink);
    outcome
}
