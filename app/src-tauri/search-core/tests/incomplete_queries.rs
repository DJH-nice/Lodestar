//! 护栏 —— **不完整的查询不能变成错误**。
//!
//! 【为什么要有这道护栏】
//!   用户**打字过程中**的输入天然是不完整的：`alpha AND`、单独一个 `(`、
//!   单独的 `NOT`、`ext:` 后面还没写值……
//!   以前这些会让 `query::parse()` 返回 `None` → 引擎返回
//!   `Err(SearchFailed)` → 前端把英文 "search failed" 显示在结果区，
//!   用户既不知道发生了什么，也不知道该怎么改。
//!
//!   【根因】解析器把「还没写完」和「解析不了」混为一谈：
//!   所有 `?` 都会把「缺一个 term」升级成**整条查询失败**。
//!
//! 【判据】往解析器里加一条「缺条件就整条失败」的路径 → 本测试必须变红。

use search_core::engine::Engine;
use search_core::types::{ListKind, SearchRequest, SortField, SortOrder};
use std::path::PathBuf;

/// 造一棵小树 —— 让引擎有东西可搜。
fn make_tree() -> PathBuf {
    static SEQ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let root = std::env::temp_dir().join(format!(
        "lodestar-incomplete-{}-{}",
        std::process::id(),
        seq
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("造目录");
    for name in ["alpha.txt", "beta.md", "photo.png"] {
        std::fs::write(root.join(name), b"x").expect("造文件");
    }
    root
}

/// 「打字过程中」一定会出现的输入 —— **缺条件**的那些。
///
/// 【怎么选这些】每一个都是用户**必然会打到**的中间态：
///   刚敲下逻辑词、刚敲下左括号、刚敲完字段名还没写值……
///
/// 【为什么必须给提示】不给提示的话，界面会显示一批「看起来正常」的结果，
///   用户根本不知道自己的查询还没写完 —— 静默错答比报错更糟。
const MUST_WARN: &[&str] = &[
    "(", ")", "()", "((((", ":", "::", ":a", "AND", "OR", "NOT", "!", "&&", "||", "=", "<", ">",
    "<=", ">=", "a AND", "a OR", "a &&", "a ||", "NOT ", "(alpha", "alpha)", "(alpha OR",
];

/// 合法、只是值特殊（空值 / 默认值）的查询 —— **不该**有提示。
///
/// `ext:` = 「有扩展名的条目」、`pic:` = 所有图片、`file:` = 只看文件 ……
/// 这些都是**完整**的查询，值可以为空。
const MUST_NOT_WARN: &[&str] = &[
    "ext:", "pic:", "file:", "folder:", "path:", "regex:", "nofileonly:", "dm:2026-01", "..",
];

/// 上面两组之外，还有一些「字段的值看不懂」的输入（`size:` / `dm:` …）——
/// 它们本来就该有提示，这里只钉住「不能变成错误」。
const MAY_WARN: &[&str] = &["size:", "dm:", "contenttype:", "childcount:", "size:>"];

#[test]
fn incomplete_queries_never_become_errors() {
    let root = make_tree();
    let (engine, _) = Engine::new(&[root.as_path()]).expect("建引擎");

    let mut failures: Vec<String> = Vec::new();
    for q in MUST_WARN.iter().chain(MAY_WARN).chain(MUST_NOT_WARN) {
        let req = SearchRequest {
            query: q.to_string(),
            sort: SortField::Name,
            order: SortOrder::Asc,
            kind: ListKind::All,
            page: 0,
            page_size: 50,
        };
        match engine.search(&req) {
            // ★ 关键断言：不能是 Err —— 界面会把错误原文显示给用户
            Err(e) => failures.push(format!("{q:?} → 引擎报错：{e}")),
            Ok(_) => {}
        }
    }

    let _ = std::fs::remove_dir_all(&root);
    assert!(
        failures.is_empty(),
        "这些输入不应该变成错误：\n  {}",
        failures.join("\n  ")
    );
}

#[test]
fn incomplete_queries_say_so() {
    let root = make_tree();
    let (engine, _) = Engine::new(&[root.as_path()]).expect("建引擎");

    let mut failures: Vec<String> = Vec::new();
    for q in MUST_WARN {
        let req = SearchRequest {
            query: q.to_string(),
            sort: SortField::Name,
            order: SortOrder::Asc,
            kind: ListKind::All,
            page: 0,
            page_size: 50,
        };
        let res = engine.search(&req).expect("不该报错");
        if res.warning.is_none() {
            failures.push(format!("{q:?} → 没有提示（用户会以为搜索正常）"));
        }
    }

    let _ = std::fs::remove_dir_all(&root);
    assert!(
        failures.is_empty(),
        "「还没写完」必须给一句提示：\n  {}",
        failures.join("\n  ")
    );
}

#[test]
fn complete_queries_are_not_warned_about() {
    // 反方向：合法查询不能被误伤（别把提示打成一片，用户会直接无视状态栏）
    let root = make_tree();
    let (engine, _) = Engine::new(&[root.as_path()]).expect("建引擎");

    let mut failures: Vec<String> = Vec::new();
    for q in MUST_NOT_WARN {
        let req = SearchRequest {
            query: q.to_string(),
            sort: SortField::Name,
            order: SortOrder::Asc,
            kind: ListKind::All,
            page: 0,
            page_size: 50,
        };
        let res = engine.search(&req).expect("不该报错");
        if res.warning.is_some() {
            failures.push(format!("{q:?} → 不该有警告：{:?}", res.warning));
        }
    }

    let _ = std::fs::remove_dir_all(&root);
    assert!(
        failures.is_empty(),
        "这些是完整的查询，不该有提示：\n  {}",
        failures.join("\n  ")
    );
}

#[test]
fn a_partial_query_still_shows_what_it_already_got() {
    // 「alpha AND」不能把已经写好的 alpha 也丢掉 ——
    //   用户看到的是「alpha 的结果 + 一句还没写完的提示」。
    let root = make_tree();
    let (engine, _) = Engine::new(&[root.as_path()]).expect("建引擎");

    let req = SearchRequest {
        query: "alpha AND".to_string(),
        sort: SortField::Name,
        order: SortOrder::Asc,
        kind: ListKind::All,
        page: 0,
        page_size: 50,
    };
    let res = engine.search(&req).expect("不该报错");
    assert_eq!(res.total_files, 1, "alpha.txt 应该还在结果里");
    assert!(res.warning.is_some(), "要提示还没写完");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_complete_query_has_no_warning() {
    // 反方向：完整的查询不能被误伤（别把「还没写完」的提示打成一片）
    let root = make_tree();
    let (engine, _) = Engine::new(&[root.as_path()]).expect("建引擎");

    for q in [
        "alpha",
        "ext:txt",
        "(alpha OR beta) AND txt",
        "NOT ext:png",
        "size:>10M",
        "dm:today",
        "pic:",
    ] {
        let req = SearchRequest {
            query: q.to_string(),
            sort: SortField::Name,
            order: SortOrder::Asc,
            kind: ListKind::All,
            page: 0,
            page_size: 50,
        };
        let res = engine.search(&req).expect("不该报错");
        assert!(res.warning.is_none(), "{q:?} 不该有警告：{:?}", res.warning);
    }

    let _ = std::fs::remove_dir_all(&root);
}
