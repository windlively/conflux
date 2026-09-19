import { invoke } from '@tauri-apps/api/core';

/** 实际生效的窗口材质：glass / vibrancy / mica / acrylic / blur / none */
export const windowMaterial = () => invoke<string>('window_material');
