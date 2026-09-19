import type { CSSProperties, ReactNode } from 'react';

import { useStyleMode } from '../style';

// 只有 Material 皮肤才用官方组件；iOS 皮肤走原生元素 + 自己写的 CSS。
// 这些组件带 lit 运行时的样式与状态层（涟漪、悬停层、聚焦环），按需引入。
import '@material/web/button/filled-button.js';
import '@material/web/button/text-button.js';
import '@material/web/chips/assist-chip.js';
import '@material/web/iconbutton/icon-button.js';
import '@material/web/progress/linear-progress.js';
import '@material/web/ripple/ripple.js';
import '@material/web/textfield/outlined-text-field.js';

/** 自定义属性要放进 style，React 的类型不认 `--x`，统一在这里转一次 */
const tokens = (map: Record<string, string>) => map as CSSProperties;

export type ProgressTone = 'accent' | 'green' | 'red' | 'orange' | 'gray';

const TONE_COLOR: Record<ProgressTone | 'neutral', string> = {
  accent: 'var(--m3-primary)',
  green: '#2e7d32',
  red: 'var(--m3-error)',
  orange: '#b26a00',
  gray: 'var(--m3-outline)',
  neutral: 'var(--m3-outline)',
};

const CHIP_TONE: Record<string, { container: string; label: string }> = {
  accent: { container: 'var(--m3-primary-container)', label: 'var(--m3-on-primary-container)' },
  green: { container: 'var(--m3-green-container)', label: 'var(--m3-on-green-container)' },
  red: { container: 'var(--m3-error-container)', label: 'var(--m3-on-error-container)' },
  orange: { container: 'var(--m3-orange-container)', label: 'var(--m3-on-orange-container)' },
  neutral: { container: 'var(--m3-surface-variant)', label: 'var(--m3-on-surface-variant)' },
};

/** 任务状态、通道状态共享一套语义色 */
function stateTone(state: string): ProgressTone | 'neutral' {
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

interface ButtonProps {
  variant?: 'filled' | 'text' | 'icon';
  danger?: boolean;
  /** 二次确认已触发：危险按钮转成实心红 */
  armed?: boolean;
  title?: string;
  disabled?: boolean;
  onClick?: () => void;
  children: ReactNode;
}

export function Button({
  variant = 'text',
  danger = false,
  armed = false,
  children,
  ...rest
}: ButtonProps) {
  const mode = useStyleMode();

  if (mode === 'material') {
    if (variant === 'icon') {
      return (
        <md-icon-button title={rest.title} disabled={rest.disabled} onClick={rest.onClick}>
          {children}
        </md-icon-button>
      );
    }
    if (variant === 'filled' || armed) {
      return (
        <md-filled-button
          title={rest.title}
          disabled={rest.disabled}
          onClick={rest.onClick}
          style={
            danger || armed
              ? tokens({
                  '--md-filled-button-container-color': 'var(--m3-error)',
                  '--md-filled-button-label-text-color': '#fff',
                })
              : undefined
          }
        >
          {children}
        </md-filled-button>
      );
    }
    return (
      <md-text-button
        title={rest.title}
        disabled={rest.disabled}
        onClick={rest.onClick}
        style={tokens(
          danger
            ? { '--md-text-button-label-text-color': 'var(--m3-error)' }
            : { '--md-text-button-label-text-color': 'var(--m3-primary)' },
        )}
      >
        {children}
      </md-text-button>
    );
  }

  const classes = [
    variant === 'filled' ? 'primary' : 'ghost',
    variant === 'icon' ? 'icon' : '',
    danger && variant !== 'filled' ? 'danger' : '',
    armed ? 'armed' : '',
  ]
    .filter(Boolean)
    .join(' ');

  return (
    <button className={classes} title={rest.title} disabled={rest.disabled} onClick={rest.onClick}>
      {children}
    </button>
  );
}

interface StatusChipProps {
  /** 任务状态或通道状态，两套状态名由这里统一映射到外观 */
  state: string;
  variant?: 'task' | 'channel';
  label: string;
  spinner?: boolean;
  extra?: ReactNode;
}

export function StatusChip({ state, variant = 'task', label, spinner, extra }: StatusChipProps) {
  const mode = useStyleMode();

  if (mode === 'material') {
    const tone = CHIP_TONE[stateTone(state)];
    return (
      <md-assist-chip
        label={label}
        style={tokens({
          '--md-assist-chip-container-color': tone.container,
          '--md-assist-chip-label-text-color': tone.label,
          '--md-assist-chip-outline-width': '0px',
          '--md-assist-chip-container-height': '24px',
          '--md-assist-chip-container-shape': '8px',
          '--md-assist-chip-label-text-size': '12px',
        })}
      >
        {spinner ? <span slot="icon" className="spinner" /> : null}
      </md-assist-chip>
    );
  }

  return (
    <span className={`chip ${variant === 'channel' ? `chip-channel-${state}` : `chip-${state}`}`}>
      {spinner && <span className="spinner" />}
      {label}
      {extra}
    </span>
  );
}

interface SegmentBarProps {
  /** flex 权重，按分片长度分配宽度 */
  grow: number;
  /** 0–1；未知进度传 null */
  ratio: number | null;
  /** 分片状态：pending / downloading / completed / idle */
  state: string;
  title: string;
}

export function SegmentBar({ grow, ratio, state, title }: SegmentBarProps) {
  const mode = useStyleMode();
  const tone = stateTone(state);

  return (
    <div className="segment" style={{ flexGrow: grow }} title={title}>
      {mode === 'material' ? (
        <md-linear-progress
          value={ratio ?? 0}
          style={tokens({
            '--md-linear-progress-active-indicator-color': TONE_COLOR[tone],
            '--md-linear-progress-track-color': 'var(--m3-surface-variant)',
            '--md-linear-progress-track-height': '6px',
            '--md-linear-progress-active-indicator-height': '6px',
          })}
        />
      ) : (
        <div
          className={`segment-fill segment-fill-${state}`}
          style={{ width: `${(ratio ?? 0) * 100}%` }}
        />
      )}
    </div>
  );
}

interface TotalBarProps {
  /** 0–100 的百分比 */
  ratio: number | null;
  state: string;
  /** 任务整体进度条还是通道分片进度条（iOS 皮肤两种高度不同） */
  variant?: 'task' | 'channel';
}

export function TotalBar({ ratio, state, variant = 'task' }: TotalBarProps) {
  const mode = useStyleMode();

  if (mode === 'material') {
    return (
      <md-linear-progress
        value={(ratio ?? 0) / 100}
        style={tokens({
          '--md-linear-progress-active-indicator-color': TONE_COLOR[stateTone(state)],
          '--md-linear-progress-track-color': 'var(--m3-surface-variant)',
        })}
      />
    );
  }

  return (
    <div className={`bar ${variant === 'channel' ? 'bar-channel' : 'bar-total'}`}>
      <div className={`bar-fill bar-fill-${state}`} style={{ width: `${ratio ?? 0}%` }} />
    </div>
  );
}

interface TextFieldProps {
  value: string;
  onChange: (value: string) => void;
  placeholder?: string;
  label?: string;
  type?: 'text' | 'number' | 'textarea';
  rows?: number;
  className?: string;
}

export function TextField({
  value,
  onChange,
  placeholder,
  label,
  type = 'text',
  rows = 2,
  className,
}: TextFieldProps) {
  const mode = useStyleMode();

  if (mode === 'material') {
    return (
      <md-outlined-text-field
        className={className}
        value={value}
        label={label}
        placeholder={placeholder}
        type={type === 'textarea' ? 'textarea' : type}
        rows={type === 'textarea' ? rows : undefined}
        style={tokens({
          '--md-outlined-text-field-top-space': type === 'textarea' ? '12px' : '4px',
          '--md-outlined-text-field-bottom-space': type === 'textarea' ? '12px' : '4px',
        })}
        onInput={(event) => onChange((event.target as HTMLInputElement).value)}
      />
    );
  }

  if (type === 'textarea') {
    return (
      <textarea
        className="input-url"
        rows={rows}
        value={value}
        placeholder={placeholder}
        onChange={(event) => onChange(event.target.value)}
      />
    );
  }

  return (
    <input
      className={className}
      value={value}
      placeholder={placeholder}
      type={type}
      onChange={(event) => onChange(event.target.value)}
    />
  );
}

/** Material 的点击涟漪；iOS 皮肤没有这个层 */
export function Ripple() {
  return useStyleMode() === 'material' ? <md-ripple /> : null;
}
