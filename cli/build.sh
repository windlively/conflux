#!/usr/bin/env bash
# 把 conflux-cli 编成 Tauri externalBin 认的名字：<name>-<target-triple>[.exe]
set -euo pipefail
cd "$(dirname "$0")"

target="${1:-$(rustc -vV | sed -n 's/^host: //p')}"
cargo build --release --target "$target"

suffix=""
case "$target" in
  *windows*) suffix=".exe" ;;
esac

out="../src-tauri/binaries/conflux-cli-$target$suffix"
mkdir -p "$(dirname "$out")"
cp "target/$target/release/conflux-cli$suffix" "$out"
chmod +x "$out"
ls -lh "$out"
