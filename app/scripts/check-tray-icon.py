#!/usr/bin/env python3
"""托盘图标体检 —— 证明「菜单栏图标没被画歪」。

【为什么要有这个脚本】
    托盘图标**已经因为缩放方向写反而坏过一次**：
    图形只占画布 9×8 像素、菜单栏里只剩 ~3.7pt 的小点，几乎看不见。
    当时这个检查只存在于 `/tmp`，已被系统清掉 ——
    于是「下次再画歪了」只能靠人眼在菜单栏里找。
    → 把它收进仓库，**下次画歪了立刻能发现**（像素数 + 包围盒会当场变样）。

【为什么用标准库自己解 PNG】
    zlib + 逐行反滤波就够了 —— 托盘图是 8 位 RGBA、非隔行，
    不值得为它引一个 Pillow 依赖（项目约束：不新增依赖）。

【判定依据】
    期望：44×44 画布（22pt @2x）・非透明像素约 360・包围盒约 31×31 且居中。
    「包围盒居中」是关键的健壮性指标 —— 图形偏到画布一角时
    非透明像素数可能看着还行，但**实际显示位置是错的**。

用法：
    python3 app/scripts/check-tray-icon.py [png路径]
"""

from __future__ import annotations

import struct
import sys
import zlib

# ---- 期望值（来自 app/scripts/gen-tray-icon.swift 生成的图形）----
EXPECT_CANVAS = (44, 44)
EXPECT_PIXELS_MIN = 200
EXPECT_PIXELS_MAX = 600
EXPECT_BOX_MIN = 24
EXPECT_BOX_MAX = 38
# 包围盒中心与画布中心的允许偏差（像素）
EXPECT_CENTER_TOLERANCE = 4.0

PNG_SIG = b"\x89PNG\r\n\x1a\n"


def _paeth(a: int, b: int, c: int) -> int:
    p = a + b - c
    pa, pb, pc = abs(p - a), abs(p - b), abs(p - c)
    if pa <= pb and pa <= pc:
        return a
    if pb <= pc:
        return b
    return c


def _unfilter(raw: bytes, width: int, height: int, channels: int) -> bytes:
    """把 PNG 的滤波数据还原成逐像素字节。"""
    bpp = channels
    stride = width * bpp
    out = bytearray()
    pos = 0
    prev = bytearray(stride)
    for _ in range(height):
        if pos >= len(raw):
            raise ValueError("PNG 数据被截断")
        ftype = raw[pos]
        pos += 1
        line = bytearray(raw[pos:pos + stride])
        if len(line) != stride:
            raise ValueError("PNG 数据被截断")
        pos += stride
        if ftype == 0:  # None
            pass
        elif ftype == 1:  # Sub
            for i in range(bpp, stride):
                line[i] = (line[i] + line[i - bpp]) & 0xFF
        elif ftype == 2:  # Up
            for i in range(stride):
                line[i] = (line[i] + prev[i]) & 0xFF
        elif ftype == 3:  # Average
            for i in range(stride):
                left = line[i - bpp] if i >= bpp else 0
                line[i] = (line[i] + ((left + prev[i]) >> 1)) & 0xFF
        elif ftype == 4:  # Paeth
            for i in range(stride):
                left = line[i - bpp] if i >= bpp else 0
                upleft = prev[i - bpp] if i >= bpp else 0
                line[i] = (line[i] + _paeth(left, prev[i], upleft)) & 0xFF
        else:
            raise ValueError(f"未知的 PNG 滤波类型：{ftype}")
        out += line
        prev = line
    return bytes(out)


def decode_rgba(path: str) -> tuple[int, int, bytes]:
    """把 PNG 解成 (宽, 高, RGBA 字节流)。只支持 8 位、非隔行。"""
    data = open(path, "rb").read()
    if data[:8] != PNG_SIG:
        raise ValueError("不是 PNG 文件")

    pos = 8
    idat = bytearray()
    width = height = depth = color = interlace = 0
    palette = b""
    trns = b""
    while pos + 8 <= len(data):
        length = struct.unpack(">I", data[pos:pos + 4])[0]
        ctype = data[pos + 4:pos + 8]
        payload = data[pos + 8:pos + 8 + length]
        pos += 12 + length
        if ctype == b"IHDR":
            width, height, depth, color, _comp, _filt, interlace = struct.unpack(
                ">IIBBBBB", payload
            )
        elif ctype == b"PLTE":
            palette = payload
        elif ctype == b"tRNS":
            trns = payload
        elif ctype == b"IDAT":
            idat += payload
        elif ctype == b"IEND":
            break

    if depth != 8:
        raise ValueError(f"只支持 8 位深度，这个是 {depth} 位")
    if interlace != 0:
        raise ValueError("不支持隔行 PNG")

    channels = {0: 1, 2: 3, 3: 1, 4: 2, 6: 4}.get(color)
    if channels is None:
        raise ValueError(f"不支持的颜色类型：{color}")

    raw = zlib.decompress(bytes(idat))
    flat = _unfilter(raw, width, height, channels)

    # 统一成 RGBA
    rgba = bytearray(width * height * 4)
    for i in range(width * height):
        src = i * channels
        dst = i * 4
        if color == 6:
            rgba[dst:dst + 4] = flat[src:src + 4]
        elif color == 2:
            rgba[dst:dst + 3] = flat[src:src + 3]
            rgba[dst + 3] = 255
        elif color == 0:
            g = flat[src]
            rgba[dst] = rgba[dst + 1] = rgba[dst + 2] = g
            rgba[dst + 3] = 255
        elif color == 4:
            g = flat[src]
            rgba[dst] = rgba[dst + 1] = rgba[dst + 2] = g
            rgba[dst + 3] = flat[src + 1]
        elif color == 3:
            idx = flat[src]
            rgba[dst:dst + 3] = palette[idx * 3:idx * 3 + 3]
            rgba[dst + 3] = trns[idx] if idx < len(trns) else 255
    return width, height, bytes(rgba)


def analyse(path: str) -> dict:
    width, height, rgba = decode_rgba(path)

    nontransparent = 0
    solid = 0  # alpha >= 8 —— 算「确实看得见」的像素
    min_x, min_y = width, height
    max_x = max_y = -1

    for y in range(height):
        row = y * width * 4
        for x in range(width):
            a = rgba[row + x * 4 + 3]
            if a == 0:
                continue
            nontransparent += 1
            if a >= 8:
                solid += 1
                if x < min_x:
                    min_x = x
                if x > max_x:
                    max_x = x
                if y < min_y:
                    min_y = y
                if y > max_y:
                    max_y = y

    box = None
    if max_x >= 0:
        box = {
            "x0": min_x,
            "y0": min_y,
            "x1": max_x,
            "y1": max_y,
            "w": max_x - min_x + 1,
            "h": max_y - min_y + 1,
            "cx": (min_x + max_x) / 2.0,
            "cy": (min_y + max_y) / 2.0,
        }
    return {
        "width": width,
        "height": height,
        "nontransparent": nontransparent,
        "solid": solid,
        "box": box,
    }


def main() -> int:
    path = sys.argv[1] if len(sys.argv) > 1 else "app/src-tauri/assets/tray.png"
    try:
        r = analyse(path)
    except (OSError, ValueError, zlib.error) as e:
        print(f"✗ 读不了 {path}：{e}", file=sys.stderr)
        return 2

    w, h = r["width"], r["height"]
    box = r["box"]
    print(f"文件            : {path}")
    print(f"画布            : {w}×{h} 像素")
    print(f"非透明像素      : {r['nontransparent']}（alpha ≥ 8 的：{r['solid']}）")
    if box:
        print(
            f"包围盒          : x {box['x0']}..{box['x1']} ・ y {box['y0']}..{box['y1']}"
            f"  →  {box['w']}×{box['h']}"
        )
        print(
            f"包围盒中心      : ({box['cx']:.1f}, {box['cy']:.1f})"
            f"  画布中心 ({w / 2 - 0.5:.1f}, {h / 2 - 0.5:.1f})"
        )
    else:
        print("包围盒          : （图案内没有可见像素！）")

    problems = []
    if (w, h) != EXPECT_CANVAS:
        problems.append(f"画布应为 {EXPECT_CANVAS[0]}×{EXPECT_CANVAS[1]}，实际 {w}×{h}")
    if not (EXPECT_PIXELS_MIN <= r["nontransparent"] <= EXPECT_PIXELS_MAX):
        problems.append(
            f"非透明像素应在 {EXPECT_PIXELS_MIN}..{EXPECT_PIXELS_MAX}，"
            f"实际 {r['nontransparent']}"
        )
    if box is None:
        problems.append("图案内没有可见像素 —— 图标整个没了？")
    else:
        if not (EXPECT_BOX_MIN <= box["w"] <= EXPECT_BOX_MAX):
            problems.append(f"包围盒宽应在 {EXPECT_BOX_MIN}..{EXPECT_BOX_MAX}，实际 {box['w']}")
        if not (EXPECT_BOX_MIN <= box["h"] <= EXPECT_BOX_MAX):
            problems.append(f"包围盒高应在 {EXPECT_BOX_MIN}..{EXPECT_BOX_MAX}，实际 {box['h']}")
        dx = abs(box["cx"] - (w / 2 - 0.5))
        dy = abs(box["cy"] - (h / 2 - 0.5))
        if dx > EXPECT_CENTER_TOLERANCE or dy > EXPECT_CENTER_TOLERANCE:
            problems.append(
                f"包围盒没居中（偏 {dx:.1f}, {dy:.1f} 像素）—— 图画歪了？"
            )

    if problems:
        print("\n✗ 不通过：")
        for p in problems:
            print(f"  - {p}")
        return 1
    print("\n✓ 通过 —— 图标画在画布中央、大小正常（菜单栏里应该是 22pt 的放大镜）")
    return 0


if __name__ == "__main__":
    sys.exit(main())
