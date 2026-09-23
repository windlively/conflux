import { ChangeDetectionStrategy, Component, computed, inject, signal } from '@angular/core';
import { MatButtonModule } from '@angular/material/button';
import { MatDialogModule, MatDialogRef } from '@angular/material/dialog';
import { MatFormFieldModule } from '@angular/material/form-field';
import { MatInputModule } from '@angular/material/input';

import {
  addTask,
  addTorrentFile,
  defaultDownloadDir,
  pickDirectory,
  pickTorrentFile,
  previewMagnet,
  previewTorrentFile,
} from '../core/api';
import { Icon } from '../core/icon';
import { Settings } from '../core/settings';
import type { Ed2kSearchResult, TorrentFile, TorrentPreview } from '../core/types';
import { Ed2kSearch } from './ed2k-search';
import { FilePicker } from './file-picker';

/** 预览里挑文件时，任务是怎么来的：磁力要么按原文再挂一次，要么是本地种子文件 */
type PreviewSource =
  | { kind: 'magnet'; magnet: string }
  | { kind: 'file'; path: string; fileName: string };

/**
 * 新建任务弹窗：字段与工具栏那套一致，目录与连接数取侧边栏设置。
 * 磁力与种子文件都能先预览出文件清单、勾选要下的文件，再开始下载。
 * 报错留在弹窗里（主界面的提示条被弹窗挡着看不见），全部成功才关闭。
 */
@Component({
  selector: 'app-new-task-dialog',
  imports: [
    Ed2kSearch,
    FilePicker,
    Icon,
    MatButtonModule,
    MatDialogModule,
    MatFormFieldModule,
    MatInputModule,
  ],
  templateUrl: './new-task-dialog.html',
  styleUrl: './new-task-dialog.scss',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class NewTaskDialog {
  private readonly ref = inject<MatDialogRef<NewTaskDialog>>(MatDialogRef);
  protected readonly settings = inject(Settings);

  protected readonly urls = signal('');
  protected readonly dir = signal('');
  protected readonly connections = signal(4);
  protected readonly busy = signal(false);
  protected readonly previewing = signal(false);
  protected readonly error = signal<string | null>(null);
  /** ed2k 搜索面板默认收着，用不到的人不用看它 */
  protected readonly showSearch = signal(false);

  private readonly preview = signal<TorrentPreview | null>(null);
  private readonly source = signal<PreviewSource | null>(null);
  private readonly selected = signal<ReadonlySet<number>>(new Set());

  constructor() {
    this.connections.set(this.settings.taskConnections());
    const preferred = this.settings.downloadDir();
    if (preferred) {
      this.dir.set(preferred);
    } else {
      void defaultDownloadDir()
        .then((dir) => this.dir.set(dir))
        .catch(() => undefined);
    }
  }

  private readonly links = computed(() =>
    this.urls()
      .split(/\s+/)
      .map((link) => link.trim())
      .filter(Boolean),
  );

  /** 只有一条磁力时才谈得上预览：一次挑一批文件对应的是同一个种子 */
  protected readonly singleMagnet = computed(() => {
    const links = this.links();
    if (links.length !== 1) return null;
    return links[0].startsWith('magnet:') ? links[0] : null;
  });

  /** 全是磁力时连接数没有意义：peer 连接由 BT 会话自己管 */
  private readonly onlyMagnets = computed(() => {
    const links = this.links();
    return links.length > 0 && links.every((link) => link.startsWith('magnet:'));
  });

  /** BT 任务（磁力或种子文件）都由会话自己管连接数 */
  protected readonly btOnly = computed(
    () => this.onlyMagnets() || this.source()?.kind === 'file',
  );

  protected readonly canPreview = computed(
    () => this.singleMagnet() !== null && !this.previewing() && !this.busy(),
  );

  protected readonly pickerFiles = computed<TorrentFile[]>(() => {
    const selected = this.selected();
    return (this.preview()?.files ?? []).map((file) => ({
      ...file,
      selected: selected.has(file.index),
    }));
  });

  /** 预览面板的标题：种子名，拿不到就用来源兜底 */
  protected readonly previewLabel = computed(() => {
    const source = this.source();
    const preview = this.preview();
    if (!preview) return '';
    if (preview.name) return preview.name;
    if (source?.kind === 'file') return source.fileName;
    return '磁力任务';
  });

  protected readonly hasPreview = computed(
    () => this.preview() !== null && this.pickerFiles().length > 0,
  );

  protected onUrls(event: Event) {
    const value = (event.target as HTMLTextAreaElement).value;
    this.urls.set(value);
    // 改了链接，之前那份清单就不是它的了
    const source = this.source();
    if (source?.kind === 'magnet' && value.trim() !== source.magnet) {
      this.dropPreview();
    }
  }

  protected onDir(event: Event) {
    this.dir.set((event.target as HTMLInputElement).value);
  }

  protected onConnections(event: Event) {
    const value = Number((event.target as HTMLInputElement).value) || 1;
    this.connections.set(Math.min(this.settings.maxTaskConnections(), Math.max(1, value)));
  }

  protected async pickDir() {
    try {
      const picked = await pickDirectory(this.dir());
      if (picked) this.dir.set(picked);
    } catch (failure) {
      this.error.set(String(failure));
    }
  }

  /** 只取元数据看一眼种子里的文件，不建任务：慢的是联网找 peer，所以按钮上要说清在等什么 */
  protected async previewNow() {
    const magnet = this.singleMagnet();
    if (!magnet) return;
    this.previewing.set(true);
    this.error.set(null);
    try {
      const preview = await previewMagnet(magnet);
      this.adopt(preview, { kind: 'magnet', magnet });
    } catch (failure) {
      this.error.set(String(failure));
    } finally {
      this.previewing.set(false);
    }
  }

  /** 导入种子文件：先解析出清单让用户挑，确认后才建任务 */
  protected async pickTorrent() {
    try {
      const picked = await pickTorrentFile(this.dir());
      if (!picked) return;
      this.busy.set(true);
      this.error.set(null);
      try {
        const preview = await previewTorrentFile(picked);
        this.adopt(preview, {
          kind: 'file',
          path: picked,
          fileName: picked.split(/[\\/]/).pop() ?? picked,
        });
        this.urls.set('');
      } catch (failure) {
        this.dropPreview();
        this.error.set(String(failure));
      } finally {
        this.busy.set(false);
      }
    } catch (failure) {
      this.error.set(String(failure));
    }
  }

  /** 从搜索结果里挑一条直接建任务：目录与连接数用弹窗里当前填的 */
  protected async downloadFound(result: Ed2kSearchResult) {
    await this.run(() => addTask(result.link, this.dir() || null, this.connections()));
  }

  protected toggleFile(index: number) {
    const next = new Set(this.selected());
    if (next.has(index)) {
      next.delete(index);
    } else {
      next.add(index);
    }
    this.selected.set(next);
  }

  protected setAll(checked: boolean) {
    const files = this.preview()?.files ?? [];
    this.selected.set(checked ? new Set(files.map((file) => file.index)) : new Set());
  }

  protected async submit() {
    const source = this.source();

    if (source?.kind === 'file') {
      const files = this.selection();
      if (files === undefined) return;
      await this.run(() => addTorrentFile(source.path, this.dir() || null, this.connections(), files));
      return;
    }

    const links = this.links();
    if (links.length === 0) {
      this.error.set('请先输入下载链接');
      return;
    }

    // 清单只对预览过的那个种子有效，批量添加时不带勾选（全部下载）
    let files: number[] | null = null;
    if (source?.kind === 'magnet' && links.length === 1 && links[0] === source.magnet) {
      const chosen = this.selection();
      if (chosen === undefined) return;
      files = chosen;
    }

    await this.run(async () => {
      const failures: string[] = [];
      for (const link of links) {
        try {
          // 目录留空表示用后端默认下载目录
          await addTask(link, this.dir() || null, this.connections(), files);
        } catch (failure) {
          failures.push(`${link}：${String(failure)}`);
        }
      }
      if (failures.length > 0) throw new Error(failures.join('\n'));
    });
  }

  /** 勾选的文件序号；null 表示全选，false 表示一个都没选（不该提交） */
  private selection(): number[] | null | undefined {
    const preview = this.preview();
    if (!preview) return null;
    const chosen = [...this.selected()].sort((a, b) => a - b);
    if (chosen.length === 0) {
      this.error.set('至少留一个文件才能开始下载，或者点「取消」放弃这个任务');
      return undefined;
    }
    return chosen.length === preview.files.length ? null : chosen;
  }

  private adopt(preview: TorrentPreview, source: PreviewSource) {
    this.preview.set(preview);
    this.source.set(source);
    this.selected.set(new Set(preview.files.map((file) => file.index)));
  }

  private dropPreview() {
    this.preview.set(null);
    this.source.set(null);
    this.selected.set(new Set());
  }

  private async run(action: () => Promise<unknown>) {
    this.busy.set(true);
    this.error.set(null);
    try {
      await action();
      this.ref.close();
    } catch (failure) {
      this.error.set(String(failure));
    } finally {
      this.busy.set(false);
    }
  }

  protected cancel() {
    this.ref.close();
  }
}
