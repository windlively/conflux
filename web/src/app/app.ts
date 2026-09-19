import { ChangeDetectionStrategy, Component, signal } from '@angular/core';
import { MatButtonModule } from '@angular/material/button';
import { MatProgressBarModule } from '@angular/material/progress-bar';

import { windowMaterial } from './core/tauri';

interface FilterDef {
  key: string;
  label: string;
}

@Component({
  selector: 'app-root',
  imports: [MatButtonModule, MatProgressBarModule],
  templateUrl: './app.html',
  styleUrl: './app.scss',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class App {
  protected readonly filters: FilterDef[] = [
    { key: 'all', label: '全部任务' },
    { key: 'active', label: '进行中' },
    { key: 'paused', label: '已暂停' },
    { key: 'completed', label: '已完成' },
    { key: 'failed', label: '失败 / 取消' },
  ];

  protected readonly filter = signal('all');

  protected readonly material = signal('读取中…');

  constructor() {
    void windowMaterial().then((material) => this.material.set(material));
  }
}
