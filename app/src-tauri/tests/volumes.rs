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
//!   ⑧ 插回来要「秒开 + 后台刷新」**两段**（承诺的一半，不能被删掉）
//!   ⑨ 状态栏的条数是**这个卷自己的**（不是引擎总数）
//!   ⑩ 引擎换代要对账（`known` 与引擎实际在索引的根对齐）
//!   ⑪ 附加失败**不记** `known` → 会重试（记了就永远不再试）
//!   ⑫ 摘根的 `Err` 分支也要清状态（否则状态栏留着幽灵行）
//!   ⑬ 摘根**不许**丢落盘槽里那份「最后的好档」
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
/// 搜索线程的源码（落盘槽在里头）。
const WORKER_SRC: &str = include_str!("../search-core/src/worker.rs");

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

/// 搜索线程的**代码**（去注释）。
fn worker_code() -> &'static str {
    static CODE: OnceLock<String> = OnceLock::new();
    CODE.get_or_init(|| code_only(WORKER_SRC))
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
    // 附加成功后必须更新 `IndexState.paths` ——
    // `watcher::desired_watch_dirs` 读的就是它，不更新 → 新卷不会被 FSEvents 监听。
    //
    // 【为什么中间多了一层 `commit_attached`】
    //   提交状态时要用**两个**数（卷自己的条数 / 引擎总数）→ 抽成了一个函数
    //   （顺便把「状态栏显示卷自己的条数」这条不变式钉住，见 `the_status_bar_shows_the_volumes_own_count`）。
    //   → 这里顺着那一层往下查：附件 → `commit_attached` → `commit_paths`。
    let commit = fn_body(volumes_code(), "commit_attached");
    assert!(
        attach.contains("commit_attached("),
        "附加成功后必须提交状态 —— \
         `watcher::desired_watch_dirs` 读的就是 `IndexState.paths`，不更新 → 新卷不会被 FSEvents 监听。\n\
         附加函数：\n{attach}"
    );
    assert!(
        commit.contains("commit_paths("),
        "`commit_attached` 必须更新 `IndexState.paths`。\n提交函数：\n{commit}"
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

// ── ⑧ 插回来要「秒开 + 后台刷新」两段 ──
//
// 【为什么这条最要紧】「插回来立刻能搜 + 后台刷一遍」是本特性**承诺的一半**
//   （设计文档 §6 M5 验收 #3）。第一版实现把「命中旧索引档」写成了**早退**
//   → 承诺的那一半**根本没实现**，而且是**静默**的：
//   插上刚在别的机器上拷过文件的 U 盘 → 搜不到那个文件 →
//   没有报错、状态栏还显示「U盘 · N 项 · 就绪」，而且**不自愈**
//   （FSEvents 不会补发拔插期间的历史事件）。
//   → 这条护栏就是为了「下次重构也删不掉它」。

#[test]
fn a_reinserted_volume_is_refreshed_in_the_background() {
    let phases = fn_body(volumes_code(), "volume_attach_phases");
    let instant = phases
        .find("attach(AttachPhase::Instant")
        .expect("命中旧档时必须**先秒开**（挂旧档 → 用户马上能搜）");
    // 【为什么只看秒开之后那一段】函数里**先**出现的是「没有旧档」那条分支的扫描
    //   （那一条本来就该先扫再挂）—— 要钉的是**秒开之后**的那一次扫描。
    let after_instant = &phases[instant..];
    let scan = after_instant
        .find("scan()?")
        .expect("命中旧档时必须**再扫一遍**（拔插期间的变化只有这条路径能进来）");
    let last = after_instant
        .find("attach(AttachPhase::Final")
        .expect("必须把新扫出来的挂上去（替换掉秒开那份）");
    assert!(
        scan < last,
        "秒开之后必须**先扫描、再挂新的** —— 反过来就退化成「等扫完才能搜」→ 承诺作废。\n编排函数：\n{phases}"
    );
    // 【为什么还要钉「不许早退」】第一版的 bug 就是「命中旧档就 `return`」——
    //   只看「有没有 scan()」会被**不可达代码**骗过去（把扫描写在 return 后面也照样有那个词）。
    //   → 秒开与扫描之间**不许**有 `return`。
    let between = &after_instant[..scan];
    assert!(
        !between.contains("return"),
        "秒开与扫描之间**不许**早退 —— 那正是第一版的 bug（命中旧档就返回 → \
         「插回来后台刷新」根本没实现，而且静默：搜不到新文件还不报错）。\n编排函数：\n{phases}"
    );
    // 附加线程必须真的走这个编排（不是各写各的）。
    let attach = fn_body(volumes_code(), "attach_volume");
    assert!(
        attach.contains("volume_attach_phases("),
        "附加线程必须走「秒开 + 刷新」那个编排 —— \
         各写各的就会出现「只秒开不刷新」或「两次都扫描」。\n附加函数：\n{attach}"
    );
    // 「刷新完成」要有日志（否则界面上与日志里都看不出后台发生过什么）。
    assert!(
        attach.contains("刷新完成"),
        "刷新完成要有一行日志 —— 没有它就无法区分「秒开」与「刷新完了」。\n附加函数：\n{attach}"
    );
}

#[test]
fn the_status_bar_shows_the_volumes_own_count() {
    let commit = fn_body(volumes_code(), "commit_attached");
    assert!(
        commit.contains("counts.own"),
        "状态栏的条数必须是**这个卷自己的** —— \
         `attach_root` 返回的是**整个引擎**的条目总数（含 $HOME），\
         拿它去填状态栏 → 「U盘 · 8,150 项」（$HOME 8,000 + U 盘 150）。\n提交函数：\n{commit}"
    );
    assert!(
        commit.contains("counts.total"),
        "索引状态（`IndexState.indexed`）的条数才是**引擎总数**。\n提交函数：\n{commit}"
    );
}

#[test]
fn the_monitor_reconciles_known_with_the_engine() {
    let monitor = fn_body(volumes_code(), "spawn_volume_monitor");
    assert!(
        monitor.contains("reconcile_known("),
        "监控循环必须做**对账** —— `known` 与引擎换代是解耦的：\
         换索引目录时若卷枚举失败（新引擎里没有那个卷），\
         `known` 里还留着它 → 差分看到「没变化」→ **永远不会重新附加**。\n监控函数：\n{monitor}"
    );
}

#[test]
fn a_failed_attach_is_not_remembered_and_gets_retried() {
    let monitor = fn_body(volumes_code(), "spawn_volume_monitor");
    assert!(
        monitor.contains("apply_attach_results("),
        "附加结果必须回传给监控线程 —— \
         以前 `attach_volume` 的 `true` 只表示「线程起动了」，\
         附加失败（扫描出错 / 被判重叠）的卷**永远不再试**。\n监控函数：\n{monitor}"
    );
    assert!(
        monitor.contains("should_attach("),
        "重试要有节流 —— 失败重试会**重新扫一遍那个盘**，\
         每秒重试一次 = 机器被扫描烤着。\n监控函数：\n{monitor}"
    );
}

#[test]
fn detaching_also_clears_the_state_when_the_root_was_never_attached() {
    let detach = fn_body(volumes_code(), "detach_volume");
    let err_at = detach
        .find("Err(e) =>")
        .expect("分离函数里要有 `Err` 分支（没挂过）");
    let err_branch = &detach[err_at..];
    assert!(
        err_branch.contains("remove_volume_state("),
        "摘根的 `Err` 分支**也要清状态** —— \
         任何「附加没成功」的卷在状态栏上都留下一行（「U盘 · 索引失败」）→ \
         拔掉之后那一行**永远留着** → 用户看到的是一个已经不存在的盘。\n分离函数：\n{detach}"
    );
    assert!(
        err_branch.contains("notify_frontend("),
        "前端那一行也要清掉 → 必须发一次通知。\n分离函数：\n{detach}"
    );
}

#[test]
fn a_volume_that_vanished_during_the_refresh_is_detached() {
    let attach = fn_body(volumes_code(), "attach_volume");
    let at = attach
        .find("AttachError::Vanished")
        .expect("附加线程里要有「盘被拔掉了」这一支");
    let branch = &attach[at..];
    assert!(
        branch.contains("detach_volume("),
        "盘在后台刷新期间被拔掉 → **秒开那一份必须一起摘掉** —— \
         否则用户还能搜到**已经拔掉的**盘上的文件（最不能接受的那种幽灵）。\n附加函数：\n{attach}"
    );
}

#[test]
fn detaching_a_root_keeps_the_last_good_archive() {
    let worker = worker_code();
    assert!(
        !worker.contains("discard("),
        "摘根时**不许**把落盘槽里已经排着的那份快照丢掉 —— \
         那份正是这个卷**最后一份好档**（含卷的条目）：\
         丢掉它，下次插回来就要全量重扫（慢，不是错，\
         但违背「插回来复用旧索引」的承诺）。\n搜索线程源码：\n{worker}"
    );
}
