import { useEffect, useState } from 'react';

import { cancelTask, pauseTask, removeTask, resumeTask, revealInDir } from '../api';
import { formatBytes, formatDuration, formatRange, formatSpeed, percent } from '../format';
import { CHANNEL_STATE_LABEL, TASK_STATUS_LABEL, type TaskView } from '../types';
import { ProgressMatrix } from './ProgressMatrix';
import { Button, SegmentBar, StatusChip, TotalBar } from './md';

/** 二次确认按钮：Tauri 的 WebView 不保证支持 window.confirm，因此放在应用内实现 */
function ConfirmButton({ label, confirmLabel, onConfirm }: {
  label: string;
  confirmLabel: string;
  onConfirm: () => void;
}) {
  const [armed, setArmed] = useState(false);

  useEffect(() => {
    if (!armed) return;
    const timer = setTimeout(() => setArmed(false), 4000);
    return () => clearTimeout(timer);
  }, [armed]);

  return (
    <Button
      danger
      armed={armed}
      onClick={() => {
        if (armed) {
          setArmed(false);
          onConfirm();
        } else {
          setArmed(true);
        }
      }}
    >
      {armed ? confirmLabel : label}
    </Button>
  );
}

interface TaskItemProps {
  task: TaskView;
  onError: (message: string) => void;
}

export function TaskItem({ task, onError }: TaskItemProps) {
  const [expanded, setExpanded] = useState(false);

  const run = (action: () => Promise<unknown>) => {
    action().catch((error) => onError(String(error)));
  };

  const overall = percent(task.downloadedBytes, task.totalBytes);
  const active = task.status === 'downloading' || task.status === 'probing' || task.status === 'queued';
  const resumable = task.status === 'paused' || task.status === 'failed' || task.status === 'cancelled';
  const hasFile = task.status === 'completed' || task.status === 'paused' || task.status === 'cancelled';
  const hasChannels = task.channels.some((channel) => channel.segmentIndex !== null);
  // 矩阵按已有进度算，历史任务也能看；只探测出总大小时展开才有内容
  const hasMatrix = task.totalBytes > 0 && task.status !== 'probing';
  const showDetails = hasChannels || hasMatrix;

  return (
    <li className="task">
      <div className="task-head">
        <div className="task-title">
          <span className="task-name" title={task.savePath}>
            {task.fileName}
          </span>
          <StatusChip
            state={task.status}
            label={TASK_STATUS_LABEL[task.status]}
            spinner={task.status === 'probing'}
          />
          <span className="spacer" />
          <span className="task-percent">
            {overall === null ? '总大小未知' : `${overall.toFixed(1)}%`}
          </span>
        </div>
        <span className="task-host">{task.url}</span>
      </div>

      <div className="progress-block">
        {task.segments.length > 1 ? (
          <div className="segments">
            {task.segments.map((segment) => {
              // 暂停、失败、重启恢复后，未完成的分片已经没有通道在跑，
              // 状态是 pending 但数据还在，得按「有数据的空闲段」上色
              const fillState =
                segment.state === 'pending' && segment.downloaded > 0 ? 'idle' : segment.state;
              const pct = percent(segment.downloaded, segment.total);
              return (
                <SegmentBar
                  key={segment.index}
                  grow={segment.total || 1}
                  ratio={pct === null ? null : pct / 100}
                  state={fillState}
                  title={`分片 #${segment.index + 1}　${formatRange(segment.start, segment.end)}　${formatBytes(
                    segment.downloaded,
                  )} / ${formatBytes(segment.total)}`}
                />
              );
            })}
          </div>
        ) : (
          // 单连接（或只有一个分片）时没有分片分布，退化成一条整体进度条
          <TotalBar ratio={overall} state={task.status} />
        )}
      </div>

      <div className="task-stats">
        <span>
          {formatBytes(task.downloadedBytes)}
          {task.totalBytes > 0 ? ` / ${formatBytes(task.totalBytes)}` : ''}
        </span>
        <span>{formatSpeed(task.speedBps)}</span>
        <span>
          {task.status === 'completed'
            ? '已完成'
            : `剩余 ${formatDuration(task.etaSeconds)}`}
        </span>
        <span>{task.supportsRange ? `${task.connections} 连接` : '单连接（服务器不支持分段）'}</span>
      </div>

      {task.error && <div className="task-error">{task.error}</div>}

      <div className="task-actions">
        {active && <Button onClick={() => run(() => pauseTask(task.id))}>暂停</Button>}
        {resumable && <Button onClick={() => run(() => resumeTask(task.id))}>继续</Button>}
        {active && (
          <Button onClick={() => run(() => cancelTask(task.id, false))}>取消</Button>
        )}
        {hasFile && (
          <Button onClick={() => run(() => revealInDir(task.savePath))}>打开所在目录</Button>
        )}
        {showDetails && (
          <Button onClick={() => setExpanded((value) => !value)}>
            {expanded ? '收起详情' : '展开详情'}
          </Button>
        )}
        <span className="spacer" />
        <Button onClick={() => run(() => removeTask(task.id, false))}>移除</Button>
        <ConfirmButton
          label="删除文件"
          confirmLabel="确认删除？"
          onConfirm={() => run(() => removeTask(task.id, true))}
        />
      </div>

      {expanded && showDetails && (
        <div className="details">
          {hasChannels && (
            <table className="channels">
              <thead>
                <tr>
                  <th>通道</th>
                  <th>当前分片</th>
                  <th className="col-progress">分片进度</th>
                  <th>速度</th>
                  <th>状态</th>
                </tr>
              </thead>
              <tbody>
                {task.channels.map((channel) => {
                  const channelPercent = percent(channel.segmentDownloaded, channel.segmentTotal);
                  return (
                    <tr key={channel.id}>
                      <td className="mono">#{channel.id + 1}</td>
                      <td className="mono">
                        {channel.segmentIndex === null
                          ? '--'
                          : `#${channel.segmentIndex + 1} · ${formatRange(channel.segmentStart, channel.segmentEnd)}`}
                      </td>
                      <td>
                        <TotalBar ratio={channelPercent} state={channel.state} variant="channel" />
                        <span className="mono small">
                          {channel.segmentTotal > 0
                            ? `${formatBytes(channel.segmentDownloaded)} / ${formatBytes(channel.segmentTotal)}`
                            : formatBytes(channel.segmentDownloaded)}
                        </span>
                      </td>
                      <td className="mono">{formatSpeed(channel.speedBps)}</td>
                      <td>
                        <StatusChip
                          variant="channel"
                          state={channel.state}
                          label={CHANNEL_STATE_LABEL[channel.state]}
                          spinner={channel.state === 'connecting' || channel.state === 'retrying'}
                        />
                        {channel.retries > 0 && <span className="retry">重试 {channel.retries}</span>}
                      </td>
                    </tr>
                  );
                })}
              </tbody>
            </table>
          )}

          <ProgressMatrix task={task} />
        </div>
      )}
    </li>
  );
}
