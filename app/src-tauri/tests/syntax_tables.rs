//! 防漂移护栏 —— 引擎的语法表与前端测试用的 fixture 必须是同一份。
//!
//! 【为什么要有这道护栏】
//! 帮助面板按「引擎认不认」把上游文档里的项标灰。判定依据是引擎的
//! 三张表（`引擎的语法表（`search-core/src/syntax.rs` 的 `MODIFIERS` / `FIELDS` /
//! `SYNTAX_OPERATORS`）。
//!
//! 前端的单元测试原来**手抄**了一份这三张表。引擎以后加一个字段，
//! 那份手抄的副本不会跟着变 —— **测试仍然是绿的**，但面板会用那份
//! 过期副本去标灰，把引擎明明支持的新字段标成「当前引擎不支持」（误灰）。
//!
//! 【现在怎么做】
//! 表只留一份：`app/shared/syntax-tables.json`。
//!   - Rust 侧（本文件）：拿引擎的**真实**表跟它对账 —— 不一致就红。
//!   - TS 侧（`app/src/lib/syntax-help.test.ts`）：读**同一份** JSON。
//!
//! 【判据】往 `search-core/src/syntax.rs` 的表里加一个词 →
//! 本测试必须变红（然后把 fixture 一起更新）。

use search_core::engine::Engine;
use search_core::types::{ListKind, SearchRequest, SortField, SortOrder};
use serde::Deserialize;
use std::collections::HashSet;

/// fixture 里的三张表 —— 跟 `app/src/lib/types.ts` 的 `SyntaxTables` 同构。
#[derive(Deserialize)]
struct SyntaxTables {
    modifiers: Vec<String>,
    fields: Vec<String>,
    operators: Vec<String>,
}

/// 编译期就把 fixture 读进来 —— 文件改了会重编译，不会读到旧内容。
const FIXTURE: &str = include_str!("../../shared/syntax-tables.json");

/// 引擎的表是**逗号分隔**的一整串（`fs_syntax_*` 返回的就是它）。
fn split_table(table: &str) -> Vec<String> {
    table
        .split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect()
}

/// 比对时按【集合】比（排序后逐项比）。
///
/// 顺序对使用方没有意义：前端只拿它建 Set 做查找，Rust 侧只做校验。
/// 只有真的**增删了词**才应该报红 —— 免得改个顺序就得同步改 fixture。
fn sorted(mut xs: Vec<String>) -> Vec<String> {
    xs.sort();
    xs
}

fn check_table(name: &str, from_engine: &str, from_fixture: &[String]) {
    let engine = sorted(split_table(from_engine));
    let fixture = sorted(from_fixture.to_vec());

    assert_eq!(
        engine, fixture,
        "引擎的 {name} 表和 app/shared/syntax-tables.json 对不上。\n\
         引擎改了表，前端测试读的 fixture 也得一起更新 —— \
         否则面板会用过期副本标灰，把引擎支持的新项误标成「不支持」。\n\
         （引擎: {engine:?}\n fixture: {fixture:?}）"
    );

    // 重复的词会让前端的 Set 查找丢词 —— 顺带查一下。
    let mut uniq = fixture.clone();
    uniq.dedup();
    assert_eq!(
        uniq.len(),
        fixture.len(),
        "fixture 的 {name} 表里有重复的词"
    );
}

#[test]
fn fixture_matches_engine_syntax_tables() {
    let fixture: SyntaxTables =
        serde_json::from_str(FIXTURE).expect("解析 app/shared/syntax-tables.json");
    check_table("modifiers", &Engine::syntax_modifiers(), &fixture.modifiers);
    check_table("fields", &Engine::syntax_fields(), &fixture.fields);
    check_table("operators", &Engine::syntax_operators(), &fixture.operators);
}

/// 造一棵小树 —— 让引擎有东西可搜。
///
/// 【为什么需要内容】空索引也能解析查询，但我们要验的是
///   「这个写法引擎**认**不认」—— 认 = 不给警告。
fn make_tree() -> std::path::PathBuf {
    static SEQ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let root = std::env::temp_dir().join(format!(
        "sextant-syntax-tables-{}-{}",
        std::process::id(),
        seq
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("造目录");
    for name in ["a.txt", "b.png", "c.pdf"] {
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
        page_size: 50,
    }
}

/// 字段的候选值 —— 挨个试，只要**有一个**写法不报警告就算「引擎真认」。
///
/// 【为什么不写死每个字段该配什么值】
///   那样加字段时得同步改这张表，忘了改就变成「测试变绿、实际没人验」。
///   → 这里给一组通用候选值，让引擎自己回答「我认不认」。
const FIELD_CANDIDATES: &[&str] = &["", "1", "pdf", "text", "today", "1M", "/tmp"];

/// 【护栏的核心】表里的每一项，**真引擎都必须认**。
///
/// 【为什么要有这道护栏】
///   前端判断「引擎认不认某项语法」的唯一依据是**它在这张表里**
///   （`app/src/lib/syntax-help.ts` 的 `isRowSupported`）。
///   → 表里有、引擎不认 = 前端显示「支持」、引擎却报警告 → 用户照抄搜不到。
///   这正是「表与实现必须一致」这条规则要防的事。
///
/// 【判据】往 `search-core/src/syntax.rs` 的表里加一个**没实现**的词 → 本测试变红。
#[test]
fn every_field_in_the_engine_table_is_really_accepted() {
    let root = make_tree();
    let (engine, _) = Engine::new(&[root.as_path()]).expect("建引擎");

    let mut failures: Vec<String> = Vec::new();
    for field in split_table(&Engine::syntax_fields()) {
        let accepted = FIELD_CANDIDATES.iter().any(|v| {
            let q = format!("{field}:{v}");
            let res = engine.search(&request(&q)).expect("查询不该失败");
            res.warning.is_none()
        });
        if !accepted {
            failures.push(format!(
                "{field}: 引擎对它的所有写法都报警告（候选值 {FIELD_CANDIDATES:?}）"
            ));
        }
    }

    let _ = std::fs::remove_dir_all(&root);
    assert!(
        failures.is_empty(),
        "字段表里有引擎不认的项 —— 前端会把它显示成「支持」，用户照抄却搜不到：\n  {}",
        failures.join("\n  ")
    );
}

/// 同上，但验的是**修饰符**。
///
/// 修饰符的写法统一是 `<修饰符>:<值>` —— 值取一个普通词即可。
#[test]
fn every_modifier_in_the_engine_table_is_really_accepted() {
    let root = make_tree();
    let (engine, _) = Engine::new(&[root.as_path()]).expect("建引擎");

    let mut failures: Vec<String> = Vec::new();
    for modifier in split_table(&Engine::syntax_modifiers()) {
        let q = format!("{modifier}:probe");
        let res = engine.search(&request(&q)).expect("查询不该失败");
        if let Some(w) = res.warning {
            failures.push(format!("{modifier}: → {w}"));
        }
    }

    let _ = std::fs::remove_dir_all(&root);
    assert!(
        failures.is_empty(),
        "修饰符表里有引擎不认的项 —— 前端会把它显示成「支持」：\n  {}",
        failures.join("\n  ")
    );
}

#[test]
fn fixture_is_populated_and_well_formed() {
    let fixture: SyntaxTables =
        serde_json::from_str(FIXTURE).expect("解析 app/shared/syntax-tables.json");

    for (name, table) in [
        ("modifiers", &fixture.modifiers),
        ("fields", &fixture.fields),
        ("operators", &fixture.operators),
    ] {
        /* 【为什么这条不是同义反复】`fixture_matches_engine_syntax_tables` 比的是
         * 两边【相等】—— 哪天有人把引擎的表清空了（重构时的手滑），
         * 那条测试照样绿，但帮助面板会拿一张空表去标灰，
         * 把引擎【其实支持】的项全标成「当前引擎不支持」（全灰）。
         * 所以这里额外钉住：条目数下限 + 每一条都干净 + 无重复。 */
        assert!(
            table.len() >= 10,
            "fixture 的 {name} 表只有 {} 条 —— 少得不像样，引擎的表是不是被清空了？",
            table.len()
        );

        let mut seen = HashSet::new();
        for entry in table {
            assert!(
                !entry.trim().is_empty(),
                "fixture 的 {name} 表里有空条目"
            );
            assert_eq!(
                entry,
                entry.trim(),
                "fixture 的 {name} 表里有带空白的条目：{entry:?}"
            );
            assert!(
                !entry.contains(','),
                "fixture 的 {name} 表里有带逗号的条目：{entry:?}\
                 （引擎的表是逗号分隔的一整串，条目里带逗号会把它切碎）"
            );
            assert!(
                seen.insert(entry),
                "fixture 的 {name} 表里有重复的条目：{entry:?}\
                 （前端拿它建 Set，重复项会让查找丢词）"
            );
        }
    }
}
