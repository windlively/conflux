import { ChangeDetectionStrategy, Component, computed, inject, input, signal } from '@angular/core';
import { MatButtonModule } from '@angular/material/button';

import { cancelTask, pauseTask, removeTask, resumeTask, revealInDir, setTaskFiles } from '../core/api';
import { formatBytes, formatDuration, formatRange, formatSpeed, percent } from '../core/format';
import { Icon } from '../core/icon';
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

  protected readonly isBt = computed(() => this.task().kind === 'bt');

  protected readonly kindLabel = computed(() => TASK_KIND_LABEL[this.task().kind]);

  protected readonly statusLabel = computed(() => {
    const task = this.task();
    // BT 的「解析资源」是把磁力换成种子元数据，说清楚在解析什么
    if (task.kind === 'bt' && task.status === 'probing') return '解析元数据';
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
    return task.supportsRange ? `${task.connections} 连接` : '单连接（服务器不支持分段）';
  }

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

  protected readonly hasDetails = computed(() => this.btDetails() !== null);

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

  protected readonly channels = computed(() =>
    this.task().channels.map((channel) => {
      const pct = percent(channel.segmentDownloaded, channel.segmentTotal);
      return {
        id: channel.id,
        label: `#${channel.id + 1}`,
        state: channel.state,
        stateLabel: CHANNEL_STATE_LABEL[channel.state],
        spinner: channel.state === 'connecting' || channel.state === 'retrying',
        retries: channel.retries,
        segmentLabel:
          channel.segmentIndex === null
            ? '--'
            : `#${channel.segmentIndex + 1} · ${formatRange(channel.segmentStart, channel.segmentEnd)}`,
        ratio: pct === null ? null : pct / 100,
        grow: channel.segmentTotal || 1,
        bytes:
          channel.segmentTotal > 0
            ? `${formatBytes(channel.segmentDownloaded)} / ${formatBytes(channel.segmentTotal)}`
            : formatBytes(channel.segmentDownloaded),
        speed: formatSpeed(channel.speedBps),
      };
    }),
  );

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
