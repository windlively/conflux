import { ChangeDetectionStrategy, Component, computed, inject } from '@angular/core';
import { MatButtonModule } from '@angular/material/button';
import { MatDialog } from '@angular/material/dialog';

import { formatSpeed } from '../core/format';
import { Icon } from '../core/icon';
import { TasksStore, type FilterKey } from '../core/tasks-store';
import { UiState } from '../core/ui-state';
import { openNewTaskDialog } from '../tasks/new-task-dialog';

/** 侧边栏：新建任务、任务筛选与设置入口；具体设置在内容区的设置页里 */
@Component({
  selector: 'app-sidebar',
  imports: [Icon, MatButtonModule],
  templateUrl: './sidebar.html',
  styleUrl: './sidebar.scss',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class Sidebar {
  protected readonly store = inject(TasksStore);
  protected readonly ui = inject(UiState);

  private readonly dialog = inject(MatDialog);

  protected readonly totalSpeedLabel = computed(() => formatSpeed(this.store.totalSpeed()));

  /** 有排队、探测或下载中的任务时，品牌标识进入流动状态 */
  protected readonly flowing = computed(
    () => (this.store.counts().find((item) => item.key === 'active')?.count ?? 0) > 0,
  );

  protected openNewTask() {
    openNewTaskDialog(this.dialog);
  }

  /** 点筛选回到任务视图，别停在设置页上 */
  protected openFilter(key: FilterKey) {
    this.store.filter.set(key);
    this.ui.showTasks();
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
}
