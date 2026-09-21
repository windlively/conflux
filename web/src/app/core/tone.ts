export type Tone = 'accent' | 'green' | 'red' | 'orange' | 'neutral';

/** 任务状态与通道状态共享一套语义色 */
export function stateTone(state: string): Tone {
  switch (state) {
    case 'completed':
    case 'finished':
      return 'green';
    case 'failed':
      return 'red';
    case 'retrying':
      return 'orange';
    case 'paused':
    case 'cancelled':
    case 'idle':
    case 'stopped':
      return 'neutral';
    default:
      return 'accent';
  }
}
