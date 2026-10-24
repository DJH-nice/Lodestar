//! 引擎的「根目录可增删」—— 外接卷索引的地基。
//!
//! 【为什么要有这个能力】
//!   插上 U 盘 → 把它加进索引；拔掉 → 把它从索引里摘掉。
//!   而**不能**用「重建整个引擎」实现：
//!   重建要重扫所有根（新盘首扫 30 秒 → 整个索引空窗 30 秒）。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use search_core::types::{ListKind, SearchRequest, SortField, SortOrder};
use search_core::Engine;

/// 临时目录/档目录的名字要带的序号。
///
/// 【为什么不能只用 pid】同一个测试二进制**并发**跑两次（比如两个 cargo 进程）
///   会拿到同一个 pid → 临时目录撞车 → 互相删对方的目录 → 测试结果随机。
static SEQ: AtomicU32 = AtomicU32::new(0);

fn next_seq() -> u32 {
    SEQ.fetch_add(1, Ordering::SeqCst)
}

/// 造一个根目录。
///
/// 【为什么 `with_beta` 是个开关】
///   这些测试要能分清「命中的是**哪个根**的文件」。
///   两个根里放**同名**文件的话，「U 盘上的文件要能搜到」数出来是 2 而不是 1，
///   「拔盘后不该还搜得到」也数不出 0（另一个根的同名文件还在）——
///   断言就测不出它想测的东西。
///   → 只有 U 盘那个根带 `beta.log` → 「beta」这个查询**唯一地**指向它。
fn fixture(tag: &str, with_beta: bool) -> PathBuf {
    let seq = next_seq();
    let dir = std::env::temp_dir().join(format!(
        "lodestar-roots-{tag}-{}-{seq}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    std::fs::write(dir.join("alpha.txt"), "hello").unwrap();
    if with_beta {
        std::fs::write(dir.join("sub/beta.log"), "x").unwrap();
    }
    dir
}

fn request(query: &str) -> SearchRequest {
    SearchRequest {
        query: query.to_string(),
        sort: SortField::Name,
        order: SortOrder::Asc,
        kind: ListKind::All,
        page: 0,
        page_size: 50,
    }
}

#[test]
fn attaching_a_root_makes_its_files_searchable() {
    let home = fixture("home", false);
    let usb = fixture("usb", true);
    let (engine, before) = Engine::new(&[home.as_path()]).unwrap();
    let entries = search_core::scan::scan(&[usb.clone()]).unwrap();
    let after = engine.attach_root(&usb, entries);
    assert!(after > before, "附加后条目数应该变多（{before} → {after}）");
    let res = engine.search(&request("beta")).unwrap();
    assert_eq!(res.total_files, 1, "U 盘上的文件要能搜到");
    assert!(
        res.entries.iter().any(|e| e.path.ends_with("beta.log")),
        "结果里要有 U 盘上的那个文件：{:?}",
        res.entries.iter().map(|e| &e.path).collect::<Vec<_>>()
    );
}

#[test]
fn detaching_a_root_removes_its_files_immediately() {
    let home = fixture("home", false);
    let usb = fixture("usb", true);
    let (engine, _) = Engine::new(&[home.as_path()]).unwrap();
    let entries = search_core::scan::scan(&[usb.clone()]).unwrap();
    engine.attach_root(&usb, entries);
    assert_eq!(engine.search(&request("beta")).unwrap().total_files, 1);

    let after = engine.detach_root(&usb);
    let res = engine.search(&request("beta")).unwrap();
    assert_eq!(res.total_files, 0, "拔盘后不该还搜得到（**不留幽灵条目**）");
    assert!(after > 0, "本机的条目还在");
}

#[test]
fn attaching_the_same_root_twice_replaces_instead_of_duplicating() {
    let home = fixture("home", false);
    let usb = fixture("usb", true);
    let (engine, _) = Engine::new(&[home.as_path()]).unwrap();
    let entries = search_core::scan::scan(&[usb.clone()]).unwrap();
    let once = engine.attach_root(&usb, entries.clone());
    let twice = engine.attach_root(&usb, entries);
    assert_eq!(once, twice, "重复附加 = 替换，不是叠加");
    assert_eq!(engine.search(&request("beta")).unwrap().total_files, 1, "不该出现两条");
}

#[test]
fn detaching_an_unknown_root_changes_nothing() {
    let home = fixture("home", false);
    let (engine, before) = Engine::new(&[home.as_path()]).unwrap();
    let after = engine.detach_root(Path::new("/Volumes/根本没插过"));
    assert_eq!(before, after);
}

#[test]
fn a_missing_root_is_skipped_but_the_others_survive() {
    let home = fixture("home", false);
    // 一个不存在的根（拔掉的卷正好撞在这个窗口里）
    let (engine, n) = Engine::new(&[home.as_path(), Path::new("/nonexistent/lodestar-xyz")]).unwrap();
    assert!(n > 0, "存在的那个根要留下");
    assert_eq!(engine.search(&request("alpha")).unwrap().total_files, 1);
}

/// 两个根**互相嵌套**时（`$HOME` 与 `$HOME/Documents` —— 设置面板的文件夹
/// 选择器允许这么配），内层那个根要被**忽略**。
///
/// 【为什么要有这条护栏】
///   根一旦嵌套，会同时踩四个坑（详见 `engine.rs` 的 `drop_nested_roots`）：
///   摘根会连带删别的根的条目、内层根的好档被**空档**覆盖、
///   刷新前后同一个文件的 `depth` 会变、同一条路径在索引里出现两遍。
#[test]
fn a_root_nested_inside_another_root_is_ignored() {
    let home = fixture("nested-home", true);
    let inner = home.join("sub");

    // 只用外层根建一次 → 基准条目数
    let (_baseline, n_home) = Engine::new(&[home.as_path()]).unwrap();
    // 两个根一起建 → 内层那个要被忽略
    let (engine, n) = Engine::new(&[home.as_path(), inner.as_path()]).unwrap();
    assert_eq!(
        n, n_home,
        "内层根要被忽略 —— 条目数不该比「只扫外层根」多"
    );
    // 内容不丢：那些文件仍在 $HOME 里 → 既不丢，也**不出现两条**
    assert_eq!(
        engine.search(&request("beta")).unwrap().total_files,
        1,
        "内层根的文件仍在 $HOME 里 —— 不该丢，也不该出现两条"
    );
}

/// 第二次启动必须是**读档**，不是「又扫了一遍」。
///
/// 【为什么要这么判】
///   只断言「条目数相等」的话，「读档」与「重扫」**分不开** ——
///   重扫出来的东西一样多，测试照样绿（档读不出来时会静默退回扫描）。
///   → 第一次落档后**往树里加一个文件**：重扫会多出一条，读档不会。
///   → 条目数不变 = 读的是档 ✓
#[test]
fn the_second_start_reads_the_archive_instead_of_rescanning() {
    let home = fixture("reload", true);
    let store_dir = std::env::temp_dir().join(format!(
        "lodestar-roots-reload-store-{}-{}",
        std::process::id(),
        next_seq()
    ));
    let _ = std::fs::remove_dir_all(&store_dir);

    let (engine, first) =
        Engine::new_with_store(&[home.as_path()], Some(store_dir.clone())).unwrap();
    drop(engine);

    // 树变了 → 「重扫」会多出一条，「读档」不会
    std::fs::write(home.join("gamma.txt"), "new").unwrap();

    let (_engine2, second) =
        Engine::new_with_store(&[home.as_path()], Some(store_dir.clone())).unwrap();
    assert_eq!(
        first, second,
        "第二次必须是读档（树已经变了，重扫会多出一条）"
    );

    let _ = std::fs::remove_dir_all(&store_dir);
}

/// 点一次「刷新」，索引必须与建引擎时**逐条相同**。
///
/// 【为什么要有这条护栏】
///   建引擎是**逐根扫**（每个根各扫各的），刷新是**一次扫所有根** ——
///   两条路径给出的**深度**必须一致，否则 `depth:` / `parents:` 这两个
///   真实查询字段会在「刷新」之后变样（用户看到的是「同一个文件，刷新前后结果不同」）。
///
/// 【为什么用集合比，而不是逐条比】
///   并行扫描之后**条目顺序是不确定的**（见 `scan.rs` 的说明）→ 只能比集合。
#[test]
fn a_rescan_produces_exactly_the_same_index_as_the_initial_scan() {
    let home = fixture("rescan-home", true);
    let usb = fixture("rescan-usb", true);
    let (engine, _) = Engine::new(&[home.as_path(), usb.as_path()]).unwrap();

    let snapshot = |engine: &Engine| -> Vec<(String, u32)> {
        let mut v: Vec<(String, u32)> = engine
            .all_entries()
            .into_iter()
            .map(|e| (e.path, e.depth))
            .collect();
        v.sort();
        v
    };

    let before = snapshot(&engine);
    engine.rescan().unwrap();
    let after = snapshot(&engine);

    assert_eq!(
        before, after,
        "刷新之后的索引必须与建引擎时逐条相同（尤其是 depth）"
    );
    // 深度是**相对各自的根**的（不是相对「所有根」）
    let depth_of = |after: &Vec<(String, u32)>, name: &str| -> Option<u32> {
        after
            .iter()
            .find(|(p, _)| p.ends_with(name) && p.starts_with(usb.to_string_lossy().as_ref()))
            .map(|(_, d)| *d)
    };
    assert_eq!(
        depth_of(&after, "alpha.txt"),
        Some(1),
        "U 盘根下的文件深度应该相对 U 盘那个根（1）"
    );
    assert_eq!(
        depth_of(&after, "beta.log"),
        Some(2),
        "U 盘根下 sub/ 里的文件深度应该相对 U 盘那个根（2）"
    );
}

/// 根被摘光之后，刷新要返回**空索引**，而不是报错。
///
/// 【为什么这是合法状态】用户把索引目录全删了 / 外接卷全拔了 → 索引就该是空的。
///   `scan::scan(&[])` 报的是「参数非法」，不是这里的情况 → 不能把错误传给上层。
#[test]
fn a_rescan_with_no_roots_left_returns_an_empty_index() {
    let home = fixture("empty", false);
    let (engine, n) = Engine::new(&[home.as_path()]).unwrap();
    assert!(n > 0);

    engine.detach_root(&home);
    let count = engine.rescan().unwrap();
    assert_eq!(count, 0, "根被摘光后刷新 → 空索引，不是报错");
    assert_eq!(
        engine.search(&request("alpha")).unwrap().total_entries,
        0,
        "空索引里不该还有任何条目"
    );
}

/// 同一个根**再附加一次**（带新的条目）= **替换**，不是叠加、也不是被拒绝。
///
/// 【为什么要有这条】外接卷重新扫描时要靠它把旧条目换成新的 ——
///   「重复附加被当成重叠而拒绝」会让卷上的变化永远进不了索引。
#[test]
fn attaching_the_same_root_again_replaces_its_entries() {
    let home = fixture("home", false);
    let usb = fixture("usb", true);
    let (engine, _) = Engine::new(&[home.as_path()]).unwrap();
    let entries = search_core::scan::scan(&[usb.clone()]).unwrap();
    let first = engine.attach_root(&usb, entries);

    // 树变了 → 重新扫 → 再附加一次
    std::fs::write(usb.join("gamma.txt"), "new").unwrap();
    let fresh = search_core::scan::scan(&[usb.clone()]).unwrap();
    let second = engine.attach_root(&usb, fresh);

    assert_eq!(
        second,
        first + 1,
        "第二次附加 = 替换（多一条新文件）；叠加会多两条，被拒绝则一条都不多"
    );
    assert_eq!(
        engine.search(&request("gamma")).unwrap().total_files,
        1,
        "新文件应该能搜到 —— 说明旧条目确实被换掉了"
    );
}

/// 附加一个**与已有根重叠**的根（`$HOME` 已有，再附加 `$HOME/Documents`）
/// → 必须**拒绝**。
///
/// 【为什么】重叠会让同一条路径在索引里出现两次（搜索显示两遍）——
///   而「根之间不许嵌套」这条不变式必须在**运行期附加**时也守住。
#[test]
fn attaching_a_root_nested_inside_an_existing_root_is_rejected() {
    let home = fixture("attach-nested-home", true);
    let inner = home.join("sub");
    let (engine, before) = Engine::new(&[home.as_path()]).unwrap();
    let entries = search_core::scan::scan(&[inner.clone()]).unwrap();
    let after = engine.attach_root(&inner, entries);
    assert_eq!(
        before, after,
        "与已有根重叠 → 不该附加（否则同一条路径会进索引两次）"
    );
    assert_eq!(
        engine.search(&request("beta")).unwrap().total_files,
        1,
        "不该出现两条"
    );
}

/// 附加一个根之后，那个根里的路径**仍然能被增量更新**。
///
/// 【为什么要有这条护栏】
///   M3 的增量更新靠 `by_path` O(1) 定位「这条路径在索引里的哪」。
///   附加根时如果忘了登记 → 增量更新把它当成「新增的文件」
///   → 索引里多出一条**旧的**（还查得到、但内容是旧的）→ **索引悄悄过期**
///   （全程不报错）。
#[test]
fn an_attached_root_can_still_be_updated_incrementally() {
    let home = fixture("home", false);
    let usb = fixture("usb", true);
    let (engine, _) = Engine::new(&[home.as_path()]).unwrap();
    let entries = search_core::scan::scan(&[usb.clone()]).unwrap();
    engine.attach_root(&usb, entries);

    // 改 U 盘上的文件 → 应用一次增量更新
    let file = usb.join("sub/beta.log");
    std::fs::write(&file, "a much longer content now").unwrap();
    let report = engine
        .apply_changes(&search_core::ChangeSet { paths: vec![file.clone()] })
        .unwrap();

    // 【为什么必须断言「没退化」】退化成全量重扫的话，索引照样是对的 ——
    //   那就抓不住「忘了登记 by_path」这个 bug 了（它表现为**多一条旧的**）。
    assert!(
        !report.fell_back,
        "这该是一次真正的增量更新 —— 退化成全量重扫就抓不住 by_path 的问题了"
    );
    let matching: Vec<_> = engine
        .all_entries()
        .into_iter()
        .filter(|e| e.path == file.to_string_lossy())
        .collect();
    assert_eq!(
        matching.len(),
        1,
        "这条路径在索引里只能有一条（忘了登记 by_path → 会多出一条旧的）"
    );
    assert_eq!(
        matching[0].size, 25,
        "大小应该更新成新的字节数 —— 忘了登记 by_path 就会停在旧值"
    );
}
