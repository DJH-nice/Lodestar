# FSearch 后台常驻模式 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让 FSearch 在不用的时候以最小占用常驻后台（菜单栏图标 + `⌥⌘Space` 唤起），需要时瞬间唤回。

**Architecture:** 进程 / webview / 索引**全部常驻**（方案 A，实测 87 MB、唤回 70–90 ms），只把 watcher 的「自动重扫」在空闲时暂停。三个新 Rust 模块（`lifecycle` 状态机 / `tray` 托盘 / `shortcut` 快捷键）+ `watcher` 加一道空闲闸 + 前端加快捷键录制 UI。

**Tech Stack:** Tauri v2.12.4（新增 `tray-icon` + `image-png` feature）/ `tauri-plugin-global-shortcut` 2.4.1 / `tauri-plugin-autostart` 2.7.0 / Vue 3 + TypeScript + vitest 5

**设计文档：** [`docs/superpowers/specs/2026-10-22-background-resident-mode-design.md`](../specs/2026-10-22-background-resident-mode-design.md)

---

## Global Constraints

> 每个 Task 的要求都隐含包含本节。

1. **上游 `src/` 一个字节都不许改。** 提交前必跑 `bash shim/check-upstream-pure.sh`。
2. **FFI 契约的唯一真相是 `shim/fsearch_shim.h`。** 本计划**不碰** FFI、不碰 `fsearch-core` 的 `src/`（只加一个 `examples/` 探针）。
3. **删文件优先用可恢复的删除工具** —— `rm -f`、glob 删除会被安全网关拦。
4. **未经明确同意不得 `brew install`。**
5. **Rust 只拷贝 C 字符串，释放只走 `fs_result_free`**，绝不调 libc `free`。
6. **Rust 侧搜索线程必须串行** —— 引擎 view 有状态。
7. **CSS 颜色只能用 `:root` 里已有的令牌**；`z-index` 走 `--z-*` 那一族。
8. **不新增 npm 依赖、不新增 Rust 依赖**（除本计划明确列出的 2 个插件 + 2 个 tauri feature）。
9. **基线测试不能破**（每个 Task 结束前都要全绿）：
   ```bash
   cd app/src-tauri && cargo test -p fsearch-core
   cd app && pnpm vitest run
   cd app && pnpm build
   cd ../.. && bash shim/check-upstream-pure.sh
   ```
10. **空闲阈值 = 20 秒**（`lifecycle::IDLE_GRACE`）。
11. **默认快捷键 = `Option+Cmd+Space`**。
12. **目标数值**：唤回到窗口可见 **< 150 ms**；空闲 CPU **< 0.5%**。
13. **托盘图标左键**：现在是前台且窗口可见 → 收起；否则 → 唤起。
14. **绝不在 `expect()` / `unwrap()` 上让托盘或快捷键的失败弄死应用** —— 失败要降级（见设计文档 §7.1）。

### 已核对的第三方 API 签名（照抄时直接用，不要猜）

| API | 签名 |
|---|---|
| `tauri::tray::TrayIconBuilder` | `.icon_as_template(bool)`、`.show_menu_on_left_click(bool)`（**默认 `true`，左键唤起必须显式关掉**）、`.on_tray_icon_event(f)`、`.on_menu_event(f)` |
| `tauri::tray::TrayIconEvent::Click` | `{ id, position, rect, button: MouseButton, button_state: MouseButtonState }` |
| `tauri::WindowEvent` | `CloseRequested { api }`（`api.prevent_close()`）、`Focused(bool)` |
| `tauri::WebviewWindow` | `.show()`、`.hide()`、`.set_focus()`、`.is_visible() -> Result<bool>`、`.is_focused() -> Result<bool>` |
| `tauri::AppHandle` | `.available_monitors() -> Result<Vec<Monitor>>`、`.primary_monitor() -> Result<Option<Monitor>>`、`.get_webview_window(label)` |
| `tauri::Monitor` | `.position() -> &PhysicalPosition<i32>`、`.size() -> &PhysicalSize<u32>` |
| `tauri_plugin_global_shortcut::GlobalShortcutExt` | `.register(sc)`、`.on_shortcut(sc, f)`、`.unregister(sc)`、`.unregister_all()` |
| `Shortcut`（= `global_hotkey::HotKey`） | `{ pub mods: Modifiers, pub key: Code, pub id: u32 }`；`Modifiers` 是 bitflags（`ALT`/`CONTROL`/`SHIFT`/`SUPER`），有 `.is_empty()` / `.intersects()` |
| 快捷键字符串语法 | 修饰键在前、只能一个主键；`Option`/`Alt`、`Control`/`Ctrl`、`Command`/`Cmd`/`Super`、`Shift`、`CmdOrCtrl`（macOS 上 = Command）；主键名如 `Space` `KeyA` `Digit1` `F1` `Escape` `ArrowUp` |
| `tauri_plugin_autostart` | `init(MacosLauncher::LaunchAgent, Option<Vec<&'static str>>)`；`app.autolaunch()` → `AutoLaunchManager`，有 `.enable()` / `.disable()` / `.is_enabled()` |

---

## 文件结构

**新增：**

| 文件 | 职责 |
|---|---|
| `app/src-tauri/src/lifecycle.rs` | 状态机（Ready/Idle）、空闲判定、唤起/收起序列、窗口位置兜底 |
| `app/src-tauri/src/tray.rs` | 菜单栏图标 + 右键菜单（打开 / 设置 / 退出） |
| `app/src-tauri/src/shortcut.rs` | 全局快捷键注册 / 改键 / 冲突报错 / 规格校验 |
| `app/src-tauri/assets/tray.png` | 22×22@2x 模板图标（黑色 + alpha），由脚本生成 |
| `app/scripts/gen-tray-icon.swift` | 生成上面那个 PNG（CoreGraphics 画放大镜） |
| `app/scripts/wake-latency.swift` | 唤回耗时测量（CGWindowList 轮询） |
| `app/scripts/idle-cpu.sh` | 空闲 CPU 测量（`ps -o time=` 差分） |
| `app/src-tauri/fsearch-core/examples/mem_probe.rs` | 引擎+索引的独立内存探针 |
| `app/src/lib/shortcut.ts` | `KeyboardEvent` → 后端快捷键规格 |
| `app/src/lib/shortcut.test.ts` | 上面那个的单元测试 |

**修改：**

| 文件 | 改什么 |
|---|---|
| `app/src-tauri/Cargo.toml` | 加 `tray-icon` + `image-png` feature；加 2 个插件依赖 |
| `app/src-tauri/src/lib.rs` | 装托盘/快捷键/看门狗；窗口事件改成「拦下来 + 隐藏」 |
| `app/src-tauri/src/watcher.rs` | `RescanDecision` 加 `Paused`；新增 `decide_rescan()`（空闲闸 + 强制补扫闸） |
| `app/src-tauri/src/settings.rs` | `Settings` 加 `global_shortcut` / `launch_at_login` |
| `app/src-tauri/src/commands.rs` | 加 `set_global_shortcut` / `set_launch_at_login` 两个命令 |
| `app/src/lib/types.ts` | `Settings` 类型加两个字段 |
| `app/src/components/SettingsPanel.vue` | 快捷键录制 UI + 开机自启开关 |
| `app/src/App.vue` | 监听 `app-awake` → 聚焦搜索框；**监听 `open-settings` → 打开设置抽屉** |
| `docs/USER-GUIDE.md` | 补「后台常驻模式」一节 |

**Task 顺序（不能乱）：** Task 1 是地基（没有它后面全无从谈起）→ 2/3 是两个唤起入口 → 4 是省 CPU → 5 是设置面 → 6 是验收。

---

### Task 1: lifecycle 状态机 + 关闭按钮隐藏

**这是整个功能的地基。** 没有 `prevent_close` + `hide()`，webview 会被销毁，后面所有「唤起」都无从谈起。

**Files:**
- Create: `app/src-tauri/src/lifecycle.rs`
- Modify: `app/src-tauri/src/lib.rs`（加 `mod lifecycle;`、`.manage(Lifecycle::new())`、改窗口事件）

**Interfaces:**
- Produces:
  - `lifecycle::Phase` — `enum { Ready, Idle }`（`Copy + PartialEq + Eq + Debug`）
  - `lifecycle::Lifecycle` — Tauri 托管状态，`.new()` / `.phase()` / `.mark_ready()` / `.mark_idle()` / `.set_visible(bool)` / `.set_focused(bool)` / `.request_catchup()` / `.take_catchup() -> bool` / `.restore_catchup()` / `.tick()`
  - `lifecycle::should_be_idle(visible: bool, frontmost: bool, since_foreground: Duration) -> bool`
  - `lifecycle::wake<R: Runtime>(app: &AppHandle<R>)` — 唤起
  - `lifecycle::toggle<R: Runtime>(app: &AppHandle<R>)` — 唤起/收起切换
  - `lifecycle::hide<R: Runtime>(app: &AppHandle<R>, win: &WebviewWindow<R>)` — 收起
  - `lifecycle::spawn_idle_watchdog<R: Runtime>(app: AppHandle<R>)` — 空闲看门狗线程
  - `lifecycle::AWAKE_EVENT: &str` = `"app-awake"`
  - `lifecycle::OPEN_SETTINGS_EVENT: &str` = `"open-settings"`
  - `lifecycle::Rect { x: i32, y: i32, w: u32, h: u32 }` + `rects_intersect` / `is_on_any_screen` / `centered_on`（纯函数）
  - `lifecycle::IDLE_GRACE: Duration` = 20 秒

- [ ] **Step 1: 写失败测试 —— 空闲判定与位置兜底（纯函数）**

创建 `app/src-tauri/src/lifecycle.rs`，先只放测试与被测的纯函数：

```rust
//! 应用生命周期 —— **状态机 + 空闲判定 + 唤起序列**。
//!
//! 【这个模块解决什么】
//!   FSearch 改成「后台常驻」之后，需要一个地方统一回答三个问题：
//!   ① 现在是「待用」还是「空闲」？
//!   ② 什么时候该从「待用」切到「空闲」（= 该暂停自动重扫了）？
//!   ③ 用户唤起时，窗口要怎么弄出来、索引要怎么补上？

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use tauri::{AppHandle, Emitter, Manager, Runtime, WebviewWindow};

/// 事件名 —— Rust 发、前端收：「该唤起并聚焦搜索框了」。
///
/// 【为什么要钉死一个字符串】Rust 发事件、前端收事件，两边各写各的就会
/// 「事件发了但没人听」—— 而这**不会报错**，只是唤起后搜索框不聚焦。
pub const AWAKE_EVENT: &str = "app-awake";

/// 事件名 —— 「打开设置面板」（托盘菜单的「设置…」用）。
pub const OPEN_SETTINGS_EVENT: &str = "open-settings";

/// **失焦多久算空闲。**
///
/// 【为什么是 20 秒】几个值的取舍：
///   - 太短（如 5 秒）：你在两个窗口间快速切一下（复制个路径再粘回来）
///     就被判空闲 → 白白触发一次 0.5 秒的补扫。
///   - 太长（如 60 秒）：你切去写代码的这段时间里，它一直在白烧 CPU。
///   20 秒覆盖「切去干别的事」，又不误伤「切一下就回来」。
pub const IDLE_GRACE: Duration = Duration::from_secs(20);

/// 空闲判定的**轮询间隔**。
///
/// 【为什么是 1 秒】跟 watcher 的对账循环同频，成本可忽略（实测 0.07% 那档）。
/// 再密没意义（判定精度到秒就够），再疏会平白多烧 CPU。
pub const IDLE_TICK: Duration = Duration::from_secs(1);

/// 应用状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// 待用 —— watcher 正常自动重扫。
    Ready,
    /// 空闲 —— 你不用它了，watcher 只监听不重扫。
    Idle,
}

/// 窗口/屏幕的矩形（物理像素）—— 纯数据，好测。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

impl Rect {
    pub fn right(&self) -> i32 {
        self.x + self.w as i32
    }
    pub fn bottom(&self) -> i32 {
        self.y + self.h as i32
    }
}

/// 两个矩形有没有交集（纯函数）。
///
/// 【为什么要有这个】窗口位置存在设置里（`window_x/y`，物理像素）。
/// 用户换了显示器或改了分辨率，那个坐标可能落在**所有屏幕之外** ——
/// 表现就是「按了快捷键，窗口弹不出来」。唤起时先查一下，落在外面就拉回来。
pub fn rects_intersect(a: Rect, b: Rect) -> bool {
    a.x < b.right() && a.right() > b.x && a.y < b.bottom() && a.bottom() > b.y
}

/// 窗口是否落在**任意一个**屏幕里（纯函数）。
pub fn is_on_any_screen(window: Rect, screens: &[Rect]) -> bool {
    screens.iter().any(|s| rects_intersect(window, *s))
}

/// 把窗口摆到某块屏幕的**正中**（纯函数）。
///
/// 【为什么摆正中而不是左上角】左上角容易被菜单栏 / Dock 挡住。
pub fn centered_on(screen: Rect, window: Rect) -> (i32, i32) {
    (
        screen.x + (screen.w as i32 - window.w as i32) / 2,
        screen.y + (screen.h as i32 - window.h as i32) / 2,
    )
}

/// **空闲判定**（纯函数 —— 本模块最重要的一段逻辑）。
///
/// | 窗口可见 | 在前台 | 结论 |
/// |---|---|---|
/// | 否 | 任意 | **空闲**（你把窗口收起来了） |
/// | 是 | 是 | 不空闲（正在用） |
/// | 是 | 否 | 距上次在前台 ≥ 20 秒 → 空闲；否则还不空闲 |
pub fn should_be_idle(visible: bool, frontmost: bool, since_foreground: Duration) -> bool {
    if !visible {
        return true;
    }
    if frontmost {
        return false;
    }
    since_foreground >= IDLE_GRACE
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: i32, y: i32, w: u32, h: u32) -> Rect {
        Rect { x, y, w, h }
    }

    #[test]
    fn hidden_window_is_idle_even_when_frontmost() {
        assert!(should_be_idle(false, true, Duration::from_secs(0)));
        assert!(should_be_idle(false, false, Duration::from_secs(0)));
    }

    #[test]
    fn frontmost_visible_window_is_not_idle() {
        assert!(!should_be_idle(true, true, Duration::from_secs(9999)));
    }

    #[test]
    fn blurred_window_is_not_idle_before_the_grace_period() {
        assert!(!should_be_idle(true, false, Duration::from_secs(19)));
    }

    #[test]
    fn blurred_window_is_idle_after_the_grace_period() {
        assert!(should_be_idle(true, false, Duration::from_secs(20)));
        assert!(should_be_idle(true, false, Duration::from_secs(21)));
    }

    #[test]
    fn grace_is_twenty_seconds() {
        assert_eq!(IDLE_GRACE, Duration::from_secs(20));
    }

    #[test]
    fn intersecting_rects_are_detected() {
        assert!(rects_intersect(rect(0, 0, 100, 100), rect(50, 50, 100, 100)));
        // 只挨着边不算有交集（窗口整个在屏幕外）
        assert!(!rects_intersect(rect(0, 0, 100, 100), rect(100, 0, 100, 100)));
    }

    #[test]
    fn window_offscreen_is_detected() {
        let screen = rect(0, 0, 1920, 1080);
        // 在屏幕内
        assert!(is_on_any_screen(rect(10, 10, 800, 600), &[screen]));
        // 整个跑到屏幕右边去了
        assert!(!is_on_any_screen(rect(3000, 100, 800, 600), &[screen]));
    }

    #[test]
    fn window_on_a_second_screen_counts_as_onscreen() {
        let screens = [rect(0, 0, 1920, 1080), rect(1920, 0, 1920, 1080)];
        assert!(is_on_any_screen(rect(2000, 100, 800, 600), &screens));
    }

    #[test]
    fn centering_puts_the_window_in_the_middle_of_the_screen() {
        let screen = rect(0, 0, 1920, 1080);
        let window = rect(9999, 9999, 1040, 700);
        let (x, y) = centered_on(screen, window);
        assert_eq!(x, (1920 - 1040) / 2);
        assert_eq!(y, (1080 - 700) / 2);
    }

    #[test]
    fn lifecycle_starts_ready_and_visible() {
        let life = Lifecycle::new();
        assert_eq!(life.phase(), Phase::Ready);
    }

    #[test]
    fn marking_idle_and_ready_flips_the_phase() {
        let life = Lifecycle::new();
        life.mark_idle();
        assert_eq!(life.phase(), Phase::Idle);
        life.mark_ready();
        assert_eq!(life.phase(), Phase::Ready);
    }

    #[test]
    fn tick_marks_idle_after_the_grace_period() {
        // 【回归测试】tick() 每秒调一次 —— 如果它顺手刷新了
        // 「上次在前台」的时刻，失焦就永远等不满 20 秒 → 永远判不出空闲。
        //
        // 【为什么直接改私有字段】测试在同一个模块里（`mod tests` 在 lifecycle.rs 内部），
        //   能访问私有字段 —— 于是不用 sleep 20 秒来等时间过去，
        //   测试是**瞬时的、确定的**（不会 flaky、不会拖慢整个测试套件）。
        let life = Lifecycle::new();
        life.set_focused(false);
        *life.last_foreground_at.lock().unwrap() = Some(Instant::now() - Duration::from_secs(21));
        life.tick();
        assert_eq!(life.phase(), Phase::Idle, "失焦 21 秒应该判空闲");
    }

    #[test]
    fn tick_keeps_working_when_blurred_for_less_than_the_grace_period() {
        let life = Lifecycle::new();
        life.set_focused(false);
        *life.last_foreground_at.lock().unwrap() = Some(Instant::now() - Duration::from_secs(19));
        life.tick();
        assert_eq!(life.phase(), Phase::Ready, "失焦未满 20 秒不该判空闲");
    }

    #[test]
    fn catchup_request_is_consumed_exactly_once() {
        let life = Lifecycle::new();
        assert!(!life.take_catchup());
        life.request_catchup();
        assert!(life.take_catchup());
        assert!(!life.take_catchup());
    }

    #[test]
    fn restoring_a_catchup_puts_it_back() {
        let life = Lifecycle::new();
        life.request_catchup();
        assert!(life.take_catchup());
        life.restore_catchup();
        assert!(life.take_catchup());
    }
}
```

- [ ] **Step 2: 跑测试确认失败**

Run:
```bash
cd app/src-tauri && cargo test -p fsearch-app lifecycle
```
Expected: **编译失败** —— `cannot find function/method ... Lifecycle`、`unresolved import` 等（`Lifecycle` 还没定义）。

> 如果报的是「找不到 `lifecycle` 这个模块」，说明 `mod lifecycle;` 还没加 —— 那正是 Step 3 要做的。

- [ ] **Step 3: 补上 `Lifecycle` 状态机本体**

在 `lifecycle.rs` 的 `#[cfg(test)] mod tests` **之前**插入：

```rust
/// 应用的生命周期状态 —— **Tauri 托管**（`app.manage(Lifecycle::new())`）。
///
/// 【为什么 `Mutex` + `AtomicBool` 混搭】
///   - `phase` / `last_foreground_at` 要成对读写，用 `Mutex` 保证一致。
///   - `visible` / `focused` / `catchup` 是**单个标志**，用 `AtomicBool` 读起来无锁 ——
///     而看门狗线程每秒都读它们。
pub struct Lifecycle {
    phase: Mutex<Phase>,
    /// 窗口可见吗（点关闭按钮隐藏 → false）。
    visible: AtomicBool,
    /// 窗口有焦点吗（= 应用在前台吗）。
    focused: AtomicBool,
    /// 最近一次「在前台」的时刻。
    last_foreground_at: Mutex<Option<Instant>>,
    /// 有人要求「立刻补扫一次」（唤醒时置位）。
    catchup: AtomicBool,
}

impl Lifecycle {
    pub fn new() -> Self {
        Self {
            phase: Mutex::new(Phase::Ready),
            // 启动时窗口是可见的（tauri.conf.json 里没写 "visible": false）。
            visible: AtomicBool::new(true),
            // 聚焦状态要等第一个 Focused 事件 —— 先按「有焦点」算，
            // 免得刚启动的那一瞬间被判成空闲。
            focused: AtomicBool::new(true),
            last_foreground_at: Mutex::new(Some(Instant::now())),
            catchup: AtomicBool::new(false),
        }
    }

    pub fn phase(&self) -> Phase {
        *self.phase.lock().unwrap()
    }

    /// 只切状态，**不动时间戳** —— 看门狗用这个。
    fn set_phase(&self, p: Phase) {
        let mut cur = self.phase.lock().unwrap();
        if *cur != p {
            *cur = p;
        }
    }

    /// 切到「待用」**并**记下「刚刚在前台」。
    pub fn mark_ready(&self) {
        self.set_phase(Phase::Ready);
        *self.last_foreground_at.lock().unwrap() = Some(Instant::now());
    }

    /// 切到「空闲」。
    pub fn mark_idle(&self) {
        self.set_phase(Phase::Idle);
    }

    pub fn set_visible(&self, v: bool) {
        self.visible.store(v, Ordering::Release);
        if v {
            self.mark_ready();
        }
    }

    /// 窗口焦点变了。`true` = 应用回到前台。
    pub fn set_focused(&self, focused: bool) {
        self.focused.store(focused, Ordering::Release);
        if focused {
            self.mark_ready();
        }
    }

    /// 要求 watcher **立刻补扫一次**（唤醒时调）。
    pub fn request_catchup(&self) {
        self.catchup.store(true, Ordering::Release);
    }

    /// 取走补扫请求（并清零）。返回它原来是不是有请求。
    pub fn take_catchup(&self) -> bool {
        self.catchup.swap(false, Ordering::AcqRel)
    }

    /// 把没消费掉的补扫请求**放回去**（这轮没扫成，下轮再试）。
    pub fn restore_catchup(&self) {
        self.catchup.store(true, Ordering::Release);
    }

    /// 看门狗每秒调一次：重新判定状态。
    pub fn tick(&self) {
        let now = Instant::now();
        let visible = self.visible.load(Ordering::Acquire);
        let frontmost = self.focused.load(Ordering::Acquire);
        let since = self
            .last_foreground_at
            .lock()
            .unwrap()
            .map(|t| now.saturating_duration_since(t))
            .unwrap_or(IDLE_GRACE);
        if should_be_idle(visible, frontmost, since) {
            self.set_phase(Phase::Idle);
        } else {
            self.set_phase(Phase::Ready);
        }
    }
}

impl Default for Lifecycle {
    fn default() -> Self {
        Self::new()
    }
}
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cd app/src-tauri && cargo test -p fsearch-app lifecycle`
Expected: **15 passed**（本文件里的 15 个测试全绿）。

- [ ] **Step 5: 加上唤起 / 收起 / 位置兜底**

在 `lifecycle.rs` 末尾（`mod tests` **之后**）追加：

```rust
/// **唤起** —— 用户按了快捷键 / 点了托盘图标。
///
/// 【顺序很重要】
///   ① 标记待用（watcher 恢复重扫）
///   ② 窗口显示 + 聚焦
///   ③ 通知前端聚焦搜索框
///   ④ **最后**才要求补扫 —— 补扫是 0.4~0.7 秒的 CPU 活，
///      跟 webview 的首次绘制抢 CPU 会把窗口拖慢。
///      **用户等的是窗口，不是索引。**
pub fn wake<R: Runtime>(app: &AppHandle<R>) {
    let Some(win) = app.get_webview_window("main") else {
        return;
    };
    {
        let life = app.state::<Lifecycle>();
        life.mark_ready();
        life.set_visible(true);
    }
    ensure_on_screen(app, &win);
    let _ = win.show();
    let _ = win.set_focus();
    if let Err(e) = app.emit(AWAKE_EVENT, ()) {
        eprintln!("[fsearch] 通知前端「已唤起」失败: {e}");
    }
    // 补扫：唤醒后索引要是新的（把空闲期间攒下的变化收进来）。
    app.state::<Lifecycle>().request_catchup();
}

/// **收起** —— 窗口隐藏到后台，进程继续活着。
pub fn hide<R: Runtime>(app: &AppHandle<R>, win: &WebviewWindow<R>) {
    let _ = win.hide();
    let life = app.state::<Lifecycle>();
    life.set_visible(false);
    life.mark_idle();
}

/// **切换** —— 托盘左键点一下。
///
/// - 现在是前台且窗口可见 → **收起**（等同点关闭按钮）
/// - 其他情况 → **唤起**
pub fn toggle<R: Runtime>(app: &AppHandle<R>) {
    let Some(win) = app.get_webview_window("main") else {
        return;
    };
    let visible = win.is_visible().unwrap_or(false);
    let focused = win.is_focused().unwrap_or(false);
    if visible && focused {
        hide(app, &win);
    } else {
        wake(app);
    }
}

/// 打开设置面板（托盘菜单的「设置…」用）。
pub fn open_settings<R: Runtime>(app: &AppHandle<R>) {
    wake(app);
    if let Err(e) = app.emit(OPEN_SETTINGS_EVENT, ()) {
        eprintln!("[fsearch] 通知前端打开设置失败: {e}");
    }
}

/// 窗口位置兜底 —— 落在所有屏幕之外就拉回主屏幕正中。
fn ensure_on_screen<R: Runtime>(app: &AppHandle<R>, win: &WebviewWindow<R>) {
    let (Ok(pos), Ok(size)) = (win.outer_position(), win.outer_size()) else {
        return;
    };
    let Ok(monitors) = app.available_monitors() else {
        return;
    };
    if monitors.is_empty() {
        return;
    }
    let window_rect = Rect {
        x: pos.x,
        y: pos.y,
        w: size.width,
        h: size.height,
    };
    let screens: Vec<Rect> = monitors
        .iter()
        .map(|m| {
            let p = m.position();
            let s = m.size();
            Rect {
                x: p.x,
                y: p.y,
                w: s.width,
                h: s.height,
            }
        })
        .collect();
    if is_on_any_screen(window_rect, &screens) {
        return;
    }
    let target = match app.primary_monitor().ok().flatten() {
        Some(m) => {
            let p = m.position();
            let s = m.size();
            Rect {
                x: p.x,
                y: p.y,
                w: s.width,
                h: s.height,
            }
        }
        None => screens[0],
    };
    let (x, y) = centered_on(target, window_rect);
    if let Err(e) = win.set_position(tauri::PhysicalPosition::new(x, y)) {
        eprintln!("[fsearch] 把窗口拉回屏幕失败: {e}");
    } else {
        crate::logging::log_line(&format!(
            "[lifecycle] 窗口原位置 ({}, {}) 不在任何屏幕上 → 拉回 ({}, {})",
            window_rect.x, window_rect.y, x, y
        ));
    }
}

/// 空闲看门狗线程 —— 每秒重新判定一次「你是不是不用我了」。
///
/// 【为什么在 Rust 侧而不是前端】
///   webview 的渲染进程在应用不前台时会被 macOS **限流**
///   （定时器与 Tauri IPC 回调一起停摆，看起来像「应用卡死」）。
///   → 前端定时器不可信，判定必须在这儿做。
pub fn spawn_idle_watchdog<R: Runtime>(app: AppHandle<R>) {
    std::thread::spawn(move || loop {
        std::thread::sleep(IDLE_TICK);
        // 【先取 state 再用】—— `app.state::<T>()` 返回的 State 是个 guard，
        // 直接 `app.state::<Lifecycle>().tick()` 会在临时值上借用 → E0716。
        let life = app.state::<Lifecycle>();
        life.tick();
    });
}
```

- [ ] **Step 6: 跑测试确认仍然通过**

Run: `cd app/src-tauri && cargo test -p fsearch-app lifecycle`
Expected: **15 passed**。

- [ ] **Step 7: 把 lifecycle 接进 `lib.rs`**

在 `app/src-tauri/src/lib.rs` 改这几处：

1) 顶部加模块声明与 import：

```rust
mod commands;
mod index_state;
mod lifecycle;
mod logging;
mod settings;
mod watcher;

use std::sync::{Arc, Mutex};

use fsearch_core::Searcher;
use index_state::IndexState;
use lifecycle::Lifecycle;
use tauri::menu::{Menu, MenuItemBuilder, MenuItemKind, HELP_SUBMENU_ID};
use tauri::{AppHandle, Emitter, Manager, Runtime};
```

2) 把 `install_menu` 后面那个「恢复窗口位置」块里的 `win.on_window_event(...)` 整个换成下面这段：

```rust
            if let Some(win) = handle.get_webview_window("main") {
                let settings = settings::load(&handle);
                if let (Some(w), Some(h)) = (settings.window_width, settings.window_height) {
                    let _ = win.set_size(tauri::PhysicalSize::new(w, h));
                }
                if let (Some(x), Some(y)) = (settings.window_x, settings.window_y) {
                    let _ = win.set_position(tauri::PhysicalPosition::new(x, y));
                }
                // 窗口事件：**关窗不退出，改为隐藏到后台**。
                //
                // 【`on_window_event` 消费 self】—— 签名是 `fn on_window_event(self, f)`，
                // 所以 `win` 会被 move 进去。而闭包里又要读窗口的位置/大小，
                // 于是闭包又借用了 `win` → 报
                // error[E0505]: cannot move out of `win` because it is borrowed。
                // 解法：先 clone 一份给闭包用（WebviewWindow 是 Clone）。
                let win_for_event = win.clone();
                let save_handle = handle.clone();
                let event_handle = handle.clone();
                win.on_window_event(move |event| match event {
                    tauri::WindowEvent::CloseRequested { api, .. } => {
                        // ① 先把位置与大小存回去
                        save_window_geometry(&win_for_event, &save_handle);
                        // ② 【关键】拦下来 ——
                        //    不拦的话 webview 会被**销毁**，
                        //    后面就再也唤不回来了（唤起会是个白屏）。
                        api.prevent_close();
                        // ③ 隐藏到后台，进程继续活着
                        lifecycle::hide(&event_handle, &win_for_event);
                    }
                    tauri::WindowEvent::Focused(focused) => {
                        // 焦点变了 → 更新「在前台」的记录（看门狗据此判空闲）
                        let life = event_handle.state::<Lifecycle>();
                        life.set_focused(focused);
                    }
                    _ => {}
                });
            }
```

3) 在 `lib.rs` 里加这个辅助函数（放在 `install_menu` 下面）：

```rust
/// 关窗时把窗口位置与大小存回设置。
///
/// 【为什么抽成函数】原来的逻辑内联在 `on_window_event` 的闭包里，
///   现在那个闭包要同时处理三件事（存位置 / 拦关闭 / 记焦点），
///   塞不下 —— 抽出来更好读。
fn save_window_geometry<R: Runtime>(win: &tauri::WebviewWindow<R>, handle: &AppHandle<R>) {
    let (pos, size) = match (win.outer_position(), win.outer_size()) {
        (Ok(p), Ok(sz)) => (p, sz),
        _ => return,
    };
    let mut s = settings::load(handle);
    s.window_x = Some(pos.x);
    s.window_y = Some(pos.y);
    s.window_width = Some(size.width);
    s.window_height = Some(size.height);
    if let Err(e) = settings::save(handle, &s) {
        eprintln!("[fsearch] 保存窗口位置失败: {e}");
    }
}
```

4) 在 `run()` 的 Builder 上加 `.manage(Lifecycle::new())`（紧挨着 `.manage(IndexState::new())`），并在 `setup` 里启动看门狗（紧挨着 `watcher::spawn_watcher(app.handle());`）：

```rust
        .manage(Mutex::new(Option::<Arc<Searcher>>::None))
        .manage(IndexState::new())
        .manage(Lifecycle::new())
```

```rust
            watcher::spawn_watcher(app.handle());
            // 【空闲看门狗】—— 每秒重新判定「你是不是不用我了」。
            // 必须在 Lifecycle 被 manage 之后启动（它要读那个状态）。
            lifecycle::spawn_idle_watchdog(app.handle().clone());
```

- [ ] **Step 8: 编译 + 跑全部基线测试**

Run:
```bash
cd app/src-tauri && cargo build 2>&1 | tail -20
cd app/src-tauri && cargo test -p fsearch-core && cargo test -p fsearch-app
cd app && pnpm vitest run && pnpm build
```
Expected: 全部通过、无 error。`cargo build` 不应有 warning（若 `lifecycle.rs` 有未用 import 要删掉）。

- [ ] **Step 9: 提交**

```bash
git add app/src-tauri/src/lifecycle.rs app/src-tauri/src/lib.rs
git commit -m "feat(app): lifecycle 状态机 —— 关窗改为隐藏到后台，webview 不再被销毁"
```

---

### Task 2: 菜单栏图标

**Files:**
- Create: `app/scripts/gen-tray-icon.swift`
- Create: `app/src-tauri/assets/tray.png`（由脚本生成，**要提交**）
- Create: `app/src-tauri/src/tray.rs`
- Modify: `app/src-tauri/Cargo.toml`（加 `tray-icon` + `image-png` feature）
- Modify: `app/src-tauri/src/lib.rs`（`pub mod tray;` + setup 里装托盘）

**Interfaces:**
- Consumes: `lifecycle::toggle(app)`、`lifecycle::open_settings(app)`（Task 1 产出）
- Produces:
  - `tray::build_tray<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<()>`
  - `tray::build_menu<R: Runtime>(app: &AppHandle<R>, warning: Option<&str>) -> tauri::Result<Menu<R>>`
  - `tray::set_warning<R: Runtime>(app: &AppHandle<R>, message: Option<&str>)` —— 改/撤掉菜单顶部的警告项
  - `tray::TRAY_OPEN_ID` / `TRAY_SETTINGS_ID` / `TRAY_QUIT_ID` / `TRAY_WARNING_ID` / `TRAY_ID`（`&str`）

- [ ] **Step 1: 写图标生成脚本**

创建 `app/scripts/gen-tray-icon.swift`：

```swift
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
//   （写成 `22.0 / S` = 0.5 就反了：22 个设计单位被映射成 11 个像素，
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
//   【⚠️ 但菜单栏里看到的是上下翻转过的】
//     NSStatusItem 的按钮在**翻转的绘图上下文**里画图标（AppKit 的老规矩），
//     所以用户实际看到的是「**镜头在左上、手柄指向右下**」——
//     也就是放大镜最自然的那个朝向。
//     → **别拿 PNG 直接跟菜单栏截图对比**（会以为画反了），
//     也不要在脚本里再补一次翻转（补了就真反了）。
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
```

- [ ] **Step 2: 生成图标并验证**

Run:
```bash
mkdir -p app/src-tauri/assets
swift app/scripts/gen-tray-icon.swift app/src-tauri/assets/tray.png
file app/src-tauri/assets/tray.png
sips -g pixelWidth -g pixelHeight -g hasAlpha app/src-tauri/assets/tray.png
```
Expected:
```
app/src-tauri/assets/tray.png: PNG image data, 44 x 44, 8-bit/color RGBA, non-interlaced
  pixelWidth: 44
  pixelHeight: 44
  hasAlpha: yes
```

> **【别只看「渲染出来了」—— 要做像素级验证】**
> 托盘图标会被 `tray-icon` 统一缩到 **18pt** 高，所以图形在 44×44 画布里
> 必须**占满大部分**（包围盒约 31×31 像素）。
> 曾经出过的事故：点→像素的换算方向写反（`U = 22.0 / S` = 0.5），
> 图形只占左下角 **9×8** 像素 → 缩到 18pt 后只有 ~3.7pt，**菜单栏里几乎看不见**，
> 而脚本「跑得好好的」、PNG「格式也对」—— 不做像素级验证就发现不了。
>
> 验证方法：解 PNG（IHDR + IDAT inflate + 逐行反滤波）统计**非透明像素数**与**包围盒**：
> ```
> 非透明像素（alpha > 8）: 363 of 1936
> 包围盒: x 6..36  y 7..37   （31 × 31，y=0 是 PNG 第一行 = 图顶）
> ```
> 顺带在真机上看一眼菜单栏（图标大小/位置是否正常）。

- [ ] **Step 3: 加 tauri feature 与托盘模块**

`app/src-tauri/Cargo.toml`，把 tauri 那行改成：

```toml
tauri = { version = "2", features = ["macos-private-api", "tray-icon", "image-png"] }
```

> `image-png` 是 `tauri::image::Image::from_bytes` 读 PNG 的前提；
> `tray-icon` 是 `tauri::tray` 模块的前提。**两个都要加。**

新建 `app/src-tauri/src/tray.rs`：

```rust
//! 菜单栏图标 —— 让用户不看 Dock 也能唤起 FSearch。
//!
//! 【左键 vs 右键】
//!   `show_menu_on_left_click` 的**默认值是 `true`** ——
//!   那样左键会直接弹菜单，唤起就没法做了。必须**显式关掉**：
//!   左键 → 唤起/收起，右键 → 弹菜单。
//!
//! 【为什么要能加「警告项」】
//!   快捷键注册失败（被别的软件占着）时，用户得**看得见** ——
//!   设置面板可能正关着，而托盘菜单是**常驻**的。
//!   → 菜单顶部插一条**禁用的**警告项（见 [`set_warning`]）。

use tauri::menu::{IsMenuItem, Menu, MenuItem, MenuItemBuilder};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
// 【为什么不要 Manager】`app.tray_by_id(..)` 是 `AppHandle` 的**固有方法**
//   （tauri 2.12 `app.rs` 的 `shared_app_impl!` 宏里），不是 trait 方法 ——
//   把 `Manager` 引进来的话编译器会报 unused import。
use tauri::{AppHandle, Runtime};

/// 菜单项 id —— Rust 侧与前端约定的是同一个字符串。
pub const TRAY_OPEN_ID: &str = "tray-open";
pub const TRAY_SETTINGS_ID: &str = "tray-settings";
pub const TRAY_QUIT_ID: &str = "tray-quit";
/// 警告项的 id（禁用状态，只是给人看的）。
pub const TRAY_WARNING_ID: &str = "tray-warning";
/// 托盘图标的 id —— 后面要靠它找回这个图标来改菜单。
pub const TRAY_ID: &str = "main";

/// 托盘图标资源（22×22@2x 的模板图）。
///
/// 【为什么用 include_bytes! 而不是运行时读文件】
///   打包成 .app 后资源在 bundle 里，运行时读路径不好算；
///   编进二进制最省事。
const TRAY_ICON: &[u8] = include_bytes!("../assets/tray.png");

/// 建托盘菜单。
///
/// `warning = Some(msg)` 时在**顶部**插一条禁用的警告项。
pub fn build_menu<R: Runtime>(
    app: &AppHandle<R>,
    warning: Option<&str>,
) -> tauri::Result<Menu<R>> {
    let mut items: Vec<MenuItem<R>> = Vec::new();
    if let Some(msg) = warning {
        let item = MenuItemBuilder::with_id(TRAY_WARNING_ID, msg).build(app)?;
        // 警告项是给人看的，不能点
        item.set_enabled(false)?;
        items.push(item);
    }
    items.push(MenuItemBuilder::with_id(TRAY_OPEN_ID, "打开 FSearch").build(app)?);
    items.push(MenuItemBuilder::with_id(TRAY_SETTINGS_ID, "设置…").build(app)?);
    items.push(MenuItemBuilder::with_id(TRAY_QUIT_ID, "退出").build(app)?);
    let refs: Vec<&dyn IsMenuItem<R>> =
        items.iter().map(|i| i as &dyn IsMenuItem<R>).collect();
    Menu::with_items(app, &refs)
}

/// 装菜单栏图标。
///
/// 【装不上不许 panic】—— 装不上只是少一个入口，
///   应用其余部分（窗口、快捷键）照常用。
pub fn build_tray<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<()> {
    let menu = build_menu(app, None)?;
    TrayIconBuilder::with_id(TRAY_ID)
        .icon(tauri::image::Image::from_bytes(TRAY_ICON)?)
        // 【模板图】macOS 按菜单栏深/浅自动反色
        .icon_as_template(true)
        // 【默认是 true】必须显式关掉，否则左键只会弹菜单
        .show_menu_on_left_click(false)
        .menu(&menu)
        .on_menu_event(|app, event| {
            match event.id().as_ref() {
                // 【菜单项要「打开」，不是「切换」】文案写的是「打开 FSearch」——
                //   窗口已经可见且聚焦时，用户点它反而把窗口**收起**了（点了反而没了）。
                //   → 菜单项走 `wake`（只唤起，不收起）；`toggle` 只留给**左键图标**。
                TRAY_OPEN_ID => crate::lifecycle::wake(app),
                TRAY_SETTINGS_ID => crate::lifecycle::open_settings(app),
                // 【真的退出】不是隐藏 —— 用户点「退出」就是要关掉它
                TRAY_QUIT_ID => app.exit(0),
                _ => {}
            }
        })
        .on_tray_icon_event(|tray, event| {
            // 【要匹配 button_state == Up】—— 不然按下和抬起会触发两次
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                crate::lifecycle::toggle(tray.app_handle());
            }
        })
        .build(app)?;
    Ok(())
}

/// 改托盘菜单顶部的警告项 —— `None` = 撤掉警告。
///
/// 【托盘图标可能还没建好】`build_tray` 失败时（或者调用顺序反了），
///   `tray_by_id` 会返回 `None` —— 这时**什么也不做**，
///   不能让「加个警告」把应用弄崩。
pub fn set_warning<R: Runtime>(app: &AppHandle<R>, message: Option<&str>) {
    let Some(tray) = app.tray_by_id(TRAY_ID) else {
        return;
    };
    let menu = match build_menu(app, message) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("[fsearch] 重建托盘菜单失败: {e}");
            return;
        }
    };
    if let Err(e) = tray.set_menu(Some(menu)) {
        eprintln!("[fsearch] 更新托盘菜单失败: {e}");
    }
}
```

- [ ] **Step 4: 在 `lib.rs` 里装托盘**

1) 加 `mod tray;`（在 `mod settings;` 后面）。
2) 在 `setup` 里，**装托盘的位置要在 `install_menu` 之后**：

```rust
            // 【装菜单栏图标】—— 装不上不许 panic：
            // 少一个入口而已，窗口和快捷键照常用。
            if let Err(e) = tray::build_tray(app.handle()) {
                eprintln!("[fsearch] 装菜单栏图标失败: {e}");
            }
```

- [ ] **Step 5: 编译并跑基线**

Run:
```bash
cd app/src-tauri && cargo build 2>&1 | tail -20 && cargo test -p fsearch-core && cargo test -p fsearch-app
cd app && pnpm vitest run && pnpm build
```
Expected: 全绿。**特别确认 `Cargo.lock` 里 tauri 的版本**：加 feature 可能把 `tauri` 从 2.12.4 顶到 2.12.5。若顶了，跑一遍 `cargo test -p fsearch-core` 确认没坏（它本来就跑）。

- [ ] **Step 6: 提交**

```bash
git add app/scripts/gen-tray-icon.swift app/src-tauri/assets/tray.png \
        app/src-tauri/src/tray.rs app/src-tauri/src/lib.rs app/src-tauri/Cargo.toml app/src-tauri/Cargo.lock
git commit -m "feat(app): 菜单栏图标 —— 左键唤起/收起，右键菜单（打开·设置·退出）"
```

---

### Task 3: 全局快捷键

**Files:**
- Create: `app/src-tauri/src/shortcut.rs`
- Modify: `app/src-tauri/Cargo.toml`（加 `tauri-plugin-global-shortcut`）
- Modify: `app/src-tauri/src/lib.rs`（`mod shortcut;` + 装插件 + 启动时注册）
- Modify: `app/src-tauri/src/settings.rs`（`Settings` 加 `global_shortcut` + `launch_at_login` 两个字段）

**Interfaces:**
- Consumes: `lifecycle::wake(app)`（Task 1）
- Produces:
  - `shortcut::apply<R: Runtime>(app: &AppHandle<R>, old: Option<&str>, spec: Option<&str>) -> Result<(), String>`
  - `shortcut::plan(old: Option<&str>, spec: Option<&str>, old_still_registered: bool) -> Action` —— 纯函数，算「该做什么」
  - `shortcut::Action` —— `enum { Nothing, Register(String), Replace { new, old }, Unregister(String) }`
  - `shortcut::validate(spec: &str) -> Result<(), String>`
  - `shortcut::DEFAULT_SHORTCUT: &str` = `"Option+Cmd+Space"`

- [ ] **Step 1: 加依赖与设置字段**

`app/src-tauri/Cargo.toml` 的 `[dependencies]` 里加：

```toml
tauri-plugin-global-shortcut = "2"
```

`app/src-tauri/src/settings.rs`：

1) 在 `pub struct Settings { ... }` 里加两个字段（放在 `auto_refresh` 之后）：

```rust
    /// **全局快捷键** —— 在任何 app 里按下它都能唤起 FSearch。
    ///
    /// `None` = 不注册（用户主动关掉了）。
    ///
    /// 【格式】`global-hotkey` 的语法：修饰键在前、只能有一个主键。
    ///   "Option+Cmd+Space" / "CmdOrCtrl+Shift+F12"
    ///
    /// 【为什么是 `Option<String>` 而不是 `String`】
    ///   「没设过」和「设成空」要能区分开 —— 前者用默认值，后者表示用户关掉了。
    #[serde(default = "default_global_shortcut")]
    pub global_shortcut: Option<String>,

    /// **开机自启** —— 登录后自动在后台驻留。
    ///
    /// 【为什么设置里还要存一份】插件本身能查当前状态（`is_enabled()`），
    ///   但登录项可能被用户手动删掉 —— 设置里存一份是为了让 UI 有个默认值。
    #[serde(default)]
    pub launch_at_login: bool,
```

2) 加默认值的函数（放在 `default_true` 旁边）：

```rust
/// 默认的全局快捷键。
///
/// 【为什么是 ⌥⌘Space】macOS 上 ⌘Space 是 Spotlight 的默认键、
///   ⌥Space 是输入法切换 —— 两者都有人用。⌥⌘Space 冲突最少。
fn default_global_shortcut() -> Option<String> {
    Some("Option+Cmd+Space".to_string())
}
```

3) 在 `impl Default for Settings` 里加：

```rust
            global_shortcut: default_global_shortcut(),
            launch_at_login: false,
```

- [ ] **Step 2: 写 `shortcut.rs`**

```rust
//! 全局快捷键 —— 在任何 app 里按一下就能唤起 FSearch。
//!
//! 【为什么注册失败必须让用户知道】
//!   `⌥⌘Space` 很可能被别的软件占着。注册失败如果**静默**，
//!   用户按了没反应，还以为功能坏了 —— 所以要把错误显示出来。

use tauri::AppHandle;
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut};

/// 默认快捷键。
pub const DEFAULT_SHORTCUT: &str = "Option+Cmd+Space";

/// 校验一个快捷键规格 —— **纯函数**（好测）。
///
/// 【为什么要「必须有修饰键」】
///   纯 `Space` / 纯 `K` 会在你正常打字时误触发唤起。
pub fn validate(spec: &str) -> Result<(), String> {
    let shortcut: Shortcut = spec
        .parse()
        .map_err(|e| format!("快捷键格式非法：{e}"))?;
    if shortcut.mods.is_empty() {
        return Err("必须带修饰键（⌘ / ⌥ / ⌃ / ⇧），否则会和正常打字冲突".to_string());
    }
    Ok(())
}

/// 换快捷键时的动作 —— **纯函数**（好测）。
///
/// 【为什么要有它】`apply` 的真正难点是**顺序**：
///   新键注册失败时，旧的必须**原封不动**还在。
///   顺序错了 → 用户既没新键也没旧键 → 唤起入口全没了。
///   把「该做什么」抽成纯函数，这个顺序就能被**确定性地**测掉。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// 什么都不用做（键没变，而且确实还注册着）。
    Nothing,
    /// 注册这个键。
    Register(String),
    /// 注册这个键，**注册成功后**再注销那个旧的。
    Replace { new: String, old: Option<String> },
    /// 注销这个键。
    Unregister(String),
}

/// 算动作。
///
/// - `old` —— 之前注册的是哪个
/// - `spec` —— 现在要注册哪个
/// - `old_still_registered` —— 旧的**是不是真的还注册着**
///   （上次注册失败过的话，设置里存着它、但实际没注册）
pub fn plan(old: Option<&str>, spec: Option<&str>, old_still_registered: bool) -> Action {
    if old == spec {
        return match spec {
            None => Action::Nothing,
            Some(s) => {
                if old_still_registered {
                    Action::Nothing
                } else {
                    // 存的键没真的注册着 → 重试注册
                    Action::Register(s.to_string())
                }
            }
        };
    }
    match (spec, old) {
        (Some(s), o) => Action::Replace {
            new: s.to_string(),
            old: o.map(|x| x.to_string()),
        },
        (None, Some(o)) => Action::Unregister(o.to_string()),
        (None, None) => Action::Nothing,
    }
}

/// **应用**一个快捷键规格。
///
/// - `old` = 之前注册的是哪个（`None` = 之前没注册）
/// - `spec` = 现在要注册哪个（`None` = 注销掉、不注册）
///
/// 【顺序：先注册新的、成功了再注销旧的】
///   如果先注销旧的、新的又注册失败（被别的软件占着），
///   用户就**既没有新键、也没有旧键** —— 唤起入口全没了。
///   先注册新的：成功 → 再注销旧的；
///   失败 → 旧的**原封不动**还在，一切照旧（只是没换成新键）。
pub fn apply<R: tauri::Runtime>(
    app: &AppHandle<R>,
    old: Option<&str>,
    spec: Option<&str>,
) -> Result<(), String> {
    // ① 校验新的 —— 格式错 / 没修饰键 → 明确报错，**不动现有注册**
    if let Some(s) = spec {
        validate(s)?;
    }
    // ② 旧的到底还注册着吗？
    let old_still_registered = match old {
        Some(o) => match o.parse::<Shortcut>() {
            Ok(sc) => app.global_shortcut().is_registered(sc),
            Err(_) => false,
        },
        None => false,
    };
    // ③ 按计划执行
    match plan(old, spec, old_still_registered) {
        Action::Nothing => Ok(()),
        Action::Register(s) => register(app, &s),
        Action::Replace { new, old } => {
            // 【关键顺序】先注册新的 ——
            //   失败的话这里就返回 Err 了，旧的**没被动过**。
            register(app, &new)?;
            // 注册成功了，现在才注销旧的。
            if let Some(o) = old {
                if let Ok(osc) = o.parse::<Shortcut>() {
                    let _ = app.global_shortcut().unregister(osc);
                }
            }
            Ok(())
        }
        Action::Unregister(o) => {
            if let Ok(osc) = o.parse::<Shortcut>() {
                let _ = app.global_shortcut().unregister(osc);
            }
            Ok(())
        }
    }
}

/// 注册一个快捷键并装上唤起 handler。
fn register<R: tauri::Runtime>(app: &AppHandle<R>, spec: &str) -> Result<(), String> {
    let shortcut: Shortcut = spec
        .parse()
        .map_err(|e| format!("快捷键格式非法：{e}"))?;
    app.global_shortcut()
        .on_shortcut(shortcut, |app, _| {
            crate::lifecycle::wake(app);
        })
        .map_err(|e| {
            format!("快捷键「{spec}」注册失败（可能已被别的软件占用）：{e}")
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_shortcut_is_option_cmd_space() {
        assert_eq!(DEFAULT_SHORTCUT, "Option+Cmd+Space");
    }

    #[test]
    fn a_valid_shortcut_passes_validation() {
        assert!(validate("Option+Cmd+Space").is_ok());
        assert!(validate("CmdOrCtrl+Shift+F12").is_ok());
        assert!(validate("Control+Option+KeyA").is_ok());
    }

    #[test]
    fn a_shortcut_without_modifiers_is_rejected() {
        let err = validate("Space").unwrap_err();
        assert!(err.contains("修饰键"), "错误信息要说明为什么要修饰键，实际是：{err}");
    }

    #[test]
    fn a_garbage_shortcut_is_rejected() {
        assert!(validate("这不是快捷键").is_err());
        assert!(validate("").is_err());
    }

    // ---- 注册顺序 —— 「新键失败时旧的必须原封不动」这条不变式 ----

    #[test]
    fn first_registration_just_registers() {
        assert_eq!(
            plan(None, Some("Option+Cmd+Space"), false),
            Action::Register("Option+Cmd+Space".to_string())
        );
    }

    #[test]
    fn changing_the_key_registers_the_new_one_first() {
        assert_eq!(
            plan(
                Some("Option+Cmd+Space"),
                Some("CmdOrCtrl+K"),
                true
            ),
            Action::Replace {
                new: "CmdOrCtrl+K".to_string(),
                old: Some("Option+Cmd+Space".to_string()),
            }
        );
    }

    #[test]
    fn the_same_key_still_registered_does_nothing() {
        assert_eq!(
            plan(
                Some("Option+Cmd+Space"),
                Some("Option+Cmd+Space"),
                true
            ),
            Action::Nothing
        );
    }

    #[test]
    fn the_same_key_not_registered_is_retried() {
        // 上次注册失败过 → 设置里存着这个键、但实际没注册。
        // 这时用户「重新录一遍同样的键」必须**重试注册**，
        // 而不是什么都不做（否则用户永远修不好）。
        assert_eq!(
            plan(
                Some("Option+Cmd+Space"),
                Some("Option+Cmd+Space"),
                false
            ),
            Action::Register("Option+Cmd+Space".to_string())
        );
    }

    #[test]
    fn clearing_unregisters_the_old_key() {
        assert_eq!(
            plan(Some("Option+Cmd+Space"), None, true),
            Action::Unregister("Option+Cmd+Space".to_string())
        );
    }

    #[test]
    fn nothing_registered_and_nothing_wanted_is_a_no_op() {
        assert_eq!(plan(None, None, false), Action::Nothing);
    }
}
```

- [ ] **Step 3: 跑测试**

Run: `cd app/src-tauri && cargo test -p fsearch-app shortcut`
Expected: **10 passed**（4 个 validate + 6 个 plan）。

- [ ] **Step 4: 在 `lib.rs` 里装插件并注册默认快捷键**

1) 加 `mod shortcut;`。
2) Builder 上加插件（在 `.plugin(tauri_plugin_dialog::init())` 后面）：

```rust
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
```

3) 在 `setup` 里，**注册快捷键**（放在装托盘那段之后）：

```rust
            // 【注册全局快捷键】——
            //   读设置里存的那个（没有就用默认值）。
            //   注册失败**不许 panic**：快捷键被别的软件占着是很常见的事，
            //   少一个入口而已，托盘图标照常能用。
            {
                let s = settings::load(app.handle());
                let spec = s.global_shortcut.clone();
                match shortcut::apply(app.handle(), None, spec.as_deref()) {
                    Ok(()) => {
                        // 注册成功 → 撤掉（万一有）警告项
                        crate::tray::set_warning(app.handle(), None);
                    }
                    Err(e) => {
                        eprintln!("[fsearch] 注册全局快捷键失败: {e}");
                        // 【D5】注册失败必须**看得见** ——
                        //   在托盘菜单顶部插一条禁用的警告项。
                        //   （托盘是常驻的；设置面板可能正关着。）
                        crate::tray::set_warning(
                            app.handle(),
                            Some("⚠️ 快捷键未生效（可能已被占用）"),
                        );
                    }
                }
            }
```

- [ ] **Step 5: 编译并跑基线**

Run:
```bash
cd app/src-tauri && cargo build 2>&1 | tail -20 && cargo test -p fsearch-core && cargo test -p fsearch-app
cd app && pnpm vitest run && pnpm build
```
Expected: 全绿。

- [ ] **Step 6: 提交**

```bash
git add app/src-tauri/Cargo.toml app/src-tauri/Cargo.lock app/src-tauri/src/shortcut.rs \
        app/src-tauri/src/settings.rs app/src-tauri/src/lib.rs
git commit -m "feat(app): 全局快捷键 —— 默认 ⌥⌘Space，注册失败有明确报错"
```

---

### Task 4: 空闲时暂停自动重扫 + 唤醒补扫

**这是「省 CPU」那一刀。** 目标：空闲 CPU 从 5.8% 降到 0.5% 以下。

**Files:**
- Modify: `app/src-tauri/src/watcher.rs`
- Modify: `app/src-tauri/src/lib.rs`（`mod lifecycle;` 已有；确认 `use lifecycle::Lifecycle;` 在 watcher 里可用）

**Interfaces:**
- Consumes: `lifecycle::Phase` / `lifecycle::Lifecycle::phase()` / `take_catchup()` / `restore_catchup()`（Task 1）
- Produces:
  - `watcher::RescanDecision::Paused`（新变体）
  - `watcher::decide_rescan(inp: &RescanInputs, idle: bool, force: bool, auto_refresh: F) -> RescanDecision`

- [ ] **Step 1: 写失败测试 —— 两道新闸**

在 `app/src-tauri/src/watcher.rs` 的 `mod tests` 里**追加**（不要动已有的测试）：

```rust
    // ---- 后台常驻模式：空闲闸 + 强制补扫闸 ----
    //
    // 【为什么新写一个 `decide_rescan` 而不是往 `decide_auto_rescan` 里加字段】
    //   原有的四道闸是**时间驱动**的（去抖 800ms、最小间隔 5s…），
    //   新的两道闸是**生命周期驱动**的（空闲/唤醒）——
    //   语义不同，混在一个函数里会越堆越难读。
    //   而且新函数**不改 `RescanInputs`** → 已有的十几个测试构造一个字都不用动。

    fn base_inputs(dirty: bool, now: Instant) -> RescanInputs {
        RescanInputs {
            dirty,
            last_event_at: Some(now - Duration::from_secs(60)),
            now,
            last_auto_scan_at: Some(now - Duration::from_secs(60)),
            last_scan_completed_at: None,
            scan_in_flight: false,
        }
    }

    #[test]
    fn idle_pauses_the_rescan() {
        let now = Instant::now();
        let inp = base_inputs(true, now);
        assert_eq!(
            decide_rescan(&inp, true, false, || true),
            RescanDecision::Paused
        );
    }

    #[test]
    fn not_idle_keeps_the_old_behavior() {
        let now = Instant::now();
        let inp = base_inputs(true, now);
        assert_eq!(
            decide_rescan(&inp, false, false, || true),
            RescanDecision::Rescan
        );
    }

    #[test]
    fn force_scans_even_when_idle() {
        // 【竞态安全】唤醒时先置 force、后清 idle 的那段窗口里，
        // force 必须压过 idle —— 否则补扫要等到下一个文件事件才发生。
        let now = Instant::now();
        let inp = base_inputs(true, now);
        assert_eq!(
            decide_rescan(&inp, true, true, || true),
            RescanDecision::Rescan
        );
    }

    #[test]
    fn force_scans_even_without_any_event() {
        // 空闲期间压根没文件变化 → 没有事件时刻。
        // 原来的闸会在这里返回 Wait（「有脏标志却没有事件时刻 → 判断不了静默期」）。
        let now = Instant::now();
        let inp = RescanInputs {
            dirty: false,
            last_event_at: None,
            now,
            last_auto_scan_at: None,
            last_scan_completed_at: None,
            scan_in_flight: false,
        };
        assert_eq!(
            decide_rescan(&inp, false, true, || true),
            RescanDecision::Rescan
        );
    }

    #[test]
    fn force_still_respects_the_auto_refresh_switch() {
        // 用户关了自动刷新 → 不该自动扫（哪怕是唤醒时的补扫）。
        let now = Instant::now();
        let inp = base_inputs(true, now);
        assert_eq!(
            decide_rescan(&inp, false, true, || false),
            RescanDecision::Disabled
        );
    }

    #[test]
    fn force_waits_while_a_scan_is_in_flight() {
        let now = Instant::now();
        let mut inp = base_inputs(true, now);
        inp.scan_in_flight = true;
        assert_eq!(decide_rescan(&inp, false, true, || true), RescanDecision::Wait);
    }

    #[test]
    fn force_skips_the_min_interval() {
        // 刚自动扫过就唤醒 —— 最小间隔（5 秒）不该挡住补扫。
        let now = Instant::now();
        let inp = RescanInputs {
            dirty: true,
            last_event_at: Some(now - Duration::from_secs(1)),
            now,
            last_auto_scan_at: Some(now - Duration::from_millis(200)),
            last_scan_completed_at: None,
            scan_in_flight: false,
        };
        // 不带 force → 最小间隔挡住
        assert_eq!(decide_rescan(&inp, false, false, || true), RescanDecision::Wait);
        // 带 force → 直接扫
        assert_eq!(decide_rescan(&inp, false, true, || true), RescanDecision::Rescan);
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cd app/src-tauri && cargo test -p fsearch-app watcher`
Expected: **编译失败** —— `cannot find function decide_rescan`、`no variant Paused`。

- [ ] **Step 3: 加 `Paused` 变体和 `decide_rescan`**

在 `app/src-tauri/src/watcher.rs` 里：

1) `pub enum RescanDecision` 里加一个变体：

```rust
    /// 【新增】应用空闲 → 不重扫，但**保留脏标志**。
    ///
    /// 【为什么不清脏标志】脏标志是「有变化等着被收进索引」的记号。
    ///   空闲时不清 → 唤醒后那一轮自然会重扫，空闲期间的变化不会丢。
    ///   （跟闸一「扫描在飞」的处理方式一样：不丢事件。）
    Paused,
```

2) 在 `pub fn decide_auto_rescan<F: FnOnce() -> bool>(...)` **之后**加新函数：

```rust
/// 这一轮要不要重扫 —— **在原有的四道闸之外再加两道**。
///
/// 【两道新闸的语义跟原有的不一样】
///   - 原有的四道闸是**时间驱动**的（去抖 800ms、最小间隔 5s、覆盖、开关）。
///   - 新的两道闸是**生命周期驱动**的（应用空闲？用户刚叫醒它？）。
///   所以单独一个函数，而不是往 `RescanInputs` 里加字段 ——
///   **已有的十几个测试构造一个字都不用动。**
///
/// 【闸的顺序不能换】
///   ① 扫描在飞 → 等（两道新闸都尊重它）
///   ② 强制补扫 → 立刻扫（**压过空闲** —— 见下面的竞态说明）
///   ③ 空闲 → 暂停（但保留脏标志）
///   ④ 原有四道闸
///
/// 【为什么「强制补扫」要压过「空闲」】
///   唤醒路径是「先置 force、后清 idle」（见 lifecycle::wake）。
///   如果这两步之间 watcher 恰好读到旧的 `idle = true`，
///   补扫就会被挡掉、要等到下一个文件事件才发生 —— 用户唤醒了却看不到新结果。
///   `force` 优先 = 「用户刚叫醒我，立刻补扫」永远压过「我以为你在空闲」。
///
/// 【`auto_refresh` 开关两道新闸都尊重】
///   用户关了自动刷新 → 不该有任何自动扫描，唤醒时的补扫也算。
pub fn decide_rescan<F: FnOnce() -> bool>(
    inp: &RescanInputs,
    idle: bool,
    force: bool,
    auto_refresh: F,
) -> RescanDecision {
    // ① 有扫描在飞 → 等（不清脏标志）
    if inp.scan_in_flight {
        return RescanDecision::Wait;
    }
    // ② 强制补扫 → 立刻扫
    if force {
        if !auto_refresh() {
            return RescanDecision::Disabled;
        }
        return RescanDecision::Rescan;
    }
    // ③ 空闲 → 不扫，但**保留脏标志**
    if idle {
        return RescanDecision::Paused;
    }
    // ④ 以下是原有的四道闸
    decide_auto_rescan(inp, auto_refresh)
}
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cd app/src-tauri && cargo test -p fsearch-app watcher`
Expected: **全部通过**（原有的 + 新增 7 个）。

- [ ] **Step 5: 把新闸接到对账循环里**

在 `app/src-tauri/src/watcher.rs` 的 `reconcile_loop` 里，把原来的这段：

```rust
        let inp = RescanInputs {
            dirty,
            last_event_at,
            now,
            last_auto_scan_at,
            last_scan_completed_at,
            scan_in_flight,
        };
        // 【开关】用闭包传进去 —— 只在**真的要扫了**的时候才读设置
        //   （读设置要开一次 store 文件；每秒读是纯浪费）。
        let decision = decide_auto_rescan(&inp, || crate::settings::load(&app).auto_refresh);
        match decision {
            // 还没到时候 → **不清脏**（下一轮重新判断，不丢事件）
            RescanDecision::Wait => continue,
            // 已被覆盖 → 清脏、不用扫
            RescanDecision::Covered => {
                signals.take_dirty();
                continue;
            }
            // 开关关着 → 清脏、不用扫
            RescanDecision::Disabled => {
                signals.take_dirty();
                crate::logging::log_line("[watch] 自动刷新已关闭，跳过");
                continue;
            }
            RescanDecision::Rescan => {}
        }
```

换成：

```rust
        let inp = RescanInputs {
            dirty,
            last_event_at,
            now,
            last_auto_scan_at,
            last_scan_completed_at,
            scan_in_flight,
        };
        // 【生命周期状态】—— 空闲时不重扫；唤醒时的补扫请求也从这儿来。
        //
        //   `take_catchup()` 是「取出并清零」——
        //   万一这轮没扫成（还有扫描在飞），要**放回去**（下面 Wait 分支）。
        let life = app.state::<crate::lifecycle::Lifecycle>();
        let idle = life.phase() == crate::lifecycle::Phase::Idle;
        let force = life.take_catchup();
        // 【开关】用闭包传进去 —— 只在**真的要扫了**的时候才读设置
        //   （读设置要开一次 store 文件；每秒读是纯浪费）。
        let decision =
            decide_rescan(&inp, idle, force, || crate::settings::load(&app).auto_refresh);
        match decision {
            // 还没到时候 → **不清脏**（下一轮重新判断，不丢事件）
            RescanDecision::Wait => {
                // 补扫请求没消费掉 → 放回去，下轮再试
                if force {
                    life.restore_catchup();
                }
                continue;
            }
            // 已被覆盖 → 清脏、不用扫
            RescanDecision::Covered => {
                signals.take_dirty();
                continue;
            }
            // 开关关着 → 清脏、不用扫
            RescanDecision::Disabled => {
                signals.take_dirty();
                crate::logging::log_line("[watch] 自动刷新已关闭，跳过");
                continue;
            }
            // 【新增】空闲 → **不清脏**（唤醒后那一轮自然会重扫）
            RescanDecision::Paused => {
                continue;
            }
            RescanDecision::Rescan => {}
        }
```

> ⚠️ `let life = app.state::<...>()` 返回的是一个 guard，
> **必须先绑到变量再用**（`app.state::<T>().phase()` 会在临时值上借用 → E0716）。

- [ ] **Step 6: 编译并跑基线**

Run:
```bash
cd app/src-tauri && cargo build 2>&1 | tail -20 && cargo test -p fsearch-core && cargo test -p fsearch-app
cd app && pnpm vitest run && pnpm build
```
Expected: 全绿。

- [ ] **Step 7: 提交**

```bash
git add app/src-tauri/src/watcher.rs
git commit -m "feat(app): 空闲时暂停自动重扫 —— 唤醒时主动补扫一次"
```

---

### Task 5: 设置项 + 快捷键录制 UI + 开机自启

**Files:**
- Modify: `app/src-tauri/src/settings.rs`（`launch_at_login` 字段 —— Task 3 已加）
- Modify: `app/src-tauri/Cargo.toml`（加 `tauri-plugin-autostart`）
- Modify: `app/src-tauri/src/commands.rs`（加两个命令）
- Modify: `app/src-tauri/src/lib.rs`（注册两个命令 + 装 autostart 插件）
- Create: `app/src/lib/shortcut.ts`
- Create: `app/src/lib/shortcut.test.ts`
- Modify: `app/src/lib/types.ts`
- Modify: `app/src/components/SettingsPanel.vue`
- Modify: `app/src/App.vue`（监听 `app-awake`；**监听 `open-settings` → 打开设置抽屉**）

**Interfaces:**
- Consumes: `shortcut::apply`（Task 3）、`lifecycle::AWAKE_EVENT`（Task 1）
- Produces:
  - IPC 命令 `set_global_shortcut(spec: Option<String>) -> Result<(), String>`
  - IPC 命令 `set_launch_at_login(enabled: bool) -> Result<(), String>`
  - `app/src/lib/shortcut.ts` 导出 `shortcutFromEvent(e: KeyEventLike): RecordedShortcut | null`

- [ ] **Step 1: 写前端纯函数的失败测试**

创建 `app/src/lib/shortcut.test.ts`：

```ts
import { describe, expect, it } from "vitest";
import { shortcutFromEvent, type KeyEventLike } from "./shortcut";

function ev(partial: Partial<KeyEventLike>): KeyEventLike {
  return {
    code: "KeyA",
    ctrlKey: false,
    altKey: false,
    shiftKey: false,
    metaKey: false,
    ...partial,
  };
}

describe("shortcutFromEvent", () => {
  it("把 ⌥⌘Space 转成后端规格", () => {
    const r = shortcutFromEvent(
      ev({ code: "Space", altKey: true, metaKey: true }),
    );
    expect(r?.spec).toBe("Option+Cmd+Space");
  });

  it("修饰键顺序固定（Ctrl → Option → Shift → Cmd）", () => {
    const r = shortcutFromEvent(
      ev({ code: "KeyF", ctrlKey: true, altKey: true, shiftKey: true, metaKey: true }),
    );
    expect(r?.spec).toBe("Ctrl+Option+Shift+Cmd+KeyF");
  });

  it("没有修饰键 → 拒绝（null）", () => {
    expect(shortcutFromEvent(ev({ code: "KeyA" }))).toBeNull();
    expect(shortcutFromEvent(ev({ code: "Space" }))).toBeNull();
  });

  it("只按了修饰键 → 拒绝（null），继续等用户按主键", () => {
    expect(shortcutFromEvent(ev({ code: "MetaLeft", metaKey: true }))).toBeNull();
    expect(shortcutFromEvent(ev({ code: "ShiftLeft", shiftKey: true }))).toBeNull();
    expect(shortcutFromEvent(ev({ code: "ControlRight", ctrlKey: true }))).toBeNull();
  });

  it("用 code 而不是 key —— 所以 ⌘ 出来是 MetaLeft 也不影响主键", () => {
    const r = shortcutFromEvent(ev({ code: "KeyA", metaKey: true }));
    expect(r?.spec).toBe("Cmd+KeyA");
  });

  it("给人看的标签用 macOS 符号", () => {
    const r = shortcutFromEvent(ev({ code: "Space", altKey: true, metaKey: true }));
    expect(r?.label).toBe("⌥⌘Space");
  });

  it("字母键的标签去掉 Key 前缀", () => {
    const r = shortcutFromEvent(ev({ code: "KeyK", metaKey: true }));
    expect(r?.label).toBe("⌘K");
  });

  it("数字键的标签去掉 Digit 前缀", () => {
    const r = shortcutFromEvent(ev({ code: "Digit7", ctrlKey: true }));
    expect(r?.label).toBe("⌃7");
  });
});
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cd app && pnpm vitest run src/lib/shortcut.test.ts`
Expected: **FAIL** —— `Cannot find module './shortcut'`。

- [ ] **Step 3: 写 `shortcut.ts`**

创建 `app/src/lib/shortcut.ts`：

```ts
/** 全局快捷键的「录制」—— 把一次按键变成后端能认的规格字符串。
 *
 *  【为什么用 `event.code` 而不是 `event.key`】
 *   `key` 受输入法 / 键盘布局影响（同一个物理键在不同布局下 `key` 不同），
 *   `code` 是**物理位置**，稳。后端（global-hotkey）认的也是物理键。
 *
 *  【后端语法】修饰键在前、只能有一个主键：
 *   "Option+Cmd+Space" / "Ctrl+Option+Shift+Cmd+KeyF"
 *   主键名用 `KeyboardEvent.code`（KeyA / Digit1 / Space / F1 / ArrowUp …），
 *   它们跟 global-hotkey 的 `parse_key` 认的名字是对得上的。
 */

/** 修饰键的标志位。 */
export interface ModState {
  ctrlKey: boolean;
  altKey: boolean;
  shiftKey: boolean;
  metaKey: boolean;
}

/** 一次 keydown 事件里我们关心的部分。 */
export interface KeyEventLike extends ModState {
  /** `KeyboardEvent.code` */
  code: string;
}

export interface RecordedShortcut {
  /** 后端规格，如 "Option+Cmd+Space" */
  spec: string;
  /** 给人看的，如 "⌥⌘Space" */
  label: string;
}

/** 修饰键 → 后端语法里的名字。**顺序固定**（后端解析要求修饰键在前）。 */
const MOD_ORDER: ReadonlyArray<readonly [keyof ModState, string]> = [
  ["ctrlKey", "Ctrl"],
  ["altKey", "Option"],
  ["shiftKey", "Shift"],
  ["metaKey", "Cmd"],
];

/** 修饰键本身 —— 不能当主键（只按了它就结束录制是不对的）。 */
const MODIFIER_CODES = new Set([
  "ControlLeft",
  "ControlRight",
  "AltLeft",
  "AltRight",
  "ShiftLeft",
  "ShiftRight",
  "MetaLeft",
  "MetaRight",
  "CapsLock",
  "OSLeft",
  "OSRight",
]);

/** 修饰键的 macOS 符号。 */
const MOD_LABEL: Record<string, string> = {
  Ctrl: "⌃",
  Alt: "⌥",
  Shift: "⇧",
  Cmd: "⌘",
};

/** 一些主键的专属写法。 */
const KEY_LABEL: Record<string, string> = {
  Space: "Space",
  Escape: "esc",
  Enter: "↩",
  Tab: "⇥",
  Backspace: "⌫",
  Delete: "⌦",
  ArrowUp: "↑",
  ArrowDown: "↓",
  ArrowLeft: "←",
  ArrowRight: "→",
};

/** 把一次 keydown 变成快捷键规格。
 *
 *  返回 `null` = 这一下**还不能**结束录制：
 *   - 只按了修饰键（还在等主键）
 *   - 没有任何修饰键（会跟正常打字打架，拒绝）
 */
export function shortcutFromEvent(e: KeyEventLike): RecordedShortcut | null {
  if (MODIFIER_CODES.has(e.code)) return null;

  const mods = MOD_ORDER.filter(([k]) => e[k]).map(([, name]) => name);
  if (mods.length === 0) return null;

  const spec = [...mods, e.code].join("+");
  return { spec, label: labelFor(mods, e.code) };
}

function labelFor(mods: string[], code: string): string {
  const m = mods.map((x) => MOD_LABEL[x] ?? x).join("");
  const k =
    KEY_LABEL[code] ??
    code.replace(/^Key/, "").replace(/^Digit/, "");
  return m + k;
}
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cd app && pnpm vitest run src/lib/shortcut.test.ts`
Expected: **8 passed**。

- [ ] **Step 5: 加后端命令**

`app/src-tauri/Cargo.toml` 的 `[dependencies]` 里加：

```toml
tauri-plugin-autostart = "2"
```

`app/src-tauri/src/commands.rs` 末尾追加：

```rust
/// 改全局快捷键 —— **先注册成功，再写盘**。
///
/// 【为什么不在前端直接改设置】
///   注册可能失败（快捷键被别的软件占用）。
///   如果先写盘再注册，设置里就会记着一个**没生效**的快捷键 ——
///   用户按了没反应，还不知道是为什么。
///   → 顺序必须是：注销旧的 → 注册新的 → 成功才写盘。
///
/// 【为什么这里直接写盘而不走前端的 `patchSettings`】
///   `set_settings` 是**整体替换**（前端那条路径要传完整对象）。
///   这里在 Rust 侧做「读 → 改一个字段 → 整体写回」，
///   跟 Rust 侧存窗口位置是同一套做法，不会把别的字段写没。
#[tauri::command]
pub fn set_global_shortcut<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    spec: Option<String>,
) -> Result<(), String> {
    let mut s = crate::settings::load(&app);
    let old = s.global_shortcut.clone();
    // ① 先注册新的
    //    失败 → 旧的**原封不动**还在（见 shortcut::apply 的说明），
    //    **不写盘** → 设置与实际状态保持一致。
    match crate::shortcut::apply(&app, old.as_deref(), spec.as_deref()) {
        Ok(()) => {
            // ② 注册成功了才写盘
            s.global_shortcut = spec;
            crate::settings::save(&app, &s)?;
            // ③ 一切正常 → 撤掉托盘上的警告项
            crate::tray::set_warning(&app, None);
            Ok(())
        }
        Err(e) => {
            // 【D5】失败要让用户看得见 ——
            //   托盘菜单顶部插一条禁用的警告项（托盘是常驻的）。
            crate::tray::set_warning(&app, Some("⚠️ 快捷键未生效（可能已被占用）"));
            Err(e)
        }
    }
}

/// 改开机自启 —— **先改系统，再写盘**。
///
/// 【为什么要先改系统】跟快捷键同理：
///   写盘成功但系统改失败 → 设置说「开机自启开着」而实际上没有 —— 骗人。
#[tauri::command]
pub fn set_launch_at_login<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    enabled: bool,
) -> Result<(), String> {
    use tauri_plugin_autostart::ManagerExt;
    let mgr = app.autolaunch();
    if enabled {
        mgr.enable().map_err(|e| format!("开启开机自启失败：{e}"))?;
    } else {
        mgr.disable().map_err(|e| format!("关闭开机自启失败：{e}"))?;
    }
    let mut s = crate::settings::load(&app);
    s.launch_at_login = enabled;
    crate::settings::save(&app, &s)?;
    Ok(())
}
```

`app/src-tauri/src/lib.rs`：
1) 在 `app_commands_handler` 的 `generate_handler!` 里加两行：

```rust
        commands::set_global_shortcut,
        commands::set_launch_at_login,
```

2) Builder 上加插件：

```rust
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
```

- [ ] **Step 6: 加前端类型**

`app/src/lib/types.ts` 的 `Settings` 接口里加：

```ts
  /** 全局快捷键（后端规格，如 "Option+Cmd+Space"）。缺省 = 用默认值。 */
  globalShortcut?: string | null;
  /** 开机自启。 */
  launchAtLogin?: boolean;
```

- [ ] **Step 7: 设置面板加「全局快捷键」与「开机自启」**

`app/src/components/SettingsPanel.vue`：

1) `<script setup>` 里加 import：

```ts
import { invoke } from "@tauri-apps/api/core";
import { shortcutFromEvent, type RecordedShortcut } from "../lib/shortcut";
```

2) 加状态与函数（放在 `const busy = ref(false);` 之后）：

```ts
/* ---- 全局快捷键（录制式） ---- */

/** 正在等用户按组合键。 */
const recording = ref(false);
/** 当前快捷键（后端规格）。null = 没设。 */
const shortcutSpec = ref<string | null>(null);
/** 当前快捷键（给人看的）。 */
const shortcutLabel = ref("");
/** 快捷键相关的错误（注册失败、被占用…）。 */
const shortcutError = ref<string | null>(null);

function startRecording(): void {
  recording.value = true;
  shortcutError.value = null;
  window.addEventListener("keydown", onRecordKey, true);
}

function stopRecording(): void {
  recording.value = false;
  window.removeEventListener("keydown", onRecordKey, true);
}

/** 录制时的按键捕获。
 *
 *  【为什么要 `capture = true】】在**捕获阶段**拦下，
 *  才不会被页面自己的 keydown 处理先吃掉。 */
function onRecordKey(e: KeyboardEvent): void {
  // 录制期间把按键全吃掉 —— 否则 ⌘W 之类的会关掉应用。
  e.preventDefault();
  e.stopPropagation();
  if (e.key === "Escape") {
    stopRecording();
    return;
  }
  const rec = shortcutFromEvent(e);
  // null = 只按了修饰键 / 没有修饰键 → 继续等
  if (!rec) return;
  stopRecording();
  void applyShortcut(rec);
}

async function applyShortcut(rec: RecordedShortcut): Promise<void> {
  busy.value = true;
  shortcutError.value = null;
  try {
    await invoke("set_global_shortcut", { spec: rec.spec });
    shortcutSpec.value = rec.spec;
    shortcutLabel.value = rec.label;
  } catch (e) {
    // 【注册失败】—— 快捷键被别的软件占了，或者格式不对。
    // 设置与实际状态保持一致（旧键继续用），只把错误显示出来。
    shortcutError.value = String(e);
  } finally {
    busy.value = false;
  }
}

async function clearShortcut(): Promise<void> {
  busy.value = true;
  shortcutError.value = null;
  try {
    // 【必须传 null，不能传空串】——
    //   `spec: Option<String>` 收到 `Some("")` 会去注册一个空快捷键然后报错；
    //   `None` 的语义是「注销掉、不注册」。
    await invoke("set_global_shortcut", { spec: null });
    shortcutSpec.value = null;
    shortcutLabel.value = "";
  } catch (e) {
    shortcutError.value = String(e);
  } finally {
    busy.value = false;
  }
}

/* ---- 开机自启 ---- */

const launchAtLogin = ref(false);

async function toggleLaunchAtLogin(): Promise<void> {
  busy.value = true;
  shortcutError.value = null;
  const next = !launchAtLogin.value;
  try {
    await invoke("set_launch_at_login", { enabled: next });
    launchAtLogin.value = next;
  } catch (e) {
    shortcutError.value = String(e);
  } finally {
    busy.value = false;
  }
}
```

> ⚠️ `clearShortcut` 传的是 **`null`**（= 注销掉、不注册），**不是空串** ——
> 后端 `spec: Option<String>` 收到 `Some("")` 会去注册一个空快捷键而报错。

3) 在 `<template>` 的 `set-block` 里（`autoRefresh` 那个 `set-row` **之后**）加两行：

```html
          <!-- 全局快捷键。 -->
          <div class="set-row">
            <div class="set-row-text">
              <span class="set-row-label">全局快捷键</span>
              <p class="set-hint">
                在任何软件里按这个组合键都能唤起 FSearch。
                点「录制」后，直接按下想要的组合键就行。
              </p>
            </div>
            <div class="shortcut-recorder">
              <button
                class="set-btn"
                :disabled="busy || loadState !== 'ready'"
                @click="recording ? stopRecording() : startRecording()"
              >
                {{ recording ? "请按下组合键…" : (shortcutLabel || "未设置") }}
              </button>
              <button
                v-if="shortcutSpec"
                class="set-btn"
                :disabled="busy"
                @click="clearShortcut"
              >
                清除
              </button>
            </div>
          </div>

          <!-- 开机自启。 -->
          <div class="set-row">
            <div class="set-row-text">
              <span class="set-row-label">开机时自动在后台驻留</span>
              <p class="set-hint">
                登录后 FSearch 会在后台待命，按全局快捷键即可唤起。
              </p>
            </div>
            <button
              class="set-switch"
              role="switch"
              :aria-checked="launchAtLogin"
              aria-label="开机时自动在后台驻留"
              :disabled="busy || loadState !== 'ready'"
              @click="toggleLaunchAtLogin"
            >
              <span class="set-switch-knob"></span>
            </button>
          </div>
```

4) 读初始值：`app/src/components/SettingsPanel.vue` 的 `onMounted` 里，
   **紧跟着** `autoRefresh.value = ...` 那一行后面加两行
   （原文是 `onMounted(async () => { settings.value = await loadSettings(); … autoRefresh.value = settings.value.autoRefresh ?? true; loadState.value = "ready"; … }`）：

```ts
    // 读当前快捷键与开机自启状态
    shortcutSpec.value = settings.value.globalShortcut ?? null;
    shortcutLabel.value = settings.value.globalShortcut ?? "";
    launchAtLogin.value = settings.value.launchAtLogin === true;
```

5) CSS（`app/src/styles.css` 末尾追加）：

```css
/* 快捷键录制按钮组 —— 两个按钮并排 */
.shortcut-recorder {
  display: flex;
  gap: 8px;
  align-items: center;
  flex-shrink: 0;
}
```

- [ ] **Step 8: App.vue 监听「已唤起」**

`app/src/App.vue`：

1) 在 `onMounted` 里加 `void listenForAwake();`（挨着 `void listenForAutoRefresh();`）
2) 在 `onBeforeUnmount` 里加 `unlistenAwake?.();`
3) 加这个函数（挨着 `listenForAutoRefresh`）：

```ts
/* ---- 后台唤起 ---- */

/** 事件名 —— 必须与 Rust 侧的 `lifecycle::AWAKE_EVENT` 一致。 */
const AWAKE_EVENT = "app-awake";

/** 取消监听的函数 —— 卸载时调它。 */
let unlistenAwake: UnlistenFn | null = null;

/** 订阅「已唤起」事件 —— 快捷键 / 托盘把窗口叫出来时 Rust 会发这个。
 *
 *  【为什么用 `focus()` 而不是 `focusEnd()`】
 *   `focus()` 会**选中全文** —— 用户唤起后直接打字就是替换掉上次的查询，
 *   不用先手动清空（Raycast 就是这个手感）。
 *   `focusEnd()` 只是把光标放到末尾（那是「追加关键字」用的）。 */
async function listenForAwake(): Promise<void> {
  try {
    const un = await listen(AWAKE_EVENT, () => {
      void nextTick(() => searchBar.value?.focus());
    });
    unlistenAwake = un;
  } catch (e) {
    /* 【什么时候会失败】不在 Tauri 环境里（单元测试、浏览器里直接开 vite dev）。
       那时不会有唤起事件 —— 不影响别的功能。 */
    console.warn("[fsearch] 监听「已唤起」事件失败（不在 Tauri 环境里？）", e);
  }
}
```

- [ ] **Step 8b: App.vue 监听「打开设置」（托盘菜单的「设置…」用）**

> ⚠️ **这一段曾经漏过**（终审 C-1）：设计文档里有这一行，本计划里没有 →
> 实现者照计划做 → Rust 发了事件、前端没人听 → 点「设置…」**不报错、也不开抽屉**。
> **教训：凡是「Rust 发事件 → 前端收事件」，计划里必须成对列出。**

`app/src/App.vue`：

1) 在 `onMounted` 里加 `void listenForOpenSettings();`
2) 在 `onBeforeUnmount` 里加 `unlistenOpenSettings?.();`
3) 加这个函数（挨着 `listenForAwake`）：

```ts
/* ---- 托盘菜单的「设置…」 ---- */

/** 事件名 —— 必须与 Rust 侧的 `lifecycle::OPEN_SETTINGS_EVENT` 一致。 */
const OPEN_SETTINGS_EVENT = "open-settings";

/** 取消监听的函数 —— 卸载时调它。 */
let unlistenOpenSettings: UnlistenFn | null = null;

async function listenForOpenSettings(): Promise<void> {
  try {
    const un = await listen(OPEN_SETTINGS_EVENT, () => {
      openPanel("settings");
      void nextTick(() => {
        document.querySelector<HTMLElement>(".panel-drawer button")?.focus();
      });
    });
    unlistenOpenSettings = un;
  } catch (e) {
    console.warn("[fsearch] 监听「打开设置」事件失败（不在 Tauri 环境里？）", e);
  }
}
```

- [ ] **Step 9: 跑全部前端测试与类型检查**

Run:
```bash
cd app && pnpm vitest run && pnpm build
```
Expected: 全绿。`pnpm build` 里的 `vue-tsc --noEmit` 必须 **0 错误**。

- [ ] **Step 10: 跑 Rust 侧基线**

Run:
```bash
cd app/src-tauri && cargo build 2>&1 | tail -20 && cargo test -p fsearch-core && cargo test -p fsearch-app
```
Expected: 全绿。

- [ ] **Step 11: 提交**

```bash
git add app/src-tauri/Cargo.toml app/src-tauri/Cargo.lock app/src-tauri/src/commands.rs \
        app/src-tauri/src/lib.rs app/src/lib/shortcut.ts app/src/lib/shortcut.test.ts \
        app/src/lib/types.ts app/src/components/SettingsPanel.vue app/src/App.vue
git commit -m "feat(app): 设置面板 —— 快捷键录制 + 开机自启；唤起时聚焦搜索框"
```

---

### Task 6: 用户手册 + 测量工具入库 + 真机验收

**Files:**
- Modify: `docs/USER-GUIDE.md`
- Create: `app/scripts/wake-latency.swift`
- Create: `app/scripts/idle-cpu.sh`
- Create: `app/src-tauri/fsearch-core/examples/mem_probe.rs`

**Interfaces:**
- Consumes: 全部前 5 个 Task
- Produces: 三个可复跑的验收脚本 + 用户手册的新章节

- [ ] **Step 1: 补用户手册**

在 `docs/USER-GUIDE.md` 里加一节（放在讲设置的那节后面）：

```markdown
## 后台常驻模式

FSearch 可以在后台常驻 —— 关掉窗口它不会退出，随时能叫回来。

### 怎么唤起

| 方式 | 说明 |
|---|---|
| **全局快捷键** | 默认 `⌥⌘Space`。在任何软件里按它都能唤起。 |
| **菜单栏图标** | 屏幕右上角那个放大镜。**左键**唤起（如果窗口已经在前台，再点一下会收起）；**右键**弹出菜单（打开 / 设置 / 退出）。 |
| **关闭按钮** | 点窗口左上角的红色按钮 —— 窗口收起来，程序继续在后台待命。 |

> 唤起后光标会自动落在搜索框里，上次的查询处于**选中**状态 ——
> 直接打字就是替换掉它。

### 后台时它占多少

| | |
|---|---|
| 内存 | 约 **87 MB**（其中搜索索引只占 10 MB，界面占 63 MB） |
| CPU | **低于 0.5%** —— 空闲时会自动暂停「文件变化自动重扫」，你唤起它时会补扫一次 |

> 空闲判定：窗口收起来算空闲；窗口还开着但你切去别的 app 超过 **20 秒**也算。

### 开机自启

设置 → 「开机时自动在后台驻留」打开即可。

### 改快捷键

设置 → 「全局快捷键」→ 点「录制」→ 直接按下想要的组合键。
必须带修饰键（`⌘` / `⌥` / `⌃` / `⇧`）—— 纯字母键会和正常打字打架。
```

- [ ] **Step 2: 把引擎内存探针加回来**

创建 `app/src-tauri/fsearch-core/examples/mem_probe.rs`：

```rust
//! 内存探针 —— 用来把「引擎 + 索引」这一块的开销从整个应用里单独量出来。
//!
//! 【为什么需要它】
//!   评估「后台待命时保留什么」需要三档内存数字：
//!     ① 完整应用（界面 + 引擎）  —— 直接量运行中的 .app
//!     ② 只有引擎（丢掉界面）     —— 这个探针量它
//!     ③ 只有空壳（引擎也丢掉）   —— ① − ② − 前端框架开销
//!
//!   实测结论（2026-10-21，索引 `$HOME` 25,917 项）：
//!     ① 87 MB ・ ② **9.8 MB** ・ ③ ≈11–16 MB
//!   → 索引很便宜，界面很贵（63 MB）。
//!
//! 用法：
//! ```bash
//! cd app/src-tauri && cargo run -p fsearch-core --example mem_probe -- ~
//! # 另一个终端：
//! footprint -p <pid>
//! ```
//!
//! 【注意】这里**不建窗口、不跑前端** —— 它量的是纯引擎 + 索引的代价。

use fsearch_core::Engine;
use std::path::PathBuf;
use std::time::Duration;

fn main() {
    let dir = std::env::args()
        .nth(1)
        .unwrap_or_else(|| std::env::var("HOME").unwrap_or_else(|_| "/".into()));
    let path = PathBuf::from(&dir);

    let t0 = std::time::Instant::now();
    let (engine, indexed) = Engine::new(&[path.as_path()]).expect("建引擎失败");
    let scan_ms = t0.elapsed().as_millis();

    println!("[mem_probe] 目录 = {dir}");
    println!("[mem_probe] 索引完成：{indexed} 项，耗时 {scan_ms} ms");
    println!("[mem_probe] PID = {}", std::process::id());
    println!(
        "[mem_probe] 常驻中……用 footprint -p {} 量内存",
        std::process::id()
    );

    // 常驻：引擎必须活着，否则会被 drop 掉、量到的就不是「引擎在内存里」的成本。
    loop {
        std::thread::sleep(Duration::from_secs(1));
        std::hint::black_box(&engine);
    }
}
```

- [ ] **Step 3: 写唤回耗时测量脚本**

创建 `app/scripts/wake-latency.swift`：

```swift
// 唤回耗时测量 —— 窗口从「看不见」到「真的在屏幕上」用了多久。
//
// 【为什么用 CGWindowList 而不是截图 / 辅助功能】
//   `screencapture` 要屏幕录制权限；AX 对 overlay 窗口不一定认得。
//   CGWindowList 是**窗口服务器给的真相** ——
//   「这个进程的窗口现在在不在屏幕上」直接问它。
//
// 用法：
//   swift app/scripts/wake-latency.swift watch   <pid>   # 持续打印可见性变化
//   swift app/scripts/wake-latency.swift launch <exe>  # 冷启动 → 窗口可见的耗时

import Foundation
import CoreGraphics

func windowVisible(pid: Int) -> (visible: Bool, w: Double, h: Double) {
    let opts: CGWindowListOption = [.optionOnScreenOnly, .excludeDesktopElements]
    guard let list = CGWindowListCopyWindowInfo(opts, kCGNullWindowID) as? [[String: Any]] else {
        return (false, 0, 0)
    }
    for win in list {
        let owner = win[kCGWindowOwnerPID as String] as? Int ?? -1
        guard owner == pid else { continue }
        guard (win[kCGWindowIsOnscreen as String] as? Bool ?? false) else { continue }
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
    FileHandle.standardError.write("用法: wake-latency <watch|launch> <pid|exe>\n".data(using: .utf8)!)
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
            print(String(format: "[%.3f] %@   w=%.0f h=%.0f", Date().timeIntervalSince(start), s, w, h))
            fflush(stdout)
            last = s
        }
        usleep(2000)
    }

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
```

- [ ] **Step 4: 写空闲 CPU 测量脚本**

创建 `app/scripts/idle-cpu.sh`：

```bash
#!/bin/bash
# 空闲 CPU 测量 —— 「这 15 秒里，这个进程烧掉了多少 CPU」。
#
# 【为什么不用 top 的 %CPU】top 的那个数字是**开机以来的平均值**，
#   会把「刚才那 0.5 秒的补扫」稀释掉，看不出空闲时的真实开销。
#   这里用 `ps -o time=` 的**差分**（CPU 时间增量 / 墙钟增量）——
#   测的就是「这一段时间里烧了多少」。
#
# 用法：bash app/scripts/idle-cpu.sh <pid> [秒数]

set -euo pipefail

PID="${1:?用法: idle-cpu.sh <pid> [秒数]}"
SECS="${2:-15}"

if ! ps -p "$PID" > /dev/null 2>&1; then
  echo "进程 $PID 不存在" >&2
  exit 1
fi

to_seconds() {
  # "0:03.71" / "1:02.50" → 秒
  python3 -c "
import sys
p = sys.argv[1].split(':')
s = 0.0
for x in p:
    s = s * 60 + float(x)
print(s)
" "$1"
}

T0=$(ps -o time= -p "$PID" | tr -d ' ')
W0=$(date +%s)
echo "采样 $SECS 秒（PID=$PID）…"
sleep "$SECS"
T1=$(ps -o time= -p "$PID" | tr -d ' ')
W1=$(date +%s)

python3 -c "
def to_s(t):
    s = 0.0
    for x in t.split(':'):
        s = s * 60 + float(x)
    return s
d = to_s('$T1') - to_s('$T0')
w = $W1 - $W0
print(f'CPU 时间增量 {d:.2f}s / 墙钟 {w}s  =>  平均 CPU 占用 {d/w*100:.2f}%')
"
```

- [ ] **Step 5: 跑真机验收（需要用户配合看结果）**

打一个 release 包：

```bash
cd app && bash scripts/bundle-app.sh
```

> ⚠️ **必须用这个脚本** —— `pnpm tauri build` 不会带 `tauri/custom-protocol` feature，
> 前端嵌不进二进制 → `.app` 白页而 Rust 侧照跑（看着像「启动了」）。

然后逐条验：

| # | 验收项 | 怎么验 | 期望 |
|---|---|---|---|
| A1 | 菜单栏图标出现 | 启动 `.app`，看屏幕右上角 | 有一个放大镜图标；**深色/浅色菜单栏下都看得清** |
| A2 | 托盘左键唤起 | 点图标 | 窗口出来（70–150 ms） |
| A3 | 托盘左键收起 | 窗口在前台时再点图标 | 窗口收起来，**程序还在**（菜单栏图标还在，Dock 图标还在） |
| A4 | 托盘右键菜单 | 右键图标 | 弹出「打开 FSearch / 设置… / 退出」 |
| A5 | 快捷键唤起 | 在别的 app（如 Safari）里按 `⌥⌘Space` | FSearch 窗口出来并聚焦搜索框 |
| A6 | 关闭按钮 = 隐藏 | 点窗口红色按钮 | 窗口消失，**程序还在**；`⌥⌘Space` 能唤回 |
| A7 | **隐藏后 webview 还活着** | 隐藏后跑：<br>`ps -o pid,rss,comm -p <pid>`（看有没有 3 个 WebKit 进程）<br>`footprint -p <pid>`（应约 25 MB，不是 0） | 4 个进程都在，footprint 约 25 MB |
| A8 | 唤回耗时 | `swift app/scripts/wake-latency.swift watch <pid>`，然后手动唤起 | 两次事件的时间差 **< 150 ms** |
| A9 | 空闲 CPU | 隐藏窗口后跑 `bash app/scripts/idle-cpu.sh <pid> 30` | **< 0.5%** |
| A10 | 空闲后索引是新的 | 隐藏窗口 → 在别处新建一个文件 → 唤起 → 约 1 秒后搜那个文件名 | 能搜到 |
| A11 | 开机自启 | 设置里打开 → 注销 → 重新登录 | FSearch 自动在后台驻留（菜单栏有图标） |
| A12 | 上游零改动 | `bash shim/check-upstream-pure.sh` | ✅ 上游是纯净的 |
| A13 | 菜单栏图标 ≠ Spotlight 的放大镜 | 看右上角菜单栏，**数放大镜**（Spotlight 自带一个） | FSearch 的那个**另有一个**，位置更靠左 |
| A14 | 两条恢复路径都要测 | ① 唤起 → 强制补扫；② 点回焦点 → 脏标志自然补扫 | 两条都能搜到空闲期间新建的文件 |
| A15 | 反复收起/唤起时的 CPU | 收起/唤起 5~10 次后 `bash app/scripts/idle-cpu.sh <pid> 30` | 空闲 CPU **< 0.5 %**（实测 0.07 %） |
| A16 | 两条启动路径 | 不带参数 / 带 `--hidden` | 前者出窗口、后者**不出**窗口（进程还在） |
| A17 | 开机自启开关显示系统真值 | 手删 `~/Library/LaunchAgents/FSearch.plist` → 再开设置面板 | 开关翻成「**关**」（显示的是**系统**真值） |
| A18 | 换键路径（`Replace` 分支） | 设置 →「全局快捷键」→「录制」→ 按一个**新**组合键 | 旧键按了没反应；新键能唤起；面板**不**出现「仍会唤起」 |
| A19 | 清除快捷键（`Unregister` 分支） | 设置 →「全局快捷键」→「清除」 | 旧键按了没反应；面板**不**出现「它仍会唤起 FSearch」 |
| **A20** | **托盘右键 →「设置…」→ 设置抽屉必须打开** | 托盘**右键** → 点「设置…」 | **设置抽屉打开**（不是只把窗口叫出来）。<br>⚠️ 这一条**曾经是坏的**：Rust 发 `open-settings`、前端没人听 → 点了不报错、也不开抽屉（终审 C-1，已修） |
| **A21** | **在另一台机器 / 另一个用户会话验证托盘图标能显示** | 换一台机器（或同一台机器**注销重登**后）跑 `.app` | 菜单栏出现 FSearch 的放大镜。<br>用途：把「**这台机器的环境问题**」与「**代码问题**」彻底分开（终审 I-2 留下的疑点） |

> **A8 / A9 是本功能的核心指标** —— 设计文档里的目标是唤回 < 150 ms、空闲 CPU < 0.5%。
> 实测基线：唤回 68–116 ms、空闲 0.27%（在 `autoRefresh` 关的情况下测的；
> 开着且目录有活动时是 5.8% —— 现在空闲时会暂停重扫，应该回落到 0.27% 那档）。

- [ ] **Step 6: 跑全部基线**

Run:
```bash
cd app/src-tauri && cargo test -p fsearch-core && cargo test -p fsearch-app
cd app && pnpm vitest run && pnpm build
cd ../.. && bash shim/check-upstream-pure.sh
```
Expected: 全绿。

- [ ] **Step 7: 提交**

```bash
git add docs/USER-GUIDE.md app/scripts/wake-latency.swift app/scripts/idle-cpu.sh \
        app/src-tauri/fsearch-core/examples/mem_probe.rs
git commit -m "docs(app): 用户手册补「后台常驻模式」；验收测量脚本入库"
```

---

## 验收总表

| 目标 | 验收项 | 期望 |
|---|---|---|
| G1 后台常驻不退出 | A6、A7 | 点关闭按钮 → 窗口消失、程序还在；隐藏后 4 个进程都在 |
| G2 唤回够快 | A8 | **< 150 ms** |
| G3 空闲几乎不耗 CPU | A9 | **< 0.5%** |
| G4 菜单栏 + 全局快捷键 | A1–A5 | 图标在、快捷键在别的 app 也能唤起 |
| G5 开机自启 | A11 | 重新登录后自动驻留 |
| G6 唤醒后索引是新的 | A10 | 唤起后 ≤ 1 s 能搜到空闲期间新增的文件 |
| 硬约束 | A12 | 上游 `src/` 逐字节一致 |

---

## 风险与预案

| 风险 | 症状 | 预案 |
|---|---|---|
| `prevent_close` 漏了 | 隐藏后唤起是白屏 | A7 能提前发现；回看 Task 1 Step 7 |
| `tauri` 被插件顶版本 | 编译报错 / 行为变化 | `Cargo.lock` 里确认版本；`cargo test -p fsearch-core` 必须仍全绿 |
| 托盘图标在深色菜单栏上看不清 | 看不见 | `icon_as_template(true)` 应自动适配；A1 目视确认 |
| 快捷键被别的软件占 | 按了没反应 | 托盘/设置里显示错误（A1 之外额外确认错误提示可见） |
| 空闲判定不生效 | A9 的 CPU 还是 5.8% | 看 `decide_rescan` 的 `idle` 是不是读到了 `Phase::Idle`；看日志里有没有「自动重扫」 |
| 补扫不发生 | A10 搜不到新文件 | 看 `request_catchup` / `take_catchup` 的配对；`force` 必须在 `idle` 之前判 |

---

## 真机验收清单（v2）

> 上表的 A1–A21 是**计划期**版本，已被实际实现修正过几处（快捷键已从 `⌥⌘Space`
> 改成 `⌥⇧K`、修复轮新增了排除规则 / watcher / 右键菜单等回归项）。
> **逐条可勾选的正式清单见**：
> [`2026-10-22-real-machine-acceptance.md`](2026-10-22-real-machine-acceptance.md)
> （A1–A28，含「本机就能做」与「需换机器」两段划分）
