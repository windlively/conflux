import { ChangeDetectionStrategy, Component, input } from '@angular/core';

import type { TaskView } from '../core/types';
import { TaskItem } from './task-item';

/** 任务列表：空态与列表二选一 */
@Component({
  selector: 'app-task-list',
  imports: [TaskItem],
  changeDetection: ChangeDetectionStrategy.OnPush,
  template: `
    @if (tasks().length === 0) {
      <div class="empty">
        <div class="empty-glyph">↓</div>
        <div>{{ emptyText() }}</div>
      </div>
    } @else {
      <ul class="tasks">
        @for (task of tasks(); track task.id) {
          <li animate.enter="card-in" animate.leave="card-out">
            <app-task-item [task]="task" />
          </li>
        }
      </ul>
    }
  `,
  styleUrl: './task-list.scss',
})
export class TaskList {
  readonly tasks = input.required<TaskView[]>();
  /** 一句话说明为什么是空的：一条任务都没有，还是当前分类下没有 */
  readonly emptyText = input.required<string>();
}
