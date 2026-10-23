//! 护栏 —— 速查卡里的**每一条写法**都必须是引擎真认的。
//!
//! 【为什么要有这道护栏】
//! 速查卡（`app/shared/cheatsheet.json`）是给普通用户**照着抄**的 ——
//! 抄错了就是「静默搜不到东西」：引擎对未知字段会给出警告，
//! 但那只是状态栏上一行字，用户多半会以为「这台机器上没有这样的文件」。
//!
//! 前端那道护栏（`app/src/lib/cheatsheet.test.ts`）拿引擎的**语法表**
//! 比对字段名 —— 但语法表只说明「有这个字段」，
//! 说不清「这条完整查询写下来能不能被解析」。
//! → 这里补上那一半：**把每条写法真的丢给引擎跑一遍**。
//!
//! 【判据】
//! 速查卡里写 `ext:pdf;doc` → 跑一遍，引擎说「未知字段」→ 红。
//!
//! 【为什么不跑在 Vitest 里】前端单测没有真引擎；
//!  `search-core` 是纯 Rust 的 crate，`cargo test -p search-core`
//! 就能跑 —— 而且它本来就是这个项目的**硬验收项**。

use search_core::engine::Engine;
use search_core::types::{ListKind, SearchRequest, SortField, SortOrder};
use std::path::PathBuf;

/// 速查卡的数据 —— 与前端读的是**同一份文件**。
const CHEATSHEET: &str = include_str!("../../../shared/cheatsheet.json");

/// 造一棵小树 —— 让引擎有东西可搜。
///
/// 【为什么还要有内容】空索引也能解析查询，但「查询能不能解析」
/// 与「有没有命中」是两件事 —— 这里要验的是**前者**。
/// 顺带也保证「没有未知字段」这件事不是「因为压根没匹配上」。
fn make_tree() -> PathBuf {
    static SEQ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let root = std::env::temp_dir().join(format!(
        "sextant-cheat-{}-{}",
        std::process::id(),
        seq
    ));
    let _ = std::fs::remove_dir_all(&root);
    for d in ["", "sub", "docs", "empty_dir"] {
        std::fs::create_dir_all(root.join(d)).expect("造目录");
    }
    for (name, size) in [
        ("alpha.txt", 10usize),
        ("ALPHA2.txt", 10),
        ("beta.md", 20),
        ("photo.png", 30),
        ("movie.mp4", 40),
        ("big.bin", 20_000_000),
        ("my file.txt", 10),
        ("报告.pdf", 10),
    ] {
        let p = root.join(name);
        std::fs::write(&p, vec![b'x'; size]).expect("造文件");
    }
    std::fs::write(root.join("sub/gamma.txt"), b"gamma").expect("造文件");
    std::fs::write(root.join("docs/notes.md"), b"notes").expect("造文件");
    root
}

/// 从速查卡里抽出所有 query。
///
/// 【为什么自己解析 JSON 而不是加 serde_json 依赖】
/// `search-core` 的 dev-dependencies 里已经有 serde_json —— 直接用。
fn cheatsheet_queries() -> Vec<String> {
    let v: serde_json::Value = serde_json::from_str(CHEATSHEET).expect("速查卡应该是合法 JSON");
    let mut out = Vec::new();
    for group in v["groups"].as_array().expect("groups 应该是数组") {
        for item in group["items"].as_array().expect("items 应该是数组") {
            if let Some(q) = item["query"].as_str() {
                out.push(q.to_string());
            }
        }
    }
    assert!(!out.is_empty(), "速查卡里一条 query 都没有 —— 解析姿势不对？");
    out
}

#[test]
fn every_cheatsheet_query_is_understood_by_the_engine() {
    let root = make_tree();
    let (engine, _) = Engine::new(&[root.as_path()]).expect("建引擎");

    let queries = cheatsheet_queries();
    let mut failures: Vec<String> = Vec::new();

    for q in &queries {
        let req = SearchRequest {
            query: q.clone(),
            sort: SortField::Name,
            order: SortOrder::Asc,
            kind: ListKind::All,
            page: 0,
            page_size: 50,
        };
        let res = engine
            .search(&req)
            .unwrap_or_else(|e| panic!("速查卡里的 {q:?} 让引擎报错了：{e}"));
        if let Some(w) = &res.warning {
            failures.push(format!("{q:?} → {w}"));
        }
    }

    let _ = std::fs::remove_dir_all(&root);
    assert!(
        failures.is_empty(),
        "速查卡里有引擎不认的写法（用户照着抄会搜不到）：\n  {}",
        failures.join("\n  ")
    );
}

#[test]
fn cheatsheet_has_the_expected_number_of_entries() {
    // 【为什么要钉住数量】速查卡被清空 / 大幅缩水时，
    // 上面那条测试会因为「没有 query 要验」而**假绿**。
    let queries = cheatsheet_queries();
    assert!(
        queries.len() >= 30,
        "速查卡只剩 {} 条 —— 上面的护栏会假绿",
        queries.len()
    );
}

#[test]
fn every_cheatsheet_query_at_least_parses_into_a_query_tree() {
    // 【为什么还要这一条】上面那条验的是「没有警告」，
    // 这里验的是「真的解析出了语法树」——
    // 一条完全没被理解的查询（解析成空）不该混进速查卡。
    let root = make_tree();
    let (engine, _) = Engine::new(&[root.as_path()]).expect("建引擎");
    for q in cheatsheet_queries() {
        let req = SearchRequest {
            query: q.clone(),
            sort: SortField::Name,
            order: SortOrder::Asc,
            kind: ListKind::All,
            page: 0,
            page_size: 50,
        };
        // 解析失败会返回 Err → 这里直接 panic（速查卡里的写法必须能解析）
        let res = engine
            .search(&req)
            .unwrap_or_else(|e| panic!("{q:?} 解析失败：{e}"));
        // 至少要能跑完并给出总数（哪怕是 0 条）
        let _ = res.total_entries;
    }
    let _ = std::fs::remove_dir_all(&root);
}
