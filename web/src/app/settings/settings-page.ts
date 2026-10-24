import { ChangeDetectionStrategy, Component, inject, signal } from '@angular/core';
import { MatButtonModule } from '@angular/material/button';
import { MatButtonToggleModule } from '@angular/material/button-toggle';
import { MatSlideToggleModule } from '@angular/material/slide-toggle';

import { pickDirectory } from '../core/api';
import { Icon, type IconName } from '../core/icon';
import { ACCENTS, Settings, THEME_MODES, type ThemeMode } from '../core/settings';
import { TasksStore } from '../core/tasks-store';
import { UiState } from '../core/ui-state';

/** 设置页：下载、网络、外观、系统、任务数据五组卡片，改动即时生效 */
@Component({
  selector: 'app-settings-page',
  imports: [Icon, MatButtonModule, MatButtonToggleModule, MatSlideToggleModule],
  templateUrl: './settings-page.html',
  styleUrl: './settings-page.scss',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class SettingsPage {
  protected readonly settings = inject(Settings);
  protected readonly store = inject(TasksStore);
  protected readonly ui = inject(UiState);

  protected readonly accents = ACCENTS;

  protected readonly themeModes: Array<{
    key: ThemeMode;
    label: string;
    full: string;
    icon: IconName;
  }> = THEME_MODES.map((mode) => ({
    ...mode,
    icon: mode.key === 'light' ? 'light' : mode.key === 'dark' ? 'dark' : 'auto',
  }));

  /** 代理输入框的草稿：保存失败时保留用户输入，不把值回滚成旧的 */
  protected readonly proxyDraft = signal(this.settings.btProxy());

  protected async pickDir() {
    try {
      const picked = await pickDirectory(this.settings.downloadDir());
      if (picked) this.settings.setDownloadDir(picked);
    } catch (error) {
      this.ui.notify(String(error));
    }
  }

  protected stepConnections(delta: number) {
    this.settings.setConnections(this.settings.connections() + delta);
  }

  protected stepMaxTaskConnections(delta: number) {
    this.settings.setMaxTaskConnections(this.settings.maxTaskConnections() + delta);
  }

  protected stepMaxTotalConnections(delta: number) {
    this.settings.setMaxTotalConnections(this.settings.maxTotalConnections() + delta);
  }

  protected onProxyInput(event: Event) {
    this.proxyDraft.set((event.target as HTMLInputElement).value);
  }

  /** 失焦或回车时提交（文本框的 change 事件） */
  protected async saveProxy() {
    const result = await this.settings.saveBtProxy(this.proxyDraft());
    if (!result.saved) return; // 原因已经显示在输入框下面
    // 存的是归一化后的地址（例如 socks5h:// 会被改写），把它回填到输入框
    this.proxyDraft.set(this.settings.btProxy());
    if (result.needsRestart) {
      this.ui.notify('BT 代理已保存；BT 会话已在运行，重启应用后生效');
    }
  }
}
