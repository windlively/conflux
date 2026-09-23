import { Injectable, computed, effect, signal } from '@angular/core';

/** 下限 220：再窄设置行的标签会被挤成两三行；上限 420 免得把内容区挤没 */
export const SIDEBAR_MIN = 220;
export const SIDEBAR_MAX = 420;
const SIDEBAR_DEFAULT = 240;

const clampWidth = (px: number) =>
  Number.isFinite(px) ? Math.min(SIDEBAR_MAX, Math.max(SIDEBAR_MIN, Math.round(px))) : SIDEBAR_DEFAULT;

function storedWidth(): number {
  const raw = localStorage.getItem('conflux.sidebarWidth');
  return raw === null ? SIDEBAR_DEFAULT : clampWidth(Number(raw));
}

/** 界面级状态：侧边栏折叠与宽度，都记在 localStorage，重启后保持 */
@Injectable({ providedIn: 'root' })
export class UiState {
  readonly sidebarOpen = signal(localStorage.getItem('conflux.sidebar') !== 'collapsed');

  readonly sidebarWidth = signal(storedWidth());

  /** 拖到比默认还窄时内外边距收一格，见 sidebar.scss 的 .sidebar-narrow */
  readonly sidebarNarrow = computed(() => this.sidebarWidth() < SIDEBAR_DEFAULT);

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

  /** 夹取范围在 setter 里做，拖拽时可以放心地把指针位置直接喂进来 */
  setSidebarWidth(px: number) {
    const width = clampWidth(px);
    this.sidebarWidth.set(width);
    localStorage.setItem('conflux.sidebarWidth', String(width));
  }

  notify(text: string) {
    this.message.set(text);
  }

  clearMessage() {
    this.message.set(null);
  }
}
