import { ChangeDetectionStrategy, Component, computed, input, output } from '@angular/core';
import { MatButtonModule } from '@angular/material/button';
import { MatCheckboxModule } from '@angular/material/checkbox';

import { formatBytes, percent } from '../core/format';
import type { TorrentFile } from '../core/types';

/**
 * 种子文件清单：预览时用来挑要下哪几个文件，任务详情里用来改勾选。
 * 勾选状态由父组件持有（`files[].selected`），这里只负责显示与把点击报上去。
 */
@Component({
  selector: 'app-file-picker',
  imports: [MatButtonModule, MatCheckboxModule],
  templateUrl: './file-picker.html',
  styleUrl: './file-picker.scss',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class FilePicker {
  readonly files = input.required<TorrentFile[]>();
  /** 只有一行的清单没什么可挑的，父组件可以直接关掉勾选框 */
  readonly selectable = input(true);

  readonly toggle = output<number>();
  readonly setAll = output<boolean>();

  /** 勾选框只在真有得挑的时候出现 */
  protected readonly choosable = computed(() => this.selectable() && this.files().length > 1);

  protected readonly rows = computed(() =>
    this.files().map((file) => ({
      ...file,
      size: formatBytes(file.length),
      done: formatBytes(file.downloaded),
      ratio: percent(file.downloaded, file.length),
      // 没勾选的文件不会再有新进度，进度条只对勾选的显示
      progress: file.selected && file.downloaded > 0,
      // 最后一个勾选着的文件不给取消：一个都不下没有意义
      locked: this.selectable() && file.selected && this.selectedCount() === 1,
    })),
  );

  protected readonly selectedCount = computed(
    () => this.files().filter((file) => file.selected).length,
  );

  protected readonly allSelected = computed(
    () => this.files().length > 0 && this.selectedCount() === this.files().length,
  );

  protected readonly selectedBytes = computed(() =>
    formatBytes(
      this.files()
        .filter((file) => file.selected)
        .reduce((sum, file) => sum + file.length, 0),
    ),
  );

  protected readonly summary = computed(() => {
    const count = this.selectedCount();
    if (!this.selectable()) return `${this.files().length} 个文件 · ${this.selectedBytes()}`;
    if (count === this.files().length) {
      return `全部 ${count} 个文件 · ${this.selectedBytes()}`;
    }
    return `已选 ${count} / ${this.files().length} 个 · ${this.selectedBytes()}`;
  });

  protected toggleAll(checked: boolean) {
    this.setAll.emit(checked);
  }
}
