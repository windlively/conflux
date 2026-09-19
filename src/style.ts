import { createContext, useContext } from 'react';

/** 皮肤：iOS（原生元素 + 自己写的 CSS）或 Material（@material/web 官方组件） */
export type StyleMode = 'ios' | 'material';

export const StyleModeContext = createContext<StyleMode>('ios');

export const useStyleMode = () => useContext(StyleModeContext);
