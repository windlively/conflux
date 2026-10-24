---
name: conflux-cli
description: 用 conflux-cli 让川流（Conflux）下载 HTTP/磁力/种子/ed2k 文件，并查询、等待、暂停或移除任务。当用户要求下载文件、查看下载进度，或需要通过命令行调用下载工具时使用。
---

# 川流命令行（conflux-cli）

`conflux-cli` 不自己下载，而是把任务交给正在运行的川流应用，和界面共用队列、连接数上限与断点续传。任务会出现在川流的任务列表里。

## 前提

1. 川流正在运行。
2. 在川流「设置 → 系统」里开启「命令行工具」（默认关闭）。
3. 找到 CLI 可执行文件，按顺序尝试：
   ```bash
   command -v conflux-cli
   ls "/Applications/Conflux.app/Contents/MacOS/conflux-cli"   # 安装版 macOS
   ls "C:/Program Files/Conflux/conflux-cli.exe"                # 安装版 Windows
   ls cli/target/release/conflux-cli                           # 仓库开发版（需先构建）
   ```
   下文用 `$CLI` 表示找到的路径。

## 最常用

```bash
# 添加并等到下载结束（退出码 0 完成，1 失败/取消）
"$CLI" add "https://example.com/file.zip" --dir "$HOME/Downloads" -c 8 --wait --json

# 只添加，拿任务 id
"$CLI" add "magnet:?xt=urn:btih:..." -c 8 --json

# 查所有任务
"$CLI" list --json

# 查单个任务 / 等待某个任务
"$CLI" status <id> --json
"$CLI" wait <id> --json
```

## 命令

| 命令 | 说明 |
| --- | --- |
| `add <链接或 .torrent 路径>` | 支持 http(s)、`magnet:`、`ed2k://`、本地 `.torrent` |
| `list` | 列出全部任务 |
| `status <id>` | 单个任务详情 |
| `wait <id>` | 轮询到完成/失败/取消，返回最终任务 |
| `pause <id>` / `resume <id>` | 暂停 / 继续 |
| `rm <id>` | 从列表移除（加上 `--delete-file` 会删本地文件） |
| `info` | 应用版本、PID、进行中数量 |

`add` 可选参数：
- `--dir <目录>`：保存目录；不传用应用的默认目录。
- `-c, --connections <n>`：连接数；不传用应用的默认连接数。
- `--files 0,2,5`：BT 任务只下指定序号的种子文件。
- `--wait`：添加后直接等待结束。

## 输出

- 加 `--json` 时输出机器可读 JSON；任务对象字段与界面一致：`id`、`fileName`、`status`、`totalBytes`、`downloadedBytes`、`speedBps`、`savePath`、`error` 等。
- `status` 的 `status` 取值：`queued`、`probing`、`downloading`、`paused`、`completed`、`failed`、`cancelled`。
- `wait` 的退出码：`0` 完成，`1` 失败/取消，`2` 参数或连接错误。`wait --json` 只在结束时输出最终任务。
- 错误走 stderr；JSON 只走 stdout，可直接用 `jq`/脚本解析。

## 排错

- `找不到命令行服务`：川流没启动，或设置里的「命令行工具」没开启。
- `连不上川流`：应用已退出，或刚关闭了命令行工具；重新打开应用并开启开关。
- `鉴权失败`：在设置里关掉再打开一次「命令行工具」，CLI 会读取新的发现文件。
- `任务不存在`：id 写错，或任务已被移除。

## 注意

- 下载会真实写入磁盘；`rm --delete-file` 会删文件，用户没有明确要求时不要加。
- 一次只执行一条命令；`wait` 可能持续很久，需要并行时在后台运行，别重复提交同一任务。
- 任务会出现在 GUI 里，连接数受应用的「总连接数上限」约束。
