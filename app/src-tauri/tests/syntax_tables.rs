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

use search_core::Engine;
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
