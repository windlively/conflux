import { ChangeDetectionStrategy, Component, computed, input } from '@angular/core';

import { stateTone } from '../core/tone';

/** 状态徽标：容器色随状态语义走（进行中=主色、完成=绿、失败=红、暂停=中性） */
@Component({
  selector: 'app-status-chip',
  changeDetection: ChangeDetectionStrategy.OnPush,
  template: `
    @if (spinner()) {
      <span class="spinner"></span>
    }
    <span>{{ label() }}</span>
  `,
  styleUrl: './status-chip.scss',
  host: {
    class: 'chip',
    '[attr.data-tone]': 'tone()',
    '[class.chip-channel]': "variant() === 'channel'",
  },
})
export class StatusChip {
  readonly state = input.required<string>();
  readonly label = input.required<string>();
  readonly spinner = input(false);
  readonly variant = input<'task' | 'channel'>('task');

  protected readonly tone = computed(() => stateTone(this.state()));
}
