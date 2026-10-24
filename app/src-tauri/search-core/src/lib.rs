//! **Lodestar 的搜索引擎内核** —— 完全自研，无任何第三方引擎代码。
//!
//! 【这个 crate 是什么】
//!   一个 macOS 文件搜索工具的索引 + 查询内核：
//!     - `scan`   并行遍历磁盘，按排除规则剪枝
//!     - `store`  索引落盘（冷启动不用重扫）
//!     - `query`  查询语法（自己写的 parser）
//!     - `engine` 索引 + 排序 + 分页 + 高亮
//!     - `worker` 串行搜索线程（与上层解耦）
//!
//! 【为什么不依赖 Tauri】
//!   它是纯逻辑层。依赖 Tauri 就没有单独 `cargo test -p search-core` 的意义，
//!   而「引擎能独立测」是本项目最重要的一道护栏。
//!
//! 【数据流】
//! ```text
//!   磁盘 ──scan──► IndexEntry[] ──store──► 每根一个档（下次启动直接读档）
//!                      │
//!                      ├──fold（索引期折叠一次）──► folded_name / folded_path
//!                      │
//!   查询串 ──query.parse──► Query ──matches──► 命中的下标
//!                                            └──highlight_ranges──► 命中区间
//! ```

pub mod engine;
pub mod exclude;
pub mod fold;
pub mod mime;
pub mod incremental;
pub mod query;
pub mod scan;
pub mod store;
pub mod syntax;
pub mod types;
pub mod worker;

pub use engine::Engine;
pub use incremental::{ChangeSet, UpdateReport};
pub use types::{
    EngineError, Entry, Highlight, IndexEntry, ListKind, SearchRequest, SearchResult, SortField,
    SortOrder,
};
pub use worker::Searcher;
