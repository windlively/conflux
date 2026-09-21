import { bootstrapApplication } from '@angular/platform-browser';

import { App } from './app/app';
import { appConfig } from './app/app.config';
import { windowMaterial } from './app/core/api';

// 平台决定侧边栏要不要给红黄绿灯留位置、拖拽带在不在，首帧就要用上，
// 所以在 bootstrap 之前先落好（材质名是异步的，到了再补）。
const root = document.documentElement;
root.dataset['platform'] = /mac/i.test(navigator.userAgent) ? 'macos' : 'windows';
void windowMaterial().then((material) => {
  root.dataset['material'] = material;
});

bootstrapApplication(App, appConfig).catch((err) => console.error(err));
