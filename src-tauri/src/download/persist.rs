use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use crate::store::{SegmentRecord, Store, TaskRecord};

use super::task::{now_millis, plan_segment_count, ChannelRuntime, Counters, Task, TaskInner};
use super::types::TaskStatus;

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
        created_at: inner.created_at,
        updated_at: now_millis(),
    }
}

fn segment_records(task: &Task) -> Vec<SegmentRecord> {
    let inner = task.inner.lock().unwrap();
    let Some(counters) = &inner.counters else {
        return Vec::new();
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
    let connections = record.connections.clamp(1, 16) as usize;
    let segments = store.load_segments(&record.id)?;

    let counters = if record.probed && segments_consistent(record, &segments) {
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
        connections,
        inner: Mutex::new(TaskInner {
            dir,
            file_name: record.file_name.clone(),
            save_path: PathBuf::from(&record.save_path),
            status: status.restored(),
            error: record.error.clone(),
            total: record.total_bytes.max(0) as u64,
            supports_range: record.supports_range,
            // 恢复的任务重新探测一次，用来确认远端文件没有被替换
            probed: false,
            counters,
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
