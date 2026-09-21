import { ChangeDetectionStrategy, Component, computed, input } from '@angular/core';
import { MatProgressBarModule } from '@angular/material/progress-bar';

import { stateTone } from '../core/tone';

/** 分片条带里的一段：宽度按分片长度分配，填充色按分片状态取语义色 */
@Component({
  selector: 'app-segment-bar',
  imports: [MatProgressBarModule],
  changeDetection: ChangeDetectionStrategy.OnPush,
  template: `
    <mat-progress-bar
      mode="determinate"
      [value]="(ratio() ?? 0) * 100"
      [attr.data-tone]="tone()"
    />
  `,
  styleUrl: './segment-bar.scss',
  host: {
    '[style.flex-grow]': 'grow()',
    '[attr.title]': 'title()',
  },
})
export class SegmentBar {
  readonly grow = input.required<number>();
  /** 0–1；未知进度传 null */
  readonly ratio = input<number | null>(null);
  readonly state = input.required<string>();
  readonly title = input('');

  protected readonly tone = computed(() => stateTone(this.state()));
}
