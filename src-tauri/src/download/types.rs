use serde::Serialize;

/// 任务走的下载协议。http 走自研多连接引擎，bt 交给进程内的 librqbit 会话。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum TaskKind {
    Http,
    Bt,
}

impl TaskKind {
    pub fn as_str(self) -> &'static str {
        match self {
            TaskKind::Http => "http",
            TaskKind::Bt => "bt",
        }
    }

    pub fn from_stored(value: &str) -> Option<TaskKind> {
        match value {
            "http" => Some(TaskKind::Http),
            "bt" => Some(TaskKind::Bt),
            _ => None,
        }
    }
}

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

/// BT 任务的实时状态。BT 没有分片通道，用 peer 数与上传量代替通道表。
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BtStats {
    /// 已连上的 peer 数
    pub peers_connected: u32,
    /// 已知的 peer 数（含尚未连上的）
    pub peers_seen: u32,
    pub uploaded_bytes: u64,
}

/// 种子里的一个文件。既用于界面展示，也是「只下这几个」的选择依据：
/// `selected` 为 false 的文件不会下载，已下载字节也停在选中时的那一刻。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TorrentFile {
    pub index: usize,
    /// 种子内的相对路径
    pub name: String,
    pub length: u64,
    pub selected: bool,
    pub downloaded: u64,
}

/// 种子的「预览」：文件清单与总大小，不建任务也不落盘。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TorrentPreview {
    pub info_hash: String,
    pub name: Option<String>,
    pub total_bytes: u64,
    pub files: Vec<TorrentFile>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskView {
    pub id: String,
    pub url: String,
    pub kind: TaskKind,
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
    /// BT 的种子文件清单；磁力要等元数据到手才有，其它协议为空
    pub files: Vec<TorrentFile>,
    /// BT 的种子标识，其它协议为 None
    pub info_hash: Option<String>,
    /// 只有 BT 任务有
    pub bt_stats: Option<BtStats>,
    pub error: Option<String>,
}
