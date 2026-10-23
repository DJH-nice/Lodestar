// 生成菜单栏图标 —— 22×22 pt（按 @2x 出 44×44 像素）的**模板图**。
//
// 【为什么要模板图】macOS 的菜单栏图标必须是「黑色 + alpha」的形状图，
//   系统会按菜单栏的深/浅自动反色。用彩色 app 图标会显得很刺眼。
//   （模板图在深色菜单栏上是白的，在浅色菜单栏上是黑的。）
//
// 【⚠️ 改完本脚本必须重新生成 PNG 并把 `app/src-tauri/assets/tray.png` 一起提交】
//   `tray.rs` 的 `include_bytes!` 读的是**仓库里那份 PNG** ——
//   改脚本不重新生成 = 改了个寂寞。
//   （本文件就出过这种事故：换算方向写反，脚本跑得好好的，
//     但画出来的图形只有 9×8 像素，被缩到 18pt 后几乎看不见。）
//
// 用法：swift app/scripts/gen-tray-icon.swift app/src-tauri/assets/tray.png

import Foundation
import CoreGraphics
import ImageIO
import UniformTypeIdentifiers

let S = 44          // 像素尺寸（22pt @2x）
// 【点 → 像素的换算，方向别搞反】
//   下面是按 **22×22 的「点」坐标系**画图的，而画布是 **44×44 像素** ——
//   同一个图形在点坐标系里占 22，在像素画布里要占 44，所以换算要**放大 2 倍**：
//       U = 画布像素 / 设计的点数 = 44 / 22 = 2
//   （写成 `22.0 / Double(S)` = 0.5 就反了：22 个设计单位被映射成 11 个像素，
//     图形只占画布的左下角 9×8 像素，缩到 18pt 后只有 ~3.7pt —— 根本看不见。）
let U = Double(S) / 22.0
// 【为什么要 Double(S)】Swift 的 `/` 不像 `+` `-` `*` 那样支持
//   「Double 除以 Int」—— `22.0 / S`（S 是 Int）编译不过。

let outPath = CommandLine.arguments.count > 1
    ? CommandLine.arguments[1]
    : "tray.png"

let cs = CGColorSpaceCreateDeviceRGB()
guard let ctx = CGContext(
    data: nil, width: S, height: S, bitsPerComponent: 8, bytesPerRow: 0,
    space: cs,
    bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue
) else {
    FileHandle.standardError.write("建不了绘图上下文\n".data(using: .utf8)!)
    exit(1)
}
ctx.setShouldAntialias(true)
ctx.clear(CGRect(x: 0, y: 0, width: S, height: S))   // 全透明底

// 黑色描边
ctx.setStrokeColor(red: 0, green: 0, blue: 0, alpha: 1)
ctx.setLineWidth(1.8 * U)
ctx.setLineCap(.round)

// 放大镜的镜圈 —— **画布没有做任何翻转变换**（没有 translateBy / scaleBy）。
//
//   【CG 的 y 轴朝上，PNG 的第 0 行是图顶】位图上下文的原点在**左下角**，
//   y 往上增加；写 PNG 时行序反过来（第 0 行 = 图顶）。脚本里**没有**去补这次翻转，
//   于是坐标与最终落点是这么对应的：
//     · 镜圈画在 CG 的 y ≈ 19（画布偏下）→ 落到 **PNG 的下半部**（行号大的一侧）
//     · 手柄画向 CG 的 y ≈ 35（画布偏上）→ 落到 **PNG 的上半部**
//   → **PNG 文件里**是「镜头在左下、手柄指向右上」。
//
//   【⚠️ 菜单栏里的朝向 —— 只是【现象观察】，不是机制结论】
//     （终审 M-3 的勘误：下面这段原来写的是「NSStatusItem 的按钮在翻转的
//       绘图上下文里画图标（AppKit 的老规矩）」—— 那是**机制断言**，
//       但写它的人只做了现象观察、**没查 AppKit 源码**，
//       而 AppKit 到底翻不翻（自定义 cell 的 drawRect: 翻转
//       ≠ button.image 翻转）各家说法不一。
//       → 这里只保留**看到的现象**。）
//
//     现象：在**本机菜单栏里**看到的是上下翻转过的 ——
//     「镜头在左上、手柄指向右下」，也就是放大镜最自然的那个朝向。
//     → 因此：**别拿 PNG 直接跟菜单栏截图对比**（会以为画反了），
//       也不要在脚本里再补一次翻转（补了就真反了）。
//
//     ⚠️ **改这个脚本之前**：先在**真机菜单栏**里确认朝向，别据这段注释再补一次翻转。
//        （顺带一提：托盘图标「能不能显示出来」是另一回事 ——
//          见 `.superpowers/sdd/2026-10-22-background-resident-mode/final-fix-report.md`。）
let c = CGPoint(x: 9.5 * U, y: 9.5 * U)
let r = 5.5 * U
ctx.strokeEllipse(in: CGRect(x: c.x - r, y: c.y - r, width: 2 * r, height: 2 * r))
// 手柄
ctx.move(to: CGPoint(x: 13.4 * U, y: 13.4 * U))
ctx.addLine(to: CGPoint(x: 17.6 * U, y: 17.6 * U))
ctx.strokePath()

guard let img = ctx.makeImage() else {
    FileHandle.standardError.write("渲染失败\n".data(using: .utf8)!)
    exit(1)
}
let outURL = URL(fileURLWithPath: outPath)
guard let dest = CGImageDestinationCreateWithURL(
    outURL as CFURL, UTType.png.identifier as CFString, 1, nil
) else {
    FileHandle.standardError.write("建不了 PNG 写入器\n".data(using: .utf8)!)
    exit(1)
}
CGImageDestinationAddImage(dest, img, nil)
if !CGImageDestinationFinalize(dest) {
    FileHandle.standardError.write("写 PNG 失败\n".data(using: .utf8)!)
    exit(1)
}
print("已生成 \(outPath)（\(S)×\(S)，模板图）")
