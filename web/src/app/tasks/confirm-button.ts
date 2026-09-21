import { ChangeDetectionStrategy, Component, DestroyRef, inject, input, output, signal } from '@angular/core';
import { MatButtonModule } from '@angular/material/button';

import { Icon, type IconName } from '../core/icon';

/** 二次确认按钮：Tauri 的 WebView 不保证支持 window.confirm，因此放在应用内实现 */
@Component({
  selector: 'app-confirm-button',
  imports: [Icon, MatButtonModule],
  changeDetection: ChangeDetectionStrategy.OnPush,
  template: `
    <button matButton [class.danger]="armed()" (click)="press()">
      <app-icon [name]="armed() ? 'close' : icon()" />
      {{ armed() ? confirmLabel() : label() }}
    </button>
  `,
  styleUrl: './confirm-button.scss',
})
export class ConfirmButton {
  readonly label = input.required<string>();
  readonly confirmLabel = input.required<string>();
  readonly icon = input<IconName>('trash');
  readonly confirmed = output<void>();

  protected readonly armed = signal(false);

  private timer?: ReturnType<typeof setTimeout>;

  constructor() {
    inject(DestroyRef).onDestroy(() => clearTimeout(this.timer));
  }

  protected press() {
    if (this.armed()) {
      this.armed.set(false);
      this.confirmed.emit();
      return;
    }
    this.armed.set(true);
    clearTimeout(this.timer);
    this.timer = setTimeout(() => this.armed.set(false), 4000);
  }
}
