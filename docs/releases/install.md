<!-- 所有版本共用的安装说明，发布流水线会把它接在「本版更新」后面。
     版本相关的内容写在 docs/releases/<tag>.md，不要放这里。 -->

**macOS**：`.dmg`，Apple Silicon（arm64）。应用未公证，首次打开请右键 →「打开」，或到「系统设置 → 隐私与安全性」放行；若仍提示「已损坏」，确认来源后执行 `sudo xattr -dr com.apple.quarantine /Applications/Conflux.app`。

**Windows**：`-setup.exe`（NSIS 安装包，x64），内含 ed2k 侧车；需要系统已有 WebView2（Win10 1803 以上一般自带）。

ed2k 侧车与 `conflux-cli` 命令行客户端都会随安装包一起装上；安装版可在「设置 → 更新」检查新版本。

**已知限制**：BT 的公网 DHT／tracker 在部分网络下不可用（拿不到 peer、卡在「解析元数据」），可在设置里填 SOCKS5 代理走别的线路；未接做种，下载完即停。
