use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use crate::store::{SegmentRecord, Store, TaskRecord};

use super::task::{
    mark_selected, now_millis, plan_segment_count, BtInfo, ChannelRuntime, Counters, Task,
    TaskInner, MAX_TASK_CONNECTIONS,
};
use super::types::{TaskKind, TaskStatus};

/// 把任务当前状态写进数据库（不含分片）。
pub fn save_task(store: &Store, task: &Task) -> Result<(), String> {
    store.save_task(&task_record(task))
}

/// 任务行 + 全部分片进度。周期性落库与状态变化都走这里。
pub fn save_all(store: &Store, task: &Task) -> Result<(), String> {
    save_task(store, task)?;
    let segments = segment_records(task);
    if !segments.is_empty() {
        store.save_segments(&task.id, &segments)?;
    }
    Ok(())
}

fn task_record(task: &Task) -> TaskRecord {
    let inner = task.inner.lock().unwrap();
    TaskRecord {
        id: task.id.clone(),
        url: task.url.clone(),
        kind: task.kind.as_str().to_string(),
        dir: inner.dir.to_string_lossy().to_string(),
        file_name: inner.file_name.clone(),
        save_path: inner.save_path.to_string_lossy().to_string(),
        status: inner.status.as_str().to_string(),
        total_bytes: inner.total as i64,
        supports_range: inner.supports_range,
        probed: inner.probed,
        single_stream: inner
            .counters
            .as_ref()
            .map(|counters| counters.single_stream)
            .unwrap_or(false),
        connections: task.connections as i64,
        error: inner.error.clone(),
        info_hash: inner.bt.as_ref().map(|bt| bt.info_hash.clone()),
        magnet: inner.bt.as_ref().and_then(|bt| bt.magnet.clone()),
        torrent_path: inner.bt.as_ref().and_then(|bt| bt.torrent_path.clone()),
        only_files: inner
            .only_files
            .as_ref()
            .and_then(|indices| serde_json::to_string(indices).ok()),
        created_at: inner.created_at,
        updated_at: now_millis(),
    }
}

fn segment_records(task: &Task) -> Vec<SegmentRecord> {
    let inner = task.inner.lock().unwrap();
    let Some(counters) = &inner.counters else {
        // BT 没有分片表，用一行覆盖整个文件的伪分片承载已下载字节，
        // 重启后界面立刻能显示断点进度，不必等种子重新核对磁盘
        return if task.kind == TaskKind::Bt && inner.total > 0 {
            vec![SegmentRecord {
                idx: 0,
                start: 0,
                len: inner.total as i64,
                downloaded: inner.downloaded as i64,
            }]
        } else {
            Vec::new()
        };
    };
    (0..counters.seg_count())
        .map(|idx| SegmentRecord {
            idx: idx as i64,
            start: counters.seg_starts[idx] as i64,
            len: counters.seg_lens[idx] as i64,
            downloaded: counters.seg_downloaded[idx].load(Ordering::SeqCst) as i64,
        })
        .collect()
}

/// 启动时把库里的任务装回内存。单个任务恢复失败只跳过它，不影响其它任务。
pub fn restore(store: &Store, default_dir: &std::path::Path) -> Vec<Arc<Task>> {
    let records = match store.load_tasks() {
        Ok(records) => records,
        Err(err) => {
            eprintln!("读取已保存的任务失败：{err}");
            return Vec::new();
        }
    };

    let mut tasks = Vec::new();
    for record in records {
        match restore_one(store, &record, default_dir) {
            Ok(task) => tasks.push(task),
            Err(err) => eprintln!("跳过无法恢复的任务 {}：{err}", record.id),
        }
    }
    tasks
}

fn restore_one(
    store: &Store,
    record: &TaskRecord,
    default_dir: &std::path::Path,
) -> Result<Arc<Task>, String> {
    let status = TaskStatus::from_stored(&record.status)
        .ok_or_else(|| format!("未知的任务状态 {}", record.status))?;
    let kind = TaskKind::from_stored(&record.kind)
        .ok_or_else(|| format!("未知的任务类型 {}", record.kind))?;
    let connections = record.connections.clamp(1, MAX_TASK_CONNECTIONS as i64) as usize;
    let segments = store.load_segments(&record.id)?;

    // 勾选过的文件要跨会话保留，重新挂载时按它只下选中的
    let only_files: Option<Vec<usize>> = record
        .only_files
        .as_deref()
        .and_then(|raw| serde_json::from_str(raw).ok());
    // 种子文件任务本地就能解析出清单，重启后不必等恢复联网才让用户看到文件列表
    let bt_files = if kind == TaskKind::Bt {
        record
            .torrent_path
            .as_deref()
            .map(Path::new)
            .and_then(|path| super::bt::parse_torrent_file(path).ok())
            .map(|preview| {
                let mut files = preview.files;
                mark_selected(&mut files, only_files.as_deref());
                files
            })
            .unwrap_or_default()
    } else {
        Vec::new()
    };

    // BT 的那行伪分片只用来显示断点进度；真正下到哪了由会话重新核对磁盘后汇报
    let downloaded = if kind == TaskKind::Bt {
        segments
            .first()
            .filter(|segment| segment.start == 0 && segment.len == record.total_bytes)
            .map(|segment| segment.downloaded.max(0) as u64)
            .unwrap_or(0)
    } else {
        0
    };
    let counters = if kind == TaskKind::Bt {
        None
    } else if record.probed && segments_consistent(record, &segments) {
        let single_stream = record.single_stream;
        let channel_count = if single_stream { 1 } else { segments.len().max(1) };
        Some(Arc::new(Counters::restored(
            segments.iter().map(|segment| segment.start as u64).collect(),
            segments.iter().map(|segment| segment.len as u64).collect(),
            segments.iter().map(|segment| segment.downloaded as u64).collect(),
            single_stream,
            channel_count,
        )))
    } else {
        if record.probed {
            eprintln!("任务 {} 的分片记录不可用，将从头下载", record.id);
        }
        None
    };
    let channel_count = match &counters {
        Some(counters) => counters.channel_bytes.len(),
        None if kind == TaskKind::Bt => 0,
        None if record.single_stream => 1,
        None => plan_segment_count(record.total_bytes.max(0) as u64, connections).max(1),
    };

    let dir = {
        let saved = PathBuf::from(&record.dir);
        if saved.as_os_str().is_empty() {
            default_dir.to_path_buf()
        } else {
            saved
        }
    };

    Ok(Arc::new(Task {
        id: record.id.clone(),
        url: record.url.clone(),
        kind,
        connections,
        inner: Mutex::new(TaskInner {
            dir,
            file_name: record.file_name.clone(),
            save_path: PathBuf::from(&record.save_path),
            status: status.restored(),
            error: record.error.clone(),
            total: record.total_bytes.max(0) as u64,
            supports_range: record.supports_range,
            // http 任务重新探测一次，用来确认远端文件没有被替换
            probed: kind == TaskKind::Bt,
            counters,
            downloaded,
            bt_stats: None,
            bt_files,
            only_files,
            bt: (kind == TaskKind::Bt).then(|| BtInfo {
                info_hash: record.info_hash.clone().unwrap_or_default(),
                magnet: record.magnet.clone(),
                torrent_path: record.torrent_path.clone(),
            }),
            channels: (0..channel_count).map(|_| ChannelRuntime::new()).collect(),
            cancel: None,
            stop: None,
            running: false,
            resume_pending: false,
            removed: false,
            speed_bps: 0,
            last_total_bytes: 0,
            last_tick: None,
            created_at: record.created_at,
        }),
    }))
}

/// 分片必须首尾相接铺满整个文件，且已提交偏移不超过分片长度；
/// 不自洽的记录宁可丢弃进度，也不能按它去拼文件。
fn segments_consistent(record: &TaskRecord, segments: &[SegmentRecord]) -> bool {
    if segments.is_empty() {
        return false;
    }
    if record.single_stream && segments.len() != 1 {
        return false;
    }
    let mut expected_start = 0i64;
    for segment in segments {
        if segment.start != expected_start
            || segment.len < 0
            || segment.downloaded < 0
            || segment.downloaded > segment.len
        {
            return false;
        }
        expected_start += segment.len;
    }
    record.total_bytes <= 0 || expected_start == record.total_bytes
}
