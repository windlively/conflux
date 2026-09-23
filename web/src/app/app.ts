import { ChangeDetectionStrategy, Component, computed, inject } from '@angular/core';
import { MatButtonModule } from '@angular/material/button';

import { Icon } from './core/icon';
import { TasksStore } from './core/tasks-store';
import { UiState } from './core/ui-state';
import { Sidebar } from './sidebar/sidebar';
import { TaskList } from './tasks/task-list';

@Component({
  selector: 'app-root',
  imports: [Icon, MatButtonModule, Sidebar, TaskList],
  templateUrl: './app.html',
  styleUrl: './app.scss',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class App {
  protected readonly store = inject(TasksStore);
  protected readonly ui = inject(UiState);

  constructor() {
    // 右键菜单里的「检查元素 / Reload」是 WebKit 自带的：后端把 webview 设成不可检查后，
    // 输入框上的编辑菜单仍会带出它，所以页面这一层直接压掉整个右键菜单。
    // 代价是输入框没有右键粘贴了（⌘C／⌘V 照旧）。
    document.addEventListener('contextmenu', (event) => event.preventDefault());
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
