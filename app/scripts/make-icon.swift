#!/usr/bin/env swift
//
// make-icon.swift —— 把「圆角矩形 + 白边」的生成图，处理成 macOS 标准的 **squircle** 图标。
//
// 用法：
//   swift app/scripts/make-icon.swift <输入.png> <输出.png>
//
// 为什么需要这一步（生成模型给的图不能直接用）：
//   ① 生成的图是「白色画布 + 中间一个圆角矩形 + 一圈投影」，
//      而 macOS 图标必须是 **squircle 铺满整个 1024 画布、四角透明**；
//      直接塞进 .icns 会看到一圈白边 + 一块比系统图标小的图案。
//   ② 图标生成模型普遍画的是**圆角矩形**（circular arc），
//      而 macOS 用的是**超椭圆**（squircle）—— 角上更「方」一点。
//      → 这里统一用 n=5 的超椭圆重新裁形。
//
// 三步：
//   ① 洪水填充去白边：从**画布边界**往里做连通区域填充，
//      近白（且低饱和）的像素置为全透明。
//      ⚠️ 用「连通性」而不是「全局阈值」——
//         星点的高光中心也是近白的，但它跟边界不连通，不会被误删。
//   ② 裁到图形本体（bbox），缩放铺满 1024 画布。
//   ③ 用超椭圆（n=5）裁出 macOS 轮廓；超椭圆比原图的圆角矩形更「方」，
//      两者之间会露出一条窄缝 —— 用图形自身的底色填满，肉眼无缝。
//
// 没有第三方依赖：只用 AppKit / CoreGraphics。
//

import AppKit
import CoreGraphics

let args = CommandLine.arguments
guard args.count == 3 else {
    FileHandle.standardError.write("用法：swift make-icon.swift <输入.png> <输出.png>\n".data(using: .utf8)!)
    exit(2)
}
let inPath = args[1]
let outPath = args[2]

let SIDE = 1024
let BYTES_PER_ROW = SIDE * 4

// ── 1. 读入并画进 RGBA8 缓冲 ──────────────────────────────────────────────
guard let src = NSImage(contentsOfFile: inPath),
      let cgSrc = src.cgImage(forProposedRect: nil, context: nil, hints: nil) else {
    FileHandle.standardError.write("读不了输入图：\(inPath)\n".data(using: .utf8)!)
    exit(1)
}

let space = CGColorSpaceCreateDeviceRGB()
var pixels = [UInt8](repeating: 255, count: BYTES_PER_ROW * SIDE)

pixels.withUnsafeMutableBytes { raw in
    guard let ctx = CGContext(
        data: raw.baseAddress, width: SIDE, height: SIDE,
        bitsPerComponent: 8, bytesPerRow: BYTES_PER_ROW, space: space,
        bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue
    ) else { return }
    ctx.interpolationQuality = .high
    ctx.draw(cgSrc, in: CGRect(x: 0, y: 0, width: SIDE, height: SIDE))
}

// ── 2. 洪水填充：从边界出发，把「近白且低饱和」的连通像素置为透明 ──────
/// ⚠️ **清 alpha 必须同时清 RGB**。
///    缓冲是 `premultipliedLast` —— CoreGraphics 认为 RGB 已经乘过 alpha。
///    只把 alpha 写成 0、RGB 还留着原来的白 (255,255,255)，
///    就造出「透明但发亮」的**非法**像素；
///    后面缩放插值时它会跟旁边的深蓝像素混合 → 图标边缘出现一条 1px 亮线
///    （实测踩过：图标左边一道细白缝）。
func clearPixel(_ idx: Int) {
    let p = idx * 4
    pixels[p] = 0; pixels[p + 1] = 0; pixels[p + 2] = 0; pixels[p + 3] = 0
}

/// 背景判定：三个通道都 >= 150，且最大最小通道差 < 40（低饱和）。
///   —— 阈值取 150 而不是 235 是有原因的：生成的图带着一圈**投影**，
///      投影是浅灰的（220~245），阈值放高就清不掉，
///      结果是「白底 + 深色方块」两层套在一起（实测踩过）。
///      150 能把投影的绝大部分吃掉；而图形本体是深色（lo ≈ 30），不受影响。
func isBackground(_ p: Int) -> Bool {
    let r = Int(pixels[p]), g = Int(pixels[p + 1]), b = Int(pixels[p + 2])
    let lo = min(r, g, b), hi = max(r, g, b)
    return lo >= 150 && (hi - lo) < 40
}

var visited = [Bool](repeating: false, count: SIDE * SIDE)
var stack: [Int] = []

func push(_ x: Int, _ y: Int) {
    guard x >= 0, x < SIDE, y >= 0, y < SIDE else { return }
    let idx = y * SIDE + x
    if visited[idx] { return }
    if !isBackground(idx * 4) { return }
    visited[idx] = true
    stack.append(idx)
}

// 边界上的所有像素作为种子
for x in 0..<SIDE { push(x, 0); push(x, SIDE - 1) }
for y in 0..<SIDE { push(0, y); push(SIDE - 1, y) }

while let idx = stack.popLast() {
    let x = idx % SIDE, y = idx / SIDE
    clearPixel(idx)                 // alpha 归零的同时把 RGB 也抹掉
    push(x + 1, y); push(x - 1, y); push(x, y + 1); push(x, y - 1)
}

// 补一刀：把**残留**的浅色低饱和像素（投影的渐变尾巴、内部白点）也清掉。
//   这些像素跟边界不连通，洪水填充够不着。
//   ⚠️ 这里用**严格**阈值（235）—— 洪水填充用 150 是为了吃投影，
//      但这一步是「无条件」删除，不能连图形内部的浅色细节也吃掉。
for i in 0..<(SIDE * SIDE) {
    if visited[i] { continue }
    let p = i * 4
    let r = Int(pixels[p]), g = Int(pixels[p + 1]), b = Int(pixels[p + 2])
    let lo = min(r, g, b), hi = max(r, g, b)
    if lo >= 235 && (hi - lo) < 30 { clearPixel(i) }
}

// ── 2b. 剥掉「贴着透明区的那圈亮边」────────────────────────────────────
// 生成图常给图标块描一圈 **1px 淡蓝白高光边**（如 (200,215,255)）。
// ⚠️ 它逃过了洪水填充：背景判定要求「低饱和」(hi-lo < 40)，
//    而这条边是**偏蓝的**（hi-lo ≈ 55）→ 被当成图形留下 →
//    图标左边出现一道 1px 白缝（实测踩过）。
// 做法：反复把「够亮 且 四邻域里有透明像素」的像素清掉，直到稳定。
//    从**透明区反向剥**，所以图形内部的亮部（星的高光、浅灰文件夹）够不着 ——
//    除非它自己就贴着透明边，那被削掉 1~2px 也无伤大雅。
for _ in 0..<12 {
    var changed = false
    var peeled = [Bool](repeating: false, count: SIDE * SIDE)
    for y in 0..<SIDE {
        for x in 0..<SIDE {
            let idx = y * SIDE + x
            if pixels[idx * 4 + 3] < 8 { continue }
            let p = idx * 4
            let r = Int(pixels[p]), g = Int(pixels[p + 1]), b = Int(pixels[p + 2])
            guard min(r, g, b) >= 150 else { continue }
            var touchesTransparent = false
            for (dx, dy) in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
                let nx = x + dx, ny = y + dy
                if nx < 0 || nx >= SIDE || ny < 0 || ny >= SIDE {
                    touchesTransparent = true; break
                }
                if pixels[(ny * SIDE + nx) * 4 + 3] < 8 { touchesTransparent = true; break }
            }
            if touchesTransparent { peeled[idx] = true; changed = true }
        }
    }
    if !changed { break }
    for i in 0..<(SIDE * SIDE) where peeled[i] { clearPixel(i) }
}

// ── 3. 求图形本体 bbox ──────────────────────────────────────────────────
var minX = SIDE, maxX = -1, minY = SIDE, maxY = -1
for y in 0..<SIDE {
    for x in 0..<SIDE {
        if pixels[(y * SIDE + x) * 4 + 3] > 8 {
            if x < minX { minX = x }
            if x > maxX { maxX = x }
            if y < minY { minY = y }
            if y > maxY { maxY = y }
        }
    }
}
guard maxX > minX, maxY > minY else {
    FileHandle.standardError.write("图里找不到不透明像素 —— 输入图可能不对\n".data(using: .utf8)!)
    exit(1)
}
let artRect = CGRect(x: minX, y: minY, width: maxX - minX + 1, height: maxY - minY + 1)

// ── 4. 采样图形底色（用于填超椭圆多出来的那条缝）────────────────────────
/// 取 bbox 左内侧几个深度处的像素，取**最暗**的那一组 ——
///   越靠里越可能是图形本体（深藏青），越靠边越可能是残留的投影（浅灰）。
func sampleBackground() -> CGColor {
    var best: (r: CGFloat, g: CGFloat, b: CGFloat)? = nil
    var bestLum: CGFloat = 2
    for depth in [0.005, 0.02, 0.05] {
        var rs: [CGFloat] = [], gs: [CGFloat] = [], bs: [CGFloat] = []
        for fy in [0.3, 0.5, 0.7] {
            let y = minY + Int(artRect.height * fy)
            let x = minX + Int(artRect.width * depth)
            guard x <= maxX, y <= maxY else { continue }
            let p = (y * SIDE + x) * 4
            if pixels[p + 3] < 8 { continue }
            rs.append(CGFloat(pixels[p]) / 255.0)
            gs.append(CGFloat(pixels[p + 1]) / 255.0)
            bs.append(CGFloat(pixels[p + 2]) / 255.0)
        }
        guard !rs.isEmpty else { continue }
        let r = rs.reduce(0, +) / CGFloat(rs.count)
        let g = gs.reduce(0, +) / CGFloat(gs.count)
        let b = bs.reduce(0, +) / CGFloat(bs.count)
        let lum = 0.2126 * r + 0.7152 * g + 0.0722 * b
        if lum < bestLum { bestLum = lum; best = (r, g, b) }
    }
    guard let c = best else { return CGColor(gray: 0.1, alpha: 1) }
    return CGColor(srgbRed: c.r, green: c.g, blue: c.b, alpha: 1)
}

// ── 5. 画最终图：超椭圆裁形 + 底色 + 图形铺满 ──────────────────────────
/// macOS squircle ≈ 超椭圆 |x/a|^n + |y/b|^n = 1，n 取 5。
func squirclePath(n: CGFloat = 5.0) -> CGPath {
    let path = CGMutablePath()
    let a = CGFloat(SIDE) / 2
    let b = CGFloat(SIDE) / 2
    let steps = 720
    let exp = 2.0 / n
    for i in 0...steps {
        let t = CGFloat(i) / CGFloat(steps) * 2 * .pi
        let ct = cos(t), st = sin(t)
        let x = a * (ct < 0 ? -1 : 1) * pow(abs(ct), exp)
        let y = b * (st < 0 ? -1 : 1) * pow(abs(st), exp)
        let p = CGPoint(x: a + x, y: b + y)
        if i == 0 { path.move(to: p) } else { path.addLine(to: p) }
    }
    path.closeSubpath()
    return path
}

// 把清好的像素做成一张 CGImage（用于回贴）
var artPixels = pixels
var artImage: CGImage?
artPixels.withUnsafeMutableBytes { raw in
    if let ctx = CGContext(
        data: raw.baseAddress, width: SIDE, height: SIDE,
        bitsPerComponent: 8, bytesPerRow: BYTES_PER_ROW, space: space,
        bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue
    ) {
        artImage = ctx.makeImage()
    }
}

var out = [UInt8](repeating: 0, count: BYTES_PER_ROW * SIDE)
out.withUnsafeMutableBytes { raw in
    guard let ctx = CGContext(
        data: raw.baseAddress, width: SIDE, height: SIDE,
        bitsPerComponent: 8, bytesPerRow: BYTES_PER_ROW, space: space,
        bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue
    ) else { return }
    ctx.clear(CGRect(x: 0, y: 0, width: SIDE, height: SIDE))
    ctx.saveGState()
    ctx.addPath(squirclePath())
    ctx.clip()
    // 底色：把图形从 bbox 缩放到铺满画布，四角露出的窄缝就是它
    ctx.setFillColor(sampleBackground())
    ctx.fill(CGRect(x: 0, y: 0, width: SIDE, height: SIDE))
    if let art = artImage {
        ctx.interpolationQuality = .high
        // ⚠️ 必须**先按 bbox 裁出图形本体，再铺满整张画布** ——
        //    ① 直接 draw(art, 0,0,1024,1024) 是 1:1，
        //       图形会缩在中间（外面套一圈底色）—— 那不是 macOS 图标该有的样子。
        //    ② 也不能用 translate+scale 去「放大」整张画布：
        //       那样被放大的是**整张 1024 画布**（含白边），
        //       图形本体既没铺满、自己的圆角边框也没落在正确位置（实测踩过）。
        let cropped = art.cropping(to: artRect)!
        // 再往里收 1.5%：把图形自身最外圈那 1px 描边/过渡带切掉，
        // 否则它会紧贴 squircle 边缘留出一道亮线。
        let zoom = 1.015
        let drawSide = CGFloat(SIDE) * zoom
        let offset = (CGFloat(SIDE) - drawSide) / 2
        ctx.draw(cropped, in: CGRect(x: offset, y: offset, width: drawSide, height: drawSide))
    }
    ctx.restoreGState()
}

// ── 6. 写出 PNG ────────────────────────────────────────────────────────
var finalImage: CGImage?
out.withUnsafeMutableBytes { raw in
    if let ctx = CGContext(
        data: raw.baseAddress, width: SIDE, height: SIDE,
        bitsPerComponent: 8, bytesPerRow: BYTES_PER_ROW, space: space,
        bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue
    ) {
        finalImage = ctx.makeImage()
    }
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
print("✓ \(outPath)  （squircle 1024×1024，四角透明；图形 bbox \(Int(artRect.width))×\(Int(artRect.height))）")
