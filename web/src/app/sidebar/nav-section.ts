import { ChangeDetectionStrategy, Component, computed, input, signal } from '@angular/core';

import { Icon, type IconName } from '../core/icon';

/** 侧边栏分组：标题可点，展开/收起都带动画；收起状态按分组记在本地 */
@Component({
  selector: 'app-nav-section',
  imports: [Icon],
  changeDetection: ChangeDetectionStrategy.OnPush,
  template: `
    <button
      type="button"
      class="section-head"
      [attr.aria-expanded]="open()"
      (click)="toggle()"
    >
      <app-icon [name]="icon()" />
      <span class="section-title">{{ title() }}</span>
      <span class="chevron" [class.chevron-open]="open()">›</span>
    </button>

    @if (open()) {
      <div class="section-body" animate.enter="section-in" animate.leave="section-out">
        <ng-content />
      </div>
    }
  `,
  styleUrl: './nav-section.scss',
})
export class NavSection {
  /** 记收起状态用的键，同时作为无障碍标题 */
  readonly storageKey = input.required<string>();
  readonly title = input.required<string>();
  readonly icon = input.required<IconName>();

  private readonly override = signal<boolean | null>(null);

  // 输入属性要等首次变更检测才有值，所以初始状态在 computed 里读本地记录
  protected readonly open = computed(
    () =>
      this.override() ?? localStorage.getItem(`conflux.section.${this.storageKey()}`) !== 'closed',
  );

  protected toggle() {
    const next = !this.open();
    this.override.set(next);
    localStorage.setItem(`conflux.section.${this.storageKey()}`, next ? 'open' : 'closed');
  }
}
