import { ChangeDetectionStrategy, Component, computed, inject, signal } from '@angular/core';
import { MatButtonModule } from '@angular/material/button';
import { MatButtonToggleModule } from '@angular/material/button-toggle';
import { MatDialog } from '@angular/material/dialog';
import { MatTooltipModule } from '@angular/material/tooltip';

import { pickDirectory } from '../core/api';
import { formatSpeed } from '../core/format';
import { Icon, type IconName } from '../core/icon';
import { ACCENTS, Settings, THEME_MODES, type ThemeMode } from '../core/settings';
import { TasksStore } from '../core/tasks-store';
import { UiState } from '../core/ui-state';
import { NewTaskDialog } from '../tasks/new-task-dialog';
import { NavSection } from './nav-section';

/** 侧边栏：新建任务、任务管理（筛选）、设置、主题色四个分组 + 速度与存储信息 */
@Component({
  selector: 'app-sidebar',
  imports: [Icon, MatButtonModule, MatButtonToggleModule, MatTooltipModule, NavSection],
  templateUrl: './sidebar.html',
  styleUrl: './sidebar.scss',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class Sidebar {
  protected readonly store = inject(TasksStore);
  protected readonly ui = inject(UiState);
  protected readonly settings = inject(Settings);

  private readonly dialog = inject(MatDialog);

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

  protected readonly totalSpeedLabel = computed(() => formatSpeed(this.store.totalSpeed()));

  /** 代理输入框的草稿：保存失败时保留用户输入，不把值回滚成旧的 */
  protected readonly proxyDraft = signal(this.settings.btProxy());

  protected openNewTask() {
    this.dialog.open(NewTaskDialog, {
      width: '560px',
      maxWidth: '92vw',
      autoFocus: 'textarea',
    });
  }

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

  /** 右缘那条拖动把手：抓住指针后按位移改宽度，范围夹取在 UiState 里 */
  protected startResize(event: PointerEvent) {
    event.preventDefault();
    const handle = event.currentTarget as HTMLElement;
    handle.setPointerCapture(event.pointerId);
    const offset = event.clientX - this.ui.sidebarWidth();
    document.body.classList.add('resizing');

    const onMove = (move: PointerEvent) => this.ui.setSidebarWidth(move.clientX - offset);
    const onEnd = (end: PointerEvent) => {
      handle.releasePointerCapture(end.pointerId);
      handle.removeEventListener('pointermove', onMove);
      handle.removeEventListener('pointerup', onEnd);
      handle.removeEventListener('pointercancel', onEnd);
      document.body.classList.remove('resizing');
    };

    handle.addEventListener('pointermove', onMove);
    handle.addEventListener('pointerup', onEnd);
    handle.addEventListener('pointercancel', onEnd);
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
