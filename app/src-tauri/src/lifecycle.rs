//! 应用生命周期 —— **状态机 + 空闲判定 + 唤起序列**。
//!
//! 【这个模块解决什么】
//!   Lodestar 改成「后台常驻」之后，需要一个地方统一回答三个问题：
//!   ① 现在是「待用」还是「空闲」？
//!   ② 什么时候该从「待用」切到「空闲」（= 该暂停自动重扫了）？
//!   ③ 用户唤起时，窗口要怎么弄出来、索引要怎么补上？
//!
//! 【为什么这里没有 `#![allow(dead_code)]`】
//!   本模块是**私有 `mod`**（`lib.rs` 里写的是 `mod lifecycle;`）——
//!   私有模块里的 `pub` 项**照样会被 `dead_code` 判死**。
//!   所以这里的规矩是：**每个 `pub` 项都要有 crate 内的调用点**，
//!   没有调用点的就别写成 `pub`。
//!   （Task 1 时这个模块还是 `pub mod` —— lib crate 里 `pub` 的项不会被判死，
//!     于是「暂时没人调」的 `phase()` / `take_catchup()` / `restore_catchup()`
//!     也没告警。现在这三个方法都接上了真实调用点——
//!     watcher 的对账循环用它判「空闲」与「唤醒补扫」——
//!     模块改回私有 `mod`，让编译器继续盯着真死代码。）
//!   blanket `#[allow(dead_code)]` 会把**以后新写的真死代码**一并盖住
//!   （而「以后记得删」没有任何强制力）—— 所以这里不挂它。

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Mutex, MutexGuard, Once};
use std::time::{Duration, Instant};

use tauri::{AppHandle, Emitter, Manager, Runtime, WebviewWindow};

/// 事件名 —— Rust 发、前端收：「该唤起并聚焦搜索框了」。
///
/// 【为什么要钉死一个字符串】Rust 发事件、前端收事件，两边各写各的就会
/// 「事件发了但没人听」—— 而这**不会报错**，只是唤起后搜索框不聚焦。
pub const AWAKE_EVENT: &str = "app-awake";

/// 事件名 —— 「打开设置面板」（托盘菜单的「设置…」用）。
pub const OPEN_SETTINGS_EVENT: &str = "open-settings";

/// 事件名 —— 「**窗口现在可见吗**」（布尔载荷）。
///
/// 【为什么需要它】前端有个 800ms 的状态轮询（`App.vue` 的 `pollStatus`）。
///   那个轮询在**窗口隐藏时是纯浪费** —— 用户看不见状态栏，
///   但每次轮询都是一次跨进程 IPC（实测每次约 3.4ms CPU，
///   空载时占整个应用 CPU 的**大头**）。
///
///   → Rust 侧把窗口可见性**告诉**前端，前端据此启停轮询。
///
/// 【为什么不用「自己查」】前端当然也能查（`getCurrentWindow().isVisible()`），
///   但那样**只有轮询在跑时才知道** —— 一旦因为「不可见」停了轮询，
///   就再也没人去发现「窗口又可见了」→ **永远醒不过来**。
///   → 可见性的**变化**必须由 Rust 推过来（事件），
///     而「当前是什么状态」由前端查（[`crate::commands::index_status`] 里的 `visible`）。
pub const VISIBILITY_EVENT: &str = "app-visibility";

/// **通知前端「窗口可见性变了」** —— 布尔载荷。
///
/// 【为什么要单独一个函数】发它的地方有三处（收起 / 唤起 / 焦点恢复），
///   事件名和载荷**只能有一个真相** —— 复制粘贴迟早会把其中一处写歪，
///   而写歪了的后果是「前端以为窗口还开着 → 继续白烧 CPU」。
pub fn emit_visibility<R: Runtime>(app: &AppHandle<R>, visible: bool) {
    if let Err(e) = app.emit(VISIBILITY_EVENT, visible) {
        eprintln!("[lodestar] 通知前端「窗口可见性」失败: {e}");
    }
}

/// **窗口现在真的在屏幕上吗** —— 问的是**窗口服务器的真相**，不是我们自己的标志。
///
/// 【为什么不问 `Lifecycle` 里的 `visible` 标志】
///   那个标志只在我们自己调 `hide()` / `wake_window()` 时才更新。
///   而窗口还可能被**系统**收走（`⌘H`）—— 那时我们的标志不知道，
///   窗口却真的不在屏幕上了。
///   → 判断「用户看不看得见」只能问窗口本身。
///
/// 【为什么这个函数重要】前端靠它决定要不要跑那个 800ms 轮询。
///   答错了的后果分两头：
///   - 隐藏时答「可见」→ 轮询照跑 → 后台白烧 CPU（本条要修的就是这个）
///   - 隐藏时答「不可见」但其实可见 → 状态栏不更新 → 用户以为坏了
///
/// 【拿不到窗口时怎么办】**保守地当成「可见」**。
///   - 猜「不可见」→ 前端停掉轮询 → 状态栏停止更新（用户看得见的故障）
///   - 猜「可见」→ 只是多跑一点轮询（性能问题）
///   → 宁可性能出问题，也不要功能出问题。
///   （真实运行时窗口一定存在，这条兜底只在 mock / 异常时生效。）
pub fn window_visible<R: Runtime>(app: &AppHandle<R>) -> bool {
    match app.get_webview_window("main") {
        Some(w) => w.is_visible().unwrap_or(true),
        None => true,
    }
}

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

/// 「前端还不知道窗口可见性」—— 见 [`Lifecycle::acked_visible`]。
const ACK_UNKNOWN: u8 = 0;
/// 前端认为「窗口不可见」。
const ACK_HIDDEN: u8 = 1;
/// 前端认为「窗口可见」。
const ACK_VISIBLE: u8 = 2;

/// 可见性 → ack 值。
fn ack_value(visible: bool) -> u8 {
    if visible { ACK_VISIBLE } else { ACK_HIDDEN }
}


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
///
/// 【差值先 `max(0)` 再 clamp】
///   窗口比屏幕大时（用户把窗口拉得比屏幕还宽）差值是**负的** ——
///   除 2 会得到负偏移，窗口的左边缘就跑到屏幕**外面**去了
///   （屏幕 1000 宽、窗口 2000 宽 → `(1000-2000)/2 = -500`）。
///   → 差值先夹到 0，再把最终坐标夹回屏幕范围内：
///   **装得下 → 正中；装不下 → 贴屏幕的左上角**（绝不出界）。
pub fn centered_on(screen: Rect, window: Rect) -> (i32, i32) {
    let dx = (screen.w as i32 - window.w as i32).max(0) / 2;
    let dy = (screen.h as i32 - window.h as i32).max(0) / 2;
    // 窗口右/下边缘能到的最远位置 —— 装不下时它等于屏幕的左上角。
    let max_x = screen.x + (screen.w as i32 - window.w as i32).max(0);
    let max_y = screen.y + (screen.h as i32 - window.h as i32).max(0);
    (
        (screen.x + dx).clamp(screen.x, max_x),
        (screen.y + dy).clamp(screen.y, max_y),
    )
}

/// **空闲判定**（纯函数 —— 本模块最重要的一段逻辑）。
///
/// | 窗口可见 | 有焦点 | 结论 |
/// |---|---|---|
/// | 否 | 任意 | **空闲**（你把窗口收起来了） |
/// | 是 | 是 | 不空闲（正在用） |
/// | 是 | 否 | 距上次在前台 ≥ 20 秒 → 空闲；否则还不空闲 |
///
/// 【为什么叫 `focused` 不叫 `frontmost`】窗口**有焦点** ⇒ 应用在前台，
///   反过来不成立（原生菜单/对话框打开时窗口失焦，应用仍在前台）。
///   所以这里判的是「窗口有没有焦点」，不是「应用是不是前台」——
///   名字用 `frontmost` 会让人以为判的是后者。
pub fn should_be_idle(visible: bool, focused: bool, since_foreground: Duration) -> bool {
    if !visible {
        return true;
    }
    if focused {
        return false;
    }
    since_foreground >= IDLE_GRACE
}

/// 取锁 —— **中毒了也继续跑**（本模块所有锁都走这里）。
///
/// 【为什么要这样】
///   中毒（poisoning）只说明「某个持锁者 panic 过」，
///   而这里持锁期间只读写 `Copy` 值 —— 数据本身是好的。
///   原来用 `.unwrap()` → 中毒时 panic → **看门狗线程直接死掉、且没有日志**，
///   表现是「空闲判定悄悄失效」，而整个省 CPU 的功能就靠这个看门狗。
///   → 中毒时取回内值（`into_inner`）继续跑，并**只记一次**日志：
///   中毒标志不会被清掉，不限次的话看门狗每秒都会刷一行。
fn lock_unpoisoned<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    match m.lock() {
        Ok(g) => g,
        Err(e) => {
            static POISON_LOGGED: Once = Once::new();
            POISON_LOGGED.call_once(|| {
                eprintln!("[lodestar] 生命周期锁中毒（取回内值继续跑）");
            });
            e.into_inner()
        }
    }
}

/// 应用的生命周期状态 —— **Tauri 托管**（`app.manage(Lifecycle::new())`）。
///
/// 【为什么 `Mutex` + `AtomicBool` 混搭】
///   - `phase` / `last_foreground_at` 各用**自己的** `Mutex` 保护 ——
///     **两者并不保证成对一致**（那是两个独立的锁，不是一把）。
///     这种短暂不一致始终是**良性**的：下一次 `tick()` 或下一个焦点事件
///     就会把它纠正过来（复审逐条推演过竞态，结论是不会产生错误行为）。
///   - `visible` / `focused` / `catchup` 是**单个标志**，用 `AtomicBool` 读起来无锁 ——
///     而看门狗线程每秒都读它们。
pub struct Lifecycle {
    phase: Mutex<Phase>,
    /// 窗口可见吗（点关闭按钮隐藏 → false）。
    visible: AtomicBool,
    /// 窗口有焦点吗。
    ///
    /// 【不等于「应用在前台」】这里只保证**「有焦点 ⇒ 在前台」**；
    ///   反过来不成立 —— 原生菜单 / 对话框打开时窗口会失焦，
    ///   但应用仍在前台（窗口 `orderOut` 时也会触发 `Focused(false)`）。
    focused: AtomicBool,
    /// 最近一次「在前台」的时刻。
    ///
    /// 【为什么是 `Option`】`new()` 写的是 `Some(now)`，`mark_ready()` 也只写
    ///   `Some(..)` → **`None` 分支永不可达**，`tick()` 里的
    ///   `unwrap_or(IDLE_GRACE)` 是个死分支。留着它无害（不改字段类型，
    ///   免得动到构造与测试），但**别把它当成「可能没有」的分支**来推理。
    last_foreground_at: Mutex<Option<Instant>>,
    /// 有人要求「立刻补扫一次」（唤醒时置位）。
    catchup: AtomicBool,
    /// **前端当前的认知**：它认为窗口是可见还是不可见。
    /// 0 = 还不知道（前端还没告诉我们），1 = 不可见，2 = 可见。
    ///
    /// 【为什么要有这个】看门狗要判断「要不要给前端推可见性事件」——
    ///   判据**不能**是「跟上次推的不一样」（那会漏），
    ///   也不能是「每次都推」（那太贵，见下）。
    ///   → 判据是「**实际状态 ≠ 前端的认知**」。
    ///
    /// 【为什么「每次都推」太贵】Tauri v2 的事件不是「发个消息」——
    ///   它是**在 webview 里执行一段 JavaScript**。
    ///   窗口隐藏时 webview 是睡着的 → 每推一次就把它**叫醒一次** →
    ///   实测每次约 **7ms CPU**（采样里能直接看到
    ///   `WebPage::runJavaScriptInFrameInScriptWorld`）。
    ///   → 每秒推一次 = 隐藏态白白多花 **0.7%**。
    ///
    /// 【这个字段怎么活起来】前端每次**真正改变了**对可见性的认知时
    ///   调 `ack_visibility` 告诉 Rust（见 `commands::ack_visibility`）。
    ///   → 稳态下 Rust 与前端认知一致 → **零 IPC**。
    ///   → 事件万一丢了，前端没 ack → Rust 知道「它还不知道」→ 继续推 → **自愈**。
    acked_visible: AtomicU8,
}

impl Lifecycle {
    pub fn new() -> Self {
        Self {
            phase: Mutex::new(Phase::Ready),
            // 启动时**按「窗口会可见」算**（`visible = true`）。
            //
            // 【为什么配置里明明是 `"visible": false`，这里却是 true】
            //   `tauri.conf.json` 的 `"visible": false` 只保证窗口**创建时**是隐藏的
            //   （那是给「开机自启」用的：登录时不该弹窗口）。
            //   但**手动启动**时 `lib.rs` 的 setup 会立刻 `show()` 它 ——
            //   所以「窗口最终可见」才是常态，这里按 true 算。
            //   开机自启那次（带 `--hidden`）由 setup 显式 `set_visible(false)` 纠正。
            //
            // 【为什么这条不能随手改成 false】
            //   `should_be_idle` 里 `!visible` **直接判空闲** →
            //   自动重扫会被暂停（Task 4 的功能）。
            //   手动启动时窗口明明在屏幕上，却按「后台」对待 = 功能坏了。
            visible: AtomicBool::new(true),
            // 聚焦状态要等第一个 Focused 事件 —— 先按「有焦点」算，
            // 免得刚启动的那一瞬间被判成空闲。
            focused: AtomicBool::new(true),
            last_foreground_at: Mutex::new(Some(Instant::now())),
            catchup: AtomicBool::new(false),
            // 【「还不知道」】—— 前端第一次告诉我们之前，看门狗会推一次。
            acked_visible: AtomicU8::new(ACK_UNKNOWN),
        }
    }

    pub fn phase(&self) -> Phase {
        *lock_unpoisoned(&self.phase)
    }

    /// 只切状态，**不动时间戳** —— 看门狗用这个。
    fn set_phase(&self, p: Phase) {
        let mut cur = lock_unpoisoned(&self.phase);
        if *cur != p {
            *cur = p;
        }
    }

    /// 切到「待用」**并**记下「刚刚在前台」。
    pub fn mark_ready(&self) {
        self.set_phase(Phase::Ready);
        *lock_unpoisoned(&self.last_foreground_at) = Some(Instant::now());
    }

    /// 切到「空闲」。
    pub fn mark_idle(&self) {
        self.set_phase(Phase::Idle);
    }

    /// 设置窗口可见性。
    ///
    /// 【注意副作用】`v == true` 时会**顺带 `mark_ready()`** ——
    ///   「窗口又可见了」= 用户又能看到它了 = 该刷新「上次在前台」的时刻。
    ///   这是**隐式**的：想只切阶段而**不**动时间戳的，
    ///   用 `set_phase`（私有）或 `mark_idle`。
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

    /// **前端当前的认知**（它认为窗口可见吗）—— `None` = 前端还没告诉我们。
    pub fn acked_visible(&self) -> Option<bool> {
        match self.acked_visible.load(Ordering::Acquire) {
            ACK_HIDDEN => Some(false),
            ACK_VISIBLE => Some(true),
            _ => None,
        }
    }

    /// **前端告诉我们它现在的认知**。
    pub fn ack_visibility(&self, visible: bool) {
        self.acked_visible
            .store(ack_value(visible), Ordering::Release);
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
        let focused = self.focused.load(Ordering::Acquire);
        let since = lock_unpoisoned(&self.last_foreground_at)
            .map(|t| now.saturating_duration_since(t))
            .unwrap_or(IDLE_GRACE);
        if should_be_idle(visible, focused, since) {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: i32, y: i32, w: u32, h: u32) -> Rect {
        Rect { x, y, w, h }
    }

    #[test]
    fn hidden_window_is_idle_even_when_focused() {
        assert!(should_be_idle(false, true, Duration::from_secs(0)));
        assert!(should_be_idle(false, false, Duration::from_secs(0)));
    }

    #[test]
    fn focused_visible_window_is_not_idle() {
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
    fn centering_clamps_a_window_bigger_than_the_screen() {
        // 【为什么要有这条】上一条只覆盖了「窗口比屏幕小」的情形。
        // 窗口比屏幕大时（用户把窗口拉得比屏幕还宽），
        // 差值是负的 → 除 2 得到负偏移 → 窗口左边缘跑到屏幕**外面**去了。
        let screen = rect(0, 0, 1000, 1000);
        let (x, y) = centered_on(screen, rect(0, 0, 2000, 2000));
        assert_eq!(x, 0, "窗口比屏幕宽时不该被推到屏幕左侧外面");
        assert_eq!(y, 0, "窗口比屏幕高时不该被推到屏幕上方外面");

        // 【为什么要再测一次「屏幕不在原点」】夹取是相对**屏幕的左上角**的 ——
        // 屏幕在第二块显示器上时，窗口该贴的是那块屏的左边，不是 (0,0)。
        let screen = rect(1920, 0, 1000, 1000);
        let (x, y) = centered_on(screen, rect(0, 0, 2000, 2000));
        assert_eq!(x, 1920, "该贴在那块屏的左边，而不是屏幕左边外面");
        assert_eq!(y, 0);
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

    // ---- 窗口可见性：Rust ↔ 前端的那条线 ----

    use tauri::Listener;

    /// 【事件名 + 载荷都要钉死】——
    ///   前端靠这个事件启停那个 800ms 的状态轮询：
    ///   - 事件名写错 → 前端收不到 → 窗口回来后轮询再也不开（状态栏死掉）
    ///   - 载荷写反 → 前端把「可见」当成「隐藏」→ 后台一直白烧 CPU，或反过来
    #[test]
    fn the_visibility_event_carries_the_right_payload() {
        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .expect("构建 mock app");
        let seen: std::sync::Arc<std::sync::Mutex<Vec<bool>>> =
            std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = seen.clone();
        app.listen(VISIBILITY_EVENT, move |e| {
            // 【为什么这么剥】mock runtime 给的载荷是**JSON 文本**（`"true"`），
            //   不是已经反序列化好的 bool —— 这里剥掉可能的引号再比。
            let payload = e.payload().trim_matches('"');
            sink.lock().unwrap().push(payload == "true");
        });
        emit_visibility(&app.handle(), false);
        emit_visibility(&app.handle(), true);
        assert_eq!(
            *seen.lock().unwrap(),
            vec![false, true],
            "可见性事件必须按顺序带上正确的布尔载荷"
        );
    }

    /// 【拿不到窗口时不能报「不可见」】
    ///   报「不可见」→ 前端停掉状态轮询 → 状态栏再也不更新（用户看得见的故障）。
    ///   报「可见」→ 只是多跑一点轮询（性能问题）。
    ///   → 宁可性能出问题，也不要功能出问题。
    #[test]
    fn window_visible_is_conservative_when_the_window_cannot_be_found() {
        // mock app 里**没有**窗口 —— 这正是这条兜底要覆盖的情况。
        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .expect("构建 mock app");
        assert!(
            window_visible(&app.handle()),
            "拿不到窗口时要保守地当成「可见」—— \
             报「不可见」会让状态栏停止更新"
        );
    }

    /// 【看门狗只在「前端还不知道」时推可见性】——
    ///   **不是**每次都推。
    ///
    ///   「每次都推」有致命代价：Tauri v2 的事件 = **在 webview 里执行一段 JS**，
    ///   窗口隐藏时 webview 睡着 → 每推一次把它叫醒一次 → 实测每次约 **7ms CPU**
    ///   → 每秒推一次 = 隐藏态白白多花 **0.7%**。
    ///
    ///   「只在变化时推」又会漏：事件万一丢了（发生在前端 listen 之前），
    ///   Rust 以为「已经说过了」→ 永远不推 → **状态栏冻住**。
    ///
    ///   → 判据是「**实际状态 ≠ 前端的认知**」：
    ///     前端 ack 过 → 不推（零 IPC）；没 ack → 推（自愈）。
    #[test]
    fn the_watchdog_pushes_only_while_the_frontend_has_not_acked() {
        let app = tauri::test::mock_builder()
            .manage(Lifecycle::new())
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .expect("构建 mock app");
        let seen: std::sync::Arc<std::sync::Mutex<Vec<bool>>> =
            std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = seen.clone();
        app.listen(VISIBILITY_EVENT, move |e| {
            let payload = e.payload().trim_matches('"');
            sink.lock().unwrap().push(payload == "true");
        });
        let life = app.state::<Lifecycle>();

        // ① 前端还没 ack → 推（mock app 里没有窗口 → 保守当成「可见」）
        watchdog_round(&app.handle());
        assert_eq!(
            *seen.lock().unwrap(),
            vec![true],
            "前端还没 ack → 必须推一次（否则它永远不知道窗口回来了）"
        );

        // ② 前端 ack 了「可见」→ 不再推（稳态零 IPC）
        life.ack_visibility(true);
        watchdog_round(&app.handle());
        assert_eq!(
            *seen.lock().unwrap(),
            vec![true],
            "前端已 ack → 不该再推（每次推都要叫醒 webview，很贵）"
        );

        // ③ 前端 ack 的是别的值 → 继续推（**自愈**：事件丢过也能补回来）
        life.ack_visibility(false);
        watchdog_round(&app.handle());
        assert_eq!(
            *seen.lock().unwrap(),
            vec![true, true],
            "前端认知与实际不符 → 必须再推一次（这正是「事件丢了」的兜底）"
        );
    }
}

/// **唤起** —— 用户按了快捷键 / 点了托盘图标。
///
/// 【顺序很重要】
///   ① 标记待用（watcher 恢复重扫）
///   ② 窗口显示 + 聚焦
///   ③ 通知前端聚焦搜索框
///   ④ **最后**才要求补扫 —— 补扫是**几百毫秒**的 CPU 活
///      （实测：索引 8119 项时全量扫描约 300 ms / 重扫约 50 ms），
///      跟 webview 的首次绘制抢 CPU 会把窗口拖慢。
///      **用户等的是窗口，不是索引。**
pub fn wake<R: Runtime>(app: &AppHandle<R>) {
    wake_window(app);
    // ③ 通知前端聚焦搜索框 ——
    //   【为什么这一条单独拎出来】`AWAKE_EVENT` 的语义是
    //   「**把键盘焦点交给搜索框**」（用户唤起后要直接打字）。
    //   它**不是**「窗口回来了」的信号 ——
    //   「打开设置」那条路要的是抽屉，不是搜索框（见 [`open_settings`]）。
    if let Err(e) = app.emit(AWAKE_EVENT, ()) {
        eprintln!("[lodestar] 通知前端「已唤起」失败: {e}");
    }
}

/// **唤起窗口** —— 显示 + 聚焦 + 请求补扫，**不发** [`AWAKE_EVENT`]。
///
/// 【为什么要把它从 [`wake`] 里拆出来】
///   `AWAKE_EVENT` 只干一件事：**把焦点交给搜索框**。
///   「打开设置」那条路（托盘菜单的「设置…」）要的是**设置抽屉** ——
///   两件事都做 = 抽屉刚打开，焦点就被抽屉【后面】的搜索框抢走
///   （用户打的字会进到看不见的搜索框里）。
///   → 拆开：「窗口回来」与「搜索框聚焦」分成两件事，各走各的。
///
/// 【为什么这里不发任何事件】
///   调用方（[`open_settings`]）自己发它要的那一个事件 ——
///   **一条路径只发一个事件**，于是「两个事件的先后顺序」这个坑
///   从根上就不存在了（顺序不定 → 焦点被抢）。
pub fn wake_window<R: Runtime>(app: &AppHandle<R>) {
    let Some(win) = app.get_webview_window("main") else {
        return;
    };
    {
        let life = app.state::<Lifecycle>();
        // 【只调 set_visible(true)】它内部会 mark_ready()（刷新「上次在前台」的时刻）
        //   —— 这里再调一次 mark_ready() 就是把同一个时间戳写两遍
        //   （无行为影响，但属于「顺带的隐式副作用」，要去掉重复）。
        life.set_visible(true);
    }
    ensure_on_screen(app, &win);
    let _ = win.show();
    let _ = win.set_focus();
    // 【告诉前端「窗口回来了」】—— 前端要靠它把状态轮询重新开起来。
    //   （隐藏时前端把轮询停了；不通知它，它就永远不知道该醒过来。）
    //
    //   【为什么放在 show 之后】前端收到就该认为「窗口可见」——
    //   放前面的话它可能先恢复轮询、再看到窗口还没显示。
    emit_visibility(app, true);
    // 补扫：唤醒后索引要是新的（把空闲期间攒下的变化收进来）。
    app.state::<Lifecycle>().request_catchup();
}

/// **收起** —— 窗口隐藏到后台，进程继续活着。
pub fn hide<R: Runtime>(app: &AppHandle<R>, win: &WebviewWindow<R>) {
    let _ = win.hide();
    let life = app.state::<Lifecycle>();
    life.set_visible(false);
    life.mark_idle();
    // 【告诉前端「窗口没了」】—— 前端据此**停掉**那个 800ms 状态轮询。
    //   窗口看不见的时候用户也看不见状态栏，那个轮询是纯浪费
    //   （实测每次约 3.4ms CPU，1.25 次/秒 → 后台常驻时它就是 CPU 的大头）。
    emit_visibility(app, false);
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
///
/// 【为什么只发【一个】事件】
///   以前这里是 `wake(app)` + `emit(OPEN_SETTINGS_EVENT)` ——
///   而 `wake` 自己还会发 [`AWAKE_EVENT`]（前端拿它去聚焦搜索框）。
///   于是这条路径上有**两个**事件，而它们的到达顺序**不保证**：
///   「聚焦搜索框」落在「打开抽屉」之后 → 抽屉刚打开就被
///   **抽屉后面**的搜索框抢走焦点（用户打的字进了看不见的搜索框）。
///
/// 【现在的做法：先发「打开设置」，再唤起窗口】
///   ① 先 `emit(OPEN_SETTINGS_EVENT)` —— 前端这时把设置抽屉打开；
///   ② 再 `wake_window(app)` —— **只**把窗口叫回来，**不发** `AWAKE_EVENT`
///      （见 [`wake_window`] 的说明：那条事件只管「聚焦搜索框」）。
///   → 这条路径上**只有一个事件**，「顺序」这个坑从根上不存在了。
///
///   （前端还有一道兜底：抽屉开着的时候，「已唤起」事件**不**去聚焦搜索框 ——
///    两条路径都指不到「抽屉被抢焦点」这件事。）
pub fn open_settings<R: Runtime>(app: &AppHandle<R>) {
    if let Err(e) = app.emit(OPEN_SETTINGS_EVENT, ()) {
        eprintln!("[lodestar] 通知前端打开设置失败: {e}");
    }
    wake_window(app);
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
    // 【没有主屏就用第一块屏】`monitors` 上面已经判过非空
    //   （`is_empty` 就 return 了）→ `monitors[0]` 一定存在，
    //   所以**不用**再写一条走不到的兜底臂。
    let target = match app.primary_monitor().ok().flatten() {
        Some(m) => work_area_rect(&m),
        None => work_area_rect(&monitors[0]),
    };
    let (x, y) = centered_on(target, window_rect);
    if let Err(e) = win.set_position(tauri::PhysicalPosition::new(x, y)) {
        eprintln!("[lodestar] 把窗口拉回屏幕失败: {e}");
    } else {
        crate::logging::log_line(&format!(
            "[lifecycle] 窗口原位置 ({}, {}) 不在任何屏幕上 → 拉回 ({}, {})",
            window_rect.x, window_rect.y, x, y
        ));
    }
}

/// 屏幕的**可见区**（物理像素）—— 摆窗口要用这个。
///
/// 【为什么不是整块屏幕】
///   macOS 上 `Monitor::size()` 是**整块屏幕**（含菜单栏与 Dock 占的那几条），
///   而 `Monitor::work_area()` 才是**没有被遮住的那块可见区域**。
///   摆在整屏正中 → 窗口顶部会钻到菜单栏底下 —— 正好和
///   「【为什么摆正中而不是左上角】左上角容易被菜单栏 / Dock 挡住」
///   这句注释的意图**相反**。
///   → 居中用 `work_area()`；「有没有完全跑到屏幕外」的判定用整屏（那是另一回事）。
fn work_area_rect(m: &tauri::Monitor) -> Rect {
    let wa = m.work_area();
    Rect {
        x: wa.position.x,
        y: wa.position.y,
        w: wa.size.width,
        h: wa.size.height,
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
        watchdog_round(&app);
    });
}

/// 看门狗的**一轮** —— 重新判空闲 + 把当前可见性推给前端。
///
/// 【抽出来是为了能测】`spawn_idle_watchdog` 是个**死循环线程**，
///   没法在单测里跑；抽成这个函数就能直接调、直接断言。
///
/// 【为什么「每次都推」而不是「变了才推」】
///   「变了才推」有个**致命**的漏洞：**事件投递是单点的**。
///   如果某一次 `app-visibility` 恰好发生在前端 `listen` 注册**之前**
///   （启动期那几毫秒的窗口），前端就停在「停轮询」；
///   而 Rust 侧认为「可见性没变过」→ **永不补发**
///   → → 窗口明明可见，**状态栏永远不更新**。
///
///   → 所以这里**每次都推**（每秒一次），让「丢事件」最多 1 秒就自愈。
///   前端那边**自己去重**（只在值真的变了时才启停轮询）——
///   见 `app/src/App.vue` 的 `applyVisibility`。
///
/// 【成本】每秒一次 `is_visible()`（跨进程问一次窗口服务器）
///   + 一次 bool 载荷的事件 —— 相对每秒一次全量重扫可以忽略
///   （实测隐藏态 4 个进程合计仍只有 0.03 ~ 0.08 %）。
pub fn watchdog_round<R: Runtime>(app: &AppHandle<R>) {
    // 【先取 state 再用】—— `app.state::<T>()` 返回的 State 是个 guard，
    // 直接 `app.state::<Lifecycle>().tick()` 会在临时值上借用 → E0716。
    let life = app.state::<Lifecycle>();
    life.tick();
    // 【把当前可见性推给前端 —— 但【只在「前端还不知道」时】】
    //   判据是「**实际状态 ≠ 前端的认知**」，不是「跟上次推的不一样」。
    //
    //   【为什么不每次都推】Tauri v2 的事件不是「发个消息」——
    //   它是**在 webview 里执行一段 JavaScript**。
    //   窗口隐藏时 webview 是睡着的 → 每推一次就把它**叫醒一次** →
    //   实测每次约 **7ms CPU**（采样里能直接看到
    //   `WebPage::runJavaScriptInFrameInScriptWorld`）。
    //   → 每秒推一次 = 隐藏态白白多花 **0.7%**。
    //
    //   【为什么这样就不会「永远推」】前端每次真正改变认知时会调
    //   `ack_visibility`（见 `commands::ack_visibility`）→ 认知一致 → 不推了。
    //
    //   【为什么这样就不会「永远不推」】事件万一丢了，前端没 ack →
    //   Rust 知道「它还不知道」→ 继续推 → **自愈**（≤1 秒）。
    let actual = window_visible(app);
    if life.acked_visible() != Some(actual) {
        emit_visibility(app, actual);
    }
}
