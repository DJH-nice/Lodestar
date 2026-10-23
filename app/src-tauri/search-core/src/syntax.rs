//! 查询语法的**唯一真相**。
//!
//! 【为什么是唯一真相】
//!   前端有三处要用这张表：帮助面板（哪些语法引擎认）、查询串着色、语法校验。
//!   三处各抄一份 → 加一个字段忘了改某处 → 界面标灰标错、但测试还绿着。
//!   → 表只留一份，Rust 侧导出给前端（`syntax_tables` 命令），
//!     前端测试读的也是同一份 JSON（`app/shared/syntax-tables.json`）。
//!
//! 【哪些是「通用惯例」，哪些是上游遗留】
//!   通用惯例（保留）：`ext:` `size:` `path:` `depth:` `dm:` 这些字段写法，
//!     以及 `AND/OR/NOT` 关键字 —— 这些是行业通用写法，不是谁独创的。
//!   上游遗留（待 M2 清理）：`childfilecount` / `nofileonly` 这类细分修饰符。
//!   **能力与写法要分开看**：写法可以通用，**实现必须自己写**。

/// 修饰符 —— 改匹配**方式**（而不是匹配**什么**）。
///
/// 裸用是开关（`file` = 只看文件），带冒号是「用这个方式匹配后面的词」
/// （`file:报告` = 用「只看文件」的方式搜「报告」）。
pub const MODIFIERS: &[&str] = &[
    "nocase",
    "exact",
    "file",
    "files",
    "folder",
    "folders",
    "path",
    "nopath",
    "regex",
    "noregex",
    "case",
    "nofileonly",
    "nofilesonly",
    "nofolderonly",
    "nofoldersonly",
];

/// 函数字段 —— 匹配**条目的某个属性**。
pub const FIELDS: &[&str] = &[
    "childcount",
    "childfilecount",
    "childfoldercount",
    "contenttype",
    "depth",
    "dm",
    "datemodified",
    "empty",
    "ext",
    "parent",
    "parents",
    "size",
    "app",
    "archive",
    "audio",
    "doc",
    "pic",
    "video",
];

/// 运算符与关键字。
pub const OPERATORS: &[&str] = &[
    "NOT", "AND", "&&", "OR", "||", "(", ")", "=", ":", "<", "<=", ">", ">=", "!", "..",
];

/// 三张表打成一个包，方便一次性序列化给前端。
#[derive(Debug, Clone, serde::Serialize)]
pub struct SyntaxTables {
    pub modifiers: Vec<String>,
    pub fields: Vec<String>,
    pub operators: Vec<String>,
}

impl SyntaxTables {
    pub fn new() -> Self {
        SyntaxTables {
            modifiers: MODIFIERS.iter().map(|s| s.to_string()).collect(),
            fields: FIELDS.iter().map(|s| s.to_string()).collect(),
            operators: OPERATORS.iter().map(|s| s.to_string()).collect(),
        }
    }
}

impl Default for SyntaxTables {
    fn default() -> Self {
        Self::new()
    }
}

/// 字段名 → 分类。用于「已知但还没实现」的字段给出**明确提示**，
/// 而不是静默返回 0 条（那会让人以为「这台机器上没有这样的文件」）。
pub fn field_supported(name: &str) -> bool {
    matches!(
        name,
        "ext" | "size" | "dm" | "datemodified" | "depth" | "empty" | "parent" | "parents"
            | "childcount" | "contenttype" | "pic" | "audio" | "video" | "doc" | "archive"
            | "app"
    )
}

/// 修饰符名 → 是否已经实现。
pub fn modifier_supported(name: &str) -> bool {
    matches!(
        name,
        "nocase" | "case" | "exact" | "file" | "files" | "folder" | "folders" | "path"
            | "nopath" | "regex" | "noregex"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tables_are_not_empty_and_have_no_dupes() {
        for (name, table) in [
            ("modifiers", MODIFIERS),
            ("fields", FIELDS),
            ("operators", OPERATORS),
        ] {
            assert!(!table.is_empty(), "{name} 表不能为空");
            let mut seen = std::collections::HashSet::new();
            for item in table.iter() {
                assert!(seen.insert(*item), "{name} 表里有重复项：{item}");
            }
        }
    }

    #[test]
    fn every_supported_field_is_actually_in_the_table() {
        // 【为什么要这条】supported 名单与表不一致 →
        //   支持但表里没有 → 前端把它标灰（误灰，用户以为不支持）。
        // 【反方向是允许的】表里有但还没实现 → 引擎会给「暂未实现」的警告，
        //   这是 M2 的清理项（把不实现的字段从表里去掉）。
        for f in [
            "ext", "size", "dm", "datemodified", "depth", "empty", "parent", "parents",
            "childcount", "contenttype", "pic", "audio", "video", "doc", "archive", "app",
        ] {
            assert!(
                field_supported(f),
                "字段 {f} 是已实现的，却没进 supported 名单"
            );
            assert!(
                FIELDS.contains(&f),
                "字段 {f} 是已实现的，却不在公开的字段表里 —— 前端会误标灰"
            );
        }
    }
}
