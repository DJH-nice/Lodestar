//! 文件变化监控 —— **文件一变，索引自动跟上**。
//!
//! 【这一层要解决什么】
//!   以前：新建/保存一个文件 → 搜它 → **搜不到**（索引是旧的）→
//!   用户得自己想起来「哦该刷新了」→ 点「刷新」→ **干等 2 秒**（`$HOME` 量级）。
//!   现在：文件一落地 → 索引自动跟上 → 结果表自动刷新。
//!
//! 【整体结构】
//! ```text
//!   FSEvents ──► notify 回调（notify 自己的线程）
//!                 └─ 只做两件事：
//!                    ① 把「**哪些路径**变了」放进脏路径集合
//!                    ② 置「脏了」+ 记时间戳（**一次**原子写）
//!                       【不碰引擎、不碰 IndexState、不拿别的锁】
//!                        ▼
//!   对账线程（每 200ms 一轮）
//!     ├─ 读 IndexState::paths()（= 当前活着的引擎索引了哪些目录）
//!     │    与「已监听的列表」对比 → 不一致就重配（先取消旧的、再加新的）
//!     └─ 「有变化待处理」且**六道闸**都过了 → 触发一次**增量更新**
//!            └─ do_apply_changes → 引擎只碰「真的变了」的那几个路径
//!                  └─ 提交状态 + emit("index-auto-refreshed") → 前端重搜
//! ```
//!
//! 【M3：从「全量重扫」改成「增量更新」】
//!   以前收到文件变化 → **把整棵子树重新扫一遍**（`$HOME` 量级 300ms+），
//!   而且会把排在后面的搜索全堵住（搜索与重扫共用同一个 worker 线程）。
//!   现在收到文件变化 → **只碰真的变了的那几个路径**（实测 0.05ms）。
//!
//!   **为什么能做到**：FSEvents 给的是**每个文件**的事件
//!   （不是「某个目录变了」这件事），所以单个文件的变化可以精确到那一条 ——
//!   不用碰它的任何邻居。实测见 `search-core/src/incremental.rs`
//!   与 `search-core/examples/bench.rs` 的第 6 节。
//!
//! 【兜底：FSEvents 说「有事件被丢掉了」】
//!   FSEvents 的事件队列满了会**丢事件**（这时它发一个
//!   「必须重扫子目录」的信号，notify 翻译成 `Flag::Rescan` 事件）。
//!   增量更新**依赖事件不丢** —— 丢了索引就永远对不上
//!   → 收到这个信号就老老实实**全量重扫**一次。
//!
//! 【六道闸 —— 按**真实求值顺序**】（纯函数 → 可测）
//!   ① **扫描在飞** → 这一轮跳过，**不清脏标志**（不丢事件）
//!   ② **强制补扫** → 唤醒时立刻扫，**压过空闲**（见下面「为什么 force 要赢」）
//!   ③ **应用空闲** → 这一轮不重扫，但**保留脏标志**（空闲期间的变化不丢）
//!   ④ **已被覆盖** → 这次变化已经被某趟**已完成**的扫描收进索引了 → 不用再扫
//!   ⑤ **静默期** 200ms → 把一次保存的多个事件合并成**一次**更新
//!   ⑥ **最小间隔** 400ms → 防「持续小改动」把引擎刷爆
//!
//!   ①②③ 在 [`decide_rescan`] 里，④⑤⑥ 在 [`decide_auto_rescan`] 里；
//!   另有「没变化 / 开关关着」两个出口也在 [`decide_auto_rescan`] 里。
//!
//!   【为什么这三道时间闸在 M3 之后变短了】
//!   它们当初是为了**省 CPU**（全量重扫 300ms+，宁可等久一点）。
//!   现在更新是**毫秒级**的，它们的作用变成了「**别把前端刷爆**」
//!   （每次更新都要给前端发一个事件，`app.emit` ≈ 7ms CPU）——
//!   于是「改一个文件 → 1 秒内可见」成为可能（实测端到端 407ms）。
//!
//! 【为什么「强制补扫」要压过「应用空闲」】
//!   watcher 读「空闲」和读「补扫请求」是**两个不同时刻**（两次独立的读），
//!   完全可能读到「**陈旧的 `idle = true`**」配「**新鲜的 `force = true`**」。
//!   这时如果空闲优先，补扫就被挡掉、要等到下一个文件事件才发生 ——
//!   用户唤醒了却看不到新结果。
//!   `force` 优先 = 「用户刚叫醒我，立刻补扫」永远压过「我以为你在空闲」。
//!
//! 【为什么用 `notify` crate 而不是裸 FSEvents FFI】—— 见简报 §2.2。
//!   `notify` 在 macOS 上的后端就是 `fsevent-sys`（一层薄封装），
//!   但它替我们处理掉了「流必须保活 / 回调在哪个队列跑 / 停流时的竞态」
//!   —— 正是容易出微妙 bug 的地方。

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use search_core::Searcher;
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use tauri::{AppHandle, Emitter, Manager, Runtime};

use crate::index_state::IndexState;

/// 【静默期】事件发生后 **200ms** 内没有新事件，才触发更新。
///
/// 【为什么从 800ms 改成 200ms】
///   原来是 800ms，因为一次更新要**重扫整棵树**（`$HOME` 量级 300ms+）——
///   扫早一点就是白烧 CPU，所以宁可等久一点。
///   现在更新是**增量**的（实测毫秒级，见 `search-core/src/incremental.rs`）——
///   「等久一点」换来的不再是省 CPU，而是**用户等更久才看到结果**。
///   → 200ms 足够把「一次保存」的那串事件（建临时文件 → 写内容 → rename，
///   它们之间只隔几毫秒）合成**一次**更新，又不至于让界面慢半拍。
///
/// 【验收口径】「改一个文件 → 索引可见 ≤ 1 秒」：
///   静默期 200ms + 最小间隔 400ms + 对账周期 200ms ≈ 800ms ✓
pub const QUIET_PERIOD: Duration = Duration::from_millis(200);

/// 【最小间隔】两次**自动更新的开始**之间至少 **400ms**。
///
/// 【为什么从 5s 改成 400ms】
///   原来是 5s，因为一次更新是**全量重扫**（`$HOME` 量级 300ms+）——
///   「持续小改动」（编辑器的自动保存、`git checkout`、下载文件夹）
///   会把引擎一直刷爆，所以宁可 5 秒才更新一次。
///   现在更新是**增量**的（毫秒级）→ 这道闸的作用从「省 CPU」
///   变成了「**别把前端刷爆**」：
///   每次更新都要给前端发一个事件（`app.emit` ≈ 7ms CPU，见 AGENTS.md 硬约束 5），
///   所以还是要有间隔，但 400ms 足够跟得上「1 秒内可见」的要求。
///
/// 【这道闸量的是「开始 → 开始」】
///   计时器记的是**开始**时刻（不是结束时刻），
///   所以两次自动更新的**开始**之间至少隔 400ms —— 不是「400ms 再加一次耗时」。
///   （这句以前写错了，见复审 Minor-4。）
///
/// 【不要拿全局的「最近一次已完成的扫描」来代替它】——
///   那是**另一个语义**（覆盖判断用，见 [`COVER_GRACE`]），两者别混。
pub const MIN_INTERVAL: Duration = Duration::from_millis(400);

/// 【对账周期】watcher 每隔多久读一次「当前活着的引擎索引了哪些目录」。
///
/// 【为什么从 1s 改成 200ms】
///   这个循环同时是**更新的触发器**（每轮判一次那几道闸）。
///   周期 1s 时，「改文件 → 索引可见」的下限就是 1s ——
///
///   跟「1 秒内可见」的要求正面冲突。
///   → 200ms 让整条链路的预算变成
///   「静默期 200ms + 最小间隔 400ms + 对账周期 200ms ≈ 800ms」✓
///   换索引目录（reindex）后要尽快开始监听新目录 —— 200ms 更快 ✓
///   而每轮读一次状态几乎不要钱（一个锁 + 一次小比较）。
pub const RECONCILE_INTERVAL: Duration = Duration::from_millis(200);

/// 【覆盖宽限】扫描完成之后，这段时间内被观察到的事件**也算「已被覆盖」**。
///
/// 【为什么要有这段宽限】
///   我们知道的是「事件被**观察到**的时刻」，不是「文件**改动**的时刻」——
///   FSEvents 投递事件有延迟（实测几十毫秒，机器负载高时更多）。
///   文件可能是在扫描**进行中**改的，而事件是扫描**结束之后**才被观察到的。
///   不留这段宽限，就会把「已经被覆盖的变化」误判成「没被覆盖」
///   → **用户点一次「刷新」，引擎还是被连扫两遍**（复审员实测的那个场景：
///   点完刷新 0.3 秒后外部写一个文件 → watcher 又扫了一整遍）。
///
/// 【代价 —— 同一个取舍】
///   扫描刚完成后的这 500ms 里到来的变化会被当成「已覆盖」
///   → 那一次变化可能没进索引，要等**下一个**事件才补上。
///   这与「宁可漏一次，也不要重复扫一整遍」是**同一个取舍**
///   （详见 [`decide_auto_rescan`] 里「覆盖」那一段的说明）。
pub const COVER_GRACE: Duration = Duration::from_millis(500);

/// 自动重扫完成后发给前端的事件名 —— 前端靠它重搜一次 + 更新状态栏。
///
/// 【事件名要钉死】Rust 发、前端收，两边各写各的就会「事件发了但没人听」——
///   而这**不会报错**，只是结果表永远不刷新。
///   Rust 侧有一道测试断言这个名字**真的被发出去**
///   （见本文件末尾 `auto_rescan_really_emits_the_auto_refresh_event`）。
pub const AUTO_REFRESH_EVENT: &str = "index-auto-refreshed";

/// 这一轮要不要触发自动重扫 —— **决策结果**。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RescanDecision {
    /// 还没到时候 → **保留脏标志**，下一轮重新判断（不丢事件）。
    Wait,
    /// 这批变化已经被某趟**已完成**的扫描覆盖 → 清脏标志、不用扫。
    Covered,
    /// 开关关着 → 清脏标志、不用扫。
    Disabled,
    /// 【新增】应用空闲 → 不重扫，但**保留脏标志**。
    ///
    /// 【为什么不清脏标志】脏标志是「有变化等着被收进索引」的记号。
    ///   空闲时不清 → 唤醒后那一轮自然会重扫，空闲期间的变化不会丢。
    ///   （跟闸一「扫描在飞」的处理方式一样：不丢事件。）
    Paused,
    /// 现在就扫。
    Rescan,
}

/// 自动重扫的决策输入 —— **全部显式传进来**（好测、好读）。
#[derive(Debug, Clone, Copy)]
pub struct RescanInputs {
    /// 有没有待处理的文件变化（事件回调置的）。
    pub dirty: bool,
    /// 最近一次事件的时刻。`None` = 还没发生过事件。
    pub last_event_at: Option<Instant>,
    /// 当前时刻。
    pub now: Instant,
    /// watcher **自己**的计时器：最近一次**自动**重扫的**开始**时刻
    /// （「最小间隔」那道闸只有它管）。
    pub last_auto_scan_at: Option<Instant>,
    /// **全局**：最近一次**已完成**的扫描（手动刷新 / 自动重扫 / 启动建索引）
    /// 的**完成**时刻。`None` = 还没成功扫过。
    pub last_scan_completed_at: Option<Instant>,
    /// 有没有扫描在飞。
    pub scan_in_flight: bool,
}

/// 「这一轮要不要触发一次自动重扫？」—— **纯逻辑**（好测）。
///
/// 【为什么抽成纯函数】
///   去抖的判断散在回调/线程里的话，就没法**确定性地**测：
///   「等 800ms 再看看」这种测试要么靠 sleep 撞时序（会 flaky），
///   要么干脆没测。抽成纯函数后，把「时刻」当参数传进去，
///   每个边界都能钉死。
///
/// 【四道闸】按顺序：
///   ① **扫描在飞** → [`RescanDecision::Wait`]
///      —— 这一轮跳过，而且**不清脏标志**（不清 = 下一轮还会重新判断，不丢事件）。
///      为什么不能无条件清：那趟在飞的扫描**开始时**可能还没发生这次变化
///      → 清了就可能永远扫不到。
///   ② **已被覆盖** → [`RescanDecision::Covered`]
///      —— 「最近一次文件变化」不晚于「最近一次**已完成**的扫描」
///      → 那次变化多半已经被收进索引了 → 不用再扫一遍。
///      **这一条就是「用户点一次刷新，引擎却被连扫两遍」的解药。**
///   ③ **静默期**（[`QUIET_PERIOD`]）：事件之后要安静够 800ms
///      —— 把一次保存的多个事件合并成**一次**重扫。
///   ④ **最小间隔**（[`MIN_INTERVAL`]）：距上次**自动**重扫的开始至少 5s
///      —— 防「持续小改动」把引擎刷爆。
///
/// 【开关】放在最后 —— 只在**真的要扫了**的时候才读设置（读设置要开一次文件）。
///   `auto_refresh` 是个**闭包**而不是一个 `bool`：
///   这样「什么时候读设置」这件事也归这个函数管，测试不用去碰真的配置文件。
///
/// 【覆盖这一条的边界 —— 不精确，但是接受的】
///   一趟扫描从开始到结束会花约 300 毫秒（`$HOME` 量级，实测 8119 项），
///   它枚举到某个目录的时刻**可能早于**那次变化落地的时刻
///   → **这趟扫描可能漏掉那个文件**。
///   → 这个不精确**是接受的**（要做到精确就得逐文件追踪，那是另一个量级的改动）。
///   → **宁可漏一次（下一轮还有别的事件会触发）也不要重复扫一整遍。**
/// 「这次变化是不是已经被某趟**已完成**的扫描覆盖了」—— **纯函数**。
///
/// 【判据】
///   「已覆盖」= 有一趟扫描在**这个变化被观察到之后**才完成
///   —— 它多半已经把这个文件收进索引了（重扫是全量的）。
///
/// 【为什么要有 [`COVER_GRACE`] 那 500ms 宽限】
///   我们知道的是「事件被**观察到**的时刻」，不是「文件**改动**的时刻」——
///   FSEvents 投递事件有延迟（实测几十毫秒，机器负载高时更多）。
///   文件可能是在扫描**进行中**改的，而事件是扫描**结束之后**才被观察到的。
///   不留这段宽限，就会把「已经被覆盖的变化」误判成「没被覆盖」
///   → **用户点一次「刷新」，引擎还是被连扫两遍**（复审员实测的那个场景：
///   点完刷新 0.3 秒后外部写一个文件 → watcher 又扫了一整遍）。
///
/// 【为什么要抽成独立函数】
///   **两道闸要用同一个判据**：
///     · [`decide_auto_rescan`] 的闸二（自动重扫）
///     · [`decide_rescan_inner`] 的「强制补扫」分支（唤醒补扫）
///   以前唤醒补扫**绕过了**这道闸（它直接 `return Rescan`），
///   → 「用户点一次刷新，紧接着唤醒」= **两趟**全量重扫（第一趟白扫）。
///   → 现在两道闸共用这一个函数 → 判据只有**一份**，不会漂移。
///
/// 【边界不精确，但这是接受的取舍】
///   一趟扫描从开始到结束要约 300 毫秒（`$HOME` 量级，实测 8119 项），
///   它枚举到某个目录的时刻**可能早于**那次变化落地的时刻
///   → **这趟扫描可能漏掉那个文件**。
///   → 宁可漏一次（下一轮还有别的事件会触发）也不要重复扫一整遍。
fn is_covered_by_completed_scan(inp: &RescanInputs) -> bool {
    let Some(event_at) = inp.last_event_at else {
        // 没事件 → 谈不上「被覆盖」
        return false;
    };
    let Some(done_at) = inp.last_scan_completed_at else {
        // 还没成功扫过 → 没有任何一趟扫描能覆盖它
        return false;
    };
    let covered_until = done_at
        .checked_add(COVER_GRACE)
        .unwrap_or(done_at);
    event_at <= covered_until
}

pub fn decide_auto_rescan<F: FnOnce() -> bool>(
    inp: &RescanInputs,
    auto_refresh: F,
) -> RescanDecision {
    // 没脏 → 没什么可做的
    if !inp.dirty {
        return RescanDecision::Wait;
    }
    // 闸一：有扫描在飞 → 这一轮跳过，**不清脏**（不丢事件）
    if inp.scan_in_flight {
        return RescanDecision::Wait;
    }
    // 有脏标志却没有事件时刻 → 判断不了静默期 → **不扫**
    // （宁可晚一点，也不要在没有依据的时候烧 2 秒 CPU）
    let Some(event_at) = inp.last_event_at else {
        return RescanDecision::Wait;
    };
    // 闸二：【覆盖】这次变化是不是已经被某趟【已完成的】扫描覆盖了？
    //
    //   【为什么必须有这一闸】用户点一次「刷新」= 一趟全量重扫。
    //   它完成之后，那次变化就已经在索引里了 —— watcher 不该为**同一批**变化
    //   再扫一整遍（那是复审 Important-1 实测到的「一次刷新 = 两次全量重扫」）。
    //
    //   【判据见 [`is_covered_by_completed_scan`]】
    //   —— 唤醒时的「强制补扫」分支也要用它（见 [`decide_rescan_inner`]）。
    if is_covered_by_completed_scan(inp) {
        return RescanDecision::Covered;
    }
    // 闸三：静默期 —— 事件之后要安静够 QUIET_PERIOD
    // 【saturating_duration_since】两个 Instant 相减在 `now < event_at`
    //   时会下溢 —— saturating 版本给出 0（当成「刚刚发生」→ 不扫）✓
    if inp.now.saturating_duration_since(event_at) < QUIET_PERIOD {
        return RescanDecision::Wait;
    }
    // 闸四：最小间隔 —— 距上次**自动**重扫的开始至少 MIN_INTERVAL
    if let Some(scan_at) = inp.last_auto_scan_at {
        if inp.now.saturating_duration_since(scan_at) < MIN_INTERVAL {
            return RescanDecision::Wait;
        }
    }
    // 闸五：开关 ——「文件变化时自动刷新索引」关掉 → 不触发重扫。
    //
    //   【为什么关掉的是【触发】而不是【监听】】
    //   监听本身很便宜（FSEvents 流是内核里的），而重扫很贵（2 秒 CPU）。
    //   所以关掉之后仍然监听，只是不重扫 —— 用户重新打开开关时
    //   下一个文件事件就能立刻生效，不用重启应用。
    if !auto_refresh() {
        return RescanDecision::Disabled;
    }
    RescanDecision::Rescan
}

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
///   watcher 读「空闲」和读「补扫请求」是**两个不同时刻**（两次独立的读），
///   完全可能读到「**陈旧的 `idle = true`**」配「**新鲜的 `force = true`**」。
///   （`lifecycle::wake` 里的顺序是「先 `mark_ready()`、后 `request_catchup()`」——
///     但那**不是**关键：关键的是 watcher 那两次读**不在同一时刻**，
///     两次读之间状态可以变。）
///   这时如果空闲优先，补扫就被挡掉、要等到下一个文件事件才发生 ——
///   用户唤醒了却看不到新结果。
///   `force` 优先 = 「用户刚叫醒我，立刻补扫」永远压过「我以为你在空闲」。
///
/// 【`auto_refresh` 开关两道新闸都尊重】
///   用户关了自动刷新 → 不该有任何自动扫描，唤醒时的补扫也算。
pub fn decide_rescan<F: FnOnce() -> bool>(
    inp: &RescanInputs,
    idle: bool,
    force: bool,
    auto_refresh: F,
) -> RescanOutcome {
    // 【返回的是【决策 + 决策时用的那个 force】】——
    //   合成一个 struct 是为了**让「传错」变成编译错误**（终审 M-6）：
    //   以前 `decide_rescan()` 返回 `RescanDecision`，
    //   `rescan_action(decision, force)` 又**单独**收一个 `force` ——
    //   两个都是 bool，调用点把「决策用的 force」和「动作用的 force」
    //   写成两个不同的值，**编译器一个字都不会说**，
    //   而 50 个用例照样全绿（后果：丢掉一次唤醒补扫）。
    //   → 现在 force 只从 outcome 里取 → 没有第二个来源可写错。
    let decision = decide_rescan_inner(inp, idle, force, auto_refresh);
    RescanOutcome { decision, force }
}

/// 决策本身（**纯函数**）—— 真正的判断逻辑在这里。
///
/// 【为什么要再抽一层】
///   `decide_rescan` 现在负责「把 force 装进 outcome」，
///   判断逻辑放这里 → 两件事各自干净，
///   而且测试可以直接对 `RescanOutcome.decision` 断言。
fn decide_rescan_inner<F: FnOnce() -> bool>(
    inp: &RescanInputs,
    idle: bool,
    force: bool,
    auto_refresh: F,
) -> RescanDecision {
    // ① 有扫描在飞 → 等（不清脏标志）
    //
    // 【这不是冗余，别当重复代码清掉】
    //   `decide_auto_rescan` 里面也有一道同样的「扫描在飞」——
    //   那是**独立于**它的一份：新的两道闸（强制补扫 / 空闲）必须尊重
    //   「有扫描在飞」这件事，而 `decide_auto_rescan` 在这道闸**之后**
    //   才轮得到被调用。
    //   （以后改 `decide_auto_rescan` 时，**别把这里当成重复删掉**。）
    if inp.scan_in_flight {
        return RescanDecision::Wait;
    }
    // ② 强制补扫（唤醒时的「补扫」）→ 立刻扫
    //
    //   【但也要尊重「覆盖」闸】—— 这是本次改动的核心。
    //
    //   【以前这里是无条件 `return Rescan`】—— 于是每次唤醒都会跑一趟
    //   **全量**重扫（$HOME 量级实测 1.7 秒 CPU / 2~3 秒墙钟），
    //   哪怕文件系统一个字节都没变。
    //   唤醒瞬间实测峰值 28.7%（主进程独占），而隐藏态只有 0.11%。
    //
    //   【为什么「补扫」不需要无条件全量】
    //   `catchup` 的语义是「把**空闲期间攒下的变化**收进索引」。
    //   而空闲期间的变化 **watcher 一直在收**（它常驻、监听的就是
    //   引擎索引的那批目录 —— 见 `desired_watch_dirs`），
    //   → 它们已经被记在 dirty 标志 + 事件时刻里了。
    //   → 如果这些变化**已经被某趟已完成的扫描覆盖**（用户刚点过「刷新」，
    //   或者 idle 期间刚好有一趟扫描跑完），那它们**已经在索引里**了
    //   → 再扫一遍是**纯浪费**。
    //
    //   【这一闸挡掉的正是两种真实浪费】
    //     · 「用户点一次刷新 → 紧接着唤醒」= 两趟全量重扫（第一趟白扫）
    //       —— 这正是 `decide_auto_rescan` 闸二当初要解决的那个问题，
    //       只是当时只修了自动重扫那条路，**唤醒这条路漏了**。
    //     · 「文件一直没变 → 唤醒」= 一次白扫的全量重扫。
    //
    //   【判断与自动重扫共用同一个判据】见 [`is_covered_by_completed_scan`]。
    //   → 判据只有一份，不会两处漂移。
    if force {
        if !auto_refresh() {
            return RescanDecision::Disabled;
        }
        // 【没脏 → 没什么要补的】—— 这是**最常见**的唤醒场景：
        //   文件系统一个字节都没变（没脏 = 没有任何待处理的变化）。
        //   → 补扫的语义是「收进 idle 期攒下的变化」；
        //     没有变化可收 → **扫了也是白扫**（全量遍历 $HOME 一遍，
        //     结果和现在的索引一模一样）。
        //
        //   【为什么这不会漏掉变化】
        //   「脏」不是「文件变了」的猜测，而是 watcher **真的收到了
        //   FSEvents 事件**才置上的（见 notify 回调：置脏 + 记时刻）。
        //   watcher 常驻、监听的就是引擎索引的那批目录 → idle 期的变化
        //   一定被记下来了 → 没脏 = **确实没有变化**，而不是「没观察到」。
        //
        //   【与「覆盖」闸的区别】
        //     · 「没脏」= 压根没有变化 → 不用扫
        //     · 「已覆盖」= 有变化，但已经在索引里了 → 不用扫
        //   两者都返回 Covered（都要清脏标志 —— 变化确实已在索引中）。
        if !inp.dirty {
            return RescanDecision::Covered;
        }
        // 变化已被覆盖 → 不用扫（清脏标志：那些变化确实已经在索引里）
        if is_covered_by_completed_scan(inp) {
            return RescanDecision::Covered;
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

/// 这一轮重扫的**动作** —— 决策（[`RescanDecision`]）之后的副作用。
///
/// 【为什么要抽出来】
///   [`decide_rescan`] 回答「**要不要扫**」，
///   但「扫之前要不要清脏标志」「没扫成要不要把补扫请求放回去」
///   这两个副作用原来只活在 `reconcile_loop` 的 match 分支里 ——
///   **删掉其中任何一行，所有测试仍然全绿**，
///   而丢的是**文件变化**（索引静默过期）。
///   → 抽成纯函数后，它们就是**可断言的返回值**。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RescanAction {
    /// 要不要执行这次重扫。
    pub rescan: bool,
    /// 要不要清「脏标志」。
    pub clear_dirty: bool,
    /// 要不要把没消费掉的补扫请求**放回去**（下轮再试）。
    pub restore_catchup: bool,
}

/// **决策结果 + 决策时用的「强制补扫」标志** —— 合成一个 struct。
///
/// 【为什么要合成】（终审 M-6）
///   `decide_rescan()` 返回决策、`rescan_action()` 又单独收一个 `force` ——
///   两个都是 `bool`，调用点写错**编译器不会报错**，
///   而所有单测照样全绿（把 `rescan_action(decision, force)` 改成
///   `rescan_action(decision, false)`，50 个用例照样绿 —— 后果是
///   「唤醒时的补扫被丢掉」，而脏标志不丢、下一个文件事件会补上）。
///   → 合成一个 struct 之后，`force` **只有一个来源**
///     （决策时那个），「传错」在**编译期**就不可能。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RescanOutcome {
    /// 决策结果。
    pub decision: RescanDecision,
    /// 「强制补扫」—— 就是**决策时用的那个**。
    pub force: bool,
}

/// 决策 → 动作（纯函数，好测）。
///
/// 【这张表就是契约，必须逐格钉住】
/// | decision   | force | rescan | clear_dirty | restore_catchup |
/// |------------|-------|--------|-------------|-----------------|
/// | `Rescan`   | 任意  | true   | **true**    | false           |
/// | `Wait`     | true  | false  | false       | **true**        |
/// | `Wait`     | false | false  | false       | false           |
/// | `Covered`  | 任意  | false  | true        | false           |
/// | `Disabled` | 任意  | false  | true        | false           |
/// | `Paused`   | 任意  | false  | **false**   | false           |
///
/// 【为什么 `Rescan` 也 clear_dirty】
///   现有代码在重扫**之前**就 `signals.take_dirty()`
///   （见 `reconcile_loop` 里那段「清脏 + 记时刻 —— 必须在做重扫【之前】」的注释：
///   重扫要几百毫秒（实测 8119 项时约 50 ms），这期间可能又来了新事件；
///   先清脏 → 那期间的新事件会**重新置脏** → 下一轮会再扫一次（不丢事件））。
///   这个行为一个字都不要改，只是把它**显式化**。
///
/// 【为什么 `Paused` 不 clear_dirty】
///   脏标志是「有变化等着被收进索引」的记号。
///   空闲时不清 → 唤醒后那一轮自然会重扫 → **空闲期间的变化不会丢**。
///
/// 【为什么 `Wait && force` 要 restore_catchup】
///   `take_catchup()` 是「取出并清零」；
///   这轮没扫成（还有扫描在飞）就必须放回去，
///   否则**这次唤醒的补扫被吞掉**（要等到下一个文件事件才发生）。
///
/// 【参数为什么是【一个】outcome 而不是「决策 + force」两个】
///   见 [`RescanOutcome`] 的说明：合成一个之后，
///   「决策用的 force」与「动作用的 force」**不可能**是两个不同的值。
pub fn rescan_action(outcome: RescanOutcome) -> RescanAction {
    let RescanOutcome { decision, force } = outcome;
    match decision {
        RescanDecision::Rescan => RescanAction {
            rescan: true,
            clear_dirty: true,
            restore_catchup: false,
        },
        RescanDecision::Wait => RescanAction {
            rescan: false,
            clear_dirty: false,
            restore_catchup: force,
        },
        RescanDecision::Covered => RescanAction {
            rescan: false,
            clear_dirty: true,
            restore_catchup: false,
        },
        RescanDecision::Disabled => RescanAction {
            rescan: false,
            clear_dirty: true,
            restore_catchup: false,
        },
        RescanDecision::Paused => RescanAction {
            rescan: false,
            clear_dirty: false,
            restore_catchup: false,
        },
    }
}

/// **真的把动作做一遍** —— 返回「该不该执行这次重扫」。
///
/// 【为什么要它，而不是在 `reconcile_loop` 里直接照着动作做】
///   `rescan_action` 只回答「该做什么」；
///   而「决策之后**做了什么**」只有真的执行一遍才看得见。
///   → 这一层让「空闲不清脏」「补扫请求被放回」能被**在真实对象上**断言：
///   `Signals` 的脏标志还在不在、`Lifecycle` 的补扫请求还在不在。
///
/// 【顺序有讲究】
///   先放回补扫请求、再清脏标志 —— 两件事互不影响，
///   但**都必须在重扫之前**做完（重扫由调用方做）。
///
/// 【为什么不是 `pub`】它要收 `Signals`（本模块私有的类型）——
///   写成 `pub` 会触发 `private_interfaces` 告警。
///   而它只在**本模块内**用（`reconcile_loop` + 单元测试）→ 私有就够。
fn apply_rescan_action(
    action: RescanAction,
    signals: &Signals,
    life: &crate::lifecycle::Lifecycle,
) -> bool {
    // 补扫请求没消费掉 → 放回去，下轮再试
    if action.restore_catchup {
        life.restore_catchup();
    }
    // 清脏标志（「脏标志」=「有变化等着被收进索引」的记号）
    if action.clear_dirty {
        signals.take_dirty();
    }
    action.rescan
}

/// macOS 用来缓存「目录视图状态」的那个文件的名字。
///
/// 【它是什么】Finder 每浏览一个目录，就会写一个 `.DS_Store`
/// —— 里面存的是窗口位置、图标排列、排序方式这类**只给 Finder 自己看**的东西。
/// macOS 会**反复重写**它（改窗口大小、换排序、图标刷新……）。
///
/// 【为什么要把它当噪声滤掉】
///   一次实测（`$HOME` 量级）：
///   ```text
///   18:31:55.318 [watch] 收到文件变化：["…/.DS_Store"] (Modify(Metadata(Any)))
///   18:31:56.189 [watch] 自动重扫 #1 完成：7 项，耗时 1.99ms
///   ```
///   → **只是 Finder 看了一眼目录，就换来一次全量重扫**
///   （`$HOME` 量级每次约 50 毫秒 —— 实测 8119 项）。
///   而 `.DS_Store` 的内容对「按文件名找文件」这件事**毫无意义**。
///
/// 【代价（说清楚，别装作没有）】
///   滤掉之后，索引里的 `.DS_Store` 可能**过时**：
///   它新建/删除时不会单独触发重扫，要等**别的**变化顺带把它带进来。
///   搜 `.DS_Store` 大概率搜不到 —— 这可以接受：
///   没有人是靠搜 `.DS_Store` 找文件的。
///
/// 【为什么上游没有这条规则】
///   上游 FSearch 是 Linux/GTK 应用，压根没有 `.DS_Store` 这回事，
///   所以**没有先例可抄** —— 这条是本项目自己定的。
pub const NOISE_FILE_NAME: &str = ".DS_Store";

/// 这个事件是不是「噪声」—— **不该触发重扫**。
///
/// 【判据】**所有**路径都是 `.DS_Store` 才算噪声。
///
/// 【为什么是「所有」而不是「任意」】
///   重命名事件带**两条**路径（从哪来、到哪去）：
///   - `.DS_Store` → `foo`：**`foo` 是个真文件，它出现了** → 这**不是**噪声 ✓
///   - `foo` → `.DS_Store`：真文件变成了 Finder 的缓存 → 这**是**噪声 ✓
///   用「任意一条路径是 `.DS_Store`」会把第一种也滤掉 → **真文件丢了索引**。
///
/// 【为什么是纯函数】好测 —— 边界（重命名、空路径表）都能确定性地钉住。
///
/// 【参数】`notify::Event` —— 只看 `paths` 与 `kind`，不碰别处。
pub fn is_noise_event(ev: &notify::Event) -> bool {
    // 空路径表 → 没有依据说它是噪声 → **不滤**（宁可多扫一次）
    if ev.paths.is_empty() {
        return false;
    }
    ev.paths.iter().all(|p| {
        p.file_name()
            .map(|n| n == NOISE_FILE_NAME)
            .unwrap_or(false)
    })
}

/// 打包字里「时刻」那部分占多少位（低 39 位）—— 约 17 年。
const TS_BITS: u32 = 39;
/// 「时刻」那部分的掩码。
const TS_MASK: u64 = (1u64 << TS_BITS) - 1;
/// 「还没发生过事件」的哨兵值 —— 打包字里「时刻」那部分的取值。
const NO_EVENT_MS: u64 = TS_MASK;
/// 脏标志 —— 打包字的最高位。
const DIRTY_BIT: u64 = 1 << 63;
/// 「事件序号」那部分占多少位（中间的 24 位）—— 1677 万个事件。
const SEQ_BITS: u32 = 64 - TS_BITS - 1;
/// 「事件序号」那部分的最大值。
const SEQ_MAX: u64 = (1u64 << SEQ_BITS) - 1;

/// 事件侧的共享信号 —— **notify 的回调只写这一个**。
///
/// 【为什么用「一个」 AtomicU64 而不是「脏标志 + 时刻」两个原子量】
///   以前是两个原子量、两个都是 `Relaxed` →
///   对账线程可能读到「脏了、但时刻是**上一次**的」。
///   现在「脏标志 + 时刻 + 事件序号」打包进**一个** u64：
///   **一次读就是一个自洽的快照** ✓
///   （与 `index_state.rs`「一次加锁读全部 → 读到的永远自洽」是同一条规矩）。
///
/// 【为什么用 AtomicU64 而不是 `Mutex<Option<Instant>>`】
///   `Instant` 没法原子地存。用「相对**基准时刻**的毫秒数」代替，
///   回调里就**一把锁都不用拿**。
///   这很重要：notify 的回调跑在**它自己的线程**上，
///   那里绝不能去碰引擎的锁、也不能碰 `IndexState` 的锁
///   （引擎的 view 是有状态的，见 `index_state.rs` 的锁序禁令）。
///
/// 【位布局】
///   bit 63        ：脏标志
///   bit 39..=62   ：事件序号（每记一次事件 +1；回绕无害 —— 生产逻辑不依赖它）
///   bit 0..=38    ：最近一次事件距【基准时刻】的毫秒数
///                   （`NO_EVENT_MS` = 还没发生过事件）
///
/// 【为什么要有「事件序号」】
///   它是「**有没有事件被吞掉**」的**独立参照**：
///   序号只增不减（只有 `mark` 会改它），所以
///   「生产线程一共记了 N 次事件」与「打包字里的序号」必须相等 ——
///   不等，就说明有一次事件被**回滚**了（= 被吞掉）。
///   少了它，「取走脏标志是不是原子的」这条不变式就只能靠源码护栏盯着。
struct Signals {
    /// 「脏标志 + 事件序号 + 最近一次事件时刻」打包在一起。
    state: AtomicU64,
    /// 基准时刻 —— 毫秒数要相对它换算成 `Instant`。
    base: Instant,
    /// **变化过的路径**（去重后）—— 增量更新要的就是这个。
    ///
    /// 【为什么需要一把锁】打包字只能装「脏标志 + 时刻 + 序号」这种定长信息；
    ///   路径集合是变长的，只能放锁后面。
    ///   这把锁**只属于 watcher 自己**（回调线程 ↔ 对账线程）——
    ///   不碰引擎的锁、不碰 `IndexState` 的锁 → 没有锁序问题 ✓
    paths: Mutex<HashSet<PathBuf>>,
    /// FSEvents 说「**有事件被丢掉了**」→ 必须**全量重扫**兜底。
    ///
    /// 【为什么需要这个兜底】FSEvents 的事件队列满了会**丢事件**，
    ///   这时它会发一个「必须重扫子目录」的信号（notify 把它翻译成
    ///   `Flag::Rescan` 事件）。我们按路径做的增量更新**依赖事件不丢**——
    ///   丢了就意味着索引与磁盘对不上，而且**永远对不上**（那个文件不会再变）。
    ///   → 收到这个信号就老老实实全量重扫一次。
    need_full_rescan: AtomicBool,
}

impl Signals {
    fn new() -> Self {
        Self {
            // 初始值 = 「不脏 + 序号 0 + 还没发生过事件」
            state: AtomicU64::new(NO_EVENT_MS),
            base: Instant::now(),
            paths: Mutex::new(HashSet::new()),
            need_full_rescan: AtomicBool::new(false),
        }
    }

    /// 记一个**变化过的路径**（只在 notify 的回调线程里调）。
    ///
    /// 【为什么用 `HashSet`】同一个文件的变化往往连着来好几个事件
    ///   （create → write → rename）。去重之后，
    ///   「一次保存」只占**一个**路径 → 增量更新只做一件事 ✓
    fn mark_path(&self, p: PathBuf) {
        self.paths.lock().unwrap().insert(p);
    }

    /// **取走**这一批变化过的路径（并清空）。
    fn take_paths(&self) -> Vec<PathBuf> {
        let mut g = self.paths.lock().unwrap();
        g.drain().collect()
    }

    /// 记下「有事件被丢掉了」→ 下一轮必须**全量重扫**。
    fn set_need_full_rescan(&self) {
        self.need_full_rescan.store(true, Ordering::Release);
    }

    /// 取走「有事件被丢掉了」这个标志（**取走即清**）。
    fn take_need_full_rescan(&self) -> bool {
        self.need_full_rescan.swap(false, Ordering::AcqRel)
    }

    /// 记一次事件（**只在 notify 的回调线程里调**）。
    ///
    /// 【一次 CAS 把「脏标志 + 序号 + 时刻」一起写进去】——
    ///   读的人要么看到旧的整套、要么看到新的整套，**不会看到半套**
    ///   （这正是把两个 `Relaxed` 原子量合成一个的原因）。
    ///   【为什么是 CAS 而不是 swap】序号要**从打包字里读出来再加一** ——
    ///   swap 拿不到「上一个序号」还保证不与别人打架；CAS 可以。
    ///
    /// 【返回值】`true` = 这是**这一批事件里的第一个**。
    ///   回调用它决定要不要打日志 —— 一次保存会产生好几个事件
    ///   （create / write / rename），全打一遍会把日志淹掉。
    ///   【注意】「一批」的粒度 = 「从上一次清脏标志之后」——
    ///   脏标志是对账线程清、回调线程置的，所以一批会被**每个对账周期**切开
    ///   （见复审 Nit-1）。
    fn mark(&self) -> bool {
        let ms = self.base.elapsed().as_millis() as u64 & TS_MASK;
        loop {
            let old = self.state.load(Ordering::Acquire);
            // 序号 +1（回绕无害：生产逻辑不依赖它）
            let seq = (((old >> TS_BITS) & SEQ_MAX).wrapping_add(1)) & SEQ_MAX;
            let new = DIRTY_BIT | (seq << TS_BITS) | ms;
            match self
                .state
                .compare_exchange_weak(old, new, Ordering::AcqRel, Ordering::Acquire)
            {
                // 成功换上去 → 「原来不脏」= 这一批的第一个
                Ok(_) => return old & DIRTY_BIT == 0,
                // 别人（对账线程在清脏）改过了 → 重来
                Err(_) => std::hint::spin_loop(),
            }
        }
    }

    /// 【一次读】脏标志 + 最近一次事件时刻 —— **自洽**。
    fn snapshot(&self) -> (bool, Option<Instant>) {
        let word = self.state.load(Ordering::Acquire);
        let dirty = word & DIRTY_BIT != 0;
        let ms = word & TS_MASK;
        let last_event_at = if ms == NO_EVENT_MS {
            None
        } else {
            Some(self.base + Duration::from_millis(ms))
        };
        (dirty, last_event_at)
    }

    /// 最近一次事件的时刻。
    ///
    /// 【为什么是 `#[cfg(test)]`】—— 它只被本文件的单元测试用；
    ///   对外的诊断入口（`dev_watch_status`）已随诊断探针一起删掉。
    ///   release 构建里**根本不存在**这个方法 → 不留死代码、不留警告。
    #[cfg(test)]
    fn last_event_at(&self) -> Option<Instant> {
        let word = self.state.load(Ordering::Acquire);
        let ms = word & TS_MASK;
        if ms == NO_EVENT_MS {
            None
        } else {
            Some(self.base + Duration::from_millis(ms))
        }
    }

    /// 已经记过多少个事件（**诊断 / 测试用**）。
    ///
    /// 【它是不变式的参照】—— 它只增不减，所以
    ///   「生产线程一共 `mark` 了 N 次」与「这里读到的序号」必须相等。
    ///
    /// 【只给测试用】生产逻辑不依赖它（回绕也无害）。
    #[cfg(test)]
    fn event_seq(&self) -> u64 {
        (self.state.load(Ordering::Acquire) >> TS_BITS) & SEQ_MAX
    }

    /// 现在是不是脏的（**只给测试用** —— 生产代码走 `snapshot()` 一次读全）。
    #[cfg(test)]
    fn is_dirty(&self) -> bool {
        self.state.load(Ordering::Acquire) & DIRTY_BIT != 0
    }

    /// 原子地「取走」脏标志 —— 返回它原来是不是脏的。
    ///
    /// 【为什么必须用原子 RMW（`fetch_and`），不能先 `load` 再 `store(false)`】
    ///   那两步之间可能又来一个事件 → 那个事件会被一起清掉 → **永远扫不到**。
    ///   `fetch_and` 是原子的：要么脏标志归我们（我们要扫了），要么归下一个事件。
    ///   （时刻与序号那两段**不清** —— 覆盖判断还要用时刻。）
    fn take_dirty(&self) -> bool {
        self.state.fetch_and(!DIRTY_BIT, Ordering::AcqRel) & DIRTY_BIT != 0
    }
}

/// 监听目标 —— **接缝**：生产是 `notify` 的 watcher，测试是假的。
///
/// 【为什么要这个接缝】「对账」这条不变式（监听列表变了要重配）
///   以前**零测试覆盖**（复审 MX6：让对账永不重配，55 条测试全绿）。
///   有了这个接缝，测试就能用假的 target 断言「到底有没有重新 watch」。
pub trait WatchTarget {
    /// 开始监听一个目录。
    fn watch_path(&mut self, path: &Path) -> Result<(), String>;
    /// 停止监听一个目录。
    fn unwatch_path(&mut self, path: &Path) -> Result<(), String>;
}

impl WatchTarget for RecommendedWatcher {
    fn watch_path(&mut self, path: &Path) -> Result<(), String> {
        // 【用完全限定名调 trait 方法】—— 否则会调到本文件这个 trait 的同名方法
        //   （无限递归）。
        Watcher::watch(self, path, RecursiveMode::Recursive).map_err(|e| e.to_string())
    }
    fn unwatch_path(&mut self, path: &Path) -> Result<(), String> {
        Watcher::unwatch(self, path).map_err(|e| e.to_string())
    }
}

/// 该监听哪些目录 —— **取自「当前活着的引擎」**。
///
/// 【为什么取自 IndexState 而不是设置】
///   设置里的是「用户配的」，引擎里的是「实际索引的」。
///   两者可能不一致（重建失败时设置已经改了、引擎还是旧的）。
///   要监听的必须是**引擎实际在索引的目录** —— 否则会去监听
///   一个根本没被索引的目录（事件来了，重扫了，索引还是没那条）。
///
/// 【这条以前零测试覆盖】（复审 MX8：改成监听设置里的目录，55 条测试全绿）
///   现在有测试盯着：引擎路径与设置路径**不一致**时，取的是**引擎**那份。
pub fn desired_watch_dirs<R: Runtime>(app: &AppHandle<R>) -> Vec<PathBuf> {
    app.state::<IndexState>()
        .engine_snapshot()
        .1
        .iter()
        .map(PathBuf::from)
        .collect()
}

/// 对账：把「已监听的列表」调整成「要监听的列表」。
///
/// 【返回】`true` = 重配过（监听列表变了）。
///
/// 【为什么单独抽出来】—— 「对账」是简报 §2.4 的核心设计决定，
///   以前**零测试覆盖**（复审 MX6：让对账永不重配，55 条测试全绿）。
///   抽出来之后，测试用假的 target 就能断言「到底有没有重新 watch」。
///
/// 【顺序要紧】先取消旧的、再加新的 —— 反过来的话，
///   新旧目录重叠的那一小段时间里会有两份监听 → 事件翻倍。
pub fn apply_watch_list<T: WatchTarget>(
    target: &mut T,
    current: &mut Vec<PathBuf>,
    desired: &[PathBuf],
    failed: &mut HashSet<PathBuf>,
) -> bool {
    if *current == desired {
        return false;
    }
    // 先取消旧的
    for p in current.iter() {
        let _ = target.unwatch_path(p);
    }
    // 再加上新的
    let mut now_watching: Vec<PathBuf> = Vec::new();
    for p in desired.iter() {
        match target.watch_path(p) {
            Ok(()) => {
                failed.remove(p);
                now_watching.push(p.clone());
            }
            Err(e) => {
                // 【只报一次】同一个目录失败一次就够了 ——
                //   每秒重试是对的（目录可能稍后才出现），但每秒打一行日志不行。
                if failed.insert(p.clone()) {
                    eprintln!("[lodestar] 监听目录 {} 失败: {e}", p.display());
                }
            }
        }
    }
    *current = now_watching;
    true
}

/// 「已知被排除的目录」缓存 —— 把重复的排除判断变成一次**组件级字符串比较**。
///
/// 【为什么需要它】引擎的排除规则是**目录级剪枝**：
///   判断一个路径会不会被索引，要**连祖先一起查**（每层一次 `stat`）——
///   实测单次约 **12.5µs**（探针 `examples/exclude_bench.rs`）。
///   而噪声（agent 的会话日志、浏览器缓存、`~/Library` 里的杂物）
///   能达到**每秒几百条** → 几百 × 12.5µs ≈ 每秒几毫秒 CPU。
///   机器上别的程序越忙，后台常驻的 CPU 就越高。
///
/// 【怎么快起来】第一次发现「`~/.minimax` 整个被排除」之后，
///   落在它下面的所有事件都**一次字符串比较**就跳过 —— 零 `stat`。
///   缓存条目会**很少**（浅层的祖先覆盖面最大）：
///   `~/.minimax` 一个条目就罩住它下面所有会话日志。
///
/// 【为什么不会漏索引】缓存里**只存确实被排除的目录**
///   （问过引擎、引擎说「排除」才存进来）
///   → 命中缓存 = 引擎也会说排除 → 跳过它与问引擎的结果**完全一致**。
pub struct ExcludedDirCache {
    dirs: Mutex<Vec<PathBuf>>,
}

/// 缓存最多记多少个目录 —— 超过就清空重来。
///
/// 【为什么要有上限】排除目录理论上可以很多（每个项目一个 `target/`）。
///   线性查找的代价是 O(条目数)，条目太多就白省了 —— 64 个足够罩住
///   绝大多数真实场景（`~/Library`、`~/.minimax`、几个 `target/`）。
///
/// 【为什么清空而不是「淘汰最旧的」】清空最简单，且**不影响正确性**
///   （只是命中率下降，下一次又会重新记上）。
const EXCLUDED_CACHE_MAX: usize = 64;

impl ExcludedDirCache {
    pub fn new() -> Self {
        Self {
            dirs: Mutex::new(Vec::new()),
        }
    }

    /// 记下一个「引擎确认被排除」的目录。
    pub fn remember(&self, dir: &Path) {
        let mut dirs = self.dirs.lock().unwrap();
        if dirs.len() >= EXCLUDED_CACHE_MAX {
            dirs.clear();
        }
        dirs.push(dir.to_path_buf());
    }

    /// 这个路径是不是落在某个「已知被排除」的目录下面？—— 命中就是**零 stat**。
    pub fn hits(&self, p: &Path) -> bool {
        let dirs = self.dirs.lock().unwrap();
        dirs.iter().any(|d| p.starts_with(d))
    }

    /// 这个路径会不会被引擎排除？—— **先查缓存，再问引擎**；
    ///   引擎说排除时，顺手把「最上层的那个被排除祖先」记下来。
    pub fn is_excluded(&self, p: &Path) -> bool {
        if self.hits(p) {
            return true;
        }
        if !search_core::Engine::path_is_excluded(p) {
            return false;
        }
        if let Some(top) = topmost_excluded_ancestor(p) {
            self.remember(&top);
        }
        true
    }
}

/// 这个事件里，**哪些路径是引擎会索引的**？
///
/// 【为什么抽成函数】生产回调与**端到端测试**必须用**同一份**判据 ——
///   抄一份就会漂移（改了回调忘了改测试 → 测试量的不是生产行为）。
///
/// 【判据】① 不是噪声事件（`.DS_Store` 之类）；
///   ② 逐个路径问排除规则（**连祖先一起查**）——
///   被排除的路径**一个字节都不会进索引**，它们的变化不该触发任何更新。
fn indexable_paths(ev: &notify::Event, cache: &ExcludedDirCache) -> Vec<PathBuf> {
    if is_noise_event(ev) {
        return Vec::new();
    }
    ev.paths
        .iter()
        .filter(|p| !cache.is_excluded(p))
        .cloned()
        .collect()
}

/// **最上层的**那个被排除的祖先（自己也算祖先）。
///
/// 【为什么要「最上层」】缓存的收益全在**覆盖面**：
///   缓存 `~/.minimax`（深 1）能罩住它下面**所有**子目录的事件；
///   缓存 `~/.minimax/v2/sessions/<某个 id>`（深 4）只罩住那一小片。
///   → 从**根往下**找第一个命中的 —— 那个就是覆盖面最大的。
///
/// 【为什么从根往下找】`Path::ancestors()` 是「自己 → 根」，
///   要反过来才是「根 → 自己」。
///   （`ancestors()` 返回的迭代器**不是** `DoubleEndedIterator`，
///     不能直接 `.rev()` —— 得先收进 Vec 再反转。）
fn topmost_excluded_ancestor(p: &Path) -> Option<PathBuf> {
    let mut chain: Vec<&Path> = p.ancestors().collect();
    chain.reverse();
    chain
        .into_iter()
        .find(|a| search_core::Engine::path_is_excluded(a))
        .map(Path::to_path_buf)
}

/// 启动文件变化监控：建 FSEvents 监听器 + 起对账线程。**常驻到进程结束**。
///
/// 【它是什么】
///   - 真正的 FSEvents 监听器活在对账线程里
///     （重配监听列表需要 `&mut`，所以它归那个线程所有）；
///   - 事件信号（`Signals`）是**回调线程与对账线程之间**的共享状态。
///
/// 【为什么不把它做成「每次 reindex 重建」】—— 见简报 §2.12：
///   对账机制已经覆盖了「换索引目录」，重建只会多一处要维护的生命周期。
///
/// 【建不起来也不该让应用起不来】—— 监听失败只意味着
///   「没有自动刷新」，其余功能照常用（用户还能点「刷新」）。
pub fn spawn_watcher<R: Runtime>(app: &AppHandle<R>) {
    let signals = Arc::new(Signals::new());
    let watched = Arc::new(Mutex::new(Vec::new()));
    let auto_rescans = Arc::new(AtomicU64::new(0));
    // 【被滤掉的噪声事件数】—— 只用来控制日志的打印频率
    // （每 100 个打一行，免得把日志淹掉）。
    let noise_events = Arc::new(AtomicU64::new(0));
    // 【被引擎排除掉的路径的变化数】—— 只为控制日志的打印频率
    //   （每 100 个打一行，免得把日志淹掉）。
    let excluded_events = Arc::new(AtomicU64::new(0));
    // 【「已知被排除的目录」缓存】—— 让重复的排除判断变成一次字符串比较。
    //   噪声能到每秒几百条，每次问引擎要 12.5µs（连祖先一起查）→
    //   缓存命中后是**零 stat**。
    let excluded_cache = Arc::new(ExcludedDirCache::new());

    let cb_signals = Arc::clone(&signals);
    let cb_noise = Arc::clone(&noise_events);
    // 【被引擎排除掉的路径的变化】—— 计数用，只为让日志能看出过滤器在干活。
    let cb_excluded = Arc::clone(&excluded_events);
    // 排除判断要用缓存 —— 回调线程独占它，不用跟别的线程抢。
    let cb_cache = Arc::clone(&excluded_cache);
    // 【notify 的回调跑在它自己的线程上】——
    //   这里**只做「记一笔」**：置脏 + 记时间戳（一次原子写）。
    //   不碰引擎、不碰 IndexState、不拿别的锁。
    let watcher = notify::recommended_watcher(
        move |res: notify::Result<notify::Event>| match res {
            Ok(ev) => {
                // 【兜底 —— 排在所有过滤之前】
                //   FSEvents 的事件队列满了会**丢事件**，这时它会发一个
                //   「必须重扫子目录」的信号（notify 翻译成 `Flag::Rescan`）。
                //   我们按路径做的增量更新**依赖事件不丢** ——
                //   丢了就意味着索引永远对不上。
                //   → 记下来，下一轮老老实实全量重扫一次。
                if ev.need_rescan() {
                    cb_signals.set_need_full_rescan();
                    crate::logging::log_line("[watch] FSEvents 说有事件被丢掉 → 下一轮全量重扫");
                    return;
                }
                // 【先滤噪声】—— `.DS_Store` 这类 macOS 私有缓存文件
                //   的变化**不该**换来一次全量重扫（见 `is_noise_event` 的说明）。
                //   【滤在置脏之前】—— 噪声**根本不进入**去抖逻辑，
                //   所以它既不会触发重扫、也不会把静默期推后。
                if is_noise_event(&ev) {
                    let n = cb_noise.fetch_add(1, Ordering::Relaxed) + 1;
                    // 【每 100 次打一行】—— 滤掉的事件可能很频繁，
                    //   每次都打会把日志淹掉；不打又看不出过滤器在不在干活。
                    if n % 100 == 1 {
                        crate::logging::log_line(&format!(
                            "滤掉噪声事件（第 {n} 个）：{:?}",
                            ev.paths
                        ));
                    }
                    return;
                }
                // 【再滤「引擎不会索引的路径」】——
                //   watcher 监听的是**索引目录**（$HOME），FSEvents 是**递归**的
                //   → 它看得见 $HOME 下的每一个变化，包括：
                //     · `~/Library/...`（引擎排除了，但 watcher 照样看得见）
                //       典型触发者：**Lodestar 自己的 webview 数据**
                //       （~/Library/WebKit/com.fsearch.macos/…）、Edge、百度网盘…
                //     · `~/.minimax/...`（agent 的会话日志，一直在写）
                //   这些路径**一个字节都不会进索引**，
                //   却每次都换来一次**全量重扫**（$HOME 量级 1.7~2.4 秒），
                //   而搜索与重扫共用**同一个 worker 线程**（严格串行）
                //   → 每次重扫都把排在它后面的搜索堵住 ~2 秒。
                //   实测（2026-10-22）：45 秒里 6 次这样的空转重扫。
                //
                //   【为什么问引擎而不是自己抄一份规则】——
                //   排除规则只有引擎的 exclude manager 这**一份真相**；
                //   抄一份就会漂移（改了 C 那边忘了改 Rust 那边 → 又开始空转）。
                //
                //   【判据】事件里**只要有一个**路径会被索引，就算数 ——
                //   引擎是整目录扫的，漏掉任何一个都可能让索引过期。
                // 【走缓存】—— 语义与直接问引擎**完全一致**（缓存里只存
                //   引擎确认排除的目录），但重复路径不用再连祖先查一遍。
                //
                //   【判据】这次要处理的是**路径**，所以判据必须是**逐个路径**的：
                //   一个事件里可能既有该索引的路径、也有不该索引的路径
                //   （FSEvents 一次会带上一串路径）——
                //   整批「有一个能索引就算数」的旧判据会把**不该索引的路径**
                //   也塞进增量更新 → 白做功（甚至让引擎去 stat 上万条排除项）。
                //   → 只把**确实会被索引**的路径收进来。
                let indexable = indexable_paths(&ev, &cb_cache);
                if indexable.is_empty() {
                    let n = cb_excluded.fetch_add(1, Ordering::Relaxed) + 1;
                    if n % 100 == 1 {
                        crate::logging::log_line(&format!(
                            "滤掉【引擎不索引】的路径变化（第 {n} 个）：{:?}",
                            ev.paths
                        ));
                    }
                    return;
                }
                // 【收进「脏路径」集合】—— 这就是增量更新的输入。
                //
                //   去重在集合里做（同一个文件的多个事件只占一个路径）。
                for p in indexable {
                    cb_signals.mark_path(p);
                }
                // 【这一批事件只打一行】——
                //   一次保存会产生好几个事件（create / write / rename），
                //   全打一遍日志会被淹掉。只打**第一个**。
                //   【「一批」的粒度】脏标志是对账线程清、回调线程置的
                //   → 一批会被**每个对账周期**切开
                //   → 准确说是「上一次清脏标志之后收到的那些事件」。
                if cb_signals.mark() {
                    let paths: Vec<String> = ev
                        .paths
                        .iter()
                        .map(|p| p.to_string_lossy().into_owned())
                        .collect();
                    crate::logging::log_line(&format!(
                        "收到文件变化：{:?} ({:?})",
                        paths,
                        ev.kind
                    ));
                }
            }
            Err(e) => eprintln!("[lodestar] 文件监控出错: {e}"),
        },
    );

    match watcher {
        Ok(watcher) => {
            let handle = app.clone();
            let t_signals = Arc::clone(&signals);
            let t_watched = Arc::clone(&watched);
            let t_count = Arc::clone(&auto_rescans);
            let spawned = std::thread::Builder::new()
                .name("fsearch-watch".to_string())
                .spawn(move || {
                    reconcile_loop(handle, watcher, t_signals, t_watched, t_count)
                });
            if let Err(e) = spawned {
                eprintln!("[lodestar] 起文件监控线程失败，「自动刷新」不可用: {e}");
            }
        }
        Err(e) => {
            eprintln!("[lodestar] 建文件监控失败，「自动刷新」不可用: {e}");
        }
    }
}

/// 对账线程 —— 每 [`RECONCILE_INTERVAL`] 一轮：
///   ① **对账**：读「当前活着的引擎索引了哪些目录」，与「已监听的列表」对比
///      → 不一致就重配（先取消旧的、再加新的）；
///   ② **去抖 + 自动重扫**：「有变化待处理」且**六道闸**都过了 → 触发一次自动重扫。
///
/// 【为什么用「对账」而不是「埋点」】—— 见简报 §2.4：
///   埋点（启动完成 / reindex 成功时调 `watcher.set_dirs(...)`）要改好几处，
///   **漏一处就静默不生效**。对账只有一个地方、一套逻辑，
///   天然覆盖所有换目录的路径。
fn reconcile_loop<R: Runtime>(
    app: AppHandle<R>,
    mut watcher: RecommendedWatcher,
    signals: Arc<Signals>,
    watched: Arc<Mutex<Vec<PathBuf>>>,
    auto_rescans: Arc<AtomicU64>,
) {
    // 上次自动重扫的**开始**时刻（「最小间隔」这道闸只有它自己管）。
    let mut last_auto_scan_at: Option<Instant> = None;
    // 监听失败过的目录 —— 只在「第一次失败」时打日志，免得每秒刷屏。
    let mut failed: HashSet<PathBuf> = HashSet::new();

    loop {
        std::thread::sleep(RECONCILE_INTERVAL);

        // ── ① 对账：当前活着的引擎索引了哪些目录 ──
        let desired = desired_watch_dirs(&app);
        {
            let mut current = watched.lock().unwrap();
            if apply_watch_list(&mut watcher, &mut current, &desired, &mut failed) {
                crate::logging::log_line(&format!(
                    "[watch] 监听目录已更新：{:?}",
                    current.as_slice()
                ));
            }
        }

        // ── ② 去抖：六道闸（见 `decide_rescan` / `decide_auto_rescan`）──
        let now = Instant::now();
        // 【一次读】脏标志 + 事件时刻 —— 自洽（不会读到「脏了但时刻是上一次的」）
        let (dirty, last_event_at) = signals.snapshot();
        // 【一次锁内读两个】有扫描在飞 + 最近一次已完成的扫描的时刻
        let (scan_in_flight, last_scan_completed_at) = app.state::<IndexState>().scan_snapshot();
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
        //   万一这轮没扫成（还有扫描在飞），要**放回去**
        //   （这个「放回去」由 `rescan_action` 决定，见下面）。
        let life = app.state::<crate::lifecycle::Lifecycle>();
        let idle = life.phase() == crate::lifecycle::Phase::Idle;
        let force = life.take_catchup();
        // 【开关】用闭包传进去 —— 只在**真的要扫了**的时候才读设置
        //   （读设置要开一次 store 文件；每秒读是纯浪费）。
        let outcome =
            decide_rescan(&inp, idle, force, || crate::settings::load(&app).auto_refresh);
        // 【副作用也从纯函数里来】——
        //   「要不要清脏标志」「要不要把补扫请求放回去」这两个动作
        //   以前只活在这个 match 分支里 —— 删掉任何一行，测试仍然全绿。
        //   现在它们是 `rescan_action` 的返回值 → **可断言**。
        let action = rescan_action(outcome);
        // 【执行动作】—— 返回「该不该执行这次更新」。
        //   注意：**清脏发生在这里**，也就是在更新【之前】（理由见下面）。
        let rescan = apply_rescan_action(action, &signals, &life);
        if !rescan {
            // 【为什么这里也要把「脏路径」清掉】
            //   脏标志被清 = 这批变化被判成了「已被覆盖」或「开关关着」——
            //   也就是说**它们不会再被处理了**。
            //   → 路径也必须一起清掉，否则集合会**越积越多**（内存泄漏），
            //   而且那些路径早就过期了。
            if action.clear_dirty {
                signals.take_paths();
                signals.take_need_full_rescan();
            }
            // 【日志】只有「开关关着」这一种要打 ——
            //   `Paused`（空闲）每轮都会走到这里，别的分支也打的话会把日志刷爆。
            if matches!(outcome.decision, RescanDecision::Disabled) {
                crate::logging::log_line("[watch] 自动刷新已关闭，跳过");
            }
            continue;
        }

        // 【记时刻 —— 必须在做重扫【之前】】
        //
        //   重扫要 2 秒（`$HOME` 量级），这期间可能又来了新事件。
        //   **清脏已经在上面做完了**（`action.clear_dirty`）——
        //   它必须在重扫之前：先清脏 → 那期间的新事件会**重新置脏**
        //   → 下一轮会再扫一次（不丢事件）。
        //   反过来（扫完再清脏）→ 这期间来的事件被一起清掉 → 永远扫不到。
        //
        //   【为什么用原子 RMW 而不是 load + store(false)】—— 见 `Signals::take_dirty`。
        // ── 取走这一批变化 ──
        //
        //   【为什么在这里取，而不是更新之前】
        //   取走之后再来的事件会**重新**进集合 → 下一轮再处理 → **不丢事件** ✓
        //   （与「清脏要在更新之前」是同一条道理）。
        let paths = signals.take_paths();
        let need_full = signals.take_need_full_rescan();
        if !need_full && paths.is_empty() {
            // 没有具体路径可处理 → 这一轮无事可做（跳过更新，也不发事件）
            continue;
        }

        last_auto_scan_at = Some(now);

        let count = auto_rescans.fetch_add(1, Ordering::Relaxed) + 1;
        let t0 = Instant::now();
        // 【两种更新方式】
        //   · 正常 → **增量**：只碰真的变了的那几个路径（毫秒级）
        //   · FSEvents 丢了事件 → **全量重扫**兜底
        //     （增量更新依赖「事件没丢」—— 丢了索引就永远对不上）
        let outcome_result = if need_full {
            auto_rescan(&app)
        } else {
            auto_update(&app, paths)
        };
        match outcome_result {
            Ok(indexed) => {
                crate::logging::log_line(&format!(
                    "[watch] 自动更新 #{count} 完成：{indexed} 项，{} 个路径，耗时 {:?}",
                    if need_full { "全量重扫" } else { "增量" },
                    t0.elapsed()
                ));
            }
            Err(e) => {
                eprintln!("[lodestar] 自动更新失败: {e}");
                crate::logging::log_line(&format!("[watch] 自动更新 #{count} 失败: {e}"));
            }
        }
    }
}

/// 跑一次**增量更新** —— 把这一批变化就地反映进索引。
///
/// 【与「自动重扫」的区别】
///   自动重扫 = 把整棵树重新扫一遍（`$HOME` 量级 300ms+，且堵住后面的搜索）。
///   增量更新 = 只碰**真的变了**的那几个路径（毫秒级）。
///   → 状态更新、事件通知这两件事**完全一致**（同一段代码），
///     变的只有「引擎内部做的是哪件事」。
fn auto_update<R: Runtime>(app: &AppHandle<R>, paths: Vec<PathBuf>) -> Result<u32, String> {
    let state = app.state::<IndexState>();
    let changes = search_core::incremental::ChangeSet { paths };
    // 【取引擎放在闭包里】—— 理由与 `auto_rescan` 完全一样（代次守卫的次序）。
    let indexed = crate::commands::do_apply_changes(
        state.inner(),
        || crate::commands::take_searcher(app.state::<Mutex<Option<Arc<Searcher>>>>().inner()),
        changes,
    )?;
    notify_frontend(&app);
    Ok(indexed)
}

/// 跑一次自动重扫 —— **走的是和「刷新」按钮同一条路**（`commands::do_rescan`）。
///
/// 【为什么要共用一条路】—— 见 `commands::do_rescan` 的说明：
///   两条路必须保证「状态更新一致、引擎操作串行、失败处理一致」。
///   而且这两条路**同时**能触发（用户点「刷新」的同时 watcher 也发现了变化）。
///
/// 【失败要分开报】（复审 Nit-2）
///   以前 `app.emit` 的错误被 `?` 一起冒泡出去 → 调用点把**任何** Err
///   都打成「自动重扫失败」→ **重扫其实是成功的**，日志却说失败 → 误导排查。
///   现在：重扫失败才报失败；事件发不出去单独报一行。
fn auto_rescan<R: Runtime>(app: &AppHandle<R>) -> Result<u32, String> {
    let state = app.state::<IndexState>();
    // 【取引擎放在闭包里】——
    //   `do_rescan` 内部是「先快照代次、后取引擎」，这个顺序不能乱
    //   （见 `commands::do_rescan` 的说明：反过来会把旧引擎的结果
    //     写进代表新引擎的状态 → 反向错位）。
    let indexed = crate::commands::do_rescan(state.inner(), || {
        crate::commands::take_searcher(app.state::<Mutex<Option<Arc<Searcher>>>>().inner())
    })?;
    // 【通知前端】结果表重搜 + 状态栏更新。
    notify_frontend(&app);
    Ok(indexed)
}

/// 通知前端「索引变了，结果表重搜一次 + 状态栏更新」。
///
/// 【为什么抽成共用的一段】增量更新与全量重扫**对前端完全一样**
///   （同一个事件、同一个载荷）—— 区别只在引擎内部做的是哪件事。
///   → 前端的契约只有**一份**，不会漂。
fn notify_frontend<R: Runtime>(app: &AppHandle<R>) {
    //   载荷是**当前的索引状态** —— 前端拿它直接更新状态栏，
    //   不用再等下一次轮询（轮询是 800ms 一轮）。
    let mut snapshot = app.state::<IndexState>().snapshot();
    // 【窗口可见性也要是真值】—— 前端拿这个载荷决定要不要继续跑那个 800ms 轮询，
    //   所以不能留 `snapshot()` 给的占位值。
    snapshot.visible = crate::lifecycle::window_visible(&app);
    if let Err(e) = app.emit(AUTO_REFRESH_EVENT, snapshot) {
        // 【更新本身是成功的】—— 只是事件发不出去（运行时已经停了）。
        eprintln!("[lodestar] 自动更新完成，但通知前端失败: {e}");
        crate::logging::log_line(&format!("[watch] 自动更新完成，但事件发不出去: {e}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use tauri::Listener;

    /// 基准时刻 —— 测试里用它当「现在」。
    fn t0() -> Instant {
        Instant::now()
    }

    /// 造一个默认的决策输入（「什么闸都没拦」的那一种）。
    fn inputs_at(now: Instant) -> RescanInputs {
        RescanInputs {
            dirty: true,
            last_event_at: Some(now - Duration::from_secs(10)),
            now,
            last_auto_scan_at: None,
            last_scan_completed_at: None,
            scan_in_flight: false,
        }
    }

    // ── 去抖：静默期 ──

    /// 没脏 → 不该扫。
    #[test]
    fn not_dirty_means_no_scan() {
        let now = t0();
        let mut inp = inputs_at(now);
        inp.dirty = false;
        assert_eq!(
            decide_auto_rescan(&inp, || true),
            RescanDecision::Wait,
            "没有待处理的变化就不该扫"
        );
    }

    /// 静默期**没过** → 不该扫（事件刚来）。
    ///
    /// 【这条钉住的是「去掉静默期判断」那个反证实验】
    ///   把静默期判断删掉 → 这里会变红。
    #[test]
    fn within_the_quiet_period_does_not_scan() {
        let now = t0();
        let mut inp = inputs_at(now);
        // 【为什么用常量】钉的是**边界**（静默期差 1ms），
        //   不是某个写死的毫秒数 —— 改了常量，这条测试仍然守着边界。
        inp.last_event_at = Some(now - (QUIET_PERIOD - Duration::from_millis(1)));
        assert_eq!(
            decide_auto_rescan(&inp, || true),
            RescanDecision::Wait,
            "事件刚发生不到静默期，不该扫"
        );
    }

    /// 静默期**刚过** → 该扫。
    #[test]
    fn after_the_quiet_period_scans() {
        let now = t0();
        let mut inp = inputs_at(now);
        inp.last_event_at = Some(now - QUIET_PERIOD);
        assert_eq!(
            decide_auto_rescan(&inp, || true),
            RescanDecision::Rescan,
            "事件发生满静默期就该扫"
        );
    }

    /// 迟到的第二个事件会把静默期**重新推后** —— 一次保存的多个事件
    /// 因此只会触发**一次**重扫。
    #[test]
    fn a_later_event_pushes_the_quiet_period_back() {
        let now = t0();
        let mut inp = inputs_at(now);
        // 第一个事件在 900ms 前 → 本来该扫了
        inp.last_event_at = Some(now - Duration::from_millis(900));
        assert_eq!(
            decide_auto_rescan(&inp, || true),
            RescanDecision::Rescan,
            "只看第一个事件的话静默期已经过了"
        );
        // 但 100ms 前又来一个 → 静默期重新算
        inp.last_event_at = Some(now - Duration::from_millis(100));
        assert_eq!(
            decide_auto_rescan(&inp, || true),
            RescanDecision::Wait,
            "刚来的新事件应该把静默期推后"
        );
    }

    // ── 去抖：最小间隔 ──

    /// 距上次自动重扫不到 5s → 不该扫。
    ///
    /// 【这条钉住的是「去掉最小间隔」那个反证实验】
    ///   把最小间隔判断删掉 → 这里会变红。
    #[test]
    fn within_the_min_interval_does_not_scan() {
        let now = t0();
        let mut inp = inputs_at(now);
        inp.last_auto_scan_at = Some(now - (MIN_INTERVAL - Duration::from_millis(1)));
        assert_eq!(
            decide_auto_rescan(&inp, || true),
            RescanDecision::Wait,
            "距上次自动更新不到最小间隔，不该扫"
        );
    }

    /// 距上次自动重扫满 5s → 该扫。
    #[test]
    fn after_the_min_interval_scans() {
        let now = t0();
        let mut inp = inputs_at(now);
        inp.last_auto_scan_at = Some(now - MIN_INTERVAL);
        assert_eq!(
            decide_auto_rescan(&inp, || true),
            RescanDecision::Rescan,
            "距上次自动更新满最小间隔就该扫"
        );
    }

    /// 还没扫过（`last_auto_scan_at = None`）→ 最小间隔这道闸不挡。
    #[test]
    fn the_first_scan_is_not_blocked_by_the_min_interval() {
        let now = t0();
        let inp = inputs_at(now);
        assert_eq!(decide_auto_rescan(&inp, || true), RescanDecision::Rescan);
    }

    /// 脏了但没有事件时刻 → 不扫（判断不了静默期，宁可晚一点）。
    #[test]
    fn dirty_without_a_timestamp_does_not_scan() {
        let now = t0();
        let mut inp = inputs_at(now);
        inp.last_event_at = None;
        assert_eq!(decide_auto_rescan(&inp, || true), RescanDecision::Wait);
    }

    // ── 闸一：有扫描在飞 ──

    /// 有扫描在飞 → 这一轮跳过（**不清脏**）。
    ///
    /// 【为什么不许清脏】那趟在飞的扫描**开始时**可能还没发生这次变化
    ///   → 清了就可能永远扫不到。下一轮还会重新判断。
    #[test]
    fn a_scan_in_flight_makes_this_round_wait() {
        let now = t0();
        let mut inp = inputs_at(now);
        inp.scan_in_flight = true;
        assert_eq!(
            decide_auto_rescan(&inp, || true),
            RescanDecision::Wait,
            "有扫描在飞时不该再发起一次"
        );
    }

    /// 【配合上一条】「不清脏」在 `Signals` 这一层是真的：
    ///   跳过一轮之后，脏标志**还在** → 下一轮还会重新判断 → 不丢事件。
    #[test]
    fn skipping_a_round_keeps_the_dirty_flag() {
        let s = Signals::new();
        s.mark();
        // 模拟「这一轮有扫描在飞 → 跳过」
        let inp = RescanInputs {
            dirty: s.is_dirty(),
            last_event_at: s.last_event_at(),
            now: Instant::now(),
            last_auto_scan_at: None,
            last_scan_completed_at: None,
            scan_in_flight: true,
        };
        assert_eq!(decide_auto_rescan(&inp, || true), RescanDecision::Wait);
        assert!(
            s.is_dirty(),
            "跳过一轮**不能**清脏标志 —— 否则这次事件就永远丢了"
        );
        assert!(s.take_dirty(), "下一轮还能取到这次事件");
    }

    // ── 闸二：覆盖（Important-1 的靶子） ──

    /// 这次变化**早于**最近一次已完成的扫描 → 已被覆盖 → 不用再扫。
    ///
    /// 【这条就是复审 Important-1 的靶子】
    ///   用户点一次「刷新」= 一趟扫描；它完成之后那次变化已经在索引里了
    ///   → watcher 不该为同一批变化**再扫一整遍**。
    #[test]
    fn an_event_covered_by_a_finished_scan_does_not_trigger_a_rescan() {
        let now = t0();
        let mut inp = inputs_at(now);
        // 事件在 2 秒前被观察到；扫描在 1 秒前完成 → 事件被覆盖
        inp.last_event_at = Some(now - Duration::from_secs(2));
        inp.last_scan_completed_at = Some(now - Duration::from_secs(1));
        assert_eq!(
            decide_auto_rescan(&inp, || true),
            RescanDecision::Covered,
            "已被覆盖的变化不该再扫一遍（否则一次刷新 = 两次全量重扫）"
        );
    }

    /// 事件在扫描**完成后**才被观察到，但还在投递宽限内 → 仍算已覆盖。
    ///
    /// 【为什么要有宽限】FSEvents 投递有延迟：文件可能是在扫描**进行中**改的，
    ///   事件却是扫描**结束之后**才被观察到的。
    ///   没有宽限 → 「已被覆盖」被误判成「没被覆盖」→ 连扫两遍（复审实测过）。
    #[test]
    fn an_event_just_after_the_scan_is_still_covered() {
        let now = t0();
        let mut inp = inputs_at(now);
        inp.last_scan_completed_at = Some(now - Duration::from_millis(100));
        inp.last_event_at = Some(now - Duration::from_millis(50));
        assert_eq!(
            decide_auto_rescan(&inp, || true),
            RescanDecision::Covered,
            "投递延迟内的观察时刻仍算已覆盖"
        );
    }

    /// 事件**晚于**覆盖窗口 → 没被覆盖 → 该扫（自动刷新照常工作）。
    #[test]
    fn an_event_outside_the_covered_window_triggers_a_rescan() {
        let now = t0();
        let mut inp = inputs_at(now);
        inp.last_scan_completed_at = Some(now - Duration::from_secs(10));
        inp.last_event_at = Some(now - Duration::from_secs(5));
        assert_eq!(
            decide_auto_rescan(&inp, || true),
            RescanDecision::Rescan,
            "变化发生在扫描之后 → 索引里没有 → 该扫"
        );
    }

    /// 从来没成功扫过（`last_scan_completed_at = None`）→ 什么都没覆盖 → 该扫。
    #[test]
    fn without_any_finished_scan_nothing_is_covered() {
        let now = t0();
        let inp = inputs_at(now);
        assert_eq!(decide_auto_rescan(&inp, || true), RescanDecision::Rescan);
    }

    // ── 闸五：开关 ──

    /// 开关关着 → 不触发（但要清脏标志，别每一轮都重新判断）。
    ///
    /// 【这条钉住的是「忽略 `auto_refresh` 开关」那个反证实验】
    ///   把开关判断删掉 → 这里会变红。
    #[test]
    fn the_switch_off_disables_the_rescan() {
        let now = t0();
        let inp = inputs_at(now);
        assert_eq!(
            decide_auto_rescan(&inp, || false),
            RescanDecision::Disabled,
            "开关关着就不该触发重扫"
        );
    }

    /// 开关开着 → 正常触发。
    #[test]
    fn the_switch_on_allows_the_rescan() {
        let now = t0();
        let inp = inputs_at(now);
        assert_eq!(decide_auto_rescan(&inp, || true), RescanDecision::Rescan);
    }

    // ── 「一秒内连建 5 个文件 → 只扫一次」的去抖语义 ──

    /// 5 个文件在 1 秒内陆续落地 → **只该触发一次**更新。
    ///
    /// 【怎么算的】
    ///   每个事件都把「最近一次事件时刻」往后推 → 静默期在**最后一个**事件
    ///   之后重新开始算 → 期间不会扫。等安静满静默期才扫那一次。
    ///   扫完**脏标志就被清掉**（生产里由 `apply_rescan_action` 清）→
    ///   后面那几轮没有待处理的变化 → 不会再扫。
    ///
    /// 【为什么要模拟「扫完清脏」】
    ///   这一版以前把 `dirty` 写成「**只要发生过事件就永远为真**」——
    ///   那不是生产的样子：脏标志是「有变化等着被收进索引」的记号，
    ///   扫描消费掉它就会清掉。不模拟这一步的话，
    ///   这条测试实际上是在测「最小间隔有多长」，而不是「批量会不会被拆开」。
    ///
    /// 【为什么要模拟「扫完之后记下时刻」】
    ///   对账线程扫完会把 `last_auto_scan_at` 更新成那一刻 ——
    ///   测试里不照做的话，最小间隔那道闸就形同虚设。
    #[test]
    fn five_files_within_one_second_trigger_one_scan() {
        let base = t0();
        // 5 个文件分别在 0 / 200 / 400 / 600 / 800ms 落地（绝对时刻）
        let event_times_ms = [0u64, 200, 400, 600, 800];
        let mut scans = 0;
        let mut last_auto_scan_at: Option<Instant> = None;
        // 【脏标志】= 有变化还没被收进索引。扫描消费掉它 → 清掉。
        let mut dirty = false;
        // 每 100ms 看一次「此刻该不该扫」，总共 3 秒
        for tick in 0..30u64 {
            let now = base + Duration::from_millis(tick * 100);
            let elapsed = tick * 100;
            // 已经发生的事件里，最晚的那个 —— 它的「年龄」就是静默期的起点
            let last_event_at = event_times_ms
                .iter()
                .filter(|ms| **ms <= elapsed)
                .map(|ms| now - Duration::from_millis(elapsed - *ms))
                .max();
            // 新事件到了 → 置脏（生产里由回调线程做）
            if event_times_ms.iter().any(|ms| *ms == elapsed) {
                dirty = true;
            }
            let inp = RescanInputs {
                dirty,
                last_event_at,
                now,
                last_auto_scan_at,
                // 【没有已完成的扫描】→ 什么都还没被覆盖
                last_scan_completed_at: None,
                scan_in_flight: false,
            };
            if decide_auto_rescan(&inp, || true) == RescanDecision::Rescan {
                scans += 1;
                last_auto_scan_at = Some(now);
                dirty = false; // 扫描消费掉了这批变化 → 清脏（生产里的行为）
            }
        }
        assert_eq!(
            scans, 1,
            "一秒内连建 5 个文件应该只更新一次，实际更新了 {scans} 次"
        );
    }

    // ── 脏路径集合（增量更新的**输入**）──

    /// 记路径 → **去重**；取走 → **清空**。
    ///
    /// 【为什么要去重】一次「保存」在 FSEvents 看来是好几个事件
    ///   （create → write → rename），全指向**同一个路径**。
    ///   不去重的话，增量更新会对同一个文件做三遍事。
    #[test]
    fn the_dirty_path_set_dedupes_and_drains() {
        let s = Signals::new();
        s.mark_path(PathBuf::from("/tmp/a.txt"));
        s.mark_path(PathBuf::from("/tmp/a.txt"));
        s.mark_path(PathBuf::from("/tmp/b.txt"));
        let paths = s.take_paths();
        assert_eq!(paths.len(), 2, "同一个路径只该占一个");
        assert!(
            s.take_paths().is_empty(),
            "取走之后要清空 —— 否则集合会越积越多"
        );
    }

    /// FSEvents 说「有事件被丢掉了」→ 记下来 → **取走即清**。
    ///
    /// 【为什么要有这个兜底】增量更新依赖「事件没丢」——
    ///   丢了就意味着索引永远对不上，所以那一轮必须退回全量重扫。
    #[test]
    fn the_full_rescan_fallback_flag_is_taken_once() {
        let s = Signals::new();
        assert!(!s.take_need_full_rescan(), "一开始不该有");
        s.set_need_full_rescan();
        assert!(s.take_need_full_rescan(), "记下之后要能读到");
        assert!(!s.take_need_full_rescan(), "取走即清");
    }

    // ── 端到端：文件变了 → 索引能搜到，**要多久** ──

    /// 端到端延迟护栏 —— 「改一个文件 → 索引里能搜到」必须 **< 1 秒**。
    ///
    /// 【这条测试量的是**整条真实链路**】
    ///   真 FSEvents 监听 → 事件过滤 → 脏路径集合 → 那几道闸
    ///   → 取路径 → 增量更新 → 引擎能搜到。
    ///   用的全是**生产同一份**的常量与判据函数
    ///   （[`RECONCILE_INTERVAL`] / [`QUIET_PERIOD`] / [`MIN_INTERVAL`] /
    ///   [`decide_rescan`] / [`indexable_paths`]）——
    ///   所以它量到的就是用户能感觉到的那一下。
    ///
    /// 【为什么这条要存在】
    ///   「增量更新本身 < 5ms」只说了**引擎内部**的耗时；
    ///   用户感觉到的是**端到端**（含事件投递与去抖等待）。
    ///   这两个数会**各自漂移** —— 所以要把端到端也钉住。
    #[test]
    fn the_end_to_end_latency_stays_under_one_second() {
        use search_core::incremental::ChangeSet;
        use search_core::types::{ListKind, SearchRequest, SortField, SortOrder};

        // ① 临时目录 + 索引它的引擎
        //
        //   【为什么不用 `std::env::temp_dir()`】
        //   macOS 的 per-user 临时目录在 `/var/folders/...`，
        //   它的**真实路径**是 `/private/var/folders/...` ——
        //   而 `/private/var` 是**排除规则**里的一条（macOS 的系统临时区）。
        //   → 用它做测试目录的话，事件会被排除规则挡掉（测不到东西）。
        //   `/tmp` 的真实路径是 `/private/tmp`，**不在**排除前缀里 ✓
        let dir = PathBuf::from("/tmp").join(format!("lodestar-e2e-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (engine, _) = search_core::Engine::new(&[dir.as_path()]).unwrap();

        // ② 真 FSEvents 监听（生产同款回调逻辑）
        let signals = Arc::new(Signals::new());
        let cache = Arc::new(ExcludedDirCache::new());
        let cb_signals = Arc::clone(&signals);
        let cb_cache = Arc::clone(&cache);
        let mut watcher = notify::recommended_watcher(
            move |res: notify::Result<notify::Event>| {
                if let Ok(ev) = res {
                    let indexable = indexable_paths(&ev, &cb_cache);
                    if !indexable.is_empty() {
                        for p in indexable {
                            cb_signals.mark_path(p);
                        }
                        // 【别忘了置脏标志】—— 生产回调也是**两件事都做**：
                        //   `mark_path` 记「**哪些路径**变了」，
                        //   `mark` 记「有变化了」（脏标志 + 时刻 + 序号）。
                        //   那几道闸看的是**脏标志**，增量更新用的是**路径集合** ——
                        //   少任何一半，这条链路都不通。
                        cb_signals.mark();
                    }
                }
            },
        )
        .unwrap();
        watcher
            .watch(&dir, notify::RecursiveMode::Recursive)
            .unwrap();
        // 等 FSEvents 把流建起来（否则第一个事件可能丢）
        std::thread::sleep(Duration::from_millis(300));

        // ③ 写一个文件 —— 记下时刻
        let started = Instant::now();
        std::fs::write(dir.join("e2e-probe.txt"), "x").unwrap();

        // ④ 照着**生产对账循环**的步骤跑：每轮判一次那几道闸，过了就更新
        let mut last_auto_scan_at: Option<Instant> = None;
        let mut applied = false;
        let deadline = started + Duration::from_secs(10);
        while Instant::now() < deadline {
            std::thread::sleep(RECONCILE_INTERVAL);
            let now = Instant::now();
            let (dirty, last_event_at) = signals.snapshot();
            let inp = RescanInputs {
                dirty,
                last_event_at,
                now,
                last_auto_scan_at,
                // 【没有已完成的扫描】→ 什么都还没被覆盖
                last_scan_completed_at: None,
                scan_in_flight: false,
            };
            // 空闲=false、强制=false —— 生产里只有唤醒时才是 force
            let outcome = decide_rescan(&inp, false, false, || true);
            let action = rescan_action(outcome);
            if !action.rescan {
                if action.clear_dirty {
                    signals.take_paths();
                }
                continue;
            }
            last_auto_scan_at = Some(now);
            let paths = signals.take_paths();
            if paths.is_empty() {
                continue;
            }
            engine
                .apply_changes(&ChangeSet { paths })
                .expect("增量更新不该失败");
            applied = true;
            break;
        }
        assert!(applied, "5 秒内没有触发任何一次更新");

        // ⑤ 引擎里能搜到它了
        let req = SearchRequest {
            query: "e2e-probe".to_string(),
            sort: SortField::Name,
            order: SortOrder::Asc,
            kind: ListKind::All,
            page: 0,
            page_size: 50,
        };
        let res = engine.search(&req).expect("搜索失败");
        assert_eq!(res.total_entries, 1, "新文件应该已经在索引里");

        let elapsed = started.elapsed();
        println!(
            "  端到端延迟（改文件 → 索引可见）：{elapsed:?}",
        );
        assert!(
            elapsed < Duration::from_secs(1),
            "端到端延迟 {elapsed:?} 超过 1 秒（验收标准 #1）"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 被排除的路径**不该**进增量更新 —— 一个都不该。
    ///
    /// 【为什么这条要钉死】验收标准：「排除目录里的变化不触发任何索引更新」。
    ///   `~/Library` 有 26.4 万条目、构建产物目录动辄几万个 ——
    ///   它们**一个字节都不进索引**，变化了也不该让引擎去 stat 它们。
    #[test]
    fn excluded_paths_never_reach_the_incremental_update() {
        let cache = ExcludedDirCache::new();
        let mk = |p: &str| {
            notify::Event::new(notify::EventKind::Create(notify::event::CreateKind::File))
                .add_path(PathBuf::from(p))
        };

        // 排除规则里的目录名（node_modules / target / Library …）
        for excluded in [
            "/Users/someone/app/node_modules/pkg/index.js",
            "/Users/someone/app/src-tauri/target/debug/deps/libfoo.dylib",
            "/Users/someone/Library/WebKit/com.sextant.macos/x.db",
            "/Users/someone/.minimax/v2/sessions/a/messages.jsonl",
        ] {
            assert!(
                indexable_paths(&mk(excluded), &cache).is_empty(),
                "「{excluded}」被排除了，不该进增量更新"
            );
        }

        // 正常路径 → 要放行
        let ok = indexable_paths(&mk("/Users/someone/Downloads/报告.md"), &cache);
        assert_eq!(ok.len(), 1, "正常路径必须放行 —— 否则索引永远不更新");
    }

    /// 一次事件里**混着**排除路径与正常路径时，只放行正常的那几个。
    ///
    /// 【为什么重要】FSEvents 一次会带上一串路径。
    ///   旧判据是「只要有一个能索引，整批就算数」——
    ///   那会把**不该索引的路径**也塞进增量更新 → 白做功。
    #[test]
    fn only_the_indexable_paths_of_a_mixed_event_are_kept() {
        let cache = ExcludedDirCache::new();
        let ev = notify::Event::new(notify::EventKind::Create(notify::event::CreateKind::File))
            .add_path(PathBuf::from("/Users/someone/Library/x.db"))
            .add_path(PathBuf::from("/Users/someone/Downloads/keep.md"))
            .add_path(PathBuf::from("/Users/someone/app/node_modules/y.js"));
        let kept = indexable_paths(&ev, &cache);
        assert_eq!(kept.len(), 1, "只该留下那一个正常路径");
        assert!(kept[0].ends_with("keep.md"));
    }

    // ── 事件名 ──

    /// 事件名要钉死 —— Rust 发、前端收，两边不一致就「事件发了但没人听」。
    #[test]
    fn the_event_name_is_index_auto_refreshed() {
        assert_eq!(AUTO_REFRESH_EVENT, "index-auto-refreshed");
    }

    // ── 信号（回调线程写 / 对账线程读）──

    /// 记一次事件 → 脏标志置起、时间戳记下。
    #[test]
    fn marking_an_event_sets_dirty_and_the_timestamp() {
        let s = Signals::new();
        assert!(!s.is_dirty());
        assert!(s.last_event_at().is_none());

        s.mark();
        assert!(s.is_dirty());
        assert!(s.last_event_at().is_some());
    }

    /// 一批事件里**只有第一个**会返回 true —— 回调用它决定要不要打日志。
    ///
    /// 【为什么重要】一次保存会产生好几个事件（create / write / rename）。
    ///   全打一遍日志会被淹掉（「自动刷新为什么触发了」就查不出来了）。
    #[test]
    fn only_the_first_event_of_a_batch_is_reported() {
        let s = Signals::new();
        assert!(s.mark(), "第一个事件应该是这一批的头");
        assert!(!s.mark(), "后面的事件不该再报一次");
        assert!(!s.mark(), "后面的事件不该再报一次");
    }

    /// 取走脏标志 → 标志清掉、返回原来是不是脏的。
    #[test]
    fn taking_dirty_clears_the_flag() {
        let s = Signals::new();
        s.mark();
        assert!(s.take_dirty());
        assert!(!s.take_dirty(), "取过一次之后就不该还脏着");
        assert!(!s.is_dirty());
    }

    /// 取走脏标志**不清时刻** —— 覆盖判断还要用它。
    #[test]
    fn taking_dirty_keeps_the_timestamp() {
        let s = Signals::new();
        s.mark();
        let before = s.last_event_at().expect("刚记过事件");
        assert!(s.take_dirty());
        assert_eq!(
            s.last_event_at(),
            Some(before),
            "取走脏标志不该把事件时刻一起清掉（覆盖判断要用它）"
        );
    }

    /// 事件时间戳不会比「现在」更晚 —— 毫秒换算不该引入未来的时刻。
    #[test]
    fn the_event_timestamp_is_not_in_the_future() {
        let s = Signals::new();
        s.mark();
        let at = s.last_event_at().expect("刚记过事件");
        assert!(at <= Instant::now(), "事件时刻不该在未来");
    }

    /// 【MX5】「取走脏标志」必须用**原子 RMW** —— 换成 load + store 会丢事件。
    ///
    /// 【怎么验】一个线程连续置位、另一个线程连续取走，
    ///   最后比对「**一共记了 N 次事件**」与「打包字里的**事件序号**」。
    ///   序号只增不减（只有 `mark` 会改它）→ 两者必须相等。
    ///   换成 load + store(false) 时：取走与置位**撞上**，
    ///   那一次置位会被**回滚**（连序号一起）→ 序号对不上 → 这条会红 ✓
    ///
    /// 【为什么不用 sleep 撞时序】撞不准、还会 flaky；
    ///   这里让两个线程**真并发**地抢同一条缓存行 ——
    ///   缓存行来回失效的那几十纳秒，就是「load 与 store 之间」的窗口。
    #[test]
    fn a_mark_racing_with_a_take_is_never_swallowed() {
        const MARKS: u64 = 200_000;

        let s = Arc::new(Signals::new());
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));

        // 消费者：不停地取走脏标志（模拟对账线程每一轮都在清脏）
        let consumer = {
            let s = Arc::clone(&s);
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    let _ = s.take_dirty();
                }
            })
        };

        // 生产者：连续记 N 次事件
        for _ in 0..MARKS {
            s.mark();
        }

        stop.store(true, Ordering::Relaxed);
        consumer.join().expect("消费者线程");

        // 全部安静下来之后：序号必须等于「记过的次数」
        assert_eq!(
            s.event_seq(),
            MARKS,
            "事件序号对不上 —— 有事件被吞掉了（取走脏标志必须用原子 RMW，\
             换成 load + store(false) 就会在撞车时把那次置位一起抹掉）"
        );
    }

    /// 事件序号只增不减 —— 它是「有没有事件被吞掉」的独立参照。
    #[test]
    fn the_event_seq_only_grows() {
        let s = Signals::new();
        assert_eq!(s.event_seq(), 0);
        s.mark();
        assert_eq!(s.event_seq(), 1);
        s.mark();
        assert_eq!(s.event_seq(), 2);
        // 取走脏标志**不改**序号（只清脏标志那一位）
        assert!(s.take_dirty());
        assert_eq!(s.event_seq(), 2, "取走脏标志不该改事件序号");
    }

    // ── 对账（MX6）与「监听引擎的目录」（MX8） ──

    /// 假的监听目标 —— 记录「到底有没有真的 watch / unwatch」。
    #[derive(Default)]
    struct FakeTarget {
        watched: Vec<PathBuf>,
        unwatched: Vec<PathBuf>,
    }

    impl WatchTarget for FakeTarget {
        fn watch_path(&mut self, path: &Path) -> Result<(), String> {
            self.watched.push(path.to_path_buf());
            Ok(())
        }
        fn unwatch_path(&mut self, path: &Path) -> Result<(), String> {
            self.unwatched.push(path.to_path_buf());
            Ok(())
        }
    }

    /// 【MX6】监听列表变了 → 必须重配（先取消旧的、再加新的）。
    ///
    /// 【这条钉住的是「让对账永不重配」那个反证实验】
    ///   把重配那段删掉 → 这里会变红。
    #[test]
    fn the_watch_list_is_reconfigured_when_the_desired_list_changes() {
        let mut target = FakeTarget::default();
        let mut current = vec![PathBuf::from("/old")];
        let mut failed = HashSet::new();

        let changed = apply_watch_list(
            &mut target,
            &mut current,
            &[PathBuf::from("/new")],
            &mut failed,
        );
        assert!(changed, "监听列表变了就该重配");
        assert_eq!(target.unwatched, vec![PathBuf::from("/old")], "要先取消旧的");
        assert_eq!(target.watched, vec![PathBuf::from("/new")], "再加新的");
        assert_eq!(current, vec![PathBuf::from("/new")]);

        // 再来一次**同样**的 desired → 不该再动（否则每秒都在重配）
        let changed_again = apply_watch_list(
            &mut target,
            &mut current,
            &[PathBuf::from("/new")],
            &mut failed,
        );
        assert!(!changed_again, "列表没变就不该重配");
        assert_eq!(target.watched.len(), 1, "没变就不该再 watch 一遍");
    }

    /// 造一个 mock app —— 引擎槽位 + 索引状态都要 manage。
    fn build_mock_app() -> tauri::App<tauri::test::MockRuntime> {
        tauri::test::mock_builder()
            .manage(Mutex::new(Option::<Arc<Searcher>>::None))
            .manage(crate::index_state::IndexState::new())
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .expect("构建 mock app")
    }

    /// 【MX8】要监听的是**引擎实际在索引的目录**，不是设置里配的那个。
    ///
    /// 【这条钉住的是「监听设置里的目录」那个反证实验】
    ///   改成读设置 → 这里会变红：
    ///   - 读得到设置的话，返回的是设置里的那份（与引擎的不一致 → 断言失败）；
    ///   - 读不到的话（mock app 没装 store 插件）→ 直接 panic → 也是失败。
    ///   两条路都红 ✓
    ///   （另外 `tests/watch_contract.rs` 里还有一道源码护栏盯着
    ///     「监听列表必须来自 `engine_snapshot`、不许碰 `index_paths`」。）
    #[test]
    fn the_watched_dirs_come_from_the_engine_not_the_settings() {
        let app = build_mock_app();
        // 引擎在索引 /engine/dir
        app.state::<crate::index_state::IndexState>()
            .commit_new_engine(vec!["/engine/dir".to_string()], 3);

        let desired = desired_watch_dirs(app.handle());
        assert_eq!(
            desired,
            vec![PathBuf::from("/engine/dir")],
            "必须监听【引擎实际在索引的目录】，不是设置里的"
        );
    }

    /// 【MX4】自动重扫完**真的**发出了 `index-auto-refreshed` 事件。
    ///
    /// 【为什么这条重要】Rust 发、前端收，两边各写各的就会「事件发了但没人听」——
    ///   而这**不会报错**，只是结果表永远不刷新。
    ///   以前只有「事件名的常量值」被钉住（两边同时改名，测试照样全绿）。
    ///
    /// 【怎么验】用 mock runtime 真发一次事件、用真的监听器收 ——
    ///   不是「检查源码里有没有 emit」那种护栏。
    #[test]
    fn auto_rescan_really_emits_the_auto_refresh_event() {
        // 造一个临时目录，里面放 3 个文件
        let dir = std::env::temp_dir().join(format!(
            "fsearch-watch-emit-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for i in 0..3 {
            std::fs::write(dir.join(format!("marker{i}.txt")), b"hello").unwrap();
        }
        let dir_str = dir.to_string_lossy().into_owned();

        let app = build_mock_app();
        let handle = app.handle().clone();

        // 监听「自动重扫完成」这个事件
        let (tx, rx) = mpsc::channel::<String>();
        handle.listen(AUTO_REFRESH_EVENT, move |event| {
            let _ = tx.send(event.payload().to_string());
        });

        // 装一个**真**引擎（自动重扫要真扫一遍）
        let refs: Vec<&Path> = vec![dir.as_path()];
        let (engine, n) = search_core::Engine::new(&refs).expect("建引擎");
        {
            let state = app.state::<Mutex<Option<Arc<Searcher>>>>();
            let mut slot = state.lock().unwrap();
            *slot = Some(Arc::new(Searcher::new(engine)));
        }
        app.state::<crate::index_state::IndexState>()
            .commit_new_engine(vec![dir_str], n);

        let indexed = auto_rescan(&handle).expect("自动重扫不该失败");
        assert!(indexed >= 3, "3 个文件应至少索引到 3 项，实际 {indexed}");

        // 事件必须真的发出来
        let payload = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("自动重扫后必须发出 index-auto-refreshed 事件");
        let got: serde_json::Value = serde_json::from_str(&payload).expect("载荷是 JSON");
        let want = serde_json::to_value(app.state::<crate::index_state::IndexState>().snapshot())
            .expect("状态可序列化");
        assert_eq!(got, want, "事件载荷必须是当前的索引状态");
    }

    // ── 噪声过滤：`.DS_Store` 这类 macOS 私有缓存文件 ──

    /// 造一个带指定路径的事件。
    fn ev_with_paths(paths: &[&str]) -> notify::Event {
        let mut ev = notify::Event::new(notify::EventKind::Any);
        ev.paths = paths.iter().map(PathBuf::from).collect();
        ev
    }

    /// `.DS_Store` 被创建 → 噪声（不该触发重扫）。
    #[test]
    fn creating_a_ds_store_is_noise() {
        let ev = ev_with_paths(&["/tmp/x/.DS_Store"]);
        assert!(
            is_noise_event(&ev),
            ".DS_Store 的创建是 macOS 的私有缓存，不该换来一次全量重扫"
        );
    }

    /// `.DS_Store` 的元数据被改（Finder 刷新目录视图）→ 噪声。
    /// 【这条是真实观测到的那个噪声】
    /// 实测日志：`[watch] 收到文件变化：["…/.DS_Store"] (Modify(Metadata(Any)))`
    #[test]
    fn touching_a_ds_store_metadata_is_noise() {
        let ev = ev_with_paths(&["/tmp/x/.DS_Store"]);
        assert!(is_noise_event(&ev));
    }

    /// 子目录里的 `.DS_Store` → 也是噪声（比的是**文件名**，不是完整路径）。
    #[test]
    fn a_ds_store_in_a_subdirectory_is_noise() {
        let ev = ev_with_paths(&["/tmp/x/deep/nested/.DS_Store"]);
        assert!(is_noise_event(&ev));
    }

    /// 普通文件 → **不是**噪声。
    #[test]
    fn an_ordinary_file_is_not_noise() {
        let ev = ev_with_paths(&["/tmp/x/marker.txt"]);
        assert!(
            !is_noise_event(&ev),
            "真实文件的变化必须照常触发重扫"
        );
    }

    /// 名字**像**但不是 `.DS_Store` 的文件 → 不是噪声。
    /// （比如 `.DS_Store.bak`、`.DS_Store2`、`my.DS_Store`）
    #[test]
    fn a_file_whose_name_merely_looks_like_ds_store_is_not_noise() {
        for name in [".DS_Store.bak", ".DS_Store2", "my.DS_Store", "DS_Store"] {
            let ev = ev_with_paths(&[&format!("/tmp/x/{name}")]);
            assert!(
                !is_noise_event(&ev),
                "「{name}」不是 macOS 的那个缓存文件，不该被滤掉"
            );
        }
    }

    /// **重命名** `.DS_Store` → `foo`：**不是**噪声。
    /// 【为什么重要】`foo` 是个**真文件**，它出现了 —— 滤掉就等于丢索引。
    /// 这是「所有路径都是 `.DS_Store` 才算噪声」这条规则存在的全部理由。
    #[test]
    fn renaming_a_ds_store_away_is_not_noise() {
        let ev = ev_with_paths(&["/tmp/x/.DS_Store", "/tmp/x/foo"]);
        assert!(
            !is_noise_event(&ev),
            "重命名出来的真文件必须照常触发重扫"
        );
    }

    /// **重命名** `foo` → `.DS_Store`：**不是**噪声。
    /// 【为什么】重命名会让 `foo` **消失** —— 目录里少了一个**真文件**，
    ///   那是真实变化，滤掉就等于索引里留着一个已经不存在的 `foo`。
    ///   （方向反过来的 `.DS_Store` → `foo` 同理：`foo` 出现了。）
    /// → **所以「所有路径都是 `.DS_Store`」这条规则是必须的**：
    ///   重命名事件带两条路径，**任一端是真文件**就说明有真实变化。
    #[test]
    fn renaming_a_file_into_a_ds_store_is_not_noise() {
        let ev = ev_with_paths(&["/tmp/x/foo", "/tmp/x/.DS_Store"]);
        assert!(
            !is_noise_event(&ev),
            "重命名让真文件 foo 消失了 —— 那是真实变化，不该被滤掉"
        );
    }

    /// 空路径表 → **不滤**（没有依据说它是噪声 —— 宁可多扫一次）。
    #[test]
    fn an_event_without_paths_is_not_noise() {
        let ev = notify::Event::new(notify::EventKind::Any);
        assert!(
            !is_noise_event(&ev),
            "没有路径就没法判断 —— 不能瞎滤"
        );
    }

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
            decide_rescan(&inp, true, false, || true).decision,
            RescanDecision::Paused
        );
    }

    #[test]
    fn not_idle_keeps_the_old_behavior() {
        let now = Instant::now();
        let inp = base_inputs(true, now);
        assert_eq!(
            decide_rescan(&inp, false, false, || true).decision,
            RescanDecision::Rescan
        );
    }

    #[test]
    fn force_scans_even_when_idle() {
        // 【竞态安全】watcher 读「空闲」与读「补扫请求」是**两个不同时刻** ——
        // 完全可能读到「陈旧的 idle = true」配「新鲜的 force = true」。
        // force 必须压过 idle —— 否则补扫要等到下一个文件事件才发生。
        let now = Instant::now();
        let inp = base_inputs(true, now);
        assert_eq!(
            decide_rescan(&inp, true, true, || true).decision,
            RescanDecision::Rescan
        );
    }

    #[test]
    fn force_skips_the_scan_when_nothing_changed() {
        // 【这条测试的语义在本次改动中变了 —— 改的是「行为」，不是「测试」】
        //
        //   原来：唤醒时的补扫**无条件**全量重扫（哪怕文件系统一个字节都没变）。
        //   现在：没脏 → **不扫**。
        //
        //   【为什么这个改动是对的】
        //   补扫的语义是「把 idle 期攒下的**变化**收进索引」。
        //   没有变化可收 → 全量遍历 $HOME 一遍，结果和现有索引一模一样 = 白扫。
        //   而实测：这一趟白扫要 1.7 秒 CPU / 2~3 秒墙钟，
        //   唤醒瞬间峰值 28.7%（主进程独占）。
        //
        //   【为什么不会漏掉变化】
        //   「脏」是 watcher **真的收到 FSEvents 事件**才置上的，
        //   而 watcher 常驻、监听的就是引擎索引的那批目录 →
        //   idle 期的变化一定被记下来了 → 没脏 = 确实没变化。
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
            decide_rescan(&inp, false, true, || true).decision,
            RescanDecision::Covered,
            "文件没变 → 唤醒不该触发全量重扫（那是纯浪费）"
        );
    }

    #[test]
    fn force_still_scans_when_there_is_a_real_change() {
        // 【正确性保底 —— 这条是本次改动的「不能变坏」的证明】
        //   idle 期真的改了文件（有脏、有事件时刻、且没被已完成的扫描覆盖）
        //   → 唤醒时**必须**重扫，否则索引会静默过期。
        let now = Instant::now();
        let inp = RescanInputs {
            dirty: true,
            // 事件发生在「最近一次已完成的扫描」**之前** → 没被覆盖
            last_event_at: Some(now - Duration::from_secs(30)),
            now,
            last_auto_scan_at: Some(now - Duration::from_secs(60)),
            // 已完成的扫描比事件**更早** → 覆盖不到这次变化
            last_scan_completed_at: Some(now - Duration::from_secs(60)),
            scan_in_flight: false,
        };
        assert_eq!(
            decide_rescan(&inp, false, true, || true).decision,
            RescanDecision::Rescan,
            "有真实且未覆盖的变化 → 唤醒时必须重扫（索引不能静默过期）"
        );
    }

    #[test]
    fn force_skips_the_scan_when_the_change_is_already_covered() {
        // 【本次改动要挡掉的第二个浪费：���用户点一次刷新，紧接着唤醒」】
        //   刷新 = 一趟全量重扫，它已经把变化收进索引了；
        //   紧接着唤醒的补扫**再来一遍** = 第一趟白扫（复审 Important-1 那个坑，
        //   当时只修了自动重扫这条路，唤醒这条路漏了）。
        let now = Instant::now();
        let inp = RescanInputs {
            dirty: true,
            // 事件发生在扫描**完成之前** → 那次扫描完成时它已经在索引里了
            last_event_at: Some(now - Duration::from_secs(10)),
            now,
            last_auto_scan_at: Some(now - Duration::from_secs(30)),
            // 扫描在事件**之后**完成 → 覆盖了这次变化
            last_scan_completed_at: Some(now - Duration::from_secs(5)),
            scan_in_flight: false,
        };
        assert_eq!(
            decide_rescan(&inp, false, true, || true).decision,
            RescanDecision::Covered,
            "变化已经被某趟已完成的扫描覆盖 → 不用再扫一遍"
        );
    }

    #[test]
    fn force_scans_when_the_change_is_too_fresh_to_have_been_scanned_yet() {
        // 【最严的那个时序 —— 这条是本次改动的「不能漏」底线】
        //
        //   变化刚刚发生（100ms 前），自动重扫**还没轮到**（静默期 800ms 没到）
        //   → 用户这时唤醒
        //   → 那批变化**还没有任何一趟扫描收过** → 补扫**必须**发生。
        //
        //   （实测场景：黑盒测到 AFTER 少扫一趟、索引项数与 BEFORE 相同，
        //     但那只覆盖了「自动重扫先跑掉了变化」的时序。
        //     这一条覆盖的是相反的时序 —— 自动重扫还没来得及跑。）
        let now = Instant::now();
        let inp = RescanInputs {
            dirty: true,
            // 事件刚发生 —— 静默期内，自动重扫还没跑
            last_event_at: Some(now - Duration::from_millis(100)),
            now,
            last_auto_scan_at: Some(now - Duration::from_secs(60)),
            // 上次扫描在 60s 前 → 远早于这次变化 → **没覆盖**
            last_scan_completed_at: Some(now - Duration::from_secs(60)),
            scan_in_flight: false,
        };
        assert_eq!(
            decide_rescan(&inp, false, true, || true).decision,
            RescanDecision::Rescan,
            "变化还没被任何扫描收过 → 唤醒时必须补扫（否则这批变化就没人收了）"
        );
    }

    #[test]
    fn force_does_not_swallow_the_change_when_no_scan_has_ever_run() {
        // 从没成功扫过（last_scan_completed_at = None）
        // → 没有任何一趟扫描能覆盖这次变化 → 必须扫。
        let now = Instant::now();
        let inp = RescanInputs {
            dirty: true,
            last_event_at: Some(now - Duration::from_secs(5)),
            now,
            last_auto_scan_at: None,
            last_scan_completed_at: None,
            scan_in_flight: false,
        };
        assert_eq!(
            decide_rescan(&inp, false, true, || true).decision,
            RescanDecision::Rescan,
            "从没扫过 → 变化肯定没被覆盖 → 必须扫"
        );
    }

    #[test]
    fn force_and_auto_rescan_agree_on_what_counts_as_covered() {
        // 【判据只有一份】—— 两道闸必须给出**同一个**答案。
        //   以前唤醒补扫绕过了「覆盖」闸 → 两处判据各写各的 → 会漂移。
        let now = Instant::now();
        let mut inp = base_inputs(true, now);
        // 事件发生在扫描完成**之前** → 已被那次扫描覆盖
        inp.last_event_at = Some(now - Duration::from_secs(10));
        inp.last_scan_completed_at = Some(now - Duration::from_secs(5));

        // 自动重扫那条路：覆盖 → Covered
        assert_eq!(
            decide_auto_rescan(&inp, || true),
            RescanDecision::Covered,
            "自动重扫：变化已被覆盖"
        );
        // 唤醒补扫那条路：也必须是 Covered（而不是各判各的）
        assert_eq!(
            decide_rescan(&inp, false, true, || true).decision,
            RescanDecision::Covered,
            "唤醒补扫：判据必须与自动重扫一致"
        );
    }

    #[test]
    fn force_still_respects_the_auto_refresh_switch() {
        // 用户关了自动刷新 → 不该自动扫（哪怕是唤醒时的补扫）。
        let now = Instant::now();
        let inp = base_inputs(true, now);
        assert_eq!(
            decide_rescan(&inp, false, true, || false).decision,
            RescanDecision::Disabled
        );
    }

    #[test]
    fn force_waits_while_a_scan_is_in_flight() {
        let now = Instant::now();
        let mut inp = base_inputs(true, now);
        inp.scan_in_flight = true;
        assert_eq!(
            decide_rescan(&inp, false, true, || true).decision,
            RescanDecision::Wait
        );
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
        assert_eq!(
            decide_rescan(&inp, false, false, || true).decision,
            RescanDecision::Wait
        );
        // 带 force → 直接扫
        assert_eq!(
            decide_rescan(&inp, false, true, || true).decision,
            RescanDecision::Rescan
        );
    }

    /// 【终审 M-6 的接线层护栏】决策与动作必须用**同一个** `force`。
    ///
    /// 【为什么以前测不出来】
    ///   `decide_rescan()` 返回决策、`rescan_action()` 又单独收一个 `force` ——
    ///   两个都是 `bool`，调用点写错**编译器不会报**，
    ///   而所有单测照样全绿（把调用点的 `force` 换成 `false` 也一样绿）。
    ///   → 现在 `force` 只从 [`RescanOutcome`] 里取，**没有第二个来源**。
    ///   这条用例钉住「outcome 带的就是决策时用的那个 force」。
    #[test]
    fn the_outcome_carries_the_force_the_decision_used() {
        let now = Instant::now();
        let mut inp = base_inputs(true, now);
        // 有扫描在飞 → 决策是 Wait；force=true 时要把补扫请求放回去。
        inp.scan_in_flight = true;
        let outcome = decide_rescan(&inp, false, true, || true);
        assert_eq!(outcome.decision, RescanDecision::Wait);
        assert!(
            outcome.force,
            "outcome 必须带着**决策时用的那个** force —— 不能是别的值"
        );
        let action = rescan_action(outcome);
        assert!(
            action.restore_catchup,
            "Wait + force → 必须把补扫请求放回去（否则这次唤醒的补扫被吞掉）"
        );
    }

    // ---- 后台常驻模式：决策 → 动作（副作用）也要钉住 ----    //
    // 【为什么要这一层】`decide_rescan` 回答「**要不要扫**」，
    //   但「扫之前要不要清脏标志」「没扫成要不要把补扫请求放回去」
    //   这两个副作用如果只活在 `reconcile_loop` 的 match 分支里，
    //   **删掉任何一行，所有测试仍然全绿** ——
    //   而丢的是**文件变化**（索引静默过期），不是文案瑕疵。
    //   → 抽成 `rescan_action`（纯函数：决策 → 动作）
    //     + `apply_rescan_action`（真的把副作用做一遍）→ 两层都能断言。

    /// 契约表 —— 「决策 + force → 动作」的完整预期。
    ///
    /// 【为什么用表驱动】这张表就是 `rescan_action` 的**契约**，
    ///   **每一格都要被断言** —— 少一格就有一条语义没人盯。
    const ACTION_CONTRACT: &[(RescanDecision, bool, RescanAction)] = &[
        // (决策, force, 动作)
        //
        // ── Rescan：真的扫 → 清脏（清脏在重扫【之前】，不丢重扫期间的新事件）
        (
            RescanDecision::Rescan,
            false,
            RescanAction {
                rescan: true,
                clear_dirty: true,
                restore_catchup: false,
            },
        ),
        (
            RescanDecision::Rescan,
            true,
            RescanAction {
                rescan: true,
                clear_dirty: true,
                restore_catchup: false,
            },
        ),
        // ── Wait + force：还有扫描在飞 → 这轮没扫成 → **把补扫请求放回去**
        (
            RescanDecision::Wait,
            true,
            RescanAction {
                rescan: false,
                clear_dirty: false,
                restore_catchup: true,
            },
        ),
        // ── Wait + 无 force：没有待消费的补扫请求 → 什么都不用放回
        (
            RescanDecision::Wait,
            false,
            RescanAction {
                rescan: false,
                clear_dirty: false,
                restore_catchup: false,
            },
        ),
        // ── Covered：这次变化已经被扫过了 → 清脏、不用扫
        (
            RescanDecision::Covered,
            false,
            RescanAction {
                rescan: false,
                clear_dirty: true,
                restore_catchup: false,
            },
        ),
        (
            RescanDecision::Covered,
            true,
            RescanAction {
                rescan: false,
                clear_dirty: true,
                restore_catchup: false,
            },
        ),
        // ── Disabled：开关关着 → 清脏、不用扫
        (
            RescanDecision::Disabled,
            false,
            RescanAction {
                rescan: false,
                clear_dirty: true,
                restore_catchup: false,
            },
        ),
        (
            RescanDecision::Disabled,
            true,
            RescanAction {
                rescan: false,
                clear_dirty: true,
                restore_catchup: false,
            },
        ),
        // ── Paused：应用空闲 → **不清脏**（空闲期间的变化不丢）、不扫
        (
            RescanDecision::Paused,
            false,
            RescanAction {
                rescan: false,
                clear_dirty: false,
                restore_catchup: false,
            },
        ),
        (
            RescanDecision::Paused,
            true,
            RescanAction {
                rescan: false,
                clear_dirty: false,
                restore_catchup: false,
            },
        ),
    ];

    /// 契约表**逐格**钉住 —— 少一格就有一条语义没人盯。
    #[test]
    fn the_action_contract_is_pinned_cell_by_cell() {
        for (decision, force, want) in ACTION_CONTRACT {
            let got = rescan_action(RescanOutcome {
                decision: *decision,
                force: *force,
            });
            assert_eq!(
                got,
                *want,
                "决策 {decision:?} + force={force} 的动作不对：\
                 期望 rescan={} clear_dirty={} restore_catchup={}，\
                 实际 rescan={} clear_dirty={} restore_catchup={}",
                want.rescan,
                want.clear_dirty,
                want.restore_catchup,
                got.rescan,
                got.clear_dirty,
                got.restore_catchup,
            );
        }
    }

    /// 语义二：**空闲时不能清脏标志** —— 空闲期间攒下的变化不能丢。
    ///
    /// 【这条钉的是「决策之后做了什么」，不是「决策结果」】
    ///   前面 7 个用例测的是 `decide_rescan` 的**返回值**；
    ///   这一条测的是**副作用真的发生了没有** ——
    ///   以前这两个副作用只活在 `reconcile_loop` 的 match 分支里，
    ///   在那儿加一行 `signals.take_dirty();` 不会有任何测试变红。
    #[test]
    fn pausing_really_keeps_the_dirty_flag() {
        let signals = Signals::new();
        signals.mark();
        assert!(signals.is_dirty(), "先确认它是脏的");

        let life = crate::lifecycle::Lifecycle::new();
        let action = rescan_action(RescanOutcome {
            decision: RescanDecision::Paused,
            force: false,
        });
        let rescan = apply_rescan_action(action, &signals, &life);

        assert!(!rescan, "空闲时不该重扫");
        assert!(
            signals.is_dirty(),
            "空闲时**不能**清脏标志 —— 空闲期间的变化会被静默丢掉"
        );
    }

    /// 语义三：**补扫请求没消费掉 → 真的放回去**（下轮再试）。
    ///
    /// 【怎么验「真的放回去」】`take_catchup()` 是「取出并清零」——
    ///   所以「放回去」= 执行完动作之后，**再取一次还能取到**。
    #[test]
    fn a_pending_catchup_is_really_restored_when_the_scan_cannot_run() {
        let life = crate::lifecycle::Lifecycle::new();
        life.request_catchup();
        // 模拟 reconcile_loop 的读法：取出并清零
        assert!(life.take_catchup(), "先消费掉这个补扫请求");

        let signals = Signals::new();
        signals.mark();
        let action = rescan_action(RescanOutcome {
            decision: RescanDecision::Wait,
            force: true,
        });
        let rescan = apply_rescan_action(action, &signals, &life);

        assert!(!rescan, "有扫描在飞 → 这一轮不扫");
        assert!(
            life.take_catchup(),
            "补扫请求必须放回去 —— 否则这次唤醒的补扫被吞掉，\
             要等到下一个文件事件才发生"
        );
        assert!(
            signals.is_dirty(),
            "还没扫 → 不能清脏（不丢事件）"
        );
    }

    /// 没有待消费的补扫请求时 → **不凭空造一个**出来。
    #[test]
    fn nothing_is_restored_when_no_catchup_was_pending() {
        let life = crate::lifecycle::Lifecycle::new();
        let signals = Signals::new();
        let action = rescan_action(RescanOutcome {
            decision: RescanDecision::Wait,
            force: false,
        });
        let _ = apply_rescan_action(action, &signals, &life);
        assert!(
            !life.take_catchup(),
            "本来就没有补扫请求，不该凭空造一个"
        );
    }

    /// 语义一（force 压过 idle）的**动作**那一半：真的扫 → 清脏（且在重扫之前）。
    #[test]
    fn a_forced_rescan_really_clears_the_dirty_flag() {
        let signals = Signals::new();
        signals.mark();
        let life = crate::lifecycle::Lifecycle::new();
        let action = rescan_action(RescanOutcome {
            decision: RescanDecision::Rescan,
            force: true,
        });
        let rescan = apply_rescan_action(action, &signals, &life);

        assert!(rescan, "强制补扫 → 要真的扫");
        assert!(
            !signals.is_dirty(),
            "重扫**之前**必须清脏 —— 重扫期间来的新事件才能重新置脏"
        );
        assert!(
            !life.take_catchup(),
            "这次补扫请求已经被消费掉了，不用再放回"
        );
    }

    /// 「已被覆盖」→ 清脏、不用扫 —— 清脏这一半是**真的**发生了。
    #[test]
    fn the_covered_case_really_clears_the_dirty_flag() {
        let signals = Signals::new();
        signals.mark();
        let life = crate::lifecycle::Lifecycle::new();
        let action = rescan_action(RescanOutcome {
            decision: RescanDecision::Covered,
            force: false,
        });
        let rescan = apply_rescan_action(action, &signals, &life);

        assert!(!rescan, "已被覆盖 → 不用再扫");
        assert!(!signals.is_dirty(), "已被覆盖 → 清脏标志");
    }

    /// 「开关关着」→ 清脏、不用扫 —— 清脏这一半是**真的**发生了。
    #[test]
    fn the_disabled_case_really_clears_the_dirty_flag() {
        let signals = Signals::new();
        signals.mark();
        let life = crate::lifecycle::Lifecycle::new();
        let action = rescan_action(RescanOutcome {
            decision: RescanDecision::Disabled,
            force: false,
        });
        let rescan = apply_rescan_action(action, &signals, &life);

        assert!(!rescan, "开关关着 → 不用扫");
        assert!(!signals.is_dirty(), "开关关着 → 清脏标志（别每一轮都重新判断）");
    }

    // ---- 排除目录缓存 ----

    /// 缓存命中 = 落在已知被排除的目录下。
    #[test]
    fn a_remembered_directory_covers_its_subtree() {
        let c = ExcludedDirCache::new();
        assert!(!c.hits(Path::new("/a/b/c.txt")), "空缓存不该命中");
        c.remember(Path::new("/a/b"));
        assert!(c.hits(Path::new("/a/b")), "它自己该命中");
        assert!(c.hits(Path::new("/a/b/c.txt")), "它的子路径该命中");
        assert!(c.hits(Path::new("/a/b/c/d/e.txt")), "再深一层也该命中");
    }

    /// 【最容易写错的一条】命中必须是**组件级**的，不能是字符串前缀 ——
    ///   否则 `/a/bc` 会被 `/a/b` 命中 → **漏掉一个该索引的目录**。
    #[test]
    fn a_sibling_with_a_shared_string_prefix_is_not_covered() {
        let c = ExcludedDirCache::new();
        c.remember(Path::new("/a/b"));
        assert!(
            !c.hits(Path::new("/a/bc")),
            "`/a/bc` 与 `/a/b` 只是字符串前缀相同 —— 不能命中"
        );
        assert!(!c.hits(Path::new("/a")), "父目录不该被子目录罩住");
        assert!(!c.hits(Path::new("/x/y/z")), "无关路径不该命中");
    }

    /// 缓存满了要清空（否则线性查找会越来越贵）——
    ///   但**正确性不受影响**（只是命中率下降）。
    #[test]
    fn the_cache_is_bounded() {
        let c = ExcludedDirCache::new();
        for i in 0..EXCLUDED_CACHE_MAX {
            c.remember(Path::new(&format!("/dir{i}")));
        }
        assert!(c.hits(Path::new("/dir0/x")), "还没满 → 条目都在");
        // 再加一个 → 满了 → 清空重来
        c.remember(Path::new("/overflow"));
        assert!(
            !c.hits(Path::new("/dir0/x")),
            "满了之后清空 → 最早的条目失效（只是命中率下降，不影响正确性）"
        );
        assert!(c.hits(Path::new("/overflow/x")), "新加的这条在");
    }

    /// 【一致性】缓存判「排除」与直接问引擎，结果必须一致 ——
    ///   不一致就意味着**漏索引**或**多余的空转重扫**。
    ///
    ///   这里用**引擎自己会排除的路径**（隐藏目录 `/.fsearch-cache-probe/`）
    ///   与**它不会排除的路径**各测一遍。
    #[test]
    fn the_cache_agrees_with_the_engine() {
        let c = ExcludedDirCache::new();
        let hidden = Path::new("/tmp/.fsearch-cache-probe/sub/file.txt");
        let normal = Path::new("/tmp/fsearch-cache-probe-normal/file.txt");

        // 先问引擎（建立基线），再问缓存 —— 两次必须一致
        let engine_hidden = search_core::Engine::path_is_excluded(hidden);
        let cache_hidden = c.is_excluded(hidden);
        assert_eq!(
            cache_hidden, engine_hidden,
            "缓存判「排除」与问引擎必须一致（不一致 = 漏索引）"
        );
        // 第一次调用会把「最上层的被排除祖先」记进缓存
        assert!(c.hits(Path::new("/tmp/.fsearch-cache-probe")));
        // 第二次走缓存 → 仍然一致
        assert_eq!(
            c.is_excluded(hidden),
            engine_hidden,
            "命中缓存后的答案也必须与引擎一致"
        );
        // 没被排除的路径不该被缓存命中
        assert_eq!(
            c.is_excluded(normal),
            search_core::Engine::path_is_excluded(normal),
            "没被排除的路径，缓存与引擎也要一致"
        );
    }
}
