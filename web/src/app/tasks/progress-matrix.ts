import {
  ChangeDetectionStrategy,
  Component,
  ElementRef,
  computed,
  effect,
  input,
  signal,
  viewChild,
} from '@angular/core';

import { formatBytes } from '../core/format';
import type { TaskView } from '../core/types';

/** 矩阵固定 100 列，行数按文件大小折算，总格数控制在 200–2000 之间 */
const COLS = 100;
const TARGET_CHUNK = 256 * 1024;
const MIN_CELLS = 200;
const MAX_CELLS = 2000;
const CELL_HEIGHT = 6;
const GAP = 1;

const TRACK_FALLBACK = 'rgba(120, 120, 128, 0.16)';
const DONE = '#2e7d32';
const ACTIVE = [103, 80, 164] as const;
/** 已落盘但没在传输的部分：暂停、失败、重启恢复后的进度 */
const HELD = '#79747e';

interface Plan {
  rows: number;
  count: number;
  bytesPerCell: number;
}

/** canvas 不认 CSS 变量，取色只能读计算样式 */
function themeColors() {
  const styles = getComputedStyle(document.documentElement);
  const read = (name: string, fallback: string) =>
    styles.getPropertyValue(name).trim() || fallback;
  const active = read('--matrix-active-rgb', ACTIVE.join(', '));
  return {
    track: read('--matrix-track', TRACK_FALLBACK),
    done: read('--matrix-done', DONE),
    active: [0, 1, 2].map((index) => Number(active.split(',')[index]) || 0),
    held: read('--matrix-held', HELD),
  };
}

function planCells(total: number): Plan {
  const wanted = Math.ceil(total / TARGET_CHUNK);
  const cells = Math.min(MAX_CELLS, Math.max(MIN_CELLS, wanted));
  const rows = Math.ceil(cells / COLS);
  const count = rows * COLS;
  return { rows, count, bytesPerCell: total / count };
}

/**
 * 已下载区间。分片模式下每个分片从自己的起点顺序写入，
 * 所以「已提交偏移」就是一段连续区间；单连接模式则是从 0 开始的前缀。
 */
function completedRanges(task: TaskView): Array<[number, number]> {
  if (task.segments.length > 1) {
    return task.segments
      .map((segment) => [segment.start, segment.start + segment.downloaded] as [number, number])
      .filter(([start, end]) => end > start);
  }
  return task.downloadedBytes > 0 ? [[0, task.downloadedBytes]] : [];
}

function fillRatios(task: TaskView, plan: Plan): Float32Array {
  const ranges = completedRanges(task);
  const fill = new Float32Array(plan.count);
  let cursor = 0;
  for (let index = 0; index < plan.count; index += 1) {
    const cellStart = index * plan.bytesPerCell;
    const cellEnd = cellStart + plan.bytesPerCell;
    while (cursor < ranges.length && ranges[cursor][1] <= cellStart) cursor += 1;
    let covered = 0;
    for (let k = cursor; k < ranges.length && ranges[k][0] < cellEnd; k += 1) {
      covered += Math.min(cellEnd, ranges[k][1]) - Math.max(cellStart, ranges[k][0]);
    }
    fill[index] = Math.min(1, covered / plan.bytesPerCell);
  }
  return fill;
}

function drawMatrix(canvas: HTMLCanvasElement, plan: Plan, fill: Float32Array, running: boolean) {
  const colors = themeColors();
  const cssWidth = canvas.clientWidth || 640;
  const cssHeight = plan.rows * CELL_HEIGHT;
  const ratio = window.devicePixelRatio || 1;
  const bitmapWidth = Math.round(cssWidth * ratio);
  const bitmapHeight = Math.round(cssHeight * ratio);
  // 只在真正变化时赋值：改 canvas 尺寸会再次触发 ResizeObserver
  if (canvas.width !== bitmapWidth) canvas.width = bitmapWidth;
  if (canvas.height !== bitmapHeight) canvas.height = bitmapHeight;
  const styleHeight = `${cssHeight}px`;
  if (canvas.style.height !== styleHeight) canvas.style.height = styleHeight;

  const context = canvas.getContext('2d');
  if (!context) return;
  context.setTransform(ratio, 0, 0, ratio, 0, 0);
  context.clearRect(0, 0, cssWidth, cssHeight);

  const cellWidth = cssWidth / COLS;
  const width = Math.max(1, cellWidth - GAP);
  const height = CELL_HEIGHT - GAP;
  for (let index = 0; index < plan.count; index += 1) {
    const x = (index % COLS) * cellWidth;
    const y = Math.floor(index / COLS) * CELL_HEIGHT;
    const ratioDone = fill[index];
    context.fillStyle = colors.track;
    context.fillRect(x, y, width, height);
    if (ratioDone > 0) {
      context.fillStyle =
        ratioDone >= 1
          ? colors.done
          : running
            ? `rgba(${colors.active[0]}, ${colors.active[1]}, ${colors.active[2]}, ${(
                0.25 +
                0.75 * ratioDone
              ).toFixed(3)})`
            : colors.held;
      context.fillRect(x, y, ratioDone >= 1 ? width : Math.max(1, width * ratioDone), height);
    }
  }
}

interface HoverTip {
  left: number;
  top: number;
  text: string;
}

/** 下载矩阵：按已下载区间给格子上色，悬停显示该格对应的字节区间 */
@Component({
  selector: 'app-progress-matrix',
  changeDetection: ChangeDetectionStrategy.OnPush,
  template: `
    @if (visible()) {
      <div class="matrix-block">
        <div class="details-title">{{ cellsLabel() }}</div>
        <div class="matrix">
          <canvas
            #matrixCanvas
            class="matrix-canvas"
            (mousemove)="onMove($event)"
            (mouseleave)="tip.set(null)"
          ></canvas>
          @if (tip(); as hover) {
            <div class="matrix-tip" [style.left.px]="hover.left" [style.top.px]="hover.top">
              {{ hover.text }}
            </div>
          }
        </div>
      </div>
    }
  `,
  styleUrl: './progress-matrix.scss',
})
export class ProgressMatrix {
  readonly task = input.required<TaskView>();

  private readonly canvasRef = viewChild<ElementRef<HTMLCanvasElement>>('matrixCanvas');

  protected readonly tip = signal<HoverTip | null>(null);

  // BT 的分片是乱序到手的，「已完成的连续前缀」画法会骗人，只在 http 任务上展示
  protected readonly visible = computed(
    () =>
      this.task().kind === 'http' &&
      this.task().totalBytes > 0 &&
      this.task().status !== 'probing',
  );

  private readonly plan = computed(() => planCells(this.task().totalBytes));

  private readonly signature = computed(() => {
    const task = this.task();
    return task.segments.length > 1
      ? task.segments.map((segment) => segment.downloaded).join(',')
      : String(task.downloadedBytes);
  });

  private readonly fill = computed(() => {
    this.signature();
    return fillRatios(this.task(), this.plan());
  });

  // 只有真正在下载时才把未完成的格子画成蓝色，否则蓝色会误报「还在传」
  private readonly running = computed(() => this.task().status === 'downloading');

  protected readonly cellsLabel = computed(
    () => `文件下载矩阵　每格约 ${formatBytes(this.plan().bytesPerCell)}，悬停可看该段区间`,
  );

  constructor() {
    effect((onCleanup) => {
      const canvas = this.canvasRef()?.nativeElement;
      if (!canvas) return;

      const plan = this.plan();
      const fill = this.fill();
      const running = this.running();
      const draw = () => drawMatrix(canvas, plan, fill, running);
      draw();

      // 侧边栏折叠、窗口缩放都会改变可用宽度，位图必须跟着重画，
      // 否则会被 CSS 拉伸成模糊的一坨
      const observer = new ResizeObserver(draw);
      observer.observe(canvas);
      onCleanup(() => observer.disconnect());
    });
  }

  protected onMove(event: MouseEvent) {
    const canvas = this.canvasRef()?.nativeElement;
    if (!canvas) return;

    const plan = this.plan();
    const fill = this.fill();
    const rect = canvas.getBoundingClientRect();
    const column = Math.min(COLS - 1, Math.floor((event.clientX - rect.left) / (rect.width / COLS)));
    const row = Math.min(plan.rows - 1, Math.floor((event.clientY - rect.top) / CELL_HEIGHT));
    const index = row * COLS + column;
    if (index < 0 || index >= plan.count) {
      this.tip.set(null);
      return;
    }

    const start = Math.round(index * plan.bytesPerCell);
    const end = Math.round(Math.min(this.task().totalBytes, (index + 1) * plan.bytesPerCell));
    const done = fill[index];
    const state =
      done >= 1 ? '已完成' : done > 0 ? (this.running() ? '下载中' : '已下载，未在传输') : '未开始';
    // 外层容器有内边距，提示框相对容器定位
    const base = canvas.parentElement?.getBoundingClientRect() ?? rect;
    this.tip.set({
      left: event.clientX - base.left,
      top: event.clientY - base.top,
      text: `${formatBytes(start)} - ${formatBytes(end)}　${(done * 100).toFixed(0)}%　${state}`,
    });
  }
}
