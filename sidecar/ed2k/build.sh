#!/usr/bin/env bash
# 把 ed2k 侧车编成两条发布链路要用的二进制，放进 src-tauri/binaries/。
# Tauri 的 externalBin 要求文件名带目标三元组后缀，这里按它认的格式命名。
#
# 需要 Go >= 1.24.2（goed2k 的 go.mod 要求）；首次构建会联网拉依赖。
set -euo pipefail
cd "$(dirname "$0")"

out="$(cd ../.. && pwd)/src-tauri/binaries"
mkdir -p "$out"

# 静态链接：侧车只在 Conflux 自己的进程组里跑，不需要 libc 之外的动态依赖
export CGO_ENABLED=0
flags=(-trimpath -ldflags "-s -w")

echo "==> darwin/arm64"
GOOS=darwin GOARCH=arm64 go build "${flags[@]}" -o "$out/ed2k-sidecar-aarch64-apple-darwin" .

echo "==> windows/amd64"
GOOS=windows GOARCH=amd64 go build "${flags[@]}" -o "$out/ed2k-sidecar-x86_64-pc-windows-msvc.exe" .

ls -lh "$out"/ed2k-sidecar-*
