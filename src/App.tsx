import { useEffect, useMemo, useState } from 'react';

import './App.css';
import {
  addTask,
  defaultDownloadDir,
  listTasks,
  onTaskRemoved,
  onTaskUpdate,
  pickDirectory,
  storageInfo,
  windowMaterial,
} from './api';
import { FILTERS, Sidebar, type FilterKey } from './components/Sidebar';
import { Button, TextField } from './components/md';
import { TaskItem } from './components/TaskItem';
import { StyleModeContext, type StyleMode } from './style';
import type { StorageInfo, TaskView } from './types';

function upsert(list: TaskView[], view: TaskView): TaskView[] {
  const index = list.findIndex((task) => task.id === view.id);
  if (index === -1) return [...list, view];
  const next = list.slice();
  next[index] = view;
  return next;
}

export default function App() {
  const [tasks, setTasks] = useState<TaskView[]>([]);
  const [filter, setFilter] = useState<FilterKey>('all');
  const [sidebarOpen, setSidebarOpen] = useState(
    () => localStorage.getItem('conflux.sidebar') !== 'collapsed',
  );
  const [styleMode, setStyleMode] = useState<StyleMode>(
    () => (localStorage.getItem('conflux.style') as StyleMode | null) ?? 'ios',
  );
  const [urls, setUrls] = useState('');
  const [dir, setDir] = useState('');
  const [connections, setConnections] = useState(4);
  const [message, setMessage] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [storage, setStorage] = useState<StorageInfo | null>(null);

  const changeStyle = (mode: StyleMode) => {
    // 属性同步改掉：画布类组件在渲染期读它取色，晚一拍会读到旧皮肤
    document.documentElement.dataset.style = mode;
    localStorage.setItem('conflux.style', mode);
    setStyleMode(mode);
  };

  useEffect(() => {
    // 折叠后红黄绿灯落在内容区上，需要让内容顶部避开它们
    document.documentElement.classList.toggle('sidebar-hidden', !sidebarOpen);
    localStorage.setItem('conflux.sidebar', sidebarOpen ? 'expanded' : 'collapsed');
  }, [sidebarOpen]);

  // 平台与窗口材质决定侧边栏是磨砂还是纯色、要不要给红黄绿灯留位置
  useEffect(() => {
    const root = document.documentElement;
    root.dataset.platform = /mac/i.test(navigator.userAgent) ? 'macos' : 'windows';
    windowMaterial()
      .then((material) => {
        root.dataset.material = material;
      })
      .catch(() => {
        root.dataset.material = 'none';
      });
  }, []);

  useEffect(() => {
    let disposed = false;
    const unlisten: Array<() => void> = [];

    (async () => {
      try {
        const [initial, defaultDir, info] = await Promise.all([
          listTasks(),
          defaultDownloadDir(),
          storageInfo(),
        ]);
        if (disposed) return;
        setTasks(initial);
        setDir((current) => current || defaultDir);
        setStorage(info);
        unlisten.push(await onTaskUpdate((view) => setTasks((prev) => upsert(prev, view))));
        unlisten.push(await onTaskRemoved((id) => setTasks((prev) => prev.filter((task) => task.id !== id))));
      } catch (error) {
        setMessage(String(error));
      }
    })();

    return () => {
      disposed = true;
      unlisten.forEach((off) => off());
    };
  }, []);

  const visible = useMemo(() => {
    const active = FILTERS.find((item) => item.key === filter);
    if (!active || filter === 'all') return tasks;
    return tasks.filter((task) => active.match(task.status));
  }, [tasks, filter]);

  const handleAdd = async () => {
    const links = urls
      .split(/[\s]+/)
      .map((link) => link.trim())
      .filter(Boolean);
    if (links.length === 0) {
      setMessage('请先输入下载链接');
      return;
    }
    setBusy(true);
    setMessage(null);
    const failures: string[] = [];
    for (const link of links) {
      try {
        await addTask(link, dir, connections);
      } catch (error) {
        failures.push(`${link}：${String(error)}`);
      }
    }
    setBusy(false);
    setUrls('');
    if (failures.length > 0) {
      setMessage(failures.join('\n'));
    }
  };

  const handlePickDir = async () => {
    try {
      const picked = await pickDirectory(dir);
      if (picked) setDir(picked);
    } catch (error) {
      setMessage(String(error));
    }
  };

  return (
    <StyleModeContext.Provider value={styleMode}>
    <div className="app">
      {/* macOS 隐藏标题栏后由这条顶部带承担跨全宽的窗口拖动，与系统标题栏位置一致 */}
      <div className="titlebar-drag" data-tauri-drag-region />

      <Sidebar
        tasks={tasks}
        filter={filter}
        onFilter={setFilter}
        storage={storage}
        collapsed={!sidebarOpen}
        styleMode={styleMode}
        onStyleMode={changeStyle}
      />

      <main className="content">
        <div className="content-inner">
          <header className="app-header">
            <div className="form">
              <TextField
                type="textarea"
                value={urls}
                onChange={setUrls}
                placeholder="粘贴下载链接，支持一次多行批量添加"
              />
              <div className="form-row">
                <Button
                  variant="icon"
                  title={sidebarOpen ? '收起侧边栏' : '展开侧边栏'}
                  onClick={() => setSidebarOpen((open) => !open)}
                >
                  {sidebarOpen ? '⟨' : '☰'}
                </Button>
                <TextField
                  className="input-dir"
                  value={dir}
                  onChange={setDir}
                  placeholder="保存目录"
                />
                <Button onClick={handlePickDir}>选择…</Button>
                <label className="connections">
                  连接数
                  <input
                    type="number"
                    min={1}
                    max={16}
                    value={connections}
                    onChange={(event) => setConnections(Math.min(16, Math.max(1, Number(event.target.value) || 1)))}
                  />
                </label>
                <Button variant="filled" onClick={handleAdd} disabled={busy}>
                  {busy ? '添加中…' : '开始下载'}
                </Button>
              </div>
            </div>

            {message && (
              <div className="message">
                {message}
                <Button onClick={() => setMessage(null)}>关闭</Button>
              </div>
            )}
          </header>

          <div className="task-list">
            {visible.length === 0 ? (
              <div className="empty">
                <div className="empty-glyph">↓</div>
                <div>
                  {tasks.length === 0
                    ? '还没有下载任务，粘贴链接后点击「开始下载」'
                    : '该分类下暂无任务'}
                </div>
              </div>
            ) : (
              <ul className="tasks">
                {visible.map((task) => (
                  <TaskItem key={task.id} task={task} onError={setMessage} />
                ))}
              </ul>
            )}
          </div>
        </div>
      </main>
    </div>
    </StyleModeContext.Provider>
  );
}
