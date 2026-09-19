import { formatSpeed } from '../format';
import type { StorageInfo, TaskStatus, TaskView } from '../types';

export type FilterKey = 'all' | 'active' | 'paused' | 'completed' | 'failed';

interface FilterDef {
  key: FilterKey;
  label: string;
  match: (status: TaskStatus) => boolean;
}

export const FILTERS: FilterDef[] = [
  { key: 'all', label: '全部任务', match: () => true },
  {
    key: 'active',
    label: '进行中',
    match: (status) => status === 'queued' || status === 'probing' || status === 'downloading',
  },
  { key: 'paused', label: '已暂停', match: (status) => status === 'paused' },
  { key: 'completed', label: '已完成', match: (status) => status === 'completed' },
  {
    key: 'failed',
    label: '失败 / 取消',
    match: (status) => status === 'failed' || status === 'cancelled',
  },
];

interface SidebarProps {
  tasks: TaskView[];
  filter: FilterKey;
  onFilter: (key: FilterKey) => void;
  storage: StorageInfo | null;
  collapsed: boolean;
}

export function Sidebar({ tasks, filter, onFilter, storage, collapsed }: SidebarProps) {
  const totalSpeed = tasks.reduce((sum, task) => sum + task.speedBps, 0);

  return (
    <aside className={collapsed ? 'sidebar sidebar-collapsed' : 'sidebar'}>
      {/* macOS 隐藏标题栏后给红黄绿灯让出的空间，窗口拖动由 App 里的顶部拖拽带负责 */}
      <div className="sidebar-top" />

      <div className="brand">
        <span className="brand-name">川流</span>
        <span className="brand-en">Conflux</span>
        <span className="brand-tag">多协议下载工具</span>
      </div>

      <nav className="filters">
        {FILTERS.map((item) => {
          const count = tasks.filter((task) => item.match(task.status)).length;
          return (
            <button
              key={item.key}
              className={item.key === filter ? 'filter filter-active' : 'filter'}
              onClick={() => onFilter(item.key)}
            >
              <span>{item.label}</span>
              <span className="filter-count">{count}</span>
            </button>
          );
        })}
      </nav>

      {totalSpeed > 0 && (
        <div className="sidebar-speed">
          <span className="sidebar-speed-label">
            <span className="pulse-dot" />
            总速度
          </span>
          <span className="mono">{formatSpeed(totalSpeed)}</span>
        </div>
      )}

      <div className="spacer" />

      <div className="sidebar-foot">
        {storage ? (
          storage.available ? (
            <>
              <div>任务数据：结构版本 {storage.schemaVersion}</div>
              <div className="path" title={storage.path ?? ''}>
                {storage.path}
              </div>
            </>
          ) : (
            <>
              <div className="warn">{storage.message ?? '未启用任务持久化'}</div>
              {storage.path && (
                <div className="path" title={storage.path}>
                  预期位置 {storage.path}
                </div>
              )}
            </>
          )
        ) : (
          <div>读取存储状态…</div>
        )}
      </div>
    </aside>
  );
}
