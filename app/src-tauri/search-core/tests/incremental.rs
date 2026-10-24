//! 增量更新的护栏 —— 「改文件 → 索引跟上」这件事不能悄悄坏掉。
//!
//! 【为什么单独一个文件】
//!   增量更新是**在已有索引上做局部修改**，
//!   它最容易出的错是「**索引与磁盘对不上**」——
//!   而这种错**不会报错**：用户只是搜不到某个文件了。
//!   → 所以这里的核心断言是「**增量之后，索引必须与全量重扫等价**」。
//!
//! 【判据为什么是「等价于全量重扫」而不是「某几个条目在不在」】
//!   逐条断言只能盖住想到的情况；
//!   「与全量重扫逐字段等价」能盖住**所有**情况 ——
//!   包括那些我没想到的（计数器错位、深度错位、漏删…）。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use search_core::incremental::ChangeSet;
use search_core::types::{ListKind, SearchRequest, SortField, SortOrder};
use search_core::Engine;

/// 临时目录（每个测试一个，互不干扰）。
fn fixture(tag: &str) -> PathBuf {
    static SEQ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "sextant-incremental-{tag}-{}-{seq}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("sub/deeper")).unwrap();
    std::fs::write(dir.join("alpha.txt"), "hello").unwrap();
    std::fs::write(dir.join("beta.log"), "world").unwrap();
    std::fs::write(dir.join("sub/gamma.md"), "nested").unwrap();
    std::fs::write(dir.join("sub/deeper/delta.md"), "deep").unwrap();
    dir
}

fn request(query: &str) -> SearchRequest {
    SearchRequest {
        query: query.to_string(),
        sort: SortField::Name,
        order: SortOrder::Asc,
        kind: ListKind::All,
        page: 0,
        page_size: 500,
    }
}

/// 索引的**可比快照**：路径 → (大小, mtime, 深度, 是否目录, 子项数, 文件数, 目录数)。
///
/// 【为什么按路径排序】索引的**条目顺序**不重要
///   （查询结果自己会排序）—— 所以比较时必须先排序，
///   否则「顺序不同」会被误判成「内容不同」。
///
/// 【为什么要把三个子项计数都放进快照】
///   M2 给 `IndexEntry` 加了 `child_file_count` / `child_folder_count`，
///   而 M3 的增量更新只维护 `child_count` →
///   **不把这两个字段纳入比对的话，漂移根本不会被抓到**
///   （实测：扩之前 19 条全绿，扩之后 8 条变红 —— `child_folder_count` 对不上）。
type Snapshot = BTreeMap<String, (i64, i64, u32, bool, u32, u32, u32)>;

/// 从引擎里取一份快照。
fn snapshot_of(engine: &Engine) -> Snapshot {
    engine
        .all_entries()
        .into_iter()
        .map(|e| {
            (
                e.path.clone(),
                (
                    e.size,
                    e.mtime,
                    e.depth,
                    e.is_folder,
                    e.child_count,
                    e.child_file_count,
                    e.child_folder_count,
                ),
            )
        })
        .collect()
}

/// 全量重扫的快照 —— **对照基准**。
fn snapshot_of_full_rescan(root: &Path) -> Snapshot {
    search_core::scan::scan(&[root.to_path_buf()])
        .unwrap()
        .into_iter()
        .map(|e| {
            (
                e.path.clone(),
                (
                    e.size,
                    e.mtime,
                    e.depth,
                    e.is_folder,
                    e.child_count,
                    e.child_file_count,
                    e.child_folder_count,
                ),
            )
        })
        .collect()
}

/// 把两边比一遍 —— 不同就把差异打出来（比「不相等」有用得多）。
#[track_caller]
fn assert_same_as_full_rescan(engine: &Engine, root: &Path) {
    let got = snapshot_of(engine);
    let want = snapshot_of_full_rescan(root);
    if got != want {
        let mut diff = String::new();
        for (path, v) in &want {
            match got.get(path) {
                None => diff.push_str(&format!("  缺失：{path}\n")),
                Some(g) if g != v => diff.push_str(&format!("  不一致：{path} 期望{v:?} 实际{g:?}\n")),
                _ => {}
            }
        }
        for path in got.keys() {
            if !want.contains_key(path) {
                diff.push_str(&format!("  多余：{path}\n"));
            }
        }
        panic!("增量之后的索引与全量重扫不等价：\n{diff}");
    }
}


/// 索引里所有条目的路径。
fn paths_of(engine: &Engine) -> Vec<String> {
    engine.all_entries().into_iter().map(|e| e.path).collect()
}

/// 只比**路径集合**（不比元信息）。
///
/// 【什么时候用它】当变化**被排除规则挡掉**时：
///   被排除的东西压根不进索引，但它**仍然会改磁盘上父目录的元信息**
///   （目录的 mtime/size 记录的是「目录最后一次被改动」，
///   改动它的可能是我们**不索引**的东西）。
///   → 这种情况下「索引内容」是对的，「目录自己的 size/mtime」会滞后。
///   真正该保证的是：**索引里的条目集合与全量重扫一致**。
#[track_caller]
fn assert_same_paths_as_full_rescan(engine: &Engine, root: &Path) {
    let mut got = paths_of(engine);
    let mut want: Vec<String> = snapshot_of_full_rescan(root)
        .into_keys()
        .collect();
    got.sort();
    want.sort();
    if got != want {
        let missing: Vec<&String> = want.iter().filter(|w| !got.contains(w)).collect();
        let extra: Vec<&String> = got.iter().filter(|g| !want.contains(g)).collect();
        panic!("索引里的条目集合与全量重扫不一致：\n  缺失：{missing:?}\n  多余：{extra:?}");
    }
}

// ─────────────────────────── 新增 ───────────────────────────

#[test]
fn a_new_file_shows_up_in_the_index() {
    let dir = fixture("new-file");
    let (engine, _) = Engine::new(&[dir.as_path()]).unwrap();

    std::fs::write(dir.join("brand-new.txt"), "hello").unwrap();
    engine
        .apply_changes(&ChangeSet {
            paths: vec![dir.join("brand-new.txt")],
        })
        .unwrap();

    let res = engine.search(&request("brand-new")).unwrap();
    assert_eq!(res.total_entries, 1, "新文件应该能被搜到");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_new_file_keeps_the_index_equal_to_a_full_rescan() {
    let dir = fixture("new-file-equiv");
    let (engine, _) = Engine::new(&[dir.as_path()]).unwrap();

    std::fs::write(dir.join("brand-new.txt"), "hello").unwrap();
    engine
        .apply_changes(&ChangeSet {
            paths: vec![dir.join("brand-new.txt")],
        })
        .unwrap();

    assert_same_as_full_rescan(&engine, &dir);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_new_file_inside_a_nested_directory_shows_up() {
    let dir = fixture("new-nested");
    let (engine, _) = Engine::new(&[dir.as_path()]).unwrap();

    std::fs::write(dir.join("sub/deeper/epsilon.md"), "x").unwrap();
    engine
        .apply_changes(&ChangeSet {
            paths: vec![dir.join("sub/deeper/epsilon.md")],
        })
        .unwrap();

    let res = engine.search(&request("epsilon")).unwrap();
    assert_eq!(res.total_entries, 1);
    assert_same_as_full_rescan(&engine, &dir);
    let _ = std::fs::remove_dir_all(&dir);
}

// ─────────────────────────── 修改 ───────────────────────────

#[test]
fn a_modified_file_gets_the_new_size() {
    let dir = fixture("modified");
    let (engine, _) = Engine::new(&[dir.as_path()]).unwrap();

    std::fs::write(dir.join("alpha.txt"), "a much longer content now").unwrap();
    engine
        .apply_changes(&ChangeSet {
            paths: vec![dir.join("alpha.txt")],
        })
        .unwrap();

    let entry = engine
        .all_entries()
        .into_iter()
        .find(|e| e.path.ends_with("alpha.txt"))
        .expect("alpha.txt 应该在索引里");
    assert_eq!(entry.size, 25, "大小应该更新成新的字节数");
    assert_same_as_full_rescan(&engine, &dir);
    let _ = std::fs::remove_dir_all(&dir);
}

// ─────────────────────────── 删除 ───────────────────────────

#[test]
fn a_deleted_file_disappears_from_the_index() {
    let dir = fixture("deleted");
    let (engine, _) = Engine::new(&[dir.as_path()]).unwrap();

    std::fs::remove_file(dir.join("alpha.txt")).unwrap();
    engine
        .apply_changes(&ChangeSet {
            paths: vec![dir.join("alpha.txt")],
        })
        .unwrap();

    let res = engine.search(&request("alpha")).unwrap();
    assert_eq!(res.total_entries, 0, "删掉的文件不该还搜得到");
    assert_same_as_full_rescan(&engine, &dir);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_deleted_directory_takes_its_whole_subtree_with_it() {
    let dir = fixture("deleted-dir");
    let (engine, _) = Engine::new(&[dir.as_path()]).unwrap();

    std::fs::remove_dir_all(dir.join("sub")).unwrap();
    engine
        .apply_changes(&ChangeSet {
            paths: vec![dir.join("sub")],
        })
        .unwrap();

    // sub / sub/gamma.md / sub/deeper / sub/deeper/delta.md 全都不该在
    for gone in ["sub", "gamma.md", "deeper", "delta.md"] {
        let res = engine.search(&request(gone)).unwrap();
        assert_eq!(res.total_entries, 0, "「{gone}」不该还搜得到");
    }
    assert_same_as_full_rescan(&engine, &dir);
    let _ = std::fs::remove_dir_all(&dir);
}

// ─────────────────────────── 重命名 ───────────────────────────

#[test]
fn a_renamed_file_moves_in_the_index() {
    let dir = fixture("renamed");
    let (engine, _) = Engine::new(&[dir.as_path()]).unwrap();

    std::fs::rename(dir.join("alpha.txt"), dir.join("alpha-renamed.txt")).unwrap();
    // FSEvents 报的是**两条**事件：旧路径（已不存在）+ 新路径
    engine
        .apply_changes(&ChangeSet {
            paths: vec![dir.join("alpha.txt"), dir.join("alpha-renamed.txt")],
        })
        .unwrap();

    let paths = paths_of(&engine);
    assert!(
        !paths.iter().any(|p| p.ends_with("/alpha.txt")),
        "旧名字不该还在索引里"
    );
    assert!(
        paths.iter().any(|p| p.ends_with("/alpha-renamed.txt")),
        "新名字应该进索引"
    );
    assert_same_as_full_rescan(&engine, &dir);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_renamed_directory_moves_its_whole_subtree() {
    let dir = fixture("renamed-dir");
    let (engine, _) = Engine::new(&[dir.as_path()]).unwrap();

    std::fs::rename(dir.join("sub"), dir.join("sub-renamed")).unwrap();
    engine
        .apply_changes(&ChangeSet {
            paths: vec![dir.join("sub"), dir.join("sub-renamed")],
        })
        .unwrap();

    let paths = paths_of(&engine);
    assert!(
        !paths.iter().any(|p| p.ends_with("/sub/gamma.md")),
        "旧目录下的文件不该还在索引里"
    );
    assert!(
        paths.iter().any(|p| p.ends_with("/sub-renamed/gamma.md")),
        "改名后的文件应该在新目录下"
    );
    assert_same_as_full_rescan(&engine, &dir);
    let _ = std::fs::remove_dir_all(&dir);
}

// ─────────────────────── 新建目录（连内容一起） ───────────────────────

#[test]
fn a_new_directory_with_contents_is_indexed_completely() {
    let dir = fixture("new-dir");
    let (engine, _) = Engine::new(&[dir.as_path()]).unwrap();

    std::fs::create_dir_all(dir.join("fresh/inner")).unwrap();
    std::fs::write(dir.join("fresh/inner/zeta.md"), "z").unwrap();
    engine
        .apply_changes(&ChangeSet {
            paths: vec![dir.join("fresh")],
        })
        .unwrap();

    assert_same_as_full_rescan(&engine, &dir);
    let res = engine.search(&request("zeta")).unwrap();
    assert_eq!(res.total_entries, 1);
    let _ = std::fs::remove_dir_all(&dir);
}

// ─────────────────────── 排除规则 / 隐藏文件 ───────────────────────

#[test]
fn a_file_inside_an_excluded_directory_never_enters_the_index() {
    let dir = fixture("excluded");
    let (engine, _) = Engine::new(&[dir.as_path()]).unwrap();

    // node_modules 是排除规则里的目录名 → 整棵子树都不进索引
    std::fs::create_dir_all(dir.join("node_modules/pkg")).unwrap();
    std::fs::write(dir.join("node_modules/pkg/index.js"), "x").unwrap();
    engine
        .apply_changes(&ChangeSet {
            paths: vec![dir.join("node_modules/pkg/index.js")],
        })
        .unwrap();

    let res = engine.search(&request("index.js")).unwrap();
    assert_eq!(res.total_entries, 0, "被排除的目录不该进索引");
    // 【为什么只比路径集合】被排除的目录**不进索引**，
    //   但它确实改动了磁盘上父目录的元信息（mtime/size）——
    //   那种滞后是「排除规则」本身带来的，是**有意接受**的。
    assert_same_paths_as_full_rescan(&engine, &dir);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_hidden_file_never_enters_the_index() {
    let dir = fixture("hidden");
    let (engine, _) = Engine::new(&[dir.as_path()]).unwrap();

    std::fs::write(dir.join(".secret"), "x").unwrap();
    engine
        .apply_changes(&ChangeSet {
            paths: vec![dir.join(".secret")],
        })
        .unwrap();

    let res = engine.search(&request("secret")).unwrap();
    assert_eq!(res.total_entries, 0, "隐藏文件不该进索引");
    assert_same_paths_as_full_rescan(&engine, &dir);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_file_that_becomes_hidden_is_dropped_from_the_index() {
    let dir = fixture("became-hidden");
    let (engine, _) = Engine::new(&[dir.as_path()]).unwrap();

    std::fs::write(dir.join("visible.txt"), "x").unwrap();
    engine
        .apply_changes(&ChangeSet {
            paths: vec![dir.join("visible.txt")],
        })
        .unwrap();
    assert_eq!(engine.search(&request("visible")).unwrap().total_entries, 1);

    // 改名成隐藏文件 → 它不该再被索引
    std::fs::rename(dir.join("visible.txt"), dir.join(".hidden-now")).unwrap();
    engine
        .apply_changes(&ChangeSet {
            paths: vec![dir.join("visible.txt"), dir.join(".hidden-now")],
        })
        .unwrap();

    assert_eq!(
        engine.search(&request("visible")).unwrap().total_entries,
        0,
        "变成隐藏文件后不该还搜得到"
    );
    assert_same_as_full_rescan(&engine, &dir);
    let _ = std::fs::remove_dir_all(&dir);
}

// ─────────────────────────── 批量 ───────────────────────────

#[test]
fn a_hundred_changes_are_applied_in_one_batch() {
    let dir = fixture("batch");
    let (engine, _) = Engine::new(&[dir.as_path()]).unwrap();

    let mut paths = Vec::new();
    for i in 0..100 {
        let p = dir.join(format!("batch-{i:03}.txt"));
        std::fs::write(&p, "x").unwrap();
        paths.push(p);
    }
    let report = engine.apply_changes(&ChangeSet { paths }).unwrap();

    assert!(!report.fell_back, "这批不该退回全量重扫");
    assert_eq!(report.applied, 100, "100 个变化应该都生效");
    assert_same_as_full_rescan(&engine, &dir);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_batch_of_changes_in_one_directory_is_applied_in_one_batch() {
    let dir = fixture("batch-one-dir");
    let (engine, _) = Engine::new(&[dir.as_path()]).unwrap();

    let mut paths = Vec::new();
    for i in 0..50 {
        let p = dir.join(format!("same-{i:03}.txt"));
        std::fs::write(&p, "x").unwrap();
        paths.push(p);
    }
    // 同一个目录的事件**同时**到达 —— 不该互相打架
    engine.apply_changes(&ChangeSet { paths }).unwrap();

    assert_same_as_full_rescan(&engine, &dir);
    let _ = std::fs::remove_dir_all(&dir);
}

// ─────────────────────────── 幂等 / 无关事件 ───────────────────────────

#[test]
fn applying_the_same_change_twice_is_harmless() {
    let dir = fixture("idempotent");
    let (engine, _) = Engine::new(&[dir.as_path()]).unwrap();

    std::fs::write(dir.join("twice.txt"), "x").unwrap();
    let changes = ChangeSet {
        paths: vec![dir.join("twice.txt")],
    };
    engine.apply_changes(&changes).unwrap();
    let first = snapshot_of(&engine);
    engine.apply_changes(&changes).unwrap();
    assert_eq!(first, snapshot_of(&engine), "重复应用不该改变索引");
    assert_same_as_full_rescan(&engine, &dir);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_path_outside_the_index_is_ignored() {
    let dir = fixture("outside");
    let (engine, _) = Engine::new(&[dir.as_path()]).unwrap();
    let before = snapshot_of(&engine);

    engine
        .apply_changes(&ChangeSet {
            paths: vec![PathBuf::from("/definitely/not/indexed/at/all.txt")],
        })
        .unwrap();

    assert_eq!(before, snapshot_of(&engine), "索引外的路径不该动索引");
    let _ = std::fs::remove_dir_all(&dir);
}

// ─────────────────────────── 退化兜底 ───────────────────────────

#[test]
fn an_unknown_parent_falls_back_to_a_full_rescan() {
    let dir = fixture("fallback");
    let (engine, _) = Engine::new(&[dir.as_path()]).unwrap();

    // 父目录**不在索引里**（它是建完索引之后才出现的）→ 算不出深度
    std::fs::create_dir_all(dir.join("brand-new-dir")).unwrap();
    std::fs::write(dir.join("brand-new-dir/inside.txt"), "x").unwrap();

    let report = engine
        .apply_changes(&ChangeSet {
            paths: vec![dir.join("brand-new-dir/inside.txt")],
        })
        .unwrap();

    assert!(
        report.fell_back,
        "父目录不在索引里 → 算不出深度 → 应该退回全量重扫"
    );
    assert!(
        report.indexed > 0,
        "退回全量重扫之后，报告里的条目数必须是**真实的**，不能是 0 \
         （上层拿它更新状态栏，写 0 会显示「已索引 0 项」）"
    );
    // 退回之后，索引仍然要与磁盘一致
    assert_same_as_full_rescan(&engine, &dir);
    let _ = std::fs::remove_dir_all(&dir);
}

// ─────────────────────────── 计数器与深度 ───────────────────────────

#[test]
fn the_child_count_of_the_parent_follows_the_changes() {
    let dir = fixture("counters");
    let (engine, _) = Engine::new(&[dir.as_path()]).unwrap();

    let count_of_root = |e: &Engine| {
        e.all_entries()
            .into_iter()
            .find(|x| x.path == dir.to_string_lossy())
            .map(|x| x.child_count)
            .unwrap()
    };
    let before = count_of_root(&engine);

    std::fs::write(dir.join("counter.txt"), "x").unwrap();
    engine
        .apply_changes(&ChangeSet {
            paths: vec![dir.join("counter.txt")],
        })
        .unwrap();
    assert_eq!(count_of_root(&engine), before + 1, "新增一个文件 → 父目录子项数 +1");

    std::fs::remove_file(dir.join("counter.txt")).unwrap();
    engine
        .apply_changes(&ChangeSet {
            paths: vec![dir.join("counter.txt")],
        })
        .unwrap();
    assert_eq!(count_of_root(&engine), before, "删掉一个文件 → 父目录子项数 -1");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_depth_of_new_entries_is_relative_to_the_index_root() {
    let dir = fixture("depth");
    let (engine, _) = Engine::new(&[dir.as_path()]).unwrap();

    std::fs::write(dir.join("sub/deeper/deepest.md"), "x").unwrap();
    engine
        .apply_changes(&ChangeSet {
            paths: vec![dir.join("sub/deeper/deepest.md")],
        })
        .unwrap();

    let entry = engine
        .all_entries()
        .into_iter()
        .find(|e| e.path.ends_with("deepest.md"))
        .expect("应该在索引里");
    assert_eq!(entry.depth, 3, "根=0，sub=1，deeper=2，文件=3");
    let _ = std::fs::remove_dir_all(&dir);
}
