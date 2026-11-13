import { ChangeDetectionStrategy, Component, computed, inject, input, signal } from '@angular/core';
import { MatButtonModule } from '@angular/material/button';

import {
  cancelTask,
  openFile,
  pauseTask,
  removeTask,
  resumeTask,
  revealInDir,
  setTaskFiles,
} from '../core/api';
import { formatBytes, formatDuration, formatRange, formatSpeed, percent } from '../core/format';
import { Icon } from '../core/icon';
import { stateTone } from '../core/tone';
import {
  CHANNEL_STATE_LABEL,
  TASK_KIND_LABEL,
  TASK_STATUS_LABEL,
  type TaskView,
} from '../core/types';

import { UiState } from '../core/ui-state';
import { ConfirmButton } from './confirm-button';
import { FilePicker } from './file-picker';
import { ProgressMatrix } from './progress-matrix';
import { SegmentBar } from './segment-bar';
import { StatusChip } from './status-chip';

/** 同一单位时省掉前半段的单位：「39.6 – 49.5 MB」比「39.6 MB - 49.5 MB」好扫 */
function compactRange(start: number, end: number): string {
  const from = formatBytes(start).split(' ');
  const to = formatBytes(end).split(' ');
  return from[1] === to[1] ? `${from[0]} – ${to[0]} ${to[1]}` : `${from.join(' ')} – ${to.join(' ')}`;
}

/** 同理：「7.13 / 9.90 MB」 */
function compactRatio(done: number, total: number): string {
  const from = formatBytes(done).split(' ');
  const to = formatBytes(total);
  return from[1] === to.split(' ')[1] ? `${from[0]} / ${to}` : `${formatBytes(done)} / ${to}`;
}

/** 任务行：标题、分片条带、统计、操作按钮，展开后是通道路由表与下载矩阵 */
@Component({
  selector: 'app-task-item',
  imports: [
    ConfirmButton,
    FilePicker,
    Icon,
    MatButtonModule,
    ProgressMatrix,
    SegmentBar,
    StatusChip,
  ],
  templateUrl: './task-item.html',
  styleUrl: './task-item.scss',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class TaskItem {
  readonly task = input.required<TaskView>();

  private readonly ui = inject(UiState);

  protected readonly expanded = signal(false);

  /** http 任务不挂协议标签，只有磁力／种子与 ed2k 才需要区分 */
  protected readonly hasKindChip = computed(() => this.task().kind !== 'http');

  protected readonly kindLabel = computed(() => TASK_KIND_LABEL[this.task().kind]);

  protected readonly statusLabel = computed(() => {
    const task = this.task();
    // BT 的「解析资源」是把磁力换成种子元数据，说清楚在解析什么
    if (task.kind === 'bt' && task.status === 'probing') return '解析元数据';
    // ed2k 的解析阶段是拉起侧车、连服务器与建 Kad 路由表，接着才开始找源
    if (task.kind === 'ed2k' && task.status === 'probing') return '接入网络中';
    return TASK_STATUS_LABEL[task.status];
  });

  protected readonly overall = computed(() =>
    percent(this.task().downloadedBytes, this.task().totalBytes),
  );

  protected readonly percentLabel = computed(() => {
    const value = this.overall();
    return value === null ? '总大小未知' : `${value.toFixed(1)}%`;
  });

  protected readonly stats = computed(() => {
    const task = this.task();
    return {
      size:
        task.totalBytes > 0
          ? `${formatBytes(task.downloadedBytes)} / ${formatBytes(task.totalBytes)}`
          : formatBytes(task.downloadedBytes),
      speed: formatSpeed(task.speedBps),
      eta: task.status === 'completed' ? '已完成' : `剩余 ${formatDuration(task.etaSeconds)}`,
      connections: this.connectionsLabel(),
      files: this.filesSummary(),
    };
  });

  /** 只下种子里的几个文件时，列表里也要一眼看出不是全下 */
  private filesSummary(): string | null {
    const files = this.task().files;
    if (files.length < 2) return null;
    const selected = files.filter((file) => file.selected).length;
    return selected === files.length
      ? `${files.length} 个文件`
      : `已选 ${selected} / ${files.length} 个文件`;
  }

  private connectionsLabel(): string {
    const task = this.task();
    if (task.kind === 'bt') {
      // 没连上任何 peer 是磁力任务最常见的问题，正在跑的任务就直接说出来
      const stats = task.btStats;
      if (stats === null) {
        const active =
          task.status === 'probing' || task.status === 'downloading' || task.status === 'queued';
        return active ? '尚未连上 peer' : '--';
      }
      return stats.peersConnected === 0
        ? '尚未连上 peer'
        : `已连 ${stats.peersConnected} 个 peer`;
    }
    if (task.kind === 'ed2k') {
      // 先找到源再谈连上：刚挂上时源表是空的，一分钟后可能一大把
      const stats = task.ed2kStats;
      if (stats === null) return '尚未找到源';
      if (stats.activePeers > 0) return `已连 ${stats.activePeers} 个源`;
      return stats.peers > 0 ? `已找到 ${stats.peers} 个源` : '尚未找到源';
    }
    return task.supportsRange ? `${task.connections} 连接` : '单连接（服务器不支持分段）';
  }

  /** ed2k 详情：也是没有分片通道，换成文件哈希、源、服务器与 Kad 节点 */
  protected readonly ed2kDetails = computed(() => {
    const task = this.task();
    if (task.kind !== 'ed2k') return null;
    const stats = task.ed2kStats;
    return {
      hash: task.infoHash ?? '--',
      peers:
        stats === null ? '--' : `已知 ${stats.peers} · 已连 ${stats.activePeers}`,
      network:
        stats === null
          ? '--'
          : `${stats.servers} 个服务器 · ${stats.kadNodes} 个 Kad 节点`,
      uploaded: stats === null ? '--' : formatBytes(stats.uploadedBytes),
    };
  });

  /** BT 详情：没有分片通道，换成种子标识、peer 与上传量 */
  protected readonly btDetails = computed(() => {
    const task = this.task();
    if (task.kind !== 'bt') return null;
    const stats = task.btStats;
    return {
      infoHash: task.infoHash ?? '--',
      peers:
        stats === null
          ? '--'
          : `已连 ${stats.peersConnected} · 已知 ${stats.peersSeen}`,
      uploaded: stats === null ? '--' : formatBytes(stats.uploadedBytes),
    };
  });

  protected readonly hasDetails = computed(
    () => this.btDetails() !== null || this.ed2kDetails() !== null,
  );

  /** 种子的文件清单：元数据到手后就能在详情里改要下哪几个 */
  protected readonly fileList = computed(() => this.task().files);

  protected readonly hasFiles = computed(() => this.fileList().length > 1);

  protected readonly segments = computed(() =>
    this.task().segments.map((segment) => {
      // 暂停、失败、重启恢复后，未完成的分片已经没有通道在跑，
      // 状态是 pending 但数据还在，得按「有数据的空闲段」上色
      const fillState =
        segment.state === 'pending' && segment.downloaded > 0 ? 'idle' : segment.state;
      const pct = percent(segment.downloaded, segment.total);
      return {
        index: segment.index,
        grow: segment.total || 1,
        ratio: pct === null ? null : pct / 100,
        state: fillState,
        title: `分片 #${segment.index + 1}　${formatRange(segment.start, segment.end)}　${formatBytes(
          segment.downloaded,
        )} / ${formatBytes(segment.total)}`,
      };
    }),
  );

  protected readonly canPause = computed(() => {
    const status = this.task().status;
    return status === 'downloading' || status === 'probing' || status === 'queued';
  });

  protected readonly canResume = computed(() => {
    const status = this.task().status;
    return status === 'paused' || status === 'failed' || status === 'cancelled';
  });

  /** 已经落盘的状态才谈得上「打开所在目录」 */
  protected readonly hasFile = computed(() => {
    const status = this.task().status;
    return status === 'completed' || status === 'paused' || status === 'cancelled';
  });

  /** 只有下载完成才是一个完整文件；暂停／取消留下的是 .part，打不开 */
  protected readonly hasCompletedFile = computed(
    () => this.task().status === 'completed' && this.task().savePath !== '',
  );

  protected readonly channels = computed(() => {
    const total = this.task().totalBytes;
    const segCount = this.task().segments.length;
    // 刻度点的间距：整份文件按分片数等分，每格中心一个点
    const mapSize = segCount > 0 ? `${100 / segCount}% 100%` : null;
    return this.task().channels.map((channel) => {
      const pct = percent(channel.segmentDownloaded, channel.segmentTotal);
      const index = channel.segmentIndex;
      const start = channel.segmentStart;
      const length = channel.segmentTotal;
      const live =
        channel.state === 'connecting' ||
        channel.state === 'downloading' ||
        channel.state === 'retrying';
      const range = index === null ? '' : compactRange(start, start + Math.max(0, length - 1));
      return {
        id: channel.id,
        number: `${channel.id + 1}`,
        tone: stateTone(channel.state),
        state: channel.state,
        stateLabel: CHANNEL_STATE_LABEL[channel.state],
        spinner: channel.state === 'connecting' || channel.state === 'retrying',
        retries: channel.retries,
        done: channel.state === 'finished',
        // 序号前的标记交给图标，这里只留数字
        segment: index === null ? null : `${index + 1}`,
        range,
        // 分片中心的百分比位置：一个点就能说明「这条连接负责文件的哪一段」
        pin: total > 0 && length > 0 ? ((start + length / 2) / total) * 100 : null,
        mapSize,
        ratio: pct === null ? null : pct / 100,
        // 单位只写一次：「7.13 / 9.90 MB」，给进度条省出宽度
        bytes: length > 0 ? compactRatio(channel.segmentDownloaded, length) : formatBytes(channel.segmentDownloaded),
        // 窄窗口里字节数让位给百分比，进度总得有个数字
        percentLabel: pct === null ? '--' : `${Math.round(pct)}%`,
        speed: formatSpeed(channel.speedBps),
        // 已完成或已停止的连接没有速度可言，整列 0 B/s 只是噪声
        quietSpeed: channel.speedBps <= 0 && !live,
        hint:
          index === null
            ? `通道 ${channel.id + 1} 还没领到分片`
            : `通道 ${channel.id + 1} 最后处理的是分片 ${index + 1}（${range}）`,
      };
    });
  });

  protected readonly hasChannels = computed(() =>
    this.task().channels.some((channel) => channel.segmentIndex !== null),
  );

  // 矩阵按已有进度算，历史任务也能看；只探测出总大小时展开才有内容。
  // BT 的完成区间不连续，一条前缀式的矩阵会骗人，所以只在 http 任务上展示
  protected readonly hasMatrix = computed(
    () =>
      this.task().kind === 'http' &&
      this.task().totalBytes > 0 &&
      this.task().status !== 'probing',
  );

  protected readonly canExpand = computed(
    () => this.hasChannels() || this.hasMatrix() || this.hasDetails(),
  );

  protected readonly pause = (id: string) => this.run(() => pauseTask(id));

  protected readonly resume = (id: string) => this.run(() => resumeTask(id));

  protected readonly cancel = (id: string) => this.run(() => cancelTask(id, false));

  protected readonly reveal = (path: string) => this.run(() => revealInDir(path));

  protected readonly open = (path: string) => this.run(() => openFile(path));

  protected readonly remove = (id: string, deleteFile = false) =>
    this.run(() => removeTask(id, deleteFile));

  /** 改勾选：把新的选择整体交给后端，运行中的任务由会话下一拍应用 */
  protected readonly toggleFile = (index: number) => {
    const next = this.task()
      .files.filter((file) => (file.index === index ? !file.selected : file.selected))
      .map((file) => file.index);
    this.commitFiles(next);
  };

  protected readonly setAllFiles = (checked: boolean) => {
    this.commitFiles(checked ? this.task().files.map((file) => file.index) : []);
  };

  /** 传全部序号等于「全选」，后端会把它存成「不限文件」 */
  private commitFiles(indices: number[]) {
    const id = this.task().id;
    this.run(() => setTaskFiles(id, indices));
  }

  private run(action: () => Promise<unknown>) {
    action().catch((error) => this.ui.notify(String(error)));
  }
}
