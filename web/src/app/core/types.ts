export type TaskStatus =
  | 'queued'
  | 'probing'
  | 'downloading'
  | 'paused'
  | 'completed'
  | 'failed'
  | 'cancelled';

export type ChannelState =
  | 'idle'
  | 'connecting'
  | 'downloading'
  | 'retrying'
  | 'finished'
  | 'stopped'
  | 'failed';

export type SegmentState = 'pending' | 'downloading' | 'completed';

export type TaskKind = 'http' | 'bt' | 'ed2k';

/** BT 任务没有分片通道，界面上用 peer 数与上传量代替通道表 */
export interface BtStats {
  peersConnected: number;
  peersSeen: number;
  uploadedBytes: number;
}

/** ed2k 也一样没有通道，换成源／服务器／Kad 节点与上传量 */
export interface Ed2kStats {
  /** 已知有这个文件的源（含还没连上的） */
  peers: number;
  /** 正在传数据的源 */
  activePeers: number;
  servers: number;
  kadNodes: number;
  uploadedBytes: number;
}

/** ed2k 搜索的一条结果：链接可以直接拿去建任务 */
export interface Ed2kSearchResult {
  hash: string;
  name: string;
  size: number;
  /** 知道有这个文件的节点数 */
  sources: number;
  /** 手里有完整文件的节点数 */
  completeSources: number;
  link: string;
}

export interface ChannelView {
  id: number;
  state: ChannelState;
  segmentIndex: number | null;
  segmentStart: number;
  segmentEnd: number;
  segmentDownloaded: number;
  segmentTotal: number;
  speedBps: number;
  retries: number;
  lastError: string | null;
}

export interface SegmentView {
  index: number;
  start: number;
  end: number;
  downloaded: number;
  total: number;
  state: SegmentState;
}

/** 种子里的一个文件：既是清单，也是「只下这几个」的选择依据 */
export interface TorrentFile {
  index: number;
  /** 种子内的相对路径 */
  name: string;
  length: number;
  selected: boolean;
  /** 已下载字节；没勾选的文件停在取消勾选的那一刻 */
  downloaded: number;
}

/** 种子的预览结果：不建任务也不落盘 */
export interface TorrentPreview {
  infoHash: string;
  name: string | null;
  totalBytes: number;
  files: TorrentFile[];
}

export interface TaskView {
  id: string;
  url: string;
  kind: TaskKind;
  fileName: string;
  savePath: string;
  status: TaskStatus;
  totalBytes: number;
  downloadedBytes: number;
  speedBps: number;
  etaSeconds: number | null;
  connections: number;
  supportsRange: boolean;
  channels: ChannelView[];
  segments: SegmentView[];
  /** BT 的种子文件清单，其它协议为空 */
  files: TorrentFile[];
  /** BT 的种子标识，其它协议为 null */
  infoHash: string | null;
  btStats: BtStats | null;
  /** ed2k 的 peer／服务器／Kad 统计，其它协议为 null */
  ed2kStats: Ed2kStats | null;
  error: string | null;
}

export interface StorageInfo {
  available: boolean;
  path: string | null;
  schemaVersion: number | null;
  message: string | null;
}

/** BT 的 SOCKS5 代理设置：value 是归一化后的地址，null 表示不用代理 */
export interface BtProxySetting {
  value: string | null;
  /** 保存时 BT 会话已经建好：这次改动要重启应用才生效 */
  sessionRunning: boolean;
}

export const TASK_STATUS_LABEL: Record<TaskStatus, string> = {
  queued: '排队中',
  probing: '解析资源',
  downloading: '下载中',
  paused: '已暂停',
  completed: '已完成',
  failed: '失败',
  cancelled: '已取消',
};

export const TASK_KIND_LABEL: Record<TaskKind, string> = {
  http: 'HTTP',
  bt: '磁力',
  ed2k: '电驴',
};

export const CHANNEL_STATE_LABEL: Record<ChannelState, string> = {
  idle: '空闲',
  connecting: '连接中',
  downloading: '下载中',
  retrying: '重试中',
  finished: '已完成',
  stopped: '已停止',
  failed: '失败',
};
