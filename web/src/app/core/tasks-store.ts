import { Injectable, computed, signal } from '@angular/core';

import { listTasks, onTaskRemoved, onTaskUpdate, storageInfo } from './api';
import type { IconName } from './icon';
import type { StorageInfo, TaskStatus, TaskView } from './types';

export type FilterKey = 'all' | 'active' | 'paused' | 'completed' | 'failed';

interface FilterDef {
  key: FilterKey;
  label: string;
  icon: IconName;
  match: (status: TaskStatus) => boolean;
}

export const FILTERS: FilterDef[] = [
  { key: 'all', label: '全部任务', icon: 'list', match: () => true },
  {
    key: 'active',
    label: '进行中',
    icon: 'download',
    match: (status) => status === 'queued' || status === 'probing' || status === 'downloading',
  },
  { key: 'paused', label: '已暂停', icon: 'pause', match: (status) => status === 'paused' },
  { key: 'completed', label: '已完成', icon: 'check', match: (status) => status === 'completed' },
  {
    key: 'failed',
    label: '失败 / 取消',
    icon: 'error',
    match: (status) => status === 'failed' || status === 'cancelled',
  },
];

@Injectable({ providedIn: 'root' })
export class TasksStore {
  readonly tasks = signal<TaskView[]>([]);
  readonly storage = signal<StorageInfo | null>(null);
  readonly filter = signal<FilterKey>('all');
  readonly error = signal<string | null>(null);
  readonly loaded = signal(false);

  readonly counts = computed(() => {
    const tasks = this.tasks();
    return FILTERS.map((item) => ({
      key: item.key,
      label: item.label,
      icon: item.icon,
      count: tasks.filter((task) => item.match(task.status)).length,
    }));
  });

  readonly visible = computed(() => {
    const key = this.filter();
    if (key === 'all') return this.tasks();
    const active = FILTERS.find((item) => item.key === key);
    if (!active) return this.tasks();
    return this.tasks().filter((task) => active.match(task.status));
  });

  readonly totalSpeed = computed(() =>
    this.tasks().reduce((sum, task) => sum + task.speedBps, 0),
  );

  constructor() {
    void this.init();
  }

  private async init() {
    try {
      const [initial, storage] = await Promise.all([listTasks(), storageInfo()]);
      this.tasks.set(initial);
      this.storage.set(storage);
      this.loaded.set(true);
      await onTaskUpdate((view) => this.upsert(view));
      await onTaskRemoved((id) => this.tasks.update((list) => list.filter((task) => task.id !== id)));
    } catch (error) {
      this.error.set(String(error));
    }
  }

  clearError() {
    this.error.set(null);
  }

  private upsert(view: TaskView) {
    this.tasks.update((list) => {
      const index = list.findIndex((task) => task.id === view.id);
      if (index === -1) return [...list, view];
      const next = list.slice();
      next[index] = view;
      return next;
    });
  }
}
