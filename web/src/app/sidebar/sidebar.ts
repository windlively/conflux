import { ChangeDetectionStrategy, Component, computed, inject } from '@angular/core';
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
}
