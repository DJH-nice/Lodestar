#!/usr/bin/env python3
"""把 libfsearch_shim.dylib 的全部 Homebrew 依赖嵌进 .app 的 Contents/Frameworks/。

【为什么需要这个】
打包出来的 .app 里，shim dylib 依赖的 18 个 Homebrew 库（glib / pango / harfbuzz /
icu…）是用**绝对路径** /opt/homebrew/opt/... 引用的。拷到别的机器上，
dyld 直接加载失败 —— app 根本起不来。这是「给别人用」的硬门槛。

【这个脚本做三件事】
1. 递归算出 shim 的**完整**依赖闭包（含 @loader_path / @rpath 形式的引用）
2. 把这些库拷进 Contents/Frameworks/，并把**所有**引用改写成 @loader_path/...
3. 自检：闭包里不能有任何绝对路径残留，且所有 @ 引用都能解析到文件

【不负责签名】
签名是 bundle-app.sh 的第 5 步（codesign --force --deep --sign -）。
本脚本只做「拷贝 + 改 install_name」——**改完签名就废了**，
所以签名必须发生在本脚本【之后】。

【为什么用 Python 而不是 bash】
闭包计算要处理 realpath（Homebrew 的 opt/ 是软链）、@loader_path / @rpath
两种前缀、同一个库被不同字面路径引用、文件真名 ≠ 引用名 ——
bash 3.2 写这些太容易出错。

用法:
    python3 app/scripts/embed-deps.py [路径/Lodestar.app]
    python3 app/scripts/embed-deps.py --check-only [路径/Lodestar.app]
"""

from __future__ import annotations

import argparse
import os
import shutil
import subprocess
import sys
from collections import deque
from pathlib import Path

# macOS 自带的库 —— 不需要嵌入。
# 【注意】只排 /System/ 与 /usr/lib/，**不排整个 /usr/**：
#   Intel Mac 的 Homebrew 装在 /usr/local/，那是**外部依赖**，得嵌入。
SYSTEM_PREFIXES = ("/System/", "/usr/lib/")

# shim 的 install_name 保持 @rpath —— 主程序 lodestar-app 是用
# @rpath/libfsearch_shim.dylib 引它的，主程序的 rpath 是
# @executable_path/../Frameworks。改成 @loader_path 会让主程序
# 的 @loader_path 指向 Contents/MacOS/，找不到 shim。
SHIM_NAME = "libfsearch_shim.dylib"
SHIM_KEEP_ID = f"@rpath/{SHIM_NAME}"


def run(cmd: list[str]) -> subprocess.CompletedProcess:
    return subprocess.run(cmd, capture_output=True, text=True, check=False)


def otool_lines(path: Path) -> list[str]:
    proc = run(["otool", "-L", str(path)])
    return proc.stdout.splitlines()


def load_id(path: Path) -> str | None:
    """读 LC_ID_DYLIB；不是 dylib（没有 id）时返回 None。"""
    proc = run(["otool", "-D", str(path)])
    lines = proc.stdout.splitlines()
    if len(lines) > 1 and lines[1].strip():
        return lines[1].strip()
    return None


def dep_literals(path: Path) -> list[str]:
    """读 LC_LOAD_DYLIB 里的**字面路径**列表。

    【坑】otool -L 对 dylib 会把 LC_ID_DYLIB 也列在依赖里
    （输出第 2 行就是它自己）—— 那不是依赖，要跳过。
    """
    ident = load_id(path)
    deps: list[str] = []
    for line in otool_lines(path)[1:]:
        stripped = line.strip()
        if not stripped:
            continue
        literal = stripped.split(" (")[0].strip()
        if not literal:
            continue
        if ident and literal == ident:
            continue  # 这是它自己的 id，不是依赖
        deps.append(literal)
    return deps


def rpaths(path: Path) -> list[str]:
    out = run(["otool", "-l", str(path)]).stdout
    result: list[str] = []
    in_rpath = False
    for line in out.splitlines():
        if line.strip().startswith("cmd LC_RPATH"):
            in_rpath = True
            continue
        if in_rpath:
            parts = line.split()
            if len(parts) >= 2 and parts[0] == "path":
                result.append(parts[1])
                in_rpath = False
    return result


def is_system(path: str) -> bool:
    return path.startswith(SYSTEM_PREFIXES)


def expand_rpath(rp: str, loader: Path, main_exe: Path, name: str) -> str:
    if rp.startswith("@loader_path/"):
        return os.path.join(str(loader.parent), name)
    if rp.startswith("@executable_path/"):
        return os.path.join(str(main_exe.parent), name)
    if rp.startswith("@rpath/"):
        # 嵌套的 @rpath：按同样的规则再展开一层
        return os.path.join(str(loader.parent), rp.split("/", 1)[1])
    return os.path.join(rp, name)


def resolve(literal: str, loader: Path, main_exe: Path) -> str | None:
    """把一条字面依赖解析成磁盘上的候选路径（不保证存在）。"""
    if literal.startswith("@loader_path/"):
        return str(loader.parent / literal[len("@loader_path/"):])
    if literal.startswith("@executable_path/"):
        return str(main_exe.parent / literal[len("@executable_path/"):])
    if literal.startswith("@rpath/"):
        name = literal[len("@rpath/"):]
        # 顺序：① 加载者同目录 ② 加载者的 rpath ③ 主程序的 rpath
        # 【为什么①放最前】Homebrew 的库之间是同目录互相引用的
        #   （libbrotlidec → @rpath/libbrotlicommon.1.dylib，
        #    两个都在 /opt/homebrew/opt/brotli/lib/），
        #   而它们的 rpath 指向的是 .app 的 Frameworks（那时还没有这些库）。
        candidates = [str(loader.parent / name)]
        for rp in rpaths(loader):
            candidates.append(expand_rpath(rp, loader, main_exe, name))
        for rp in rpaths(main_exe):
            candidates.append(expand_rpath(rp, loader, main_exe, name))
        for cand in candidates:
            if os.path.exists(cand):
                return cand
        return candidates[0] if candidates else None
    if literal.startswith("/"):
        return literal
    return None


def canonical_name(real: Path) -> str:
    """打包时的**规范文件名** = LC_ID_DYLIB 的 basename。

    【坑】Homebrew 的 opt/ 是软链，磁盘真名带版本号：
        引用名 libicuuc.78.dylib   ← 真名 libicuuc.78.3.dylib
    dyld 找的是**引用名**，所以落盘必须用规范名。
    """
    ident = load_id(real)
    if ident and ident.startswith("/"):
        return os.path.basename(ident)
    return real.name


def build_plan(shim: Path, main_exe: Path) -> tuple[dict[Path, set[str]], dict[Path, list[tuple[str, Path]]]]:
    """算出依赖闭包。

    返回:
      targets  —— {真实文件路径: 引用它的所有字面路径}
      deps_of  —— {真实文件路径: [(字面路径, 目标真实文件)]}
    """
    targets: dict[Path, set[str]] = {}
    deps_of: dict[Path, list[tuple[str, Path]]] = {}

    queue: deque[Path] = deque([shim.resolve()])
    seen: set[Path] = set()

    while queue:
        current = queue.popleft()
        real = current.resolve()
        if real in seen:
            continue
        seen.add(real)
        deps_of.setdefault(real, [])

        for literal in dep_literals(current):
            candidate = resolve(literal, current, main_exe)
            if candidate is None:
                raise SystemExit(
                    f"✗ 无法解析依赖 {literal!r}（来自 {current}）—— 脚本需要更新"
                )
            if is_system(candidate):
                continue
            real_target = Path(candidate).resolve()
            if not real_target.exists():
                raise SystemExit(
                    f"✗ 依赖不存在: {candidate}\n"
                    f"    来自 {current}\n"
                    f"    → 装上对应的 brew 包，或更新本脚本"
                )
            targets.setdefault(real_target, set()).add(literal)
            deps_of[real].append((literal, real_target))
            queue.append(real_target)

    return targets, deps_of


def install_name_tool(args: list[str], target: Path) -> None:
    proc = run(["install_name_tool", *args, str(target)])
    if proc.returncode != 0:
        raise SystemExit(
            f"✗ install_name_tool 失败: {' '.join(args)} {target}\n{proc.stderr}"
        )


def embed(app: Path) -> None:
    frameworks = app / "Contents" / "Frameworks"
    shim = frameworks / SHIM_NAME
    main_exe = app / "Contents" / "MacOS" / "lodestar-app"

    if not shim.exists():
        raise SystemExit(f"✗ 找不到 {shim}")
    if not main_exe.exists():
        raise SystemExit(f"✗ 找不到 {main_exe}")

    print("=== 1/4 计算依赖闭包 ===")
    targets, deps_of = build_plan(shim, main_exe)
    # 根（shim 自己）不在 targets 里 —— targets 只装「外部库」
    canon = {real: canonical_name(real) for real in targets}

    # 规范名必须唯一，否则 @loader_path/<name> 会指向错的那个
    seen_names: dict[str, Path] = {}
    for real, name in canon.items():
        if name in seen_names and seen_names[name] != real:
            raise SystemExit(
                f"✗ 规范名冲突: {name}\n"
                f"    {seen_names[name]}\n    {real}\n"
                f"    → 需要给其中一个改名"
            )
        seen_names[name] = real

    total = sum(p.stat().st_size for p in targets)
    print(f"  外部依赖库: {len(targets)} 个，合计 {total / 1048576:.1f} MB")
    for real in sorted(targets, key=lambda p: p.stat().st_size, reverse=True):
        print(f"    {real.stat().st_size / 1048576:6.1f} MB  {canon[real]}")

    print()
    print("=== 2/4 拷贝到 Contents/Frameworks/ ===")
    for real, name in canon.items():
        dest = frameworks / name
        # 【为什么 copy 真名而不是软链】os.path.realpath 已经把软链解开了，
        #   shutil.copy2 拷的是**内容**。落盘名用规范名。
        shutil.copy2(str(real), str(dest))
        print(f"  {real.name}  →  {name}")

    print()
    print("=== 3/4 改写 install_name ===")
    # 3a) 每个库自己的 id
    for real, name in canon.items():
        dest = frameworks / name
        install_name_tool(["-id", f"@loader_path/{name}"], dest)
    # shim 自己的 id 保持 @rpath（主程序用它 + 主程序的 rpath 找得到）
    install_name_tool(["-id", SHIM_KEEP_ID], shim)
    print(f"  库 id → @loader_path/<规范名>（{len(canon)} 个）")
    print(f"  shim id → {SHIM_KEEP_ID}（保持不变）")

    # 3b) 每个库对外部库的引用
    changed = 0
    for real, deps in deps_of.items():
        # 找出这个库是谁：shim 还是某个外部库
        if real == shim.resolve():
            holder = shim
        else:
            holder = frameworks / canon[real]
        for literal, target in deps:
            if target not in canon:
                continue  # 系统库，不改
            new_ref = f"@loader_path/{canon[target]}"
            if literal == new_ref:
                continue  # 已经是目标形态（如 @loader_path/libicudata.78.dylib）
            install_name_tool(["-change", literal, new_ref], holder)
            changed += 1
    print(f"  引用改写: {changed} 条 → @loader_path/...")

    print()
    print("=== 4/4 完成（签名由 bundle-app.sh 的下一步做）===")
    print(f"  Frameworks 现有 {len(list(frameworks.glob('*.dylib')))} 个 dylib")


def check(app: Path) -> int:
    """自检：闭包里没有绝对路径残留，且所有 @ 引用都能解析到文件。"""
    frameworks = app / "Contents" / "Frameworks"
    shim = frameworks / SHIM_NAME
    main_exe = app / "Contents" / "MacOS" / "lodestar-app"

    print("=== 自检：依赖闭包 ===")
    if not shim.exists():
        print(f"✗ 找不到 {shim}", file=sys.stderr)
        return 1

    queue: deque[Path] = deque([shim])
    seen: set[Path] = set()
    abs_paths: list[str] = []
    unresolved: list[str] = []
    n_files = 0

    while queue:
        current = queue.popleft()
        real = current.resolve()
        if real in seen:
            continue
        seen.add(real)
        n_files += 1

        for literal in dep_literals(current):
            if literal.startswith("/"):
                if is_system(literal):
                    continue
                abs_paths.append(f"{current.name} → {literal}")
                # 【继续往下走】虽然这条已经违规了，但把它排进队列，
                #   才能把整条链上的问题一次报全（而不是只报第一层 8 条）。
                target = Path(literal)
            else:
                candidate = resolve(literal, current, main_exe)
                if candidate is None or not os.path.exists(candidate):
                    unresolved.append(f"{current.name} → {literal}")
                    continue
                if is_system(candidate):
                    continue
                if not candidate.startswith(str(frameworks)):
                    abs_paths.append(f"{current.name} → {literal}（解析到 {candidate}）")
                target = Path(candidate)

            if target.exists():
                queue.append(target)

    rc = 0
    if abs_paths:
        print("✗ 闭包里还有绝对路径依赖（.app 挪到别的机器会起不来）：", file=sys.stderr)
        for line in abs_paths:
            print(f"    {line}", file=sys.stderr)
        rc = 1
    else:
        print("✓ 闭包里没有绝对路径依赖")

    if unresolved:
        print("✗ 有 @ 引用解析不到文件：", file=sys.stderr)
        for line in unresolved:
            print(f"    {line}", file=sys.stderr)
        rc = 1
    else:
        print("✓ 所有 @ 引用都能解析到 Frameworks 里的文件")

    n_libs = len(list(frameworks.glob("*.dylib")))
    print(f"  闭包内 Mach-O: {n_files} 个；Frameworks 里: {n_libs} 个 dylib")
    return rc


def find_app(arg: str | None) -> Path:
    if arg:
        candidate = Path(arg)
        if candidate.suffix != ".app":
            candidate = candidate / "Lodestar.app"
        if not candidate.exists():
            raise SystemExit(f"✗ 找不到 .app: {candidate}")
        return candidate.resolve()

    repo = Path(__file__).resolve().parents[2]
    default = repo / "app" / "src-tauri" / "target" / "release" / "bundle" / "macos" / "Lodestar.app"
    if default.exists():
        return default.resolve()
    raise SystemExit(f"✗ 找不到默认 .app: {default}\n    用法: {sys.argv[0]} <路径/Lodestar.app>")


def main() -> int:
    parser = argparse.ArgumentParser(description="把 Homebrew 依赖嵌进 .app")
    parser.add_argument("app", nargs="?", help="Lodestar.app 路径（默认用 target/release/bundle/macos/）")
    parser.add_argument("--check-only", action="store_true", help="只跑自检，不改任何东西")
    args = parser.parse_args()

    app = find_app(args.app)
    print(f".app: {app}")
    print()

    if args.check_only:
        return check(app)

    embed(app)
    print()
    return check(app)


if __name__ == "__main__":
    sys.exit(main())
