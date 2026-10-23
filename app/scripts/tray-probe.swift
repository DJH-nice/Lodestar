// 托盘状态项探针 —— 回答「状态项到底建出来没有、菜单栏有没有给它腾位置」。
//
// 【为什么要有这个脚本】（终审 I-2）
//   「托盘图标不渲染」这件事，之前的诊断链断在一环：
//   复审指出，那个「纯 AppKit 探针也不显示」的对照**不成立** ——
//   探针的状态项也建在 `app.run()` **之前**，跟 FSearch 犯的是同一个错，
//   构不成有效对照。
//   → 要判定，得有两样东西：
//     ① 一个能**创建**状态项、并且能报告它状态的探针（early / late 两种时序）
//     ② 一个能读**窗口服务器**的工具（状态项在不在菜单栏、菜单栏有没有给它腾位置）
//   这个脚本两样都有。
//
// 【用法】
//   swift tray-probe.swift list  [进程名子串]   # 列出状态项 + 菜单栏排布
//   swift tray-probe.swift early [图标路径]    # 状态项建在 app.run() 之前
//   swift tray-probe.swift late  [图标路径]    # 状态项建在 didFinishLaunching 之后
//   swift tray-probe.swift both  [图标路径]    # 两个都建（同一进程内直接对照）
//
// 【读结果的三个地方】
//   ① `button.window.isVisible` —— 状态项的按钮窗口是不是活的
//   ② 「窗口服务器」那几行 —— 它有没有真的进菜单栏（layer 25、位置在 y=0）
//   ③ `list` 模式下的菜单栏排布 —— **别人有没有被挤开**
//      （菜单栏从右往左排；多一个状态项 → 别的项整体左移。
//        挤开了 = 菜单栏**收下了**这个新项。）
import AppKit
import CoreGraphics
import Foundation

let args = CommandLine.arguments
let mode = args.count > 1 ? args[1] : "list"
let pattern = args.count > 2 && !args[2].hasPrefix("--") ? args[2] : "fsearch-app"
let iconPath = args.count > 3 ? args[3] : (args.count > 2 && args[2].hasPrefix("/") ? args[2] : "")

/// 窗口服务器视角：本进程有哪些 layer-25 窗口（= 状态项）。
func layer25Windows(of pid: pid_t? = nil) -> [(owner: String, name: String, x: Int, y: Int, w: Int, h: Int, id: Int)] {
    let wl = (CGWindowListCopyWindowInfo(.optionAll, kCGNullWindowID) as? [[String: Any]]) ?? []
    var out: [(String, String, Int, Int, Int, Int, Int)] = []
    for w in wl {
        guard (w[kCGWindowLayer as String] as? Int) == 25 else { continue }
        if let pid, (w[kCGWindowOwnerPID as String] as? pid_t) != pid { continue }
        let b = w[kCGWindowBounds as String] as? [String: Any] ?? [:]
        out.append((
            "\(w[kCGWindowOwnerName as String] ?? "")",
            "\(w[kCGWindowName as String] ?? "")",
            Int((b["X"] as? Double) ?? 0), Int((b["Y"] as? Double) ?? 0),
            Int((b["Width"] as? Double) ?? 0), Int((b["Height"] as? Double) ?? 0),
            (w[kCGWindowNumber as String] as? Int) ?? 0
        ))
    }
    return out
}

/// 菜单栏上的状态项排布（主屏那一排：y == 0、高 37）。
func menuBarLayout() -> [(owner: String, name: String, x: Int, w: Int)] {
    layer25Windows()
        .filter { $0.y == 0 && $0.h == 37 }
        .map { (owner: $0.owner, name: $0.name, x: $0.x, w: $0.w) }
        .sorted { $0.x < $1.x }
}

func printLayout(_ tag: String) {
    print("=== 菜单栏排布（\(tag)）===")
    for it in menuBarLayout() {
        print(String(format: "  x=%4d w=%2d owner='%@'", it.x, it.w, it.owner))
    }
    print("  合计 \(menuBarLayout().count) 项")
}

/// 当前**在屏幕上**的状态项数量。
/// 【为什么要它】屏幕锁定 / 全屏 app 时，菜单栏不合成 →
///   这个数会变成 0（**对所有 app 都是 0**）→
///   这时「有没有被合成」这个信号是**无效的**，别拿它下结论。
func onScreenStatusItemCount() -> Int {
    let os = (CGWindowListCopyWindowInfo(.optionOnScreenOnly, kCGNullWindowID) as? [[String: Any]]) ?? []
    return os.filter { ($0[kCGWindowLayer as String] as? Int) == 25 }.count
}

if mode == "list" {
    let pids = pidsMatching(pattern)
    print("== 进程 \(pattern) 的 pid: \(pids) ==")
    for pid in pids {
        for w in layer25Windows(of: pid) {
            print("  id=\(w.id) name='\(w.name)' \(w.w)x\(w.h) @(\(w.x),\(w.y))")
        }
    }
    printLayout("现在")
    print("== 在屏幕上的状态项数 = \(onScreenStatusItemCount()) ==")
    print("   （0 = 菜单栏当前没被合成 —— 屏幕锁定或全屏 app 时就是 0，")
    print("     这时「有没有被画出来」这个信号无效）")
    exit(0)
}

// ---- 创建状态项的两种时序 ----

let app = NSApplication.shared
app.setActivationPolicy(.accessory)

var earlyItem: NSStatusItem?
var lateItem: NSStatusItem?

func makeItem() -> NSStatusItem {
    let it = NSStatusBar.system.statusItem(withLength: NSStatusItem.variableLength)
    if !iconPath.isEmpty, let img = NSImage(contentsOfFile: iconPath) {
        img.size = NSSize(width: 18, height: 18)
        img.isTemplate = true
        it.button?.image = img
    }
    return it
}

func report(_ tag: String, _ it: NSStatusItem?) {
    guard let it = it, let win = it.button?.window else {
        print("[\(tag)] 状态项不存在")
        return
    }
    print("[\(tag)] isVisible=\(win.isVisible) isOnActiveSpace=\(win.isOnActiveSpace) frame=\(win.frame)")
    let me = ProcessInfo.processInfo.processIdentifier
    for w in layer25Windows(of: me) {
        print("[\(tag)]   窗口服务器: id=\(w.id) name='\(w.name)' \(w.w)x\(w.h) @(\(w.x),\(w.y))")
    }
}

if mode == "early" || mode == "both" {
    earlyItem = makeItem()
    print("模式 \(mode)：状态项已建在 app.run() 之前")
}

class Delegate: NSObject, NSApplicationDelegate {
    func applicationDidFinishLaunching(_ n: Notification) {
        if mode == "late" || mode == "both" {
            lateItem = makeItem()
            print("模式 \(mode)：状态项已建在 applicationDidFinishLaunching 之后")
        }
        print("--- 报告 ---")
        report("early", earlyItem)
        report("late", lateItem)
        printLayout("创建之后")
        print("== 在屏幕上的状态项数 = \(onScreenStatusItemCount()) ==")
        DispatchQueue.main.asyncAfter(deadline: .now() + 2.0) {
            print("--- 2 秒后再报一次 ---")
            report("early", earlyItem)
            report("late", lateItem)
            exit(0)
        }
    }
}

/// 找 pid（pgrep）。
func pidsMatching(_ name: String) -> [pid_t] {
    let proc = Process()
    proc.executableURL = URL(fileURLWithPath: "/usr/bin/pgrep")
    proc.arguments = ["-f", name]
    let pipe = Pipe()
    proc.standardOutput = pipe
    try? proc.run()
    let data = pipe.fileHandleForReading.readDataToEndOfFile()
    proc.waitUntilExit()
    guard let text = String(data: data, encoding: .utf8) else { return [] }
    return text.split(separator: "\n").compactMap { pid_t($0.trimmingCharacters(in: .whitespacesAndNewlines)) }
}

let delegate = Delegate()
app.delegate = delegate
app.run()
