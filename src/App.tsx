import { useEffect, useState } from 'react';

import './App.css';
import { addTask, defaultDownloadDir, listTasks, onTaskRemoved, onTaskUpdate } from './api';
import { TaskItem } from './components/TaskItem';
import type { TaskView } from './types';

function upsert(list: TaskView[], view: TaskView): TaskView[] {
  const index = list.findIndex((task) => task.id === view.id);
  if (index === -1) return [...list, view];
  const next = list.slice();
  next[index] = view;
  return next;
}

export default function App() {
  const [tasks, setTasks] = useState<TaskView[]>([]);
  const [urls, setUrls] = useState('');
  const [dir, setDir] = useState('');
  const [connections, setConnections] = useState(4);
  const [message, setMessage] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    let disposed = false;
    const unlisten: Array<() => void> = [];

    (async () => {
      try {
        const [initial, defaultDir] = await Promise.all([listTasks(), defaultDownloadDir()]);
        if (disposed) return;
        setTasks(initial);
        setDir((current) => current || defaultDir);
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

  return (
    <div className="app">
      <header className="app-header">
        <div className="brand">
          <span className="brand-name">川流</span>
          <span className="brand-en">Conflux</span>
          <span className="brand-tag">多协议下载工具</span>
        </div>

        <div className="form">
          <textarea
            className="input-url"
            placeholder="粘贴下载链接，支持一次多行批量添加"
            value={urls}
            rows={2}
            onChange={(event) => setUrls(event.target.value)}
          />
          <div className="form-row">
            <input
              className="input-dir"
              value={dir}
              onChange={(event) => setDir(event.target.value)}
              placeholder="保存目录"
            />
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
            <button className="primary" onClick={handleAdd} disabled={busy}>
              {busy ? '添加中…' : '开始下载'}
            </button>
          </div>
        </div>

        {message && (
          <div className="message">
            {message}
            <button className="ghost" onClick={() => setMessage(null)}>
              关闭
            </button>
          </div>
        )}
      </header>

      <main className="task-list">
        {tasks.length === 0 ? (
          <div className="empty">还没有下载任务，粘贴链接后点击「开始下载」</div>
        ) : (
          <ul className="tasks">
            {tasks.map((task) => (
              <TaskItem key={task.id} task={task} onError={setMessage} />
            ))}
          </ul>
        )}
      </main>
    </div>
  );
}
