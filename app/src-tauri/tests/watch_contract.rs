//! 护栏 —— 「文件变化时自动刷新索引」这条链路的**契约**。
//!
//! 【为什么要有这个文件】
//!   批次 D 的独立复审做了 5 条变异（MX4~MX8），**全部 55/55 全绿** ——
//!   也就是说「Rust 侧真的发了事件」「脏标志是原子取的」「对账真的会重配」
//!   「开关在 Rust 侧真的生效」「监听的是引擎的目录」这五条**一条都没被钉住**。
//!   这五条全是**静默失效**型：变了不报错，只是功能悄悄死掉。
//!
//!   现在 `watcher.rs` 里已经补了**真测试**（决策纯函数、并发压测、
//!   mock runtime 真发真收事件、假的监听目标）。
//!   但真测试只能钉住「函数内部」的改动 ——
//!   **调用点**上的改动（比如把 `|| settings::load(&app).auto_refresh`
//!   换成 `|| true`）真测试是抓不到的（测试直接喂参数，不走调用点）。
//!   这个文件就是补那一块：**读源码，断言契约还在**。
//!
//! 【判据】—— 下面每一条都注明了「怎么改会红」：
//!   ① 真的 emit 了 `AUTO_REFRESH_EVENT`（删掉 emit → 红）
//!   ② `take_dirty` 用的是**原子 RMW**（换成 load + store → 红）
//!   ③ 对账真的会重配（去掉重配 → 红）
//!   ④ 开关在 Rust 侧真的生效（忽略 `auto_refresh` → 红）
//!   ⑤ 监听列表来自**引擎**（改成设置里的目录 → 红）
//!   ⑥ 噪声过滤器**真的被回调调用了**（摘掉过滤 → 红）
//!
//! 【为什么要先去掉注释再断言】
//!   这些契约在源码里都有**大段解释性注释**——
//!   直接对原文断言，注释里出现同一个词就会「假绿」。
//!   所以先把 `//` 之后的部分去掉，只对**真正的代码**断言。

use std::sync::OnceLock;

/// watcher 源码 —— 编译期就嵌进来（不会读到别的副本）。
const WATCHER_SRC: &str = include_str!("../src/watcher.rs");

/// 去掉行注释后的源码（`//` 之后的部分）。
///
/// 【为什么要去注释】见文件开头的说明。
fn code_only() -> &'static str {
    static CODE: OnceLock<String> = OnceLock::new();
    CODE.get_or_init(|| {
        WATCHER_SRC
            .lines()
            .map(|line| match line.find("//") {
                Some(i) => &line[..i],
                None => line,
            })
            .collect::<Vec<_>>()
            .join("\n")
    })
}

/// 某个函数体（从 `fn 名字` 到下一个顶层 `fn` / `impl` / 结尾）。
fn fn_body(src: &str, name: &str) -> String {
    let start = src
        .find(&format!("fn {name}"))
        .unwrap_or_else(|| panic!("源码里找不到 `fn {name}` —— 改名了？护栏要跟着改"));
    let rest = &src[start..];
    // 下一个顶层 `fn `（缩进为 0）就是边界
    let end = rest
        .find("\nfn ")
        .or_else(|| rest.find("\nimpl "))
        .unwrap_or(rest.len());
    rest[..end].to_string()
}

/// ① 自动重扫完**真的**发出了 `index-auto-refreshed`。
///
/// 【怎么改会红】把 `app.emit(AUTO_REFRESH_EVENT, snapshot)` 删掉
///   （或把事件名改成别的）。
#[test]
fn the_auto_refresh_event_is_really_emitted() {
    let code = code_only();
    assert!(
        code.contains("emit(AUTO_REFRESH_EVENT"),
        "watcher 里找不到 `emit(AUTO_REFRESH_EVENT, …)` —— \
         自动重扫完不通知前端的话，结果表永远不会自动刷新（而且**不会报错**）。"
    );
}

/// ② `take_dirty` 必须用**原子 RMW**，不能先 load 再 store。
///
/// 【怎么改会红】把 `fetch_and` 换成 `load` + `store(false)`：
///   那两步之间来的事件会被一起清掉 → **永远扫不到**。
#[test]
fn taking_the_dirty_flag_is_an_atomic_read_modify_write() {
    let code = code_only();
    let body = fn_body(code, "take_dirty");
    let uses_rmw = body.contains(".swap(")
        || body.contains(".fetch_and(")
        || body.contains(".fetch_update(")
        || body.contains(".fetch_or(");
    assert!(
        uses_rmw,
        "`take_dirty` 里没有原子 RMW（swap / fetch_and / fetch_update）—— \
         换成 load + store(false) 的话，置位与取走撞上时那个事件会被吞掉 \
         （注释里明说不能这么写）。"
    );
    assert!(
        !body.contains(".store(false)"),
        "`take_dirty` 里出现了 `store(false)` —— 那是「先 load 再清」的写法，\
         中间来的事件会被吞掉。必须用原子 RMW。"
    );
}

/// ③ 对账真的会重配监听列表。
///
/// 【怎么改会红】把 `apply_watch_list(...)` 的调用删掉，
///   或者让它永远返回 false（= 永不重配）。
#[test]
fn the_watch_list_is_reconciled() {
    let code = code_only();
    assert!(
        code.contains("apply_watch_list("),
        "watcher 里找不到 `apply_watch_list(...)` 的调用 —— \
         监听列表就再也不会跟着引擎的索引目录变（换目录后自动刷新直接失效，\
         而且**不会报错**）。"
    );
    let body = fn_body(code, "apply_watch_list");
    assert!(
        body.contains("watch_path(") && body.contains("unwatch_path("),
        "`apply_watch_list` 里没有真的 watch / unwatch —— 对账成了空转。"
    );
}

/// ④ 开关「文件变化时自动刷新索引」在 Rust 侧**真的**生效。
///
/// 【怎么改会红】把 `|| crate::settings::load(&app).auto_refresh`
///   换成 `|| true`（= 忽略开关，永远重扫）。
#[test]
fn the_auto_refresh_switch_is_read_from_the_settings() {
    let code = code_only();
    assert!(
        code.contains(".auto_refresh"),
        "watcher 里没有读 `auto_refresh` —— \
         「文件变化时自动刷新索引」这个开关在 Rust 侧就没生效 \
         （关掉之后照样重扫，CPU 白烧）。"
    );
    assert!(
        code.contains("settings::load("),
        "watcher 里没有读设置 —— 开关的值拿不到。"
    );
}

/// ⑤ 监听列表来自**引擎实际在索引的目录**，不是设置里配的那个。
///
/// 【怎么改会红】把 `engine_snapshot()` 换成设置里的 `index_paths`。
#[test]
fn the_watch_list_comes_from_the_engine() {
    let code = code_only();
    assert!(
        code.contains("engine_snapshot("),
        "watcher 里没有读 `engine_snapshot()` —— \
         监听列表就不可能是「引擎实际在索引的目录」。"
    );
    let body = fn_body(code, "desired_watch_dirs");
    assert!(
        !body.contains("index_paths"),
        "`desired_watch_dirs` 里出现了设置里的 `index_paths` —— \
         必须监听**引擎实际在索引的目录**（设置 ≠ 引擎时会去监听一个根本没被索引的目录：\
         事件来了、重扫了，索引里还是没有那条）。"
    );
}

/// ⑥ 噪声过滤器**真的被 notify 回调调用了**，而且识别出噪声后会**直接返回**。
///
/// 【为什么要有这条】
///   真测试（`watcher.rs` 里的 `is_noise_event` 那几条）只能钉住**函数内部**——
///   把整个过滤器从回调里摘掉，它们照样全绿（这正是 MX4~MX8 那一类缺口）。
///   而摘掉过滤器的后果是：`.DS_Store` 每被 Finder 改一次，就换来一次
///   **全量重扫**（`$HOME` 量级 0.6~1.4 秒 CPU）—— CPU 白烧，且**不报错**。
///
/// 【怎么改会红】
///   · 把回调里的 `if is_noise_event(&ev) { … return; }` 整块删掉 → 红
///   · 识别出噪声后**不 return**（让它继续往下走置脏）→ 红
#[test]
fn the_noise_filter_is_actually_called_by_the_callback() {
    let code = code_only();
    // 回调 = notify 那个闭包里 `Ok(ev) => {` 到 `Err(e)` 之间的那一段
    let cb = code
        .split("Ok(ev)")
        .nth(1)
        .and_then(|rest| rest.split("Err(e)").next())
        .expect("watcher 里找不到 notify 回调的 `Ok(ev) => {` 分支 —— 护栏要跟着改");
    let at = cb
        .find("is_noise_event(")
        .expect(
            "notify 回调里没有调用 `is_noise_event()` —— \
             `.DS_Store` 这类 macOS 私有缓存文件的变化会照常触发全量重扫 \
             （CPU 白烧，而且不报错）。",
        );
    assert!(
        cb[at..].contains("return;"),
        "回调里认出了噪声事件却没有 `return` —— \
         它照样会往下走到「置脏」那一步 → 照样触发重扫。"
    );
}
