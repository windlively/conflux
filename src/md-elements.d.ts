import type { CSSProperties, ReactNode } from 'react';

/**
 * @material/web 自定义元素在 JSX 里的最小声明：只写本项目真正用到的属性。
 * React 19 会把与元素属性同名的 props 按属性写入，其余按 attribute 写入。
 */
interface MdElementProps {
  children?: ReactNode;
  className?: string;
  style?: CSSProperties;
  title?: string;
  disabled?: boolean;
  label?: string;
  value?: string | number;
  type?: string;
  rows?: number;
  placeholder?: string;
  slot?: string;
  onClick?: () => void;
  onInput?: (event: Event) => void;
}

declare module 'react' {
  namespace JSX {
    interface IntrinsicElements {
      'md-filled-button': MdElementProps;
      'md-text-button': MdElementProps;
      'md-outlined-button': MdElementProps;
      'md-icon-button': MdElementProps;
      'md-outlined-text-field': MdElementProps;
      'md-linear-progress': MdElementProps;
      'md-assist-chip': MdElementProps;
      'md-ripple': MdElementProps;
    }
  }
}
