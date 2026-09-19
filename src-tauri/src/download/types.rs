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

impl TaskStatus {
    /// 入库用的稳定字符串，与 serde 输出保持一致，避免前后端与数据库出现两套写法。
    pub fn as_str(self) -> &'static str {
        match self {
            TaskStatus::Queued => "queued",
            TaskStatus::Probing => "probing",
            TaskStatus::Downloading => "downloading",
            TaskStatus::Paused => "paused",
            TaskStatus::Completed => "completed",
            TaskStatus::Failed => "failed",
            TaskStatus::Cancelled => "cancelled",
        }
    }

    /// as_str 的逆：解析入库字符串，认不出来时返回 None。
    pub fn from_stored(value: &str) -> Option<TaskStatus> {
        match value {
            "queued" => Some(TaskStatus::Queued),
            "probing" => Some(TaskStatus::Probing),
            "downloading" => Some(TaskStatus::Downloading),
            "paused" => Some(TaskStatus::Paused),
            "completed" => Some(TaskStatus::Completed),
            "failed" => Some(TaskStatus::Failed),
            "cancelled" => Some(TaskStatus::Cancelled),
            _ => None,
        }
    }

    /// 进程被强杀时留下的中间状态：恢复时一律按「已暂停」处理，
    /// 否则界面上会出现没有任何线程在跑的「下载中」。
    pub fn restored(self) -> TaskStatus {
        match self {
            TaskStatus::Queued | TaskStatus::Probing | TaskStatus::Downloading => TaskStatus::Paused,
            other => other,
        }
    }
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
