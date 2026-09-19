use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Instant;

use tokio_util::sync::CancellationToken;

use super::path::{file_name_from_url, part_path, unique_path};
use super::types::*;

/// 小于该体积直接单连接，分段调度的收益低于额外开销
pub const MIN_MULTI_CONNECTION_BYTES: u64 = 16 * 1024 * 1024;
/// 单个分片的下限，避免出现大量极短连接
pub const MIN_SEGMENT_BYTES: u64 = 4 * 1024 * 1024;

pub fn plan_segment_count(total: u64, connections: usize) -> usize {
    let connections = connections.max(1);
    if connections == 1 || total < MIN_MULTI_CONNECTION_BYTES {
        return 1;
    }
    let by_size = (total / MIN_SEGMENT_BYTES).max(1) as usize;
    connections.min(by_size).max(1)
}

/// 分片下载的共享计数器。热路径（每收到一块数据）只做原子加，
/// 界面快照所需的通道状态放在 Task 的互斥量里，两者互不阻塞。
pub struct Counters {
    pub seg_starts: Vec<u64>,
    pub seg_lens: Vec<u64>,
    pub seg_downloaded: Vec<AtomicU64>,
    pub channel_bytes: Vec<AtomicU64>,
    pub attempts: Vec<AtomicU32>,
    pub pending: Mutex<VecDeque<usize>>,
    pub single_stream: bool,
    pub stream_completed: AtomicBool,
}

impl Counters {
    fn empty(seg_count: usize, channel_count: usize, single_stream: bool) -> Self {
        Counters {
            seg_starts: Vec::with_capacity(seg_count),
            seg_lens: Vec::with_capacity(seg_count),
            seg_downloaded: (0..seg_count).map(|_| AtomicU64::new(0)).collect(),
            channel_bytes: (0..channel_count).map(|_| AtomicU64::new(0)).collect(),
            attempts: (0..seg_count).map(|_| AtomicU32::new(0)).collect(),
            pending: Mutex::new(VecDeque::new()),
            single_stream,
            stream_completed: AtomicBool::new(false),
        }
    }

    pub fn segmented(total: u64, connections: usize, channel_count: usize) -> Self {
        let seg_count = plan_segment_count(total, connections);
        let base = total.div_ceil(seg_count as u64);
        let mut counters = Self::empty(seg_count, channel_count, false);
        let mut start = 0u64;
        for i in 0..seg_count {
            let remaining = total - start;
            let len = if i + 1 == seg_count { remaining } else { base.min(remaining) };
            counters.seg_starts.push(start);
            counters.seg_lens.push(len);
            start += len;
        }
        counters.reset_pending();
        counters
    }

    /// 无 Range 支持时的降级模式：单通道顺序下载，失败只能从头重来。
    pub fn single_stream(total: Option<u64>, channel_count: usize) -> Self {
        let mut counters = Self::empty(1, channel_count, true);
        counters.seg_starts.push(0);
        counters.seg_lens.push(total.unwrap_or(0));
        counters.reset_pending();
        counters
    }

    /// 从数据库恢复：分片边界与已提交偏移都来自落库的数据，
    /// 不再重新划分，否则偏移会与临时文件里的内容对不上。
    pub fn restored(
        seg_starts: Vec<u64>,
        seg_lens: Vec<u64>,
        seg_downloaded: Vec<u64>,
        single_stream: bool,
        channel_count: usize,
    ) -> Self {
        let seg_count = seg_lens.len();
        debug_assert_eq!(seg_starts.len(), seg_count);
        debug_assert_eq!(seg_downloaded.len(), seg_count);
        let counters = Counters {
            seg_starts,
            seg_lens,
            seg_downloaded: seg_downloaded.into_iter().map(AtomicU64::new).collect(),
            channel_bytes: (0..channel_count).map(|_| AtomicU64::new(0)).collect(),
            attempts: (0..seg_count).map(|_| AtomicU32::new(0)).collect(),
            pending: Mutex::new(VecDeque::new()),
            single_stream,
            stream_completed: AtomicBool::new(false),
        };
        counters.reset_pending();
        counters
    }

    pub fn seg_count(&self) -> usize {
        self.seg_lens.len()
    }

    pub fn reset_pending(&self) {
        let mut pending = self.pending.lock().unwrap();
        pending.clear();
        for i in 0..self.seg_lens.len() {
            if self.seg_downloaded[i].load(Ordering::SeqCst) < self.seg_lens[i] {
                pending.push_back(i);
            }
        }
        if self.single_stream
            && !self.stream_completed.load(Ordering::SeqCst)
            && self.seg_lens.first().map(|len| *len == 0).unwrap_or(false)
        {
            pending.clear();
            pending.push_back(0);
        }
    }

    /// 丢弃全部已提交进度（临时文件与记录对不上时使用），随后必须清空临时文件。
    pub fn reset_progress(&self) {
        for counter in &self.seg_downloaded {
            counter.store(0, Ordering::SeqCst);
        }
        self.stream_completed.store(false, Ordering::SeqCst);
        self.reset_pending();
    }

    pub fn total_downloaded(&self) -> u64 {
        self.seg_downloaded
            .iter()
            .map(|d| d.load(Ordering::Relaxed))
            .sum()
    }

    pub fn is_complete(&self) -> bool {
        if self.single_stream {
            if self.stream_completed.load(Ordering::SeqCst) {
                return true;
            }
            let len = self.seg_lens.first().copied().unwrap_or(0);
            return len > 0 && self.seg_downloaded[0].load(Ordering::SeqCst) >= len;
        }
        (0..self.seg_lens.len()).all(|i| self.seg_downloaded[i].load(Ordering::SeqCst) >= self.seg_lens[i])
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopKind {
    Pause,
    Cancel { delete_file: bool },
}

pub struct ChannelRuntime {
    pub state: ChannelState,
    pub segment: Option<usize>,
    pub retries: u32,
    pub last_error: Option<String>,
    pub speed_bps: u64,
    pub last_bytes: u64,
}

impl ChannelRuntime {
    pub fn new() -> Self {
        ChannelRuntime {
            state: ChannelState::Idle,
            segment: None,
            retries: 0,
            last_error: None,
            speed_bps: 0,
            last_bytes: 0,
        }
    }
}

impl Default for ChannelRuntime {
    fn default() -> Self {
        Self::new()
    }
}

pub struct TaskInner {
    pub dir: PathBuf,
    pub file_name: String,
    pub save_path: PathBuf,
    pub status: TaskStatus,
    pub error: Option<String>,
    pub total: u64,
    pub supports_range: bool,
    pub probed: bool,
    pub counters: Option<std::sync::Arc<Counters>>,
    pub channels: Vec<ChannelRuntime>,
    pub cancel: Option<CancellationToken>,
    pub stop: Option<StopKind>,
    pub running: bool,
    /// 用户在暂停尚未收尾时点了继续：等本轮结束后自动重启
    pub resume_pending: bool,
    /// 用户已请求移除：下载线程退出后再从列表中删除
    pub removed: bool,
    pub speed_bps: u64,
    pub last_total_bytes: u64,
    pub last_tick: Option<Instant>,
    pub created_at: i64,
}

pub fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or(0)
}

pub struct Task {
    pub id: String,
    pub url: String,
    pub connections: usize,
    pub inner: Mutex<TaskInner>,
}

impl Task {
    pub fn new(id: String, url: String, dir: PathBuf, connections: usize) -> Self {
        let connections = connections.clamp(1, 16);
        let file_name = file_name_from_url(&url).unwrap_or_else(|| "download.bin".to_string());
        let save_path = unique_path(&dir, &file_name);
        Task {
            id,
            url,
            connections,
            inner: Mutex::new(TaskInner {
                dir,
                file_name,
                save_path,
                status: TaskStatus::Queued,
                error: None,
                total: 0,
                supports_range: false,
                probed: false,
                counters: None,
                channels: Vec::new(),
                cancel: None,
                stop: None,
                running: false,
                resume_pending: false,
                removed: false,
                speed_bps: 0,
                last_total_bytes: 0,
                last_tick: None,
                created_at: now_millis(),
            }),
        }
    }

    pub fn part_path(&self) -> PathBuf {
        let inner = self.inner.lock().unwrap();
        part_path(&inner.save_path)
    }

    pub fn update_channel<F: FnOnce(&mut ChannelRuntime)>(&self, id: usize, f: F) {
        let mut inner = self.inner.lock().unwrap();
        if let Some(channel) = inner.channels.get_mut(id) {
            f(channel);
        }
    }

    pub fn is_running(&self) -> bool {
        self.inner.lock().unwrap().running
    }

    /// 记录首个错误；后续错误不覆盖，避免重试噪声掩盖根因。
    pub fn set_error(&self, message: String) {
        let mut inner = self.inner.lock().unwrap();
        if inner.error.is_none() {
            inner.error = Some(message);
        }
    }

    /// 依据通道累计字节数换算速度；用 EMA 平滑，避免界面数字剧烈跳动。
    pub fn refresh_speeds(&self) {
        let now = Instant::now();
        let mut inner = self.inner.lock().unwrap();
        let elapsed = match inner.last_tick {
            Some(last) => now.duration_since(last).as_secs_f64(),
            None => 0.0,
        };
        inner.last_tick = Some(now);
        if elapsed <= 0.0 {
            return;
        }
        let Some(counters) = inner.counters.clone() else {
            return;
        };

        for (id, channel) in inner.channels.iter_mut().enumerate() {
            let bytes = counters.channel_bytes[id].load(Ordering::Relaxed);
            let delta = bytes.saturating_sub(channel.last_bytes);
            channel.last_bytes = bytes;
            let instant = delta as f64 / elapsed;
            channel.speed_bps = if channel.speed_bps == 0 {
                instant as u64
            } else {
                (channel.speed_bps as f64 * 0.6 + instant * 0.4) as u64
            };
        }

        let total = counters.total_downloaded();
        let delta = total.saturating_sub(inner.last_total_bytes);
        inner.last_total_bytes = total;
        let instant = delta as f64 / elapsed;
        inner.speed_bps = if inner.speed_bps == 0 {
            instant as u64
        } else {
            (inner.speed_bps as f64 * 0.6 + instant * 0.4) as u64
        };
    }

    pub fn zero_speeds(&self) {
        let mut inner = self.inner.lock().unwrap();
        inner.speed_bps = 0;
        for channel in inner.channels.iter_mut() {
            channel.speed_bps = 0;
        }
    }

    pub fn snapshot(&self) -> TaskView {
        let inner = self.inner.lock().unwrap();
        let mut channels: Vec<ChannelView> = Vec::with_capacity(inner.channels.len());
        let mut segments: Vec<SegmentView> = Vec::new();
        let mut downloaded = 0u64;

        if let Some(counters) = &inner.counters {
            if !counters.single_stream {
                let active: Vec<usize> = inner.channels.iter().filter_map(|c| c.segment).collect();
                for i in 0..counters.seg_lens.len() {
                    let len = counters.seg_lens[i];
                    let done = counters.seg_downloaded[i].load(Ordering::Relaxed).min(len);
                    downloaded += done;
                    let state = if done >= len {
                        SegmentState::Completed
                    } else if active.contains(&i) {
                        SegmentState::Downloading
                    } else {
                        SegmentState::Pending
                    };
                    segments.push(SegmentView {
                        index: i,
                        start: counters.seg_starts[i],
                        end: counters.seg_starts[i] + len.saturating_sub(1),
                        downloaded: done,
                        total: len,
                        state,
                    });
                }
            } else {
                downloaded = counters.total_downloaded();
            }

            for (id, channel) in inner.channels.iter().enumerate() {
                let (start, end, seg_done, seg_total) = match channel.segment {
                    Some(idx) if idx < counters.seg_lens.len() => {
                        let len = counters.seg_lens[idx];
                        (
                            counters.seg_starts[idx],
                            counters.seg_starts[idx] + len.saturating_sub(1),
                            counters.seg_downloaded[idx].load(Ordering::Relaxed).min(len),
                            len,
                        )
                    }
                    _ => (0, 0, 0, 0),
                };
                channels.push(ChannelView {
                    id,
                    state: channel.state,
                    segment_index: channel.segment,
                    segment_start: start,
                    segment_end: end,
                    segment_downloaded: seg_done,
                    segment_total: seg_total,
                    speed_bps: channel.speed_bps,
                    retries: channel.retries,
                    last_error: channel.last_error.clone(),
                });
            }
        } else {
            for id in 0..inner.channels.len() {
                let channel = &inner.channels[id];
                channels.push(ChannelView {
                    id,
                    state: channel.state,
                    segment_index: None,
                    segment_start: 0,
                    segment_end: 0,
                    segment_downloaded: 0,
                    segment_total: 0,
                    speed_bps: channel.speed_bps,
                    retries: channel.retries,
                    last_error: channel.last_error.clone(),
                });
            }
        }

        let total = inner.total;
        let eta_seconds = if inner.speed_bps > 0 && total > downloaded {
            Some((total - downloaded) / inner.speed_bps)
        } else {
            None
        };

        TaskView {
            id: self.id.clone(),
            url: self.url.clone(),
            file_name: inner.file_name.clone(),
            save_path: inner.save_path.to_string_lossy().to_string(),
            status: inner.status,
            total_bytes: total,
            downloaded_bytes: downloaded,
            speed_bps: inner.speed_bps,
            eta_seconds,
            connections: self.connections,
            supports_range: inner.supports_range,
            channels,
            segments,
            error: inner.error.clone(),
        }
    }
}
