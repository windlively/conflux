import { ChangeDetectionStrategy, Component, inject, input, output, signal } from '@angular/core';
import { MatButtonModule } from '@angular/material/button';
import { MatFormFieldModule } from '@angular/material/form-field';
import { MatInputModule } from '@angular/material/input';

import { searchEd2k } from '../core/api';
import { formatBytes } from '../core/format';
import { Icon } from '../core/icon';
import type { Ed2kSearchResult } from '../core/types';

/**
 * ed2k 资源搜索：侧车同时问 eD2k 服务器与 Kad 网络，等两边都回完一轮才出结果，
 * 所以慢是正常的（几十秒也可能）。选中一条结果由外层决定怎么建任务。
 */
@Component({
  selector: 'app-ed2k-search',
  imports: [Icon, MatButtonModule, MatFormFieldModule, MatInputModule],
  templateUrl: './ed2k-search.html',
  styleUrl: './ed2k-search.scss',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class Ed2kSearch {
  /** 外层正忙（比如正在建任务）时，搜索与「下载」都要禁掉 */
  readonly busy = input(false);
  readonly pick = output<Ed2kSearchResult>();

  protected readonly query = signal('');
  protected readonly searching = signal(false);
  protected readonly results = signal<Ed2kSearchResult[]>([]);
  protected readonly error = signal<string | null>(null);

  protected readonly formatBytes = formatBytes;

  protected onQuery(event: Event) {
    this.query.set((event.target as HTMLInputElement).value);
  }

  protected async run() {
    const query = this.query().trim();
    if (!query) {
      this.error.set('先填个关键词再搜');
      return;
    }
    this.searching.set(true);
    this.error.set(null);
    try {
      const results = await searchEd2k(query);
      this.results.set(results);
      if (results.length === 0) {
        this.error.set('没搜到结果：换个关键词，或者稍后再试（网络刚起来时源少）');
      }
    } catch (failure) {
      this.results.set([]);
      this.error.set(String(failure));
    } finally {
      this.searching.set(false);
    }
  }
}
