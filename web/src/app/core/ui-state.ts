import { Injectable, effect, signal } from '@angular/core';

/** 界面级状态：目前只有侧边栏折叠，折叠状态记在 localStorage，重启后保持 */
@Injectable({ providedIn: 'root' })
export class UiState {
  readonly sidebarOpen = signal(localStorage.getItem('conflux.sidebar') !== 'collapsed');

  /** 应用级提示条：新建任务的失败、任务操作的报错都落在这里 */
  readonly message = signal<string | null>(null);

  constructor() {
    effect(() => {
      const open = this.sidebarOpen();
      // 折叠后红黄绿灯会落在内容区上，需要让内容顶部避开它们
      document.documentElement.classList.toggle('sidebar-hidden', !open);
      localStorage.setItem('conflux.sidebar', open ? 'expanded' : 'collapsed');
    });
  }

  toggleSidebar() {
    this.sidebarOpen.update((open) => !open);
  }

  notify(text: string) {
    this.message.set(text);
  }

  clearMessage() {
    this.message.set(null);
  }
}
