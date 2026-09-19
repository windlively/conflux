# 川流 Conflux

跨平台（macOS / Windows）多协议下载工具。当前版本实现 HTTP/HTTPS 多连接分段下载，界面可以看到每个下载通道自己的进度。

## 运行

```bash
pnpm install
pnpm tauri dev
```

## 测试与打包

```bash
cd src-tauri && cargo test --test engine   # 引擎端到端测试（内置 Range 测试服务器）
pnpm tauri build --debug                   # 调试包，产物在 src-tauri/target/debug/bundle/
pnpm tauri build                           # 正式包（尚未配置签名）
```

## 手工验证多连接下载

```bash
node tools/range-server.mjs --port 8787 --size 160 --throttle 400
# 然后在应用里粘贴 http://127.0.0.1:8787/test.bin，连接数填 4
```

`--throttle` 是每连接限速（KB/s）。不限速时本机回环太快，界面来不及显示过程。

## 结构

- `src-tauri/src/download/` — 下载引擎：资源探测、分片划分与调度、任务与队列管理
- `src-tauri/tests/engine.rs` — 引擎端到端测试，自带最小 HTTP 测试服务器
- `src/` — React 界面：任务总进度、通道进度表格、文件分片地图
- `tools/range-server.mjs` — 本地 Range 测试服务器

协议规划、架构决策与实现进度见 `~/跨平台多协议下载工具开发方案.md`（随项目同步更新）。
