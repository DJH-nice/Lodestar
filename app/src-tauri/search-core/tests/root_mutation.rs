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

/// 造一个根目录。
///
/// 【为什么 `with_beta` 是个开关】
///   这些测试要能分清「命中的是**哪个根**里的文件」。
///   两个根里放**同名**文件的话，「U 盘上的文件要能搜到」数出来是 2 而不是 1，
///   「拔盘后不该还搜得到」也数不出 0（另一个根的同名文件还在）——
///   断言就测不出它想测的东西。
///   → 只有 U 盘那个根带 `beta.log` → 「beta」这个查询**唯一地**指向它。
fn fixture(tag: &str, with_beta: bool) -> PathBuf {
    static SEQ: AtomicU32 = AtomicU32::new(0);
    let seq = SEQ.fetch_add(1, Ordering::SeqCst);
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
/// 选择器允许这么配），读档回来不能多出条目。
///
/// 【为什么要有这条护栏】
///   外层根的档里**已经包含了**内层根的文件 → 同一条路径落进**两个**档 →
///   读两份 = 索引里出现两份。
///   而「每根一个档」的布局会把它**放大**：每冷启动一次就多一份
///   （实测 6 → 10 → 16 条，**越滚越大**，没有任何报错）。
#[test]
fn nested_roots_do_not_duplicate_entries_across_a_reload() {
    let home = fixture("nested-home", true);
    let inner = home.join("sub");
    let store_dir = std::env::temp_dir().join(format!(
        "lodestar-roots-nested-store-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&store_dir);

    let (engine, first) = Engine::new_with_store(
        &[home.as_path(), inner.as_path()],
        Some(store_dir.clone()),
    )
    .unwrap();
    drop(engine);

    // 第二次：读档（不扫描）
    let (_engine2, second) = Engine::new_with_store(
        &[home.as_path(), inner.as_path()],
        Some(store_dir.clone()),
    )
    .unwrap();
    assert_eq!(
        first, second,
        "读档回来的条目数必须与扫出来的一致 —— **不重复、不越滚越大**"
    );

    let _ = std::fs::remove_dir_all(&store_dir);
}
