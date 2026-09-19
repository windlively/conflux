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

export interface TaskView {
  id: string;
  url: string;
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
  error: string | null;
}

export interface StorageInfo {
  available: boolean;
  path: string | null;
  schemaVersion: number | null;
  message: string | null;
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

export const CHANNEL_STATE_LABEL: Record<ChannelState, string> = {
  idle: '空闲',
  connecting: '连接中',
  downloading: '下载中',
  retrying: '重试中',
  finished: '已完成',
  stopped: '已停止',
  failed: '失败',
};
