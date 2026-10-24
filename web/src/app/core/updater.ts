import { Injectable, computed, inject, signal } from '@angular/core';
import { getVersion } from '@tauri-apps/api/app';
import { relaunch } from '@tauri-apps/plugin-process';
import { check, type Update } from '@tauri-apps/plugin-updater';

import { Settings } from './settings';

/** 从 GitHub Releases 检查并安装更新；开发态没有安装包，不自动检查 */
@Injectable({ providedIn: 'root' })
export class Updater {
  private readonly settings = inject(Settings);

  readonly currentVersion = signal<string | null>(null);
  readonly availableVersion = signal<string | null>(null);
  readonly notes = signal<string | null>(null);
  readonly checking = signal(false);
  readonly installing = signal(false);
  readonly checked = signal(false);
  readonly error = signal<string | null>(null);
  readonly downloaded = signal(0);
  readonly totalBytes = signal<number | null>(null);
  readonly readyToRestart = signal(false);

  readonly progress = computed(() => {
    const total = this.totalBytes();
    if (!total) return null;
    return Math.min(100, Math.round((this.downloaded() / total) * 100));
  });

  private update: Update | null = null;

  constructor() {
    void this.loadVersion();
    // dev 用 http://localhost:4300 加载，生产包走 tauri:// 或 http://tauri.localhost，没有端口；
    // 开发态装更新没有意义，也会把开发目录弄乱
    if (this.settings.autoUpdate() && !window.location.port) {
      window.setTimeout(() => void this.check(false), 2000);
    }
  }

  private async loadVersion() {
    try {
      this.currentVersion.set(await getVersion());
    } catch {
      // 浏览器预览里没有 Tauri
    }
  }

  async check(manual = true) {
    if (this.checking() || this.installing()) return;
    this.checking.set(true);
    this.error.set(null);
    try {
      const update = await check();
      if (this.update) {
        await this.update.close().catch(() => undefined);
      }
      this.update = update;
      this.checked.set(true);
      this.readyToRestart.set(false);
      if (update) {
        this.availableVersion.set(update.version);
        this.notes.set(update.body ?? null);
      } else {
        this.availableVersion.set(null);
        this.notes.set(null);
      }
    } catch (error) {
      // 自动检查失败不打扰用户（例如暂时断网、首个更新版还没发布）
      if (manual) {
        this.error.set(String(error));
        this.checked.set(true);
      }
    } finally {
      this.checking.set(false);
    }
  }

  async install() {
    const update = this.update;
    if (!update || this.installing()) return;
    this.installing.set(true);
    this.error.set(null);
    this.downloaded.set(0);
    this.totalBytes.set(null);
    try {
      await update.downloadAndInstall((event) => {
        if (event.event === 'Started') {
          this.totalBytes.set(event.data.contentLength ?? null);
        } else if (event.event === 'Progress') {
          this.downloaded.update((value) => value + event.data.chunkLength);
        } else if (event.event === 'Finished') {
          this.readyToRestart.set(true);
        }
      });
      this.readyToRestart.set(true);
      // Windows 会由安装器接管退出；macOS 需要自己重启到新版本
      await this.restart();
    } catch (error) {
      this.error.set(String(error));
    } finally {
      this.installing.set(false);
    }
  }

  async restart() {
    try {
      await relaunch();
    } catch (error) {
      this.error.set(String(error));
    }
  }
}
