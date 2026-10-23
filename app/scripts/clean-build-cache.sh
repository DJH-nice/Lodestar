#!/usr/bin/env bash
# 清理 Rust 构建中间产物 —— 不动依赖库、不动已打好的 .app
#
# 【为什么需要它】app/ 的体积 99% 在 target/，而 target/ 里混着三类东西：
#   1. 必须留的：rlib / rmeta（依赖库，增量编译靠它）
#   2. 随用随生成的：deps/*.o（codegen 中间产物）、incremental/（增量缓存）
#   3. 打包完就没用的：release/deps、release/build
# 本脚本只动第 2、3 类。
#
# 【安全性依据】
#   - 删 deps/*.o 零风险：链接用的是 rlib，而 .rcgu.o 已被 ar 归档进 rlib
#     （`ar -t lib*.rlib` 能看到），裸 .o 只是中间副本。
#   - 删 release/deps + release/build 安全：实测 `cargo build --workspace` 不会重建它们，
#     只有 `cargo build --release` / `pnpm tauri build` 才会。
#   - 删 incremental/ 安全：只是缓存，代价是下次编译慢一点。
#
# 【用法】
#   bash app/scripts/clean-build-cache.sh              # 默认：清 .o + release 产物
#   bash app/scripts/clean-build-cache.sh --all        # 连 incremental/ 和 debug/build 一起清
#   bash app/scripts/clean-build-cache.sh --dry-run    # 只报体积，不删
#
# 【注意】删除走 mavis-trash（进回收站，可恢复）—— 磁盘空间要真正释放，
#         还得清空回收站。mavis-trash 是「全或无」：批里只要有一个路径不存在，
#         整批都会被拒绝，所以本脚本先逐个检查存在性。

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
TARGET="$ROOT/app/src-tauri/target"
DEBUG="$TARGET/debug"
RELEASE="$TARGET/release"
STAGE="$TARGET/.clean-stage"
TRASH="/Users/dongjiahao/.minimax/bin/mavis-trash"

DRY_RUN=0
ALL=0
for arg in "$@"; do
  case "$arg" in
    --dry-run) DRY_RUN=1 ;;
    --all) ALL=1 ;;
    *) echo "未知参数: $arg" >&2; exit 64 ;;
  esac
done

human() { du -sh "$1" 2>/dev/null | cut -f1; }

echo "清理前："
echo "  app/            $(human "$ROOT/app")"
echo "  target/debug    $(human "$DEBUG")"
echo "  target/release  $(human "$RELEASE")"
echo

targets=()

# 1. codegen 中间产物（量大、零风险）
if [ -d "$DEBUG/deps" ]; then
  o_count=$(find "$DEBUG/deps" -maxdepth 1 -name "*.o" -type f | wc -l | tr -d ' ')
  if [ "$o_count" -gt 0 ]; then
    if [ "$DRY_RUN" -eq 1 ]; then
      echo "[dry-run] deps/*.o  $o_count 个"
    else
      mkdir -p "$STAGE"
      find "$DEBUG/deps" -maxdepth 1 -name "*.o" -type f -print0 \
        | xargs -0 -r -n 500 sh -c 'mv -- "$@" "$0"' "$STAGE"
      targets+=("$STAGE")
      echo "[x] deps/*.o  $o_count 个 → 回收站"
    fi
  fi
fi

# 2. release 产物（打包完无用；已打好的 .app 在 release/bundle/，不动它）
for p in "$RELEASE/deps" "$RELEASE/build"; do
  if [ -e "$p" ]; then
    if [ "$DRY_RUN" -eq 1 ]; then
      echo "[dry-run] ${p#$ROOT/}  $(human "$p")"
    else
      targets+=("$p")
      echo "[x] ${p#$ROOT/}  $(human "$p") → 回收站"
    fi
  fi
done

# 3. --all：连增量缓存和 build script 输出一起清
if [ "$ALL" -eq 1 ]; then
  for p in "$DEBUG/incremental" "$DEBUG/build"; do
    if [ -e "$p" ]; then
      if [ "$DRY_RUN" -eq 1 ]; then
        echo "[dry-run] ${p#$ROOT/}  $(human "$p")"
      else
        targets+=("$p")
        echo "[x] ${p#$ROOT/}  $(human "$p") → 回收站"
      fi
    fi
  done
fi

if [ "$DRY_RUN" -eq 1 ]; then
  echo; echo "[dry-run] 未做任何删除"
  exit 0
fi

if [ "${#targets[@]}" -eq 0 ]; then
  echo "没有需要清理的内容"
  exit 0
fi

# mavis-trash 全或无：先过滤掉不存在的路径，否则整批会被拒
existing=()
for p in "${targets[@]}"; do
  [ -e "$p" ] && existing+=("$p")
done
if [ "${#existing[@]}" -eq 0 ]; then
  echo "没有需要清理的内容"
  exit 0
fi

MAVIS_TRASH_FORCE_MV=1 "$TRASH" -- "${existing[@]}" >/dev/null

# 隐藏名（.clean-stage）Finder 看不见，去掉点前缀才能被 Finder 永久移除
mv "$HOME/.Trash/.clean-stage" "$HOME/.Trash/clean-stage" 2>/dev/null || true

echo
echo "清理后："
echo "  app/            $(human "$ROOT/app")"
echo "  target/debug    $(human "$DEBUG")"
echo "  target/release  $(human "$RELEASE")"
echo
echo "提示：文件在回收站里，还要清空回收站才真正释放磁盘。"
echo "      已打好的 Lodestar.app 在 app/src-tauri/target/release/bundle/macos/ —— 未受影响。"
