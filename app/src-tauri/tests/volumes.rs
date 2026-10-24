//! 护栏 —— 「插上 U 盘就能搜到里面的文件」这条链路的**契约**。
//!
//! 【为什么要有这个文件】
//!   `volumes.rs` 里的单元测试钉的是**纯函数**（差分、状态机）与
//!   `IndexState.paths` 的增删 —— 但那条链路上最要命的几个判断
//!   **都在调用点上**，纯函数测试看不见：
//!     - 枚举失败时，监控循环会不会把「所有卷都被拔了」当成事实；
//!     - 扫描是不是真的**不在搜索 worker 上**做；
//!     - 附加与分离用的路径是不是**逐字相同**的那一份；
//!     - 附加/分离之后有没有去更新 `IndexState.paths`。
//!   这些**改错了都不报错**，只是功能悄悄死掉
//!   （U 盘搜不到 / 拔了还搜得到 / 插上以后文件变化永远进不了索引）。
//!
//! 【为什么是「读源码」而不是真跑】
//!   `volumes` 现在是**私有 `mod`**（见 `lib.rs`）→ 集成测试（外部 crate）
//!   够不到它的内部 API → 只能读源码。
//!   这正是本仓库已有的做法（见 `tests/watch_contract.rs`）。
//!
//! 【判据】下面每一条都注明了「怎么改会红」：
//!   ① 枚举失败 → 这一轮什么都不做（别把瞬时错误当成「卷都被拔了」）
//!   ② 扫描在**自己的线程**上（不在监控线程、不在搜索 worker 上）
//!   ③ 附加与分离用**同一个**路径（拼写不同 → 摘根静默无效 → 幽灵条目）
//!   ④ 附加后 `commit_paths`、分离后 `remove_path`（watcher 靠它监听）
//!   ⑤ 拔盘竞态：扫完复查挂载表；枚举失败时保守当「还在」
//!   ⑥ 重建引擎时带上已挂载的卷；枚举失败时保持原根列表
//!   ⑦ `setup()` 里起了监控线程 + 注册了卷状态；`volumes` 是**私有** `mod`
//!
//! 【为什么要先去掉注释再断言】
//!   这些契约在源码里都有**大段解释性注释** ——
//!   直接对原文断言，注释里出现同一个词就会「假绿」。
//!   所以先把 `//` 之后的部分去掉，只对**真正的代码**断言。

use std::sync::OnceLock;

/// 卷监控的源码 —— 编译期就嵌进来（不会读到别的副本）。
const VOLUMES_SRC: &str = include_str!("../src/volumes.rs");
/// 设置的源码（重建引擎的那一段在里头）。
const SETTINGS_SRC: &str = include_str!("../src/settings.rs");
/// 应用入口的源码（`setup()` 与模块声明在里头）。
const LIB_SRC: &str = include_str!("../src/lib.rs");

/// 去掉行注释后的源码（`//` 之后的部分）。
fn code_only(src: &str) -> String {
    src.lines()
        .map(|line| match line.find("//") {
            Some(i) => &line[..i],
            None => line,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// 卷监控的**代码**（去注释）。
fn volumes_code() -> &'static str {
    static CODE: OnceLock<String> = OnceLock::new();
    CODE.get_or_init(|| code_only(VOLUMES_SRC))
}

/// 设置的**代码**（去注释）。
fn settings_code() -> &'static str {
    static CODE: OnceLock<String> = OnceLock::new();
    CODE.get_or_init(|| code_only(SETTINGS_SRC))
}

/// 某个函数体（从 `fn 名字` 到下一个顶层 `fn ` / `impl ` / 结尾）。
///
/// 【为什么只找 `fn 名字` 而不找 `fn 名字(`】
///   这些函数大多是**泛型**的（`fn spawn_volume_monitor<R: Runtime>(…)`）
///   → 括号前面还有 `<R: Runtime>` → 按括号找会找不到。
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

/// ── ① 枚举失败 ≠ 没有卷 ──

#[test]
fn a_failed_enumeration_makes_the_monitor_do_nothing() {
    let monitor = fn_body(volumes_code(), "spawn_volume_monitor");
    let anchor = "mounted_local_volumes() else";
    let at = monitor
        .find(anchor)
        .expect("监控循环必须用 `let Ok(list) = mounted_local_volumes() else { … }` 的写法");
    let tail = &monitor[at + anchor.len()..];
    let branch = &tail[..tail.find('}').unwrap_or(tail.len())];
    assert!(
        branch.contains("continue"),
        "枚举失败时这一轮必须**什么都不做**（continue）—— \
         否则一次瞬时错误会被当成「所有卷都被拔了」→ 拆索引 → 下一秒重挂 → \
         U 盘被全量重扫一遍。\n分支内容：\n{branch}"
    );
}

// ── ② 扫描绝不在搜索 worker 上做 ──

#[test]
fn the_scan_runs_on_its_own_thread() {
    let monitor = fn_body(volumes_code(), "spawn_volume_monitor");
    let attach = fn_body(volumes_code(), "attach_volume");
    assert!(
        attach.contains(".spawn("),
        "附加必须在**自己的线程**上做 —— \
         几十万文件的盘要扫几十秒，占着监控线程就检测不到拔盘了。\n附加函数：\n{attach}"
    );
    assert!(
        attach.contains("scan_volume"),
        "扫描要在附加线程里做（不是监控线程）。\n附加函数：\n{attach}"
    );
    assert!(
        !monitor.contains("scan_volume"),
        "监控线程**不能**自己扫 —— 扫的时候它就没法发现拔盘了。\n监控函数：\n{monitor}"
    );
    // 【扫描不许碰引擎】搜索线程（worker）只可能通过 `Searcher` 的方法被碰到 ——
    //   扫描函数里一个 `Searcher` 方法都没有 → 扫描就不在 worker 上。
    let scan = fn_body(volumes_code(), "scan_volume");
    assert!(
        !scan.contains("attach_root") && !scan.contains("detach_root"),
        "扫描过程不许调引擎的挂/摘接口 —— 那会占住搜索 worker。\n扫描函数：\n{scan}"
    );
}

// ── ③ 附加与分离用「同一个」路径 ──

#[test]
fn attaching_and_detaching_use_the_very_same_path() {
    let attach = fn_body(volumes_code(), "attach_volume");
    let detach = fn_body(volumes_code(), "detach_volume");
    assert!(
        attach.contains("attach_root(&volume"),
        "附加要用传进来的那份路径（`volume`）。\n附加函数：\n{attach}"
    );
    assert!(
        detach.contains("detach_root(volume)"),
        "分离要用**同一个**路径 —— \
         `detach_root` 比的是索引空间写法，拼写不同 → 静默无效 → 幽灵条目。\n分离函数：\n{detach}"
    );
    // 【不许重新拼路径】两个函数里都不该出现「把路径再拼一遍」的写法。
    for (name, body) in [("attach_volume", &attach), ("detach_volume", &detach)] {
        assert!(
            !body.contains("PathBuf::from") && !body.contains(".join("),
            "`{name}` 里不许重新拼路径 —— 拼出来的那份可能与挂时那份**逐字不同** \
             → 摘根静默无效 → 盘拔了还搜得到它的文件。\n函数体：\n{body}"
        );
    }
}

// ── ④ 附加/分离后要更新 `IndexState.paths` ──

#[test]
fn the_watched_paths_are_updated_on_attach_and_on_detach() {
    let attach = fn_body(volumes_code(), "attach_volume");
    let detach = fn_body(volumes_code(), "detach_volume");
    assert!(
        attach.contains("commit_paths("),
        "附加成功后必须更新 `IndexState.paths` —— \
         `watcher::desired_watch_dirs` 读的就是它，不更新 → 新卷不会被 FSEvents 监听。\n\
         附加函数：\n{attach}"
    );
    assert!(
        detach.contains("remove_path("),
        "分离后必须把路径**去掉** —— \
         不去掉 → watcher 继续监听一个已经不存在的路径，每秒对账都失败一次。\n\
         分离函数：\n{detach}"
    );
}

// ── ⑤ 拔盘竞态 ──

#[test]
fn the_mount_table_is_rechecked_after_the_scan() {
    let attach = fn_body(volumes_code(), "attach_volume");
    assert!(
        attach.contains("still_there"),
        "扫完必须复查挂载表 —— 扫的这几十秒里用户可能把盘拔了。\n附加函数：\n{attach}"
    );
    assert!(
        attach.contains("unwrap_or(true)"),
        "复查时**枚举失败**要保守当「还在」（不丢弃扫描结果）—— \
         宁可多索引一个死盘，也不要把活盘的条目扔了。\n附加函数：\n{attach}"
    );
}

// ── ⑥ 重建引擎时带上已挂载的卷 ──

#[test]
fn rebuilding_the_engine_keeps_the_mounted_volumes() {
    let rebuild = fn_body(settings_code(), "rebuild_engine");
    assert!(
        rebuild.contains("mounted_local_volumes()"),
        "重建引擎必须带上**已挂载的卷** —— 否则换一次索引目录就把 U 盘踢出索引。\n\
         重建函数：\n{rebuild}"
    );
    assert!(
        rebuild.contains("if let Ok(list)"),
        "枚举失败时**保持原根列表** —— 宁可少索引一个卷，也不能把用户正在用的根踢掉。\n\
         重建函数：\n{rebuild}"
    );
}

// ── ⑦ 监控线程的启动与模块可见性 ──

#[test]
fn the_monitor_is_started_and_the_state_is_registered() {
    assert!(
        LIB_SRC.contains("volumes::spawn_volume_monitor("),
        "`setup()` 里必须起卷监控线程 —— 否则插上 U 盘永远没人管。"
    );
    assert!(
        LIB_SRC.contains("manage(volumes::VolumeState::default())"),
        "必须注册卷状态 —— 否则 `app.state::<VolumeState>()` 拿不到。"
    );
    assert!(
        !LIB_SRC.contains("pub mod volumes;"),
        "`volumes` 必须是**私有** `mod` —— \
         `pub mod` 会让 `dead_code` 护栏失效（私有 `mod` 里的 `pub` 项 \
         没有 crate 内调用点时编译器会报出来）。"
    );
}
