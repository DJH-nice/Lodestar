#!/usr/bin/env bash
# 打包一个【可分发版】的 Lodestar.app。
#
# 【和上一版的根本区别】
#   上一版要把 shim dylib 的 **18 个 Homebrew 依赖**（glib/pango/harfbuzz/icu…）
#   全部嵌进 Contents/Frameworks/，并改写它们的引用 ——
#   那 42M 的 Frameworks 是 .app 变大的唯一原因（ICU 一家 34M）。
#   现在引擎是**纯 Rust**（`search-core` crate），**没有任何外部 dylib** →
#   这一步整个删掉了。.app 从 60M 降到十几 M。
#
# 【不包含】Developer ID 签名、公证（$99/年，用户明确不做）——
#   别人首次打开仍需 xattr -cr 或「仍要打开」。
#
# 用法（从仓库根）：
#     bash app/scripts/bundle-app.sh
#     bash app/scripts/bundle-app.sh --self-check <二进制路径>
#         只跑「二进制相关」的自检（rpath / 前端嵌入 / devUrl）。
#         **回归测试用**：把 custom-protocol feature 去掉重编后，
#         这个模式必须【红】—— 那是「前端没嵌进 .app」的判据。
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
APP_DIR="$REPO/app"
TAURI_DIR="$APP_DIR/src-tauri"
DIST_DIR="$APP_DIR/dist"

# ---------------------------------------------------------------- 工具函数

# 读出某个 Mach-O 的全部 rpath，每行一条。
#
# 【为什么不用 `otool -l | grep -A2 LC_RPATH | grep path` 直接拿】
#   后面接 `grep -q` 的话，grep -q 命中即退出 → 上游收到 SIGPIPE(141)
#   → `set -o pipefail` 下整条管道判为失败 → 条件判断会走错分支。
#   同样，`find ... | head -30` 在 pipefail 下也会因为 SIGPIPE 让脚本以 141 退出。
#   所以：**先把输出收进变量，再在变量上做判断**；要截断就用 `head -N <<< "$VAR"`。
read_rpaths() {
  otool -l "$1" | awk '/cmd LC_RPATH/{f=1; next} f && $1=="path"{print $2; f=0}'
}

# 从 cargo metadata 推导 target 目录。
# 【为什么要推导而不是写死 target/release】——
#   环境里有 CARGO_TARGET_DIR 时，cargo 与 tauri bundler 都会跟着它走，
#   写死路径会让脚本找不到产物、误报「打包产物不存在」。
cargo_target_dir() {
  local dir
  if dir="$(cd "$TAURI_DIR" && cargo metadata --no-deps --format-version 1 2>/dev/null \
      | python3 -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])' 2>/dev/null)"; then
    printf '%s' "$dir"
  else
    printf '%s' "$TAURI_DIR/target"
  fi
}

# ------------------------------------------------------- 自检：二进制相关

# 只跟【二进制】有关的自检。$1 = 二进制路径。
# 拆出来单独跑，是为了让「去掉 feature 后必须变红」这个回归测试可复现。
check_binary() {
  local bin="$1"
  local rc=0
  local rpaths bin_strings
  rpaths="$(read_rpaths "$bin")"

  echo "=== 自检：二进制 ==="

  # 1) rpath 里【不能】有仓库绝对路径
  #    （功能无害，但会把家目录路径烤进要分发的 .app）
  local abs_rpaths=""
  while IFS= read -r rp; do
    [[ -z "$rp" ]] && continue
    case "$rp" in
      @executable_path/*) ;;
      *) abs_rpaths+="$rp"$'\n' ;;
    esac
  done <<< "$rpaths"
  if [[ -z "${abs_rpaths//[$'\n' ]/}" ]]; then
    echo "✓ rpath 里没有绝对路径"
  else
    echo "✗ rpath 里还有绝对路径：${abs_rpaths//$'\n'/ }" >&2
    rc=1
  fi

  # 2) 【前端必须嵌进二进制】—— 最关键的一条
  #    tauri 的 custom-protocol feature 关掉时，tauri-codegen 会走
  #    `else if dev && config.build.dev_url.is_some() { EmbeddedAssets::default() }`
  #    —— dist 不嵌进去，webview 会去请求 http://localhost:1420（vite dev server）
  #    → 连不上 → 白页，GUI 完全不可用（Rust 侧引擎照跑，所以「看起来启动了」）。
  #
  #    判据：二进制里能找到 dist/assets/ 下【实际产出的】那些带 hash 的文件名。
  #    —— 去掉 feature 时二进制里不会有它们，这里就红。
  #
  #    【为什么不用 devUrl 当判据】—— 实测踩过：
  #    `http://localhost:1420/` 这个字符串在【两种构建里都在】，
  #    因为 tauri.conf.json 整个被嵌进二进制，而 devUrl 是 Url 类型，
  #    重新序列化时会补上尾斜杠。它跟「webview 实际去不去连那个地址」无关。
  shopt -s nullglob
  local assets=("$DIST_DIR"/assets/*.js "$DIST_DIR"/assets/*.css)
  shopt -u nullglob
  if [ "${#assets[@]}" -eq 0 ]; then
    echo "✗ $DIST_DIR/assets/ 里没有 js/css —— 前端没构建过" >&2
    rc=1
  else
    bin_strings="$(strings "$bin")"
    local hit=0 base
    for a in "${assets[@]}"; do
      base="$(basename "$a")"
      if [[ "$bin_strings" == *"$base"* ]]; then
        echo "  · 命中前端资源 $base"
        hit=$((hit + 1))
      else
        echo "  · 缺 前端资源 $base"
      fi
    done
    if [ "$hit" -eq "${#assets[@]}" ]; then
      echo "✓ 前端已嵌进二进制（$hit/${#assets[@]} 个前端资源都在，custom-protocol feature 生效）"
    else
      echo "✗ 前端【没嵌进二进制】（$hit/${#assets[@]}）—— .app 会白页！" >&2
      echo "    webview 会去连 http://localhost:1420（vite dev server），连不上就白屏。" >&2
      echo "    原因多半是编译 Rust 时没加 feature tauri/custom-protocol。" >&2
      rc=1
    fi
  fi

  # 3) 【不许有外部 dylib 依赖】—— 「轻量」这条线的护栏
  #
  #    引擎是纯 Rust 的 → 二进制只该依赖系统框架（/System/...、/usr/lib/...）。
  #    一旦出现 Homebrew 或别的第三方路径（/opt/homebrew/...）→
  #    ① .app 挪到别的机器就起不来；
  #    ② 体积会重新涨回去（上一版就是 42M 的 Frameworks）。
  #    → 出现非系统依赖就红。
  local deps non_system
  deps="$(otool -L "$bin")"
  non_system="$(awk 'NR>1 {print $1}' <<< "$deps" \
    | grep -v '^/System/' | grep -v '^/usr/lib/' | grep -v '^@rpath/' \
    | grep -v '^@executable_path/' | grep -v '^@loader_path/' || true)"
  if [ -z "$non_system" ]; then
    echo "✓ 二进制只依赖系统框架（没有外部 dylib）"
  else
    echo "✗ 二进制有非系统依赖（.app 挪到别的机器会起不来）：" >&2
    printf '    %s\n' $non_system >&2
    rc=1
  fi

  return "$rc"
}

# ---------------------------------------------------------- 自检：.app 相关

# 检查【.app 目录结构】有关的自检。$1 = .app 路径。
check_bundle() {
  local app_dir="$1"
  local rc=0

  echo "=== 自检：.app 结构 ==="

  # 签名必须【真的有效】——
  #   check_bundle 是在签名【之后】跑的（脚本顺序：构建 → 签名 → 自检），
  #   所以这里验的就是刚签的那一份。
  #   （以前根本不验签名：脚本说「全部自检通过」，产物却可能没签 → 双击开不了。）
  if codesign --verify --deep --strict "$app_dir"; then
    echo "✓ 签名有效（codesign --verify --deep --strict）"
  else
    echo "✗ 签名校验失败 —— .app 在 Finder 里双击会被 Gatekeeper 拦下" >&2
    rc=1
  fi

  # Frameworks 应该是空的 —— 有东西就说明混进外部依赖了
  if [ -d "$app_dir/Contents/Frameworks" ]; then
    local fw_count
    fw_count="$(find "$app_dir/Contents/Frameworks" -type f | wc -l | tr -d ' ')"
    if [ "$fw_count" -eq 0 ]; then
      echo "✓ Contents/Frameworks/ 是空的（没有外部 dylib 要带）"
    else
      echo "✗ Contents/Frameworks/ 里有 $fw_count 个文件 —— 引擎不该有外部依赖" >&2
      rc=1
    fi
  else
    echo "✓ 没有 Contents/Frameworks/ 目录（没有外部 dylib 要带）"
  fi

  return "$rc"
}

# ================================================================== 主流程

# --- 模式：只跑二进制自检（回归测试用）---
if [ "${1:-}" = "--self-check" ]; then
  [ -n "${2:-}" ] || { echo "用法: $0 --self-check <二进制路径>" >&2; exit 2; }
  check_binary "$2"
  exit $?
fi

# --- 目标目录（跟着 CARGO_TARGET_DIR 走）---
TARGET_DIR="$(cargo_target_dir)"
BUNDLE="$TARGET_DIR/release/bundle/macos"
APP="$BUNDLE/Lodestar.app"
BIN="$APP/Contents/MacOS/lodestar-app"

echo "=== 1/5 构建 + 打包 .app ==="
# 【必须用 `tauri build`，不能用 `tauri bundle`】
#
#   `tauri bundle` **只**打包，它既不编译 Rust 也不跑 build.beforeBuildCommand。
#   而编译 Rust 时必须带 feature `tauri/custom-protocol` ——
#   crates/tauri-cli/src/interface/rust.rs 的 build_options() 里：
#       features.push("tauri/custom-protocol".into());
#   这个调用**只在 `tauri build` 的路径上**（cli/src/build.rs），
#   `tauri bundle`（cli/src/bundle.rs）根本不碰它。
#
#   少了这个 feature，tauri-codegen 会走
#       else if dev && config.build.dev_url.is_some() { EmbeddedAssets::default() }
#   —— dist 不嵌进二进制，webview 去请求 http://localhost:1420（vite dev server）
#   → 连不上 → **白页，GUI 完全不可用**（Rust 侧引擎照跑，所以「看起来启动了」）。
#
# 【不需要先构建什么 C 垫片】—— 上一版这里要先 `cd shim && make`。
#   引擎现在是纯 Rust 的，cargo 直接编进去，什么都不用先构建。
(cd "$APP_DIR" && pnpm tauri build --bundles app)

[ -d "$APP" ] || { echo "打包产物不存在: $APP" >&2; exit 1; }

echo
echo "=== 2/5 删掉 rpath 里的仓库绝对路径 ==="
# 打包出来的 .app 要分发，不能把 /Users/<谁>/... 这种家目录路径烤进去。
# 【必须在签名之前做】—— install_name_tool 会改二进制，改完签名就失效了。
rpaths="$(read_rpaths "$BIN")"
removed=0
while IFS= read -r rp; do
  [ -n "$rp" ] || continue
  case "$rp" in
    @executable_path/*) continue ;;
  esac
  install_name_tool -delete_rpath "$rp" "$BIN" 2>/dev/null || true
  echo "  已删除 rpath: $rp"
  removed=$((removed + 1))
done <<< "$rpaths"
[ "$removed" -eq 0 ] && echo "  （没有绝对路径 rpath 可删）"

echo
echo "=== 3/5 装入许可文件 ==="
# 【装在哪】Contents/Resources/licenses/ —— macOS 的标准位置。
#   Finder 里右键 .app →「显示包内容」就能看到。
#
# 【和上一版的区别】上一版这里装的是 **GPL 许可证 + 完整对应源码压缩包（14MB）**
#   —— 那是分发 GPL 程序的义务。现在引擎完全自研、没有任何第三方引擎代码，
#   **不再有「必须附带对应源码」的义务** → 源码包这一项整个删掉，
#   .app 的 Resources 从 14M 降到 KB 级。
#
# 【必须��签名之前】—— 签完名再往包里加文件，签名就失效了。
LICENSE_DIR="$APP/Contents/Resources/licenses"
mkdir -p "$LICENSE_DIR"
if [ -f "$REPO/LICENSE" ]; then
  cp "$REPO/LICENSE" "$LICENSE_DIR/LICENSE"
fi
if [ -f "$REPO/THIRD-PARTY-NOTICES.md" ]; then
  cp "$REPO/THIRD-PARTY-NOTICES.md" "$LICENSE_DIR/THIRD-PARTY-NOTICES.md"
fi

cat > "$LICENSE_DIR/源码获取方式.txt" <<'SRC_NOTICE'
Lodestar —— 源码获取方式
======================

本软件的源码在项目仓库里（LICENSE 文件里写着地址）。

如果你手上的这份二进制没有附带仓库地址，说明分发者还没填 ——
请通过分发渠道联系分发者索取。
SRC_NOTICE

REPO_URL="$(cd "$REPO" && git remote get-url origin 2>/dev/null || true)"
if [ -n "$REPO_URL" ]; then
  printf '  项目仓库：%s\n' "$REPO_URL" >> "$LICENSE_DIR/源码获取方式.txt"
fi

echo "  许可文件：$(ls -1 "$LICENSE_DIR" | tr '\n' ' ')"

echo
echo "=== 4/5 ad-hoc 签名 ==="
# Tauri 自己不签：bundle.macOS.signingIdentity 没配 →
# bundler 的 keychain() 返回 None → 整段签名逻辑被跳过。
# 这里手搓一个 ad-hoc 签名（codesign -s -），没有 Developer ID（用户已确认不买账号）。
# ad-hoc 能让 Gatekeeper 少弹一次，但别人首次打开仍需「右键 → 打开」。
#
# 【签名失败必须 exit 1】—— 失败还 exit 0 的话，脚本报「全部自检通过」，
#   产物却没签名 → 在 Finder 里双击就开不了（Gatekeeper 直接拦）。
if ! codesign --force --deep --sign - "$APP"; then
  echo "✗ ad-hoc 签名失败 —— .app 在 Finder 里双击会被 Gatekeeper 拦下" >&2
  exit 1
fi
echo "ad-hoc 签名完成"

# ================================================================== 5/5 打 .dmg
echo
echo "=== 5/5 打 .dmg（macOS 的标准分发容器）==="

# 【为什么 .app 还要配一个 .dmg】
#   `.app` 是**一个目录**。双击能跑，但传输要压缩、首次打开会被 Gatekeeper 拦。
#   `.dmg` 是 macOS 上把软件交给别人的**标准容器**。
#
# 【为什么自己搓而不用 tauri's dmg bundler】
#   `tauri build --bundles dmg` 会**重新打包**一个 .app —— 那个 .app 里
#   没有我们装进去的许可文件。
#   → 只能拿**已经打好、已经签好名**的 .app 装进 .dmg。
DMG_OUT="$BUNDLE/Lodestar.dmg"
DMG_STAGE="$(mktemp -d)"
cleanup_dmg_stage() { [ -d "$DMG_STAGE" ] && rm -rf "$DMG_STAGE"; }
trap cleanup_dmg_stage EXIT

# APFS 上 `cp -R` 是**写时复制**（不占额外空间、瞬时完成）
cp -R "$APP" "$DMG_STAGE/"
# 标准的「拖到应用程序」快捷方式
ln -s /Applications "$DMG_STAGE/Applications"

# 映像里放一份说明 —— 对方第一次打开会被 Gatekeeper 拦，得知道怎么办
cat > "$DMG_STAGE/请先读我.txt" <<'DMG_README'
Lodestar macOS —— 使用说明
=========================

安装
----
1. 把左边的「Lodestar.app」拖到右边的「应用程序」里。
2. 首次打开时，macOS 可能会说「无法验证开发者」。
   —— 因为这个 app 没有付费的 Apple 开发者证书（Developer ID）。
   —— 绕过办法：对着 Lodestar.app **右键 → 打开** → 再点「打开」。
      （或者在终端里执行：xattr -cr /Applications/Lodestar.app）

使用
----
· 打开后到「设置」里选要索引的目录，点「应用并重建索引」。
· 搜索框直接打字；支持 ext: / path: / size: / dm: 等语法（按 ⌘/ 看速查表）。
· 顶部菜单栏「帮助 → 完整手册」是完整说明。

提示
----
· 索引整个家目录（$HOME）时，建议把设置里的「文件变化时自动刷新索引」关掉
  —— 家目录里别的程序一直在写东西，自动重扫会持续占用 CPU。

许可与源码
----
· 许可证全文、第三方组件声明、以及源码获取方式都装在应用包里：
  在访达里右键 Lodestar.app →「显示包内容」→ Contents/Resources/licenses/
· 应用内也可以看：顶部菜单栏「帮助 → 致谢与许可」。
DMG_README

# 打包：UDZO = 压缩的只读映像
hdiutil create -srcfolder "$DMG_STAGE" -volname "Lodestar" -fs HFS+ -format UDZO -ov "$DMG_OUT" >/dev/null
# 映像本身也签一下 —— 没签名的映像挂载后 macOS 会说「已被损坏」
codesign --force --sign - "$DMG_OUT" >/dev/null 2>&1 || true
echo "dmg 完成：$DMG_OUT"

echo
echo "=== .dmg 自检（挂载 → 验内容 → 卸载）==="
DMG_MOUNT="$(mktemp -d)"
if hdiutil attach "$DMG_OUT" -nobrowse -readonly -mountpoint "$DMG_MOUNT" >/dev/null 2>&1; then
  if [ -d "$DMG_MOUNT/Lodestar.app" ] && [ -L "$DMG_MOUNT/Applications" ]; then
    echo "  ✓ 映像里有 Lodestar.app"
    echo "  ✓ 映像里有「应用程序」快捷方式"
    echo "  ✓ 说明文件在：$([ -f "$DMG_MOUNT/请先读我.txt" ] && echo '有' || echo '缺')"
  else
    echo "  ✗ 映像内容不对" >&2
    hdiutil detach "$DMG_MOUNT" >/dev/null 2>&1 || true
    exit 1
  fi
  hdiutil detach "$DMG_MOUNT" >/dev/null 2>&1 || true
else
  echo "  ✗ 映像挂不上" >&2
  exit 1
fi
rmdir "$DMG_MOUNT" 2>/dev/null || true

echo
echo "=== 产物 ==="
echo "$APP"
du -sh "$APP"
echo "$DMG_OUT"
du -sh "$DMG_OUT"

echo
echo "=== .app 结构 ==="
# 【不用 `find | head`】—— head 先退出会让上游吃 SIGPIPE，
#   `set -o pipefail` 下整条管道判为 141，脚本就此失败。
app_tree="$(find "$APP" -maxdepth 3 -not -path '*/_up_/*' | sed "s|$APP|Lodestar.app|")"
head -30 <<< "$app_tree"

echo
echo "=== 二进制的 rpath ==="
read_rpaths "$BIN"

echo
echo "=== 二进制的外部依赖（应该只有系统框架）==="
otool -L "$BIN"

echo
check_binary "$BIN" || exit 1
check_bundle "$APP" || exit 1
echo
echo "✅ 全部自检通过"
