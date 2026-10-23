//! 防漂移护栏 —— 手册里的语法表必须覆盖引擎的**每一张**表。
//!
//! 【为什么要有这道护栏】
//! 手册（`docs/USER-GUIDE.md`）是给人看的，引擎的语法表是代码里的真相。
//! 两者一旦漂移，用户照手册写了个引擎不支持的语法 → **静默搜不到东西**：
//! 引擎对未知字段是「静默返回 0 结果」，既不报错也不提示
//! （未实现的字段落到「什么都不匹配」）。
//! 只有引擎在解析查询**之前**扫一遍 `word:` 形式的 token，
//! 把「未知字段」写进 `FsResult.warning` —— 但那也只是状态栏上一行字，
//! 用户多半会以为「这台机器上没有这样的文件」。
//!
//! 本项目的护栏模式本来就是为这件事立的（`tests/syntax_tables.rs` 让引擎的表
//! 与前端的 fixture 对账）。手册是给人看的，**要享受同等待遇**。
//!
//! 【判据】
//! 往 `search-core/src/syntax.rs` 的表里加一个词 → **本测试必须变红**
//! （然后把 `docs/USER-GUIDE.md` 一起更新）。
//!
//! 【怎么判「手册里出现了这个 token」】
//!   - **词元**（只由字母/数字/下划线组成，如 `ext` / `parent` / `NOT`）：
//!     按【词边界】找 —— 前后不能是字母/数字/下划线，且不分大小写。
//!     这样 `parent` 不会因为 `parents:` 里恰好有这几个字符就算「出现过」，
//!     `AND` 也不会被 `android` 蒙混过去。
//!   - **标点 token**（`(` `>` `..` 这种）：直接找字面量。
//!     它们太短，套词边界规则会误伤 —— `size:>10M` 里的 `>` 后面紧跟着数字，
//!     按词边界判就会漏掉一个**确实存在**的写法。

use search_core::Engine;
use std::path::PathBuf;

/// 手册的绝对路径 —— 用 `CARGO_MANIFEST_DIR` 定位，测试从哪个目录跑都对。
///
/// （`CARGO_MANIFEST_DIR` = `app/src-tauri`，手册在仓库根的 `docs/` 下，
///  所以是 `../../docs/USER-GUIDE.md`。）
fn manual_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../docs/USER-GUIDE.md")
}

/// 读手册 —— 读不到就直接失败，并说清路径。
fn read_manual() -> (String, PathBuf) {
    let path = manual_path();
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "读不到使用手册 {}：{e}\n\
             （路径写错了？手册应该在仓库根的 docs/USER-GUIDE.md。）",
            path.display()
        )
    });
    (text, path)
}

/// 引擎的表是**逗号分隔**的一整串（`fs_syntax_*` 返回的就是它）。
fn split_table(table: &str) -> Vec<String> {
    table
        .split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect()
}

/// 是不是「词字符」—— 判定词边界用（只看 ASCII：字母/数字/下划线）。
fn is_word_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// 手册里有没有这个 token。判定规则见文件头的说明。
fn manual_has_token(manual: &str, token: &str) -> bool {
    // 标点 token：直接找字面量。
    if !token.chars().all(|c| c.is_alphanumeric() || c == '_') {
        return manual.contains(token);
    }

    // 词元：找【词边界】的整词出现（不分大小写）。
    //
    // 【为什么 lower 之后再找边界】大小写只影响「是不是同一个词」，
    // 不影响「前后是不是词边界」—— 边界字符集里没有大小写字母。
    let hay = manual.to_lowercase();
    let needle = token.to_lowercase();
    let bytes = hay.as_bytes();
    let mut from = 0usize;
    while from <= hay.len() {
        let Some(rel) = hay[from..].find(&needle) else {
            break;
        };
        let start = from + rel;
        let end = start + needle.len();
        // needle 全是 ASCII —— 命中处的首字节一定是独立的一个 ASCII 字符，
        // 所以 start + 1 一定落在字符边界上，切片不会 panic。
        let before_ok = start == 0 || !is_word_char(bytes[start - 1]);
        let after_ok = end >= bytes.len() || !is_word_char(bytes[end]);
        if before_ok && after_ok {
            return true;
        }
        from = start + 1;
    }
    false
}

#[test]
fn user_guide_covers_every_engine_syntax_token() {
    let (manual, path) = read_manual();

    let mut missing: Vec<String> = Vec::new();
    for (table_name, table) in [
        ("修饰符", Engine::syntax_modifiers()),
        ("函数字段", Engine::syntax_fields()),
        ("运算符", Engine::syntax_operators()),
    ] {
        for token in split_table(&table) {
            if !manual_has_token(&manual, &token) {
                missing.push(format!("{table_name}表里的 {token:?}"));
            }
        }
    }

    assert!(
        missing.is_empty(),
        "使用手册里少了这些【引擎真的支持】的语法 token：\n  {}\n\n\
         引擎的语法表改了，手册必须跟着改 —— 否则用户照手册写了个引擎不认的写法，\
         会【静默搜不到东西】（引擎对未知字段既不报错也不提示）。\n\
         手册路径：{}",
        missing.join("\n  "),
        path.display()
    );
}

#[test]
fn user_guide_is_a_real_manual_not_a_stub() {
    let (manual, path) = read_manual();

    /* 【为什么这条不是同义反复】
     * 上一条只管「token 有没有出现」——
     * 有人把手册删到只剩一张 token 表，那条测试照样绿，
     * 但用户拿到的是一张没有「怎么用」的表。
     * 所以这里额外钉住：手册有【该有的章节】、而且不是空壳。 */
    const REQUIRED_SECTIONS: [&str; 10] = [
        "# Sextant 使用手册",
        "## 快速上手",
        "## 界面一览",
        "## 搜索",
        "### 搜索语法速查",
        "## 索引目录",
        "## 书签与最近搜索",
        "## 刷新",
        "## 快捷键总表",
        "## 常见问题",
    ];

    for section in REQUIRED_SECTIONS {
        assert!(
            manual.contains(section),
            "使用手册里没有「{section}」这一节 —— 手册被删残了？\n（路径：{}）",
            path.display()
        );
    }

    assert!(
        manual.len() > 4000,
        "使用手册只有 {} 字节 —— 短得不像样，是被清空了吗？\n（路径：{}）",
        manual.len(),
        path.display()
    );
}
