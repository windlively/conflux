import {
  ApplicationConfig,
  inject,
  provideAppInitializer,
  provideBrowserGlobalErrorListeners,
} from '@angular/core';

import { Settings } from './core/settings';

export const appConfig: ApplicationConfig = {
  providers: [
    provideBrowserGlobalErrorListeners(),
    // 设置服务在构造函数里做一次性同步（主题、托盘开关、Dock 图标），启动时就要实例化
    provideAppInitializer(() => {
      inject(Settings);
    }),
  ],
};
