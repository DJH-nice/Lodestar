#!/usr/bin/env swift
//
// white-to-alpha.swift —— 把「白底 + 圆角方块 + 投影」的生成图，
// 转成 macOS 应用图标该有的「透明底 + 方块 + 投影」。
//
// 用法：
//   swift white-to-alpha.swift <输入.png> <输出.png> [目标边长]
//
// 为什么不用 make-icon.swift 的 squircle 重裁：
//   生成图里的方块**自己就有漂亮的圆角**，还带柔和投影；
//   拿超椭圆去重裁会把角上的渐变描边切出小豁口（实测踩过）。
//   → 这里改成「白底转透明」，方块原样保留，格式和现图标完全一致。
//
// 白底转透明（screen-to-alpha）：
//   白色是「不透明」而不是「有颜色」→ 不能直接把白像素删掉，
//   否则投影会跟着一起消失。
//   正确做法：把白当作「被黑色以 alpha 遮罩叠在下面」，反解：
//       a     = 1 - min(r,g,b)/255
//       color = (obs/255 - (1-a)) / a
//   → 纯白 a=0（完全透明），灰投影 a≈0.1（半透明黑），方块本体 a=1。
//
// 居中与缩放全部在**输出缓冲区的像素坐标**里做 ——
// CoreGraphics 的位图上下翻转规则容易搞错，直接搬像素最不容易出错。
//

import AppKit
import CoreGraphics

let args = CommandLine.arguments
guard args.count >= 3 else {
    FileHandle.standardError.write("用法：swift white-to-alpha.swift <输入.png> <输出.png> [目标边长]\n".data(using: .utf8)!)
    exit(2)
}
let inPath = args[1]
let outPath = args[2]
let target = args.count >= 4 ? Int(args[3])! : 1024

guard let src = NSImage(contentsOfFile: inPath),
      let cgSrc = src.cgImage(forProposedRect: nil, context: nil, hints: nil) else {
    FileHandle.standardError.write("读不了输入图：\(inPath)\n".data(using: .utf8)!)
    exit(1)
}

let W = cgSrc.width, H = cgSrc.height
let space = CGColorSpaceCreateDeviceRGB()

// ── 1. 读入原始 RGB ──────────────────────────────────────────────────────
var rgb = [UInt8](repeating: 255, count: W * H * 4)
rgb.withUnsafeMutableBytes { raw in
    guard let ctx = CGContext(
        data: raw.baseAddress, width: W, height: H,
        bitsPerComponent: 8, bytesPerRow: W * 4, space: space,
        bitmapInfo: CGImageAlphaInfo.noneSkipLast.rawValue
    ) else { return }
    ctx.draw(cgSrc, in: CGRect(x: 0, y: 0, width: W, height: H))
}

// ── 2. 白底转透明（存成预乘 RGBA，CoreGraphics 上下文要的就是这个）──────
// ⚠️ 三个坑（实测踩过）：
//   ① alpha 不能只按亮度算 —— 青/橙描边亮度高（~150~210），
//      按亮度算会被当成半透明，描边整圈发虚。
//      → 归属用**饱和度**判：有彩色（sat>18）或够暗（lum<150）= 方块内容，
//        纯灰亮（阴影/白底）才走 alpha 渐变。
//   ② alpha 改用**亮度**算（不是 min 通道），压缩噪声才不会让 alpha 跳成雪花。
//   ③ 反解颜色时除以极小 a 会放大噪声几十倍 → 阴影区必须用**中性灰**。
var rgba = [UInt8](repeating: 0, count: W * H * 4)
var alphaF = [Double](repeating: 0, count: W * H)
var lumF = [Double](repeating: 0, count: W * H)
var isTile = [Bool](repeating: false, count: W * H)
for i in 0..<(W * H) {
    let p = i * 4
    let r = Double(rgb[p]), g = Double(rgb[p + 1]), b = Double(rgb[p + 2])
    let lum = 0.299 * r + 0.587 * g + 0.114 * b
    let sat = max(r, max(g, b)) - min(r, min(g, b))
    lumF[i] = lum
    let tile = (sat > 18 || lum < 150)
    isTile[i] = tile
    alphaF[i] = tile ? 1.0 : max(0, 1.0 - lum / 255.0)
}
// 3×3 盒式平滑 alpha（只影响阴影区的毛刺；方块边缘反而变成正确的抗锯齿）
var alphaS = alphaF
for y in 1..<(H - 1) {
    for x in 1..<(W - 1) {
        var s = 0.0
        for dy in -1...1 { for dx in -1...1 { s += alphaF[(y + dy) * W + (x + dx)] } }
        alphaS[y * W + x] = s / 9.0
    }
}
for i in 0..<(W * H) {
    let p = i * 4
    let a = alphaS[i]
    if a <= 0.004 { continue }
    if isTile[i] {
        // 方块内容：保留原色，alpha 负责淡出边缘
        rgba[p] = UInt8(Double(rgb[p]) * a)
        rgba[p + 1] = UInt8(Double(rgb[p + 1]) * a)
        rgba[p + 2] = UInt8(Double(rgb[p + 2]) * a)
    } else {
        // 阴影：中性灰，避免除以小 alpha 放大彩色噪声
        let c = min(1.0, max(0.0, (lumF[i] / 255.0 - (1 - a)) / a))
        rgba[p] = UInt8(c * a * 255)
        rgba[p + 1] = UInt8(c * a * 255)
        rgba[p + 2] = UInt8(c * a * 255)
    }
    rgba[p + 3] = UInt8(a * 255)
}

/// 找出「方块本体」（alpha ≈ 1）的像素范围
func tileBounds(_ buf: [UInt8], _ w: Int, _ h: Int) -> (Int, Int, Int, Int)? {
    var minX = w, maxX = -1, minY = h, maxY = -1
    for y in 0..<h {
        for x in 0..<w {
            if buf[(y * w + x) * 4 + 3] > 200 {
                if x < minX { minX = x }
                if x > maxX { maxX = x }
                if y < minY { minY = y }
                if y > maxY { maxY = y }
            }
        }
    }
    return maxX > minX ? (minX, minY, maxX, maxY) : nil
}

guard let b0 = tileBounds(rgba, W, H) else {
    FileHandle.standardError.write("找不到不透明的方块本体\n".data(using: .utf8)!)
    exit(1)
}

// ── 3. 缩放：让方块本体在成品里占 81% 宽（与现图标一致）─────────────────
let scale = (Double(target) * 0.81) / Double(b0.2 - b0.0 + 1)
let outSide = Int((Double(W) * scale).rounded())

func render() -> [UInt8] {
    var buf = [UInt8](repeating: 0, count: outSide * outSide * 4)
    buf.withUnsafeMutableBytes { raw in
        guard let ctx = CGContext(
            data: raw.baseAddress, width: outSide, height: outSide,
            bitsPerComponent: 8, bytesPerRow: outSide * 4, space: space,
            bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue
        ) else { return }
        ctx.clear(CGRect(x: 0, y: 0, width: outSide, height: outSide))
        ctx.interpolationQuality = .high
        // 直接把 rgba 缓冲做成纹理画进目标
        var tmp: CGImage?
        rgba.withUnsafeMutableBytes { raw2 in
            guard let c2 = CGContext(
                data: raw2.baseAddress, width: W, height: H,
                bitsPerComponent: 8, bytesPerRow: W * 4, space: space,
                bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue
            ) else { return }
            tmp = c2.makeImage()
        }
        guard let img = tmp else { return }
        ctx.draw(img, in: CGRect(x: 0, y: 0, width: Double(W) * scale, height: Double(H) * scale))
    }
    return buf
}

var buf = render()
guard let b1 = tileBounds(buf, outSide, outSide) else {
    FileHandle.standardError.write("缩放后找不到方块本体\n".data(using: .utf8)!)
    exit(1)
}

// ── 4. 在**像素坐标**里把方块搬到正中 ───────────────────────────────────
let shiftX = (outSide / 2) - (b1.0 + b1.2) / 2
let shiftY = (outSide / 2) - (b1.1 + b1.3) / 2
if shiftX != 0 || shiftY != 0 {
    var moved = [UInt8](repeating: 0, count: outSide * outSide * 4)
    for y in 0..<outSide {
        for x in 0..<outSide {
            let sx = x - shiftX, sy = y - shiftY
            guard sx >= 0, sx < outSide, sy >= 0, sy < outSide else { continue }
            let s = (sy * outSide + sx) * 4, d = (y * outSide + x) * 4
            moved[d] = buf[s]; moved[d + 1] = buf[s + 1]
            moved[d + 2] = buf[s + 2]; moved[d + 3] = buf[s + 3]
        }
    }
    buf = moved
}

// ── 5. 写出 PNG ─────────────────────────────────────────────────────────
var finalImage: CGImage?
buf.withUnsafeMutableBytes { raw in
    guard let ctx = CGContext(
        data: raw.baseAddress, width: outSide, height: outSide,
        bitsPerComponent: 8, bytesPerRow: outSide * 4, space: space,
        bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue
    ) else { return }
    finalImage = ctx.makeImage()
}
guard let final = finalImage else {
    FileHandle.standardError.write("渲染失败\n".data(using: .utf8)!)
    exit(1)
}
let rep = NSBitmapImageRep(cgImage: final)
guard let data = rep.representation(using: .png, properties: [:]) else {
    FileHandle.standardError.write("编码 PNG 失败\n".data(using: .utf8)!)
    exit(1)
}
try data.write(to: URL(fileURLWithPath: outPath))
print("✓ \(outPath)  \(outSide)×\(outSide)，方块本体 \(b1.2 - b1.0 + 1)px，投影保留")
