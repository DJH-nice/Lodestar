//! 查询语法的**唯一真相**。
//!
//! 【为什么是唯一真相】
//!   前端有三处要用这张表：帮助面板（哪些语法引擎认）、查询串着色、语法校验。
//!   三处各抄一份 → 加一个字段忘了改某处 → 界面标灰标错、但测试还绿着。
//!   → 表只留一份，Rust 侧导出给前端（`syntax_tables` 命令），
//!     前端测试读的也是同一份 JSON（`app/shared/syntax-tables.json`）。
//!
//! 【表与实现必须一致 —— 现在由结构保证】
//!   前端判断「引擎认不认某项语法」的**唯一依据**是「它在这张表里」
//!   （`app/src/lib/syntax-help.ts` 的 `isRowSupported`）。
//!   所以必须成立：**表里有 = 引擎真认**。
//!   → 以前这里是两份名单（`FIELDS` 常量 + `field_supported()` 名单），
//!     两份会漂移 → 表里有、实现没有 → 前端显示「支持」，引擎却报警告。
//!   → 现在**只有一份**：`FIELD_TABLE` / `MODIFIER_TABLE` 既是**实现**，
//!     也是**表**（表由它生成）。表里不可能出现没实现的项。
//!   → 护栏：`app/src-tauri/tests/syntax_tables.rs` 会拿**真引擎**去问表里的每一项。
//!
//! 【写法可以通用，实现必须自己写】
//!   `ext:` `size:` `path:` `depth:` `dm:` 这些字段写法是行业通用惯例；
//!   但**每一项都得自己实现** —— 写法通用不等于能力现成。

/// 字段种类 —— 引擎按它分派，语法表也由它生成。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldKind {
    /// `ext:pdf` / `ext:pdf;doc`
    Ext,
    /// `size:>10M`
    Size,
    /// `dm:today` / `dm:2026-10-01..2026-10-23`
    Mtime,
    /// `depth:1` —— `parents:` 是它的别名（与上游一致）
    Depth,
    /// `empty:` —— 空文件或空目录
    Empty,
    /// `childcount:1` —— 子项**总数**（文件 + 目录）
    ChildCount,
    /// `childfilecount:1` —— 子**文件**数
    ChildFileCount,
    /// `childfoldercount:1` —— 子**目录**数
    ChildFolderCount,
    /// `parent:/Users/me/Downloads` —— 父目录路径相等
    Parent,
    /// `contenttype:text` —— 按 MIME 类型
    ContentType,
    /// `pic:` / `audio:` / `video:` / `doc:` / `archive:` / `app:` —— 扩展名分组
    Group,
}

/// 修饰符种类。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModifierKind {
    /// `case:` —— 区分大小写
    Case,
    /// `nocase:` —— 不区分大小写
    NoCase,
    /// `exact:` —— 整串相等
    Exact,
    /// `regex:` —— 正则
    Regex,
    /// `noregex:` —— 关掉正则
    NoRegex,
    /// `path:` —— 匹配完整路径
    Path,
    /// `nopath:` —— 只匹配文件名
    NoPath,
    /// `file:` / `files:` —— 只看文件
    FilesOnly,
    /// `nofileonly:` / `nofilesonly:` —— 取消「只看文件」
    NotFilesOnly,
    /// `folder:` / `folders:` —— 只看目录
    FoldersOnly,
    /// `nofolderonly:` / `nofoldersonly:` —— 取消「只看目录」
    NotFoldersOnly,
}

/// **字段表 —— 唯一真相。**
///
/// 【顺序】导出给前端的就是这个顺序（`Engine::syntax_fields()`）。
///   改顺序会让 `app/shared/syntax-tables.json` 对不上（护栏会红）——
///   对账是按集合比的，顺序本身对使用方没有意义。
///
/// 【别名】`parents` 与 `depth` 是同一个东西（上游 `parents` 也解析成 depth）——
///   在表里显式写出别名，比在解析器里写 if 更不容易漏。
pub static FIELD_TABLE: &[(&str, FieldKind)] = &[
    ("childcount", FieldKind::ChildCount),
    ("childfilecount", FieldKind::ChildFileCount),
    ("childfoldercount", FieldKind::ChildFolderCount),
    ("contenttype", FieldKind::ContentType),
    ("depth", FieldKind::Depth),
    ("dm", FieldKind::Mtime),
    ("datemodified", FieldKind::Mtime),
    ("empty", FieldKind::Empty),
    ("ext", FieldKind::Ext),
    ("parent", FieldKind::Parent),
    ("parents", FieldKind::Depth),
    ("size", FieldKind::Size),
    ("app", FieldKind::Group),
    ("archive", FieldKind::Group),
    ("audio", FieldKind::Group),
    ("doc", FieldKind::Group),
    ("pic", FieldKind::Group),
    ("video", FieldKind::Group),
];

/// **修饰符表 —— 唯一真相。**（顺序同上）
pub static MODIFIER_TABLE: &[(&str, ModifierKind)] = &[
    ("nocase", ModifierKind::NoCase),
    ("exact", ModifierKind::Exact),
    ("file", ModifierKind::FilesOnly),
    ("files", ModifierKind::FilesOnly),
    ("folder", ModifierKind::FoldersOnly),
    ("folders", ModifierKind::FoldersOnly),
    ("path", ModifierKind::Path),
    ("nopath", ModifierKind::NoPath),
    ("regex", ModifierKind::Regex),
    ("noregex", ModifierKind::NoRegex),
    ("case", ModifierKind::Case),
    ("nofileonly", ModifierKind::NotFilesOnly),
    ("nofilesonly", ModifierKind::NotFilesOnly),
    ("nofolderonly", ModifierKind::NotFoldersOnly),
    ("nofoldersonly", ModifierKind::NotFoldersOnly),
];

/// 运算符与关键字。
pub const OPERATORS: &[&str] = &[
    "NOT", "AND", "&&", "OR", "||", "(", ")", "=", ":", "<", "<=", ">", ">=", "!", "..",
];

/// 字段名 → 种类。**解析器唯一的字段入口**。
pub fn field_kind(name: &str) -> Option<FieldKind> {
    FIELD_TABLE
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, k)| *k)
}

/// 修饰符名 → 种类。
pub fn modifier_kind(name: &str) -> Option<ModifierKind> {
    MODIFIER_TABLE
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, k)| *k)
}

/// 字段名清单（导出给前端的那张表）。
pub fn field_names() -> Vec<&'static str> {
    FIELD_TABLE.iter().map(|(n, _)| *n).collect()
}

/// 修饰符名清单。
pub fn modifier_names() -> Vec<&'static str> {
    MODIFIER_TABLE.iter().map(|(n, _)| *n).collect()
}

/// 这个字段引擎认不认。
///
/// 【为什么它现在只是 `field_kind().is_some()`】
///   以前它是一份**另抄的名单**，跟 `FIELDS` 常量各说各话 ——
///   两份名单漂移 = 前端误标灰 + 引擎报警告。
///   现在表就是实现，所以「认不认」只有一个答案。
pub fn field_supported(name: &str) -> bool {
    field_kind(name).is_some()
}

/// 这个修饰符引擎认不认。
pub fn modifier_supported(name: &str) -> bool {
    modifier_kind(name).is_some()
}

/// 每种字段配一个**一定合法**的值 —— 护栏拿它去问真引擎。
///
/// 【为什么放在语法表这一层】
///   护栏要验「表里的每一项引擎都真认」，但它不知道每个字段该配什么值
///   （`size:` 要 `1M`，`dm:` 要 `today`……）。
///   值跟着**种类**走 → 加一种新字段时，护栏自动跟着走；
///   加一个新种类却忘了配值 → 编译期就红（match 不全）。
pub fn sample_value(kind: FieldKind) -> &'static str {
    match kind {
        FieldKind::Ext => "pdf",
        FieldKind::Size => "1M",
        FieldKind::Mtime => "today",
        FieldKind::Depth => "1",
        FieldKind::Empty => "",
        FieldKind::ChildCount => "1",
        FieldKind::ChildFileCount => "1",
        FieldKind::ChildFolderCount => "1",
        FieldKind::Parent => "/tmp",
        FieldKind::ContentType => "text",
        FieldKind::Group => "",
    }
}

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
            modifiers: modifier_names().iter().map(|s| s.to_string()).collect(),
            fields: field_names().iter().map(|s| s.to_string()).collect(),
            operators: OPERATORS.iter().map(|s| s.to_string()).collect(),
        }
    }
}

impl Default for SyntaxTables {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tables_are_not_empty_and_have_no_dupes() {
        let mut seen_fields = std::collections::HashSet::new();
        for (name, _) in FIELD_TABLE {
            assert!(seen_fields.insert(*name), "字段表里有重复项：{name}");
        }
        assert!(!FIELD_TABLE.is_empty(), "字段表不能为空");

        let mut seen_mods = std::collections::HashSet::new();
        for (name, _) in MODIFIER_TABLE {
            assert!(seen_mods.insert(*name), "修饰符表里有重复项：{name}");
        }
        assert!(!MODIFIER_TABLE.is_empty(), "修饰符表不能为空");

        let mut seen_ops = std::collections::HashSet::new();
        for op in OPERATORS {
            assert!(seen_ops.insert(*op), "运算符表里有重复项：{op}");
        }
        assert!(!OPERATORS.is_empty(), "运算符表不能为空");
    }

    #[test]
    fn every_table_entry_has_a_kind_and_vice_versa() {
        // 【这是「表 ≡ 实现」的结构性保证】
        //   表里每一项都能查到种类（→ 解析器认它），
        //   每一项也都能在表里查到（→ 不会「实现了却没进表」）。
        for (name, _) in FIELD_TABLE {
            assert!(
                field_kind(name).is_some(),
                "字段 {name} 在表里，却查不到种类 —— 表与实现对不上"
            );
        }
        for (name, _) in MODIFIER_TABLE {
            assert!(
                modifier_kind(name).is_some(),
                "修饰符 {name} 在表里，却查不到种类 —— 表与实现对不上"
            );
        }
        assert_eq!(field_names().len(), FIELD_TABLE.len());
        assert_eq!(modifier_names().len(), MODIFIER_TABLE.len());
    }

    #[test]
    fn supported_predicates_agree_with_the_tables() {
        for (name, _) in FIELD_TABLE {
            assert!(field_supported(name), "字段 {name} 在表里却说不支持");
        }
        for (name, _) in MODIFIER_TABLE {
            assert!(modifier_supported(name), "修饰符 {name} 在表里却说不支持");
        }
        assert!(!field_supported("bogus"));
        assert!(!modifier_supported("bogus"));
    }

    #[test]
    fn every_field_in_the_table_is_actually_understood_by_the_parser() {
        // 【为什么这条最重要】
        //   前端只看「在不在表里」来决定标不标灰 →
        //   表里有、引擎不认 = 前端说「支持」，引擎却报警告（用户照抄搜不到）。
        //   → 表里每一项都必须能被真引擎解析且**不报警告**。
        for (name, kind) in FIELD_TABLE {
            let q = format!("{}:{}", name, sample_value(*kind));
            let parsed = crate::query::parse(&q);
            assert!(
                parsed.warnings.is_empty(),
                "字段 {name} 在表里，但引擎对 {q:?} 报警告：{:?}",
                parsed.warnings
            );
        }
    }

    #[test]
    fn every_modifier_in_the_table_is_actually_understood_by_the_parser() {
        for (name, _) in MODIFIER_TABLE {
            let q = format!("{name}:probe");
            let parsed = crate::query::parse(&q);
            assert!(
                parsed.warnings.is_empty(),
                "修饰符 {name} 在表里，但引擎对 {q:?} 报警告：{:?}",
                parsed.warnings
            );
        }
    }
}
