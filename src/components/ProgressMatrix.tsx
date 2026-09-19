import { useEffect, useMemo, useRef, useState, type MouseEvent } from 'react';

import { formatBytes } from '../format';
import type { TaskView } from '../types';

/** 矩阵固定 100 列，行数按文件大小折算，总格数控制在 200–2000 之间 */
const COLS = 100;
const TARGET_CHUNK = 256 * 1024;
const MIN_CELLS = 200;
const MAX_CELLS = 2000;
const CELL_HEIGHT = 6;
const GAP = 1;

const TRACK = 'rgba(120, 120, 128, 0.16)';
const DONE = '#34c759';
const ACTIVE = [0, 122, 255] as const;
/** 已落盘但没在传输的部分：暂停、失败、重启恢复后的进度 */
const HELD = '#8e8e93';

interface Plan {
  rows: number;
  count: number;
  bytesPerCell: number;
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

interface HoverTip {
  left: number;
  top: number;
  text: string;
}

export function ProgressMatrix({ task }: { task: TaskView }) {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const [tip, setTip] = useState<HoverTip | null>(null);

  const total = task.totalBytes;
  const plan = useMemo(() => planCells(total), [total]);
  // 速度每 250ms 抖一次，但区间划分与已下载偏移都不会因此改变：
  // 用签名当依赖，避免通道速度变化引起无谓重绘
  const signature = task.segments.length > 1
    ? task.segments.map((segment) => segment.downloaded).join(',')
    : String(task.downloadedBytes);
  const fill = useMemo(() => fillRatios(task, plan), [plan, signature]);
  // 只有真正在下载时才把未完成的格子画成蓝色，否则蓝色会误报「还在传」
  const running = task.status === 'downloading';

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;

    const draw = () => {
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
        context.fillStyle = TRACK;
        context.fillRect(x, y, width, height);
        if (ratioDone > 0) {
          context.fillStyle = ratioDone >= 1
            ? DONE
            : running
              ? `rgba(${ACTIVE[0]}, ${ACTIVE[1]}, ${ACTIVE[2]}, ${(0.25 + 0.75 * ratioDone).toFixed(3)})`
              : HELD;
          context.fillRect(x, y, ratioDone >= 1 ? width : Math.max(1, width * ratioDone), height);
        }
      }
    };

    draw();
    // 侧边栏折叠、窗口缩放都会改变可用宽度，位图必须跟着重画，
    // 否则会被 CSS 拉伸成模糊的一坨
    const observer = new ResizeObserver(draw);
    observer.observe(canvas);
    return () => observer.disconnect();
  }, [fill, plan, signature, running]);

  if (total <= 0 || task.status === 'probing') return null;

  const handleMove = (event: MouseEvent<HTMLCanvasElement>) => {
    const canvas = canvasRef.current;
    if (!canvas) return;
    const rect = canvas.getBoundingClientRect();
    const column = Math.min(COLS - 1, Math.floor((event.clientX - rect.left) / (rect.width / COLS)));
    const row = Math.min(plan.rows - 1, Math.floor((event.clientY - rect.top) / CELL_HEIGHT));
    const index = row * COLS + column;
    if (index < 0 || index >= plan.count) {
      setTip(null);
      return;
    }
    const start = Math.round(index * plan.bytesPerCell);
    const end = Math.round(Math.min(total, (index + 1) * plan.bytesPerCell));
    const done = fill[index];
    const state = done >= 1
      ? '已完成'
      : done > 0
        ? running
          ? '下载中'
          : '已下载，未在传输'
        : '未开始';
    // 外层容器有内边距，提示框相对容器定位
    const base = canvas.parentElement?.getBoundingClientRect() ?? rect;
    setTip({
      left: event.clientX - base.left,
      top: event.clientY - base.top,
      text: `${formatBytes(start)} - ${formatBytes(end)}　${(done * 100).toFixed(0)}%　${state}`,
    });
  };

  return (
    <div className="matrix-block">
      <div className="details-title">
        文件下载矩阵　每格约 {formatBytes(plan.bytesPerCell)}，悬停可看该段区间
      </div>
      <div className="matrix">
        <canvas
          ref={canvasRef}
          className="matrix-canvas"
          onMouseMove={handleMove}
          onMouseLeave={() => setTip(null)}
        />
        {tip && (
          <div className="matrix-tip" style={{ left: tip.left, top: tip.top }}>
            {tip.text}
          </div>
        )}
      </div>
    </div>
  );
}
