//! 护栏 —— 新字段必须在**整条链路**上都对：扫描 → 索引档 → 查询。
//!
//! 【为什么单元测试不够】
//!   `query.rs` 的单测用的是**手搓的** `IndexEntry`（child_count 直接填数）。
//!   但真正会出错的地方在链路中间：
//!     - 扫描器有没有把「子文件 / 子目录」分开数（数错了，字段值就是错的）；
//!     - 索引档有没有把这两个数**存进去**（存漏了，读档后全变 0，
//!       `childfilecount:` 会给出一批**看着正常、其实全错**的结果）。
//!   → 这里用**真的目录树**跑一遍：扫描 → 落档 → 读档 → 查询。
//!
//! 【判据】
//!   索引档格式一改（`store.rs` 的 VERSION），这条测试必须变红
//!   —— 除非新字段也一起存了、一起读了。

use search_core::engine::Engine;
use search_core::types::{ListKind, SearchRequest, SortField, SortOrder};
use std::path::PathBuf;

/// 造一棵小树 —— 结构是刻意设计的：
///
/// ```text
/// root/
///   a.txt        文件
///   b.png        文件
///   c.pdf        文件
///   sub/         目录（2 个文件 + 1 个子目录）
///     x.txt
///     y.md
///     deeper/
///       z.txt
///   empty/       空目录
/// ```
///
/// 根目录 = 3 个文件 + 2 个目录（sub / empty）
/// sub   = 2 个文件 + 1 个目录（deeper）
fn make_tree() -> PathBuf {
    static SEQ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let root = std::env::temp_dir().join(format!(
        "lodestar-fields-{}-{}",
        std::process::id(),
        seq
    ));
    let _ = std::fs::remove_dir_all(&root);
    for d in ["", "sub", "sub/deeper", "empty"] {
        std::fs::create_dir_all(root.join(d)).expect("造目录");
    }
    for name in ["a.txt", "b.png", "c.pdf", "sub/x.txt", "sub/y.md", "sub/deeper/z.txt"] {
        std::fs::write(root.join(name), b"x").expect("造文件");
    }
    root
}

fn request(query: &str) -> SearchRequest {
    SearchRequest {
        query: query.to_string(),
        sort: SortField::Name,
        order: SortOrder::Asc,
        kind: ListKind::All,
        page: 0,
        page_size: 100,
    }
}

/// 跑一条查询，返回命中的**名字**（排序后）。
fn hits(engine: &Engine, query: &str) -> Vec<String> {
    let res = engine.search(&request(query)).expect("查询不该失败");
    assert!(
        res.warning.is_none(),
        "查询 {query:?} 报警告了：{:?}",
        res.warning
    );
    let mut names: Vec<String> = res.entries.iter().map(|e| e.name.clone()).collect();
    names.sort();
    names
}

/// 【核心】扫描 → 落档 → **读档** → 查询。
///
/// 【为什么要读档】只测扫描的话，索引档存漏字段这个 bug 抓不到 ——
///   而那正是「子文件数全是 0」这种**静默错答**的来源。
#[test]
fn child_counts_survive_the_round_trip_and_match_the_tree() {
    let root = make_tree();
    let store_path = std::env::temp_dir().join(format!(
        "lodestar-fields-store-{}.bin",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&store_path);

    // 第一次：扫描 → 落档
    let (engine, _) =
        Engine::new_with_store(&[root.as_path()], Some(store_path.clone())).expect("建引擎");
    let before = hits(&engine, "childcount:5");
    drop(engine);

    // 第二次：读档（不扫描）
    let (engine2, _) =
        Engine::new_with_store(&[root.as_path()], Some(store_path.clone())).expect("读档");
    let after = hits(&engine2, "childcount:5");

    assert_eq!(
        before, after,
        "读档后的结果必须跟扫描后一致 —— 索引档漏了字段就会不一样"
    );
    // 根目录：3 个文件 + 2 个目录 = 5 个子项 → 全树只有它命中
    let root_name = root.file_name().unwrap().to_string_lossy().to_string();
    assert_eq!(
        after,
        vec![root_name],
        "整棵树里只有索引根有 5 个子项"
    );

    let _ = std::fs::remove_file(&store_path);
    let _ = std::fs::remove_dir_all(&root);
}

/// 子文件数 / 子目录数：分开数，且读档后不变。
#[test]
fn child_file_and_folder_counts_are_correct() {
    let root = make_tree();
    let store_path = std::env::temp_dir().join(format!(
        "lodestar-fields-store2-{}.bin",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&store_path);

    let (engine, _) =
        Engine::new_with_store(&[root.as_path()], Some(store_path.clone())).expect("建引擎");

    // 根目录：3 个文件（a.txt / b.png / c.pdf）
    let files = hits(&engine, "childfilecount:3");
    assert_eq!(files.len(), 1, "只有根目录有 3 个子文件：{files:?}");
    // 根目录：2 个目录（sub / empty）
    let folders = hits(&engine, "childfoldercount:2");
    assert_eq!(folders.len(), 1, "只有根目录有 2 个子目录：{folders:?}");
    // sub：2 个文件（x.txt / y.md）
    let sub_files = hits(&engine, "childfilecount:2");
    assert_eq!(sub_files, vec!["sub".to_string()]);
    // sub：1 个目录（deeper）
    let sub_folders = hits(&engine, "childfoldercount:1");
    assert_eq!(sub_folders, vec!["sub".to_string()]);

    drop(engine);
    // 读档后结论必须一样
    let (engine2, _) =
        Engine::new_with_store(&[root.as_path()], Some(store_path.clone())).expect("读档");
    assert_eq!(hits(&engine2, "childfilecount:3").len(), 1);
    assert_eq!(hits(&engine2, "childfoldercount:2").len(), 1);
    assert_eq!(hits(&engine2, "childfilecount:2"), vec!["sub".to_string()]);

    let _ = std::fs::remove_file(&store_path);
    let _ = std::fs::remove_dir_all(&root);
}

/// `contenttype:` 按 MIME 类型筛。
#[test]
fn contenttype_filters_by_mime() {
    let root = make_tree();
    let (engine, _) = Engine::new(&[root.as_path()]).expect("建引擎");

    // 文本类：a.txt / x.txt / y.md / z.txt
    let text = hits(&engine, "contenttype:text");
    assert_eq!(
        text,
        vec!["a.txt", "x.txt", "y.md", "z.txt"],
        "contenttype:text 应该只命中文本类"
    );
    // 图片
    assert_eq!(hits(&engine, "contenttype:image"), vec!["b.png"]);
    // 单个类型
    assert_eq!(hits(&engine, "contenttype:pdf"), vec!["c.pdf"]);
    // 内置的类型组名也认
    assert_eq!(hits(&engine, "contenttype:pic"), vec!["b.png"]);

    let _ = std::fs::remove_dir_all(&root);
}

/// `parents:` 是 `depth:` 的别名（与上游一致）。
#[test]
fn parents_is_the_same_as_depth() {
    let root = make_tree();
    let (engine, _) = Engine::new(&[root.as_path()]).expect("建引擎");

    // 索引根的 depth = 0，它的直接子项 depth = 1
    assert_eq!(hits(&engine, "parents:0").len(), 1, "索引根本身");
    let d1 = hits(&engine, "parents:1");
    assert!(d1.contains(&"a.txt".to_string()));
    assert!(d1.contains(&"sub".to_string()));
    // 与 depth: 的结果必须完全一样
    assert_eq!(d1, hits(&engine, "depth:1"));

    let _ = std::fs::remove_dir_all(&root);
}
