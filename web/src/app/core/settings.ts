import { Injectable, computed, effect, signal } from '@angular/core';

import {
  setAppIcon,
  setBtProxy,
  setCliEnabled,
  setCloseToTray,
  setMaxConnections,
  setTrayEnabled,
  setWindowTheme,
} from './api';

export interface AccentOption {
  key: string;
  label: string;
}

/** 与 styles.scss 里 `html[data-accent=…]` 的取值一一对应 */
export const ACCENTS: AccentOption[] = [
  { key: 'violet', label: '紫罗兰' },
  { key: 'azure', label: '靛蓝' },
  { key: 'cyan', label: '湖蓝' },
  { key: 'spring-green', label: '青绿' },
  { key: 'orange', label: '琥珀' },
  { key: 'rose', label: '玫红' },
];

export type ThemeMode = 'system' | 'light' | 'dark';

/** label 是分段控件里显示的短标签（侧边栏窄），full 用于悬停提示与无障碍 */
export const THEME_MODES: Array<{ key: ThemeMode; label: string; full: string }> = [
  { key: 'light', label: '浅色', full: '浅色外观' },
  { key: 'dark', label: '深色', full: '深色外观' },
  { key: 'system', label: '系统', full: '跟随系统外观' },
];

const KEY_DIR = 'conflux.dir';
const KEY_CONNECTIONS = 'conflux.connections';
const KEY_MAX_TASK_CONNECTIONS = 'conflux.maxTaskConnections';
const KEY_MAX_TOTAL_CONNECTIONS = 'conflux.maxTotalConnections';
const KEY_BT_PROXY = 'conflux.btProxy';
const KEY_ACCENT = 'conflux.accent';
const KEY_THEME = 'conflux.theme';
const KEY_TRAY = 'conflux.trayIcon';
const KEY_CLOSE_TO_TRAY = 'conflux.closeToTray';
const KEY_CLI = 'conflux.cli';
const KEY_AUTO_UPDATE = 'conflux.autoUpdate';

/** 新任务默认开的连接数，和 Rust 侧 DownloadManager 的默认值保持一致 */
export const DEFAULT_CONNECTIONS = 8;
/** 单个任务的通道数硬上限，与 Rust 侧 Task::new / persist 的夹取保持一致 */
export const TASK_CONNECTIONS_LIMIT = 32;
/** 单任务上限的默认值；用户可以在设置里往上加 */
export const DEFAULT_TASK_CONNECTIONS_LIMIT = 12;
/** 全局上限的上限：3 个任务同时跑、每个开满单任务上限 */
export const TOTAL_CONNECTIONS_LIMIT = TASK_CONNECTIONS_LIMIT * 3;
/** 全局上限的默认值：够两个任务各开 8 条，再多的任务按通道排队 */
export const DEFAULT_TOTAL_CONNECTIONS = 16;

const clamp = (value: number, max = TASK_CONNECTIONS_LIMIT) =>
  Math.min(max, Math.max(1, Math.round(value) || 1));

const darkQuery = window.matchMedia('(prefers-color-scheme: dark)');

/** 应用设置：只存本机（localStorage），新建任务弹窗拿它当初值 */
@Injectable({ providedIn: 'root' })
export class Settings {
  readonly downloadDir = signal(localStorage.getItem(KEY_DIR) ?? '');
  readonly connections = signal(
    clamp(Number(localStorage.getItem(KEY_CONNECTIONS)) || DEFAULT_CONNECTIONS),
  );
  readonly maxTaskConnections = signal(
    clamp(
      Number(localStorage.getItem(KEY_MAX_TASK_CONNECTIONS)) || DEFAULT_TASK_CONNECTIONS_LIMIT,
    ),
  );
  readonly maxTotalConnections = signal(
    clamp(
      Number(localStorage.getItem(KEY_MAX_TOTAL_CONNECTIONS)) || DEFAULT_TOTAL_CONNECTIONS,
      TOTAL_CONNECTIONS_LIMIT,
    ),
  );
  readonly accent = signal(localStorage.getItem(KEY_ACCENT) ?? ACCENTS[0].key);
  readonly themeMode = signal<ThemeMode>(readStoredTheme());
  /** 是否在状态栏（macOS 菜单栏 / Windows 通知区域）显示图标；默认开启 */
  readonly trayIcon = signal(localStorage.getItem(KEY_TRAY) !== '0');
  /** 关闭主窗口时缩到状态栏继续下载；默认开启 */
  readonly closeToTray = signal(localStorage.getItem(KEY_CLOSE_TO_TRAY) !== '0');
  /** 是否允许本机 conflux-cli 提交任务；默认关闭 */
  readonly cliEnabled = signal(localStorage.getItem(KEY_CLI) === '1');
  /** 启动时自动检查 GitHub Releases 更新；默认开启 */
  readonly autoUpdate = signal(localStorage.getItem(KEY_AUTO_UPDATE) !== '0');
  readonly systemDark = signal(darkQuery.matches);
  /** BT 的 SOCKS5 代理；空串表示不用代理 */
  readonly btProxy = signal(localStorage.getItem(KEY_BT_PROXY) ?? '');
  /** 后端拒绝这次代理设置时的原因，显示在输入框下面 */
  readonly btProxyError = signal<string | null>(null);

  /** 新任务的连接数：默认值，但不越过单任务上限 */
  readonly taskConnections = computed(() =>
    Math.min(this.connections(), this.maxTaskConnections()),
  );

  /** 实际生效的外观：跟随系统时看系统，否则看用户选择 */
  readonly theme = computed<'light' | 'dark'>(() => {
    const mode = this.themeMode();
    if (mode === 'system') return this.systemDark() ? 'dark' : 'light';
    return mode;
  });

  constructor() {
    darkQuery.addEventListener('change', (event) => this.systemDark.set(event.matches));

    effect(() => localStorage.setItem(KEY_DIR, this.downloadDir()));
    effect(() => localStorage.setItem(KEY_CONNECTIONS, String(this.connections())));
    effect(() =>
      localStorage.setItem(KEY_MAX_TASK_CONNECTIONS, String(this.maxTaskConnections())),
    );
    effect(() =>
      localStorage.setItem(KEY_MAX_TOTAL_CONNECTIONS, String(this.maxTotalConnections())),
    );
    // 总连接数上限由后端的全局闸门执行：开局推一次，之后每次改动都推
    effect(() => {
      void setMaxConnections(this.maxTotalConnections()).catch(() => undefined);
    });
    effect(() => localStorage.setItem(KEY_BT_PROXY, this.btProxy()));
    // 托盘由后端持有，本机设置只负责在启动和切换时把开关推过去
    effect(() => {
      const enabled = this.trayIcon();
      localStorage.setItem(KEY_TRAY, enabled ? '1' : '0');
      void setTrayEnabled(enabled).catch(() => undefined);
    });
    effect(() => {
      const enabled = this.closeToTray();
      localStorage.setItem(KEY_CLOSE_TO_TRAY, enabled ? '1' : '0');
      void setCloseToTray(enabled).catch(() => undefined);
    });
    effect(() => {
      const enabled = this.cliEnabled();
      localStorage.setItem(KEY_CLI, enabled ? '1' : '0');
      void setCliEnabled(enabled).catch(() => undefined);
    });
    effect(() => localStorage.setItem(KEY_AUTO_UPDATE, this.autoUpdate() ? '1' : '0'));
    // BT 代理存在后端内存里、建会话时生效；开局把本机保存的值推一次（空值推了也无妨）。
    // 这一步失败不提示：只有用户主动保存时，才需要把原因显示在设置里。
    void this.saveBtProxy(this.btProxy(), { report: false });
    effect(() => {
      const accent = this.accent();
      document.documentElement.dataset['accent'] = accent;
      localStorage.setItem(KEY_ACCENT, accent);
    });
    effect(() => {
      const theme = this.theme();
      const root = document.documentElement;
      root.dataset['theme'] = theme;
      root.style.colorScheme = theme;
      localStorage.setItem(KEY_THEME, this.themeMode());
      // 原生材质与 WebView 的 prefers-color-scheme 都挂在窗口外观上
      void setWindowTheme(this.themeMode() === 'system' ? null : theme).catch(() => undefined);
      // Dock 图标用浅/深两套图：macOS 不会按外观切 .icns，启动和换主题时主动换
      void setAppIcon(theme === 'dark').catch(() => undefined);
    });
  }

  setDownloadDir(dir: string) {
    this.downloadDir.set(dir);
  }

  setConnections(value: number) {
    this.connections.set(clamp(Math.min(value, this.maxTaskConnections())));
  }

  setMaxTaskConnections(value: number) {
    const limit = clamp(value);
    this.maxTaskConnections.set(limit);
    // 默认连接数不能高过单任务上限，否则弹窗得默默改小，不如这里就对齐
    if (this.connections() > limit) this.connections.set(limit);
  }

  setMaxTotalConnections(value: number) {
    this.maxTotalConnections.set(clamp(value, TOTAL_CONNECTIONS_LIMIT));
  }

  setAccent(key: string) {
    this.accent.set(key);
  }

  setThemeMode(mode: ThemeMode) {
    this.themeMode.set(mode);
  }

  setTrayIcon(enabled: boolean) {
    this.trayIcon.set(enabled);
  }

  setCloseToTray(enabled: boolean) {
    this.closeToTray.set(enabled);
  }

  setCliEnabled(enabled: boolean) {
    this.cliEnabled.set(enabled);
  }

  setAutoUpdate(enabled: boolean) {
    this.autoUpdate.set(enabled);
  }

  /**
   * 保存 BT 代理：后端按 librqbit 的规则校验（只认 socks5://），
   * 通过后把归一化结果写回本机设置。返回是否保存成功、以及是否要重启才生效。
   * `report: false` 表示失败也不打扰用户（启动时把已存的值推给后端用）。
   */
  async saveBtProxy(
    raw: string,
    options: { report?: boolean } = {},
  ): Promise<{ saved: boolean; needsRestart: boolean }> {
    try {
      const state = await setBtProxy(raw);
      this.btProxy.set(state.value ?? '');
      this.btProxyError.set(null);
      return { saved: true, needsRestart: state.sessionRunning };
    } catch (error) {
      if (options.report !== false) this.btProxyError.set(String(error));
      return { saved: false, needsRestart: false };
    }
  }
}

function readStoredTheme(): ThemeMode {
  const stored = localStorage.getItem(KEY_THEME);
  return stored === 'light' || stored === 'dark' || stored === 'system' ? stored : 'system';
}
