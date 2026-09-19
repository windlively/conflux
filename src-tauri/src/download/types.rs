use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum TaskStatus {
    Queued,
    Probing,
    Downloading,
    Paused,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ChannelState {
    Idle,
    Connecting,
    Downloading,
    Retrying,
    Finished,
    Stopped,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum SegmentState {
    Pending,
    Downloading,
    Completed,
}

/// 单个下载通道的实时视图。通道完成一个分片后会领取下一个，
/// 因此 segmentIndex 会变化，前端必须跟随更新而不是认为进度倒退。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelView {
    pub id: usize,
    pub state: ChannelState,
    pub segment_index: Option<usize>,
    pub segment_start: u64,
    pub segment_end: u64,
    pub segment_downloaded: u64,
    pub segment_total: u64,
    pub speed_bps: u64,
    pub retries: u32,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SegmentView {
    pub index: usize,
    pub start: u64,
    pub end: u64,
    pub downloaded: u64,
    pub total: u64,
    pub state: SegmentState,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskView {
    pub id: String,
    pub url: String,
    pub file_name: String,
    pub save_path: String,
    pub status: TaskStatus,
    /// 0 表示总大小未知
    pub total_bytes: u64,
    pub downloaded_bytes: u64,
    pub speed_bps: u64,
    pub eta_seconds: Option<u64>,
    pub connections: usize,
    pub supports_range: bool,
    pub channels: Vec<ChannelView>,
    pub segments: Vec<SegmentView>,
    pub error: Option<String>,
}
