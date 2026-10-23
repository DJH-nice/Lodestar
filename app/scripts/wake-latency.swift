// 唤回耗时测量 —— 窗口从「看不见」到「真的在屏幕上」用了多久。
//
// 【为什么用 CGWindowList 而不是截图 / 辅助功能】
//   `screencapture` 要屏幕录制权限；AX 对 overlay 窗口不一定认得。
//   CGWindowList 是**窗口服务器给的真相** ——
//   「这个进程的窗口现在在不在屏幕上」直接问它。
//
// 【⚠️ 只数 layer 0 的窗口 —— 这是本脚本最容易踩的坑】
//   macOS 上**菜单栏的托盘图标本身也是一个窗口**，而且它的 layer 不是 0
//   （NSStatusItem 的图标在 layer 25 一带）。
//   → 如果只按「这个进程有没有 on-screen 窗口」来判，
//     窗口隐藏之后托盘图标**还在屏幕上** → 脚本会一直报 VISIBLE
//     → 「窗口到底藏没藏起来」这件事测不出来（而且会得出「没藏住」的错误结论）。
//   → 所以这里**只认 layer == 0**（普通应用窗口层）。
//
// 用法：
//   swift app/scripts/wake-latency.swift watch   <pid>        # 持续打印可见性变化
//   swift app/scripts/wake-latency.swift launch <exe>       # 冷启动 → 窗口可见的耗时
//   swift app/scripts/wake-latency.swift once   <pid>        # 只打印一次当前状态（退出码 0=可见）

import Foundation
import CoreGraphics

/// 判定「这个进程的**应用窗口**现在在不在屏幕上」。
///
/// 【为什么只看 layer 0】见文件头的说明 —— 托盘图标（layer 25）不算。
/// 【为什么还要 w/h > 100】应用自己的辅助窗口（工具栏、阴影、辅助面板）
///   也是 layer 0，但尺寸很小 —— 用尺寸把它们剔掉。
func windowVisible(pid: Int) -> (visible: Bool, w: Double, h: Double) {
    let opts: CGWindowListOption = [.optionOnScreenOnly, .excludeDesktopElements]
    guard let list = CGWindowListCopyWindowInfo(opts, kCGNullWindowID) as? [[String: Any]] else {
        return (false, 0, 0)
    }
    for win in list {
        let owner = win[kCGWindowOwnerPID as String] as? Int ?? -1
        guard owner == pid else { continue }
        guard (win[kCGWindowIsOnscreen as String] as? Bool ?? false) else { continue }
        // 【关键】只看 layer 0 —— 托盘图标 / 状态栏项在更高的 layer 上。
        let layer = win[kCGWindowLayer as String] as? Int ?? -1
        guard layer == 0 else { continue }
        let b = win[kCGWindowBounds as String] as? [String: Any] ?? [:]
        let w = b["Width"] as? Double ?? 0
        let h = b["Height"] as? Double ?? 0
        // 过滤掉工具栏 / 阴影这类小窗口
        if w > 100 && h > 100 { return (true, w, h) }
    }
    return (false, 0, 0)
}

let args = CommandLine.arguments
guard args.count >= 3 else {
    FileHandle.standardError.write("用法: wake-latency <watch|launch|once> <pid|exe>\n".data(using: .utf8)!)
    exit(2)
}

switch args[1] {
case "watch":
    guard let pid = Int(args[2]) else { exit(2) }
    let start = Date()
    var last: String? = nil
    while true {
        let (vis, w, h) = windowVisible(pid: pid)
        let s = vis ? "VISIBLE" : "HIDDEN"
        if s != last {
            let now = Date()
            // 【为什么打【绝对时间戳】】相对时间只有「探针自己启动」这一个原点，
            //   跟别的工具（shell、探针、别的进程）对不上账 ——
            //   而「唤回耗时」恰恰要跟「触发的那一刻」比。
            //   → 绝对时间戳（epoch 秒）可以和任何一边的日志对账。
            print(String(
                format: "[%.3f] epoch=%.6f  %@   w=%.0f h=%.0f",
                now.timeIntervalSince(start), now.timeIntervalSince1970, s, w, h
            ))
            fflush(stdout)
            last = s
        }
        usleep(2000)
    }

case "once":
    guard let pid = Int(args[2]) else { exit(2) }
    let (vis, w, h) = windowVisible(pid: pid)
    print(String(format: "%@ w=%.0f h=%.0f", vis ? "VISIBLE" : "HIDDEN", w, h))
    exit(vis ? 0 : 1)

case "launch":
    let exe = args[2]
    let proc = Process()
    proc.executableURL = URL(fileURLWithPath: exe)
    proc.standardOutput = FileHandle.nullDevice
    proc.standardError = FileHandle.nullDevice
    let t0 = Date()
    try! proc.run()
    let pid = Int(proc.processIdentifier)
    print("launched pid=\(pid)")
    fflush(stdout)
    for _ in 0..<20000 {
        let (vis, w, h) = windowVisible(pid: pid)
        if vis {
            let ms = Date().timeIntervalSince(t0) * 1000
            print(String(format: "WINDOW_VISIBLE_MS=%.1f  w=%.0f h=%.0f", ms, w, h))
            exit(0)
        }
        usleep(2000)
    }
    print("TIMEOUT: 窗口 40 秒内没出现")
    exit(1)

default:
    exit(2)
}
