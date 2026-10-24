import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';

import type {
  BtProxySetting,
  Ed2kSearchResult,
  StorageInfo,
  TaskView,
  TorrentPreview,
  TrayAction,
} from './types';

/** `files` 是预览后勾选的文件序号，null 表示全都下 */
export const addTask = (
  url: string,
  dir: string | null,
  connections: number,
  files: number[] | null = null,
) => invoke<string>('add_task', { url, dir, connections, files });

export const addTorrentFile = (
  path: string,
  dir: string | null,
  connections: number,
  files: number[] | null = null,
) => invoke<string>('add_torrent_file', { path, dir, connections, files });

/** 预览磁力：只取元数据看文件清单，不建任务也不落盘 */
export const previewMagnet = (magnet: string) => invoke<TorrentPreview>('preview_magnet', { magnet });

/** 预览本地种子文件：纯本地解析，不用联网 */
export const previewTorrentFile = (path: string) =>
  invoke<TorrentPreview>('preview_torrent_file', { path });

/** 搜 ed2k：由侧车同时问 eD2k 服务器与 Kad，等两边回完一轮才返回 */
export const searchEd2k = (query: string) => invoke<Ed2kSearchResult[]>('search_ed2k', { query });

/** 改任务勾选的文件；传全部序号等于「全选」 */
export const setTaskFiles = (id: string, files: number[]) =>
  invoke<void>('set_task_files', { id, files });

export const pauseTask = (id: string) => invoke<void>('pause_task', { id });

export const resumeTask = (id: string) => invoke<void>('resume_task', { id });

export const cancelTask = (id: string, deleteFile = false) =>
  invoke<void>('cancel_task', { id, deleteFile });

export const removeTask = (id: string, deleteFile = false) =>
  invoke<void>('remove_task', { id, deleteFile });

export const listTasks = () => invoke<TaskView[]>('list_tasks');

export const defaultDownloadDir = () => invoke<string>('default_download_dir');

export const storageInfo = () => invoke<StorageInfo>('storage_info');

/** 实际生效的窗口材质：glass / vibrancy / mica / acrylic / blur / none */
export const windowMaterial = () => invoke<string>('window_material');

/** 设置所有任务合计的连接数上限（后端的全局闸门），返回夹取后真正生效的值 */
export const setMaxConnections = (limit: number) =>
  invoke<number>('set_max_connections', { limit });

/** 显示／隐藏状态栏图标（macOS 菜单栏、Windows 通知区域） */
export const setTrayEnabled = (enabled: boolean) =>
  invoke<void>('set_tray_enabled', { enabled });

/** 让 Dock 图标跟随界面深浅色（macOS 的 .icns 不会自己切） */
export const setAppIcon = (dark: boolean) => invoke<void>('set_app_icon', { dark });

/** 开启本机命令行服务，让 conflux-cli 能提交任务 */
export const setCliEnabled = (enabled: boolean) =>
  invoke<void>('set_cli_enabled', { enabled });

/** 关窗时缩到状态栏（true）还是退出应用（false） */
export const setCloseToTray = (enabled: boolean) =>
  invoke<void>('set_close_to_tray', { enabled });

/** 托盘菜单的动作：点了某个任务，或点了「新建任务…」 */
export const onTrayAction = (handler: (action: TrayAction) => void): Promise<UnlistenFn> =>
  listen<TrayAction>('tray:action', (event) => handler(event.payload));

/** 设置 BT 的 SOCKS5 代理（peer 连接与 tracker 请求走它）；空串表示关闭代理 */
export const setBtProxy = (url: string) => invoke<BtProxySetting>('set_bt_proxy', { url });

/**
 * 设置窗口外观：原生材质（玻璃／磨砂）与 WebView 的 prefers-color-scheme 都跟着它走，
 * 传 null 表示跟随系统。
 */
export const setWindowTheme = async (theme: 'light' | 'dark' | null) => {
  const { getCurrentWindow } = await import('@tauri-apps/api/window');
  await getCurrentWindow().setTheme(theme);
};

export const onTaskUpdate = (handler: (task: TaskView) => void): Promise<UnlistenFn> =>
  listen<TaskView>('task:update', (event) => handler(event.payload));

export const onTaskRemoved = (handler: (id: string) => void): Promise<UnlistenFn> =>
  listen<string>('task:removed', (event) => handler(event.payload));

export const revealInDir = async (path: string) => {
  const { revealItemInDir } = await import('@tauri-apps/plugin-opener');
  await revealItemInDir(path);
};

/** 调用系统目录选择框；用户取消时返回 null */
/** 选择本地 .torrent 种子文件（单选），默认从保存目录开始找 */
export const pickTorrentFile = async (defaultPath = ''): Promise<string | null> => {
  const { open } = await import('@tauri-apps/plugin-dialog');
  const picked = await open({
    title: '选择种子文件',
    multiple: false,
    defaultPath: defaultPath || undefined,
    filters: [{ name: 'BitTorrent 种子', extensions: ['torrent'] }],
  });
  return typeof picked === 'string' ? picked : null;
};

export const pickDirectory = async (defaultPath: string): Promise<string | null> => {
  const { open } = await import('@tauri-apps/plugin-dialog');
  const picked = await open({
    title: '选择保存目录',
    directory: true,
    multiple: false,
    defaultPath: defaultPath || undefined,
  });
  return typeof picked === 'string' ? picked : null;
};
