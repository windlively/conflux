import { ChangeDetectionStrategy, Component, computed, inject } from '@angular/core';
import { MatButtonModule } from '@angular/material/button';
import { MatDialog } from '@angular/material/dialog';

import { onTrayAction } from './core/api';
import { Icon } from './core/icon';
import { TasksStore } from './core/tasks-store';
import type { TrayAction } from './core/types';
import { Updater } from './core/updater';
import { UiState } from './core/ui-state';
import { SettingsPage } from './settings/settings-page';
import { Sidebar } from './sidebar/sidebar';
import { openNewTaskDialog } from './tasks/new-task-dialog';
import { TaskList } from './tasks/task-list';

@Component({
  selector: 'app-root',
  imports: [Icon, MatButtonModule, SettingsPage, Sidebar, TaskList],
  templateUrl: './app.html',
  styleUrl: './app.scss',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class App {
  protected readonly store = inject(TasksStore);
  protected readonly ui = inject(UiState);
  protected readonly updater = inject(Updater);

  private readonly dialog = inject(MatDialog);

  constructor() {
    // 右键菜单里的「检查元素 / Reload」是 WebKit 自带的：后端把 webview 设成不可检查后，
    // 输入框上的编辑菜单仍会带出它，所以页面这一层直接压掉整个右键菜单。
    // 代价是输入框没有右键粘贴了（⌘C／⌘V 照旧）。
    document.addEventListener('contextmenu', (event) => event.preventDefault());
    void onTrayAction((action) => this.handleTrayAction(action)).catch(() => undefined);
  }

  /** 状态栏菜单：新建任务，或定位到被点的那条任务 */
  private handleTrayAction(action: TrayAction) {
    if (action.action === 'new-task') {
      if (this.dialog.openDialogs.length === 0) openNewTaskDialog(this.dialog);
      return;
    }
    if (action.id) this.revealTask(action.id);
  }

  private revealTask(id: string) {
    this.ui.showTasks();
    this.store.filter.set('all');
    // 等分类切换把这条任务渲染出来，再滚动过去给它一圈高亮
    requestAnimationFrame(() => {
      requestAnimationFrame(() => {
        const element = document.querySelector<HTMLElement>(`[data-task-id="${CSS.escape(id)}"]`);
        if (!element) return;
        element.scrollIntoView({ behavior: 'smooth', block: 'center' });
        element.classList.add('task-flash');
        window.setTimeout(() => element.classList.remove('task-flash'), 1600);
      });
    });
  }

  protected readonly emptyText = computed(() =>
    this.store.tasks().length === 0 ? '还没有下载任务，点侧边栏的「新建任务」开始' : '该分类下暂无任务',
  );

  /** 读取失败与操作报错共用一个提示条，和 React 版一致 */
  protected readonly messageText = computed(() => this.store.error() ?? this.ui.message());

  protected dismiss() {
    this.store.clearError();
    this.ui.clearMessage();
  }
}
