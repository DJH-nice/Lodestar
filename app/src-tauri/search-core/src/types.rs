//! 数据类型 —— **与前端契约一一对应**。
//!
//! 【这份文件是「前端零改动」的地基】
//!   序列化后的 JSON 形状必须和旧引擎时代**完全一样**：
//!   字段名、枚举的小写写法、`Option` 的 null 语义，一处都不能变。
//!   前端（`app/src/lib/types.ts`）不认 Rust 类型，只认这份 JSON。
//!   → 改任何一个字段名，前端就静默拿到 undefined，界面少一块但不报错。
//!
//! 【枚举的 serde 写法】
//!   `#[serde(rename_all = "lowercase")]` → `ListKind::All` 序列化成 `"all"`。
//!   前端那边是 `'all' | 'files' | 'folders'`，对得上。

use serde::Serialize;

/// 结果集里要哪一段。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ListKind {
    All,
    Files,
    Folders,
}

/// 排序维度。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SortField {
    Name,
    Path,
    Size,
    Mtime,
}

/// 排序方向。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SortOrder {
    Asc,
    Desc,
}

/// 一段命中区间（**字节偏移**）。
///
/// 【基准】相对谁？
///   - `Entry::highlights`      → 相对 `name`（文件名，不含路径）
///   - `Entry::path_highlights` → 相对 `path`（完整路径）
///   两套坐标**不能混用** —— 前端渲染「位置」列时用的是 `dir`，
///   而 `dir` 是 `path` 的严格前缀，所以落在 `dir` 内的偏移两套同值。
///
/// 【长度】本引擎给出的是**精确长度**（不是「一直高亮到结尾」那种哨兵值），
///   前端 `highlight.ts` 仍然会按渲染串长度截断，两边兼容。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Highlight {
    pub start: u32,
    pub length: u32,
}

/// 一条命中结果。
#[derive(Debug, Clone, Serialize)]
pub struct Entry {
    /// 文件名（不含路径）。索引根目录那一条也是 basename。
    pub name: String,
    /// 完整路径（文件或目录自身）。
    pub path: String,
    /// 父目录。索引根目录这一项是它自己的父目录。
    pub dir: String,
    /// 扩展名（不含点号），没有就是空串。
    pub extension: String,
    /// 字节数；目录是 0。
    pub size: i64,
    /// Unix 秒。
    pub mtime: i64,
    pub is_folder: bool,
    /// 相对 `name` 的命中区间。
    pub highlights: Vec<Highlight>,
    /// 相对 `path` 的命中区间（只有搜路径时才有）。
    pub path_highlights: Vec<Highlight>,
}

/// 一次搜索的结果。
#[derive(Debug, Clone, Serialize)]
pub struct SearchResult {
    /// 本页的条目。
    pub entries: Vec<Entry>,
    /// 该 kind 的总命中数（不受分页影响）。
    pub total_entries: u32,
    /// 命中里文件的数量。
    pub total_files: u32,
    /// 命中里目录的数量。
    pub total_folders: u32,
    /// false = 搜索被取消，结果是部分的。
    pub complete: bool,
    /// 引擎内部耗时（毫秒）—— **不含**排队时间。
    pub elapsed_ms: u64,
    /// 非空 = 查询里有未知字段等可提示的问题。
    pub warning: Option<String>,
    /// 「全部」列表里目录段与文件段的分界行号（页内下标）。
    /// 前端据此插入分组分隔行。
    pub group_boundary: Option<u32>,
}

/// 一次搜索请求。
#[derive(Debug, Clone)]
pub struct SearchRequest {
    pub query: String,
    pub sort: SortField,
    pub order: SortOrder,
    pub kind: ListKind,
    pub page: u32,
    pub page_size: u32,
}

/// 引擎错误。
///
/// 【为什么不叫 ShimError】shim 已经不存在了 —— 这个 crate 不再有 C 边界。
/// 但**错误语义保持不变**，因为前端与上层逻辑依赖它们区分失败原因。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineError {
    InvalidArg,
    NoMemory,
    ScanFailed,
    /// 搜索失败。
    ///
    /// 【为什么现在没人产生它】查询的**语法**问题不再走错误路径 ——
    ///   解析器永不失败：「还没写完」= 已解析的部分 + 一句中文提示
    ///   （见 `query::parse`）。真正的内部故障用 `Internal`。
    SearchFailed,
    NotFound,
    Internal(String),
}

impl std::fmt::Display for EngineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EngineError::InvalidArg => write!(f, "invalid argument"),
            EngineError::NoMemory => write!(f, "out of memory"),
            EngineError::ScanFailed => write!(f, "scan failed"),
            EngineError::SearchFailed => write!(f, "search failed"),
            EngineError::NotFound => write!(f, "not found"),
            EngineError::Internal(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for EngineError {}

/// 引擎里的一条**索引条目**（内部用，不直接序列化给前端）。
///
/// 【为什么内部结构比 `Entry` 简单】
///   `Entry` 是给前端的成品（每条都要 String 分配）；
///   内部条目保留**折叠后的名字/路径**（`folded_name` / `folded_path`），
///   查询时直接字节比较，不用每条重新折叠 ——
///   这是本引擎最重要的一个提速点（见 `fold.rs` 的说明）。
#[derive(Debug, Clone)]
pub struct IndexEntry {
    pub name: String,
    pub path: String,
    pub dir: String,
    pub extension: String,
    pub size: i64,
    pub mtime: i64,
    pub is_folder: bool,
    /// 索引根目录的深度 = 0，其下每层 +1。
    pub depth: u32,
    /// 子项数量（目录才有意义）：文件 + 目录。
    pub child_count: u32,
    /// 子**文件**数量（目录才有意义）—— `childfilecount:` 字段用。
    pub child_file_count: u32,
    /// 子**目录**数量（目录才有意义）—— `childfoldercount:` 字段用。
    pub child_folder_count: u32,
    /// NFD + 小写之后的文件名。
    pub folded_name: String,
    /// NFD + 小写之后的完整路径。
    pub folded_path: String,
}

impl IndexEntry {
    /// 前端要的成品。
    pub fn to_entry(&self) -> Entry {
        Entry {
            name: self.name.clone(),
            path: self.path.clone(),
            dir: self.dir.clone(),
            extension: self.extension.clone(),
            size: self.size,
            mtime: self.mtime,
            is_folder: self.is_folder,
            highlights: Vec::new(),
            path_highlights: Vec::new(),
        }
    }
}
