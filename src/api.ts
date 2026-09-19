import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';

import type { StorageInfo, TaskView } from './types';

export const addTask = (url: string, dir: string | null, connections: number) =>
  invoke<string>('add_task', { url, dir, connections });

export const pauseTask = (id: string) => invoke<void>('pause_task', { id });

export const resumeTask = (id: string) => invoke<void>('resume_task', { id });

export const cancelTask = (id: string, deleteFile = false) =>
  invoke<void>('cancel_task', { id, deleteFile });

export const removeTask = (id: string, deleteFile = false) =>
  invoke<void>('remove_task', { id, deleteFile });

export const listTasks = () => invoke<TaskView[]>('list_tasks');

export const defaultDownloadDir = () => invoke<string>('default_download_dir');

export const storageInfo = () => invoke<StorageInfo>('storage_info');

/** 实际生效的窗口材质：vibrancy / mica / acrylic / blur / none */
export const windowMaterial = () => invoke<string>('window_material');

export const onTaskUpdate = (handler: (task: TaskView) => void) =>
  listen<TaskView>('task:update', (event) => handler(event.payload));

export const onTaskRemoved = (handler: (id: string) => void) =>
  listen<string>('task:removed', (event) => handler(event.payload));

export const revealInDir = async (path: string) => {
  const { revealItemInDir } = await import('@tauri-apps/plugin-opener');
  await revealItemInDir(path);
};

/** 调用系统目录选择框；用户取消时返回 null */
export const pickDirectory = async (defaultPath: string) => {
  const { open } = await import('@tauri-apps/plugin-dialog');
  const picked = await open({
    title: '选择保存目录',
    directory: true,
    multiple: false,
    defaultPath: defaultPath || undefined,
  });
  return typeof picked === 'string' ? picked : null;
};
