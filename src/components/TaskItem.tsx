import { useEffect, useState } from 'react';

import { cancelTask, pauseTask, removeTask, resumeTask, revealInDir } from '../api';
import { formatBytes, formatDuration, formatRange, formatSpeed, percent } from '../format';
import { CHANNEL_STATE_LABEL, TASK_STATUS_LABEL, type TaskView } from '../types';

interface ConfirmButtonProps {
  label: string;
  confirmLabel: string;
  className?: string;
  onConfirm: () => void;
}

/** 二次确认按钮：Tauri 的 WebView 不保证支持 window.confirm，因此放在应用内实现 */
function ConfirmButton({ label, confirmLabel, className, onConfirm }: ConfirmButtonProps) {
  const [armed, setArmed] = useState(false);

  useEffect(() => {
    if (!armed) return;
    const timer = setTimeout(() => setArmed(false), 4000);
    return () => clearTimeout(timer);
  }, [armed]);

  return (
    <button
      className={className}
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
    </button>
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
  const showDetails = task.channels.length > 0;

  return (
    <li className="task">
      <div className="task-head">
        <div className="task-title">
          <span className="task-name" title={task.savePath}>
            {task.fileName}
          </span>
          <span className={`chip chip-${task.status}`}>{TASK_STATUS_LABEL[task.status]}</span>
          <span className="spacer" />
          <span className="task-percent">
            {overall === null ? '总大小未知' : `${overall.toFixed(1)}%`}
          </span>
        </div>
        <span className="task-host">{task.url}</span>
      </div>

      <div className="bar bar-total">
        <div
          className={`bar-fill bar-fill-${task.status}`}
          style={{ width: `${overall ?? 0}%` }}
        />
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
        {active && <button onClick={() => run(() => pauseTask(task.id))}>暂停</button>}
        {resumable && <button onClick={() => run(() => resumeTask(task.id))}>继续</button>}
        {active && (
          <button className="ghost" onClick={() => run(() => cancelTask(task.id, false))}>
            取消
          </button>
        )}
        {hasFile && (
          <button className="ghost" onClick={() => run(() => revealInDir(task.savePath))}>
            打开所在目录
          </button>
        )}
        {showDetails && (
          <button className="ghost" onClick={() => setExpanded((value) => !value)}>
            {expanded ? '收起详情' : `展开详情（${task.channels.length} 通道）`}
          </button>
        )}
        <span className="spacer" />
        <button className="ghost" onClick={() => run(() => removeTask(task.id, false))}>
          移除
        </button>
        <ConfirmButton
          className="ghost danger"
          label="删除文件"
          confirmLabel="确认删除？"
          onConfirm={() => run(() => removeTask(task.id, true))}
        />
      </div>

      {expanded && showDetails && (
        <div className="details">
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
                      <div className="bar bar-channel">
                        <div
                          className={`bar-fill bar-fill-${channel.state}`}
                          style={{ width: `${channelPercent ?? 0}%` }}
                        />
                      </div>
                      <span className="mono small">
                        {channel.segmentTotal > 0
                          ? `${formatBytes(channel.segmentDownloaded)} / ${formatBytes(channel.segmentTotal)}`
                          : formatBytes(channel.segmentDownloaded)}
                      </span>
                    </td>
                    <td className="mono">{formatSpeed(channel.speedBps)}</td>
                    <td>
                      <span className={`chip chip-channel-${channel.state}`}>
                        {CHANNEL_STATE_LABEL[channel.state]}
                      </span>
                      {channel.retries > 0 && <span className="retry">重试 {channel.retries}</span>}
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>

          {task.segments.length > 1 && (
            <>
              <div className="details-title">文件分片地图（每个方块是一个分片）</div>
              <div className="segments">
                {task.segments.map((segment) => (
                  <div
                    key={segment.index}
                    className={`segment segment-${segment.state}`}
                    title={`分片 #${segment.index + 1}　${formatRange(segment.start, segment.end)}　${formatBytes(
                      segment.downloaded,
                    )} / ${formatBytes(segment.total)}`}
                  />
                ))}
              </div>
            </>
          )}
        </div>
      )}
    </li>
  );
}
