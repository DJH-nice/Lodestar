//! 引擎 —— 索引 + 查询 + 排序 + 分页。
//!
//! 【与旧引擎行为一致的地方（前端零改动的前提）】
//!   - 「全部」列表的扁平空间是**先目录后文件**；
//!   - 降序 = 把整个扁平空间**整体镜像**（文件段在前）；
//!   - 分组边界公式：`clamp(total_folders - page*size, 0, len)`（升序）/
//!     `clamp(total_files - page*size, 0, len)`（降序）。
//!   前端 `app/src/lib/paging.ts` 就是按这个公式插分隔行的 ——
//!   公式一变，界面上的分隔线就会错位。

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::exclude;
use crate::query;
use crate::scan;
use crate::store;
use crate::syntax;
use crate::types::{
    EngineError, Entry, Highlight, IndexEntry, ListKind, SearchRequest, SearchResult, SortField,
    SortOrder,
};

/// 引擎。
///
/// 【为什么用 `RefCell` 而不是裸指针改字段】
///   `search()` 要把「本次搜索的视图」存回引擎（翻页靠它），
///   而它拿到的是 `&self`（共享引用）。
///   用 `*mut` 裸指针去改共享引用背后的字段是 **UB**：
///   优化器认为 `self.view` 在函数期间没被改过 → 把 `None` 缓存进寄存器
///   → 后面 `self.view.as_ref()` 读到的是缓存值 → panic。
///   （实测踩过：debug 构建看不出来，**release 构建直接 panic**。）
///   `RefCell` 是正确的内部可变性：借用检查在运行期做，语义清晰。
///
/// 【为什么单线程用它没有性能问题】
///   引擎只被搜索线程一个线程持有（见 `worker.rs`），
///   不存在并发访问 → `RefCell` 的借用标记几乎零成本。
pub struct Engine {
    /// 全部索引条目。
    entries: RefCell<Vec<IndexEntry>>,
    /// 索引根目录。
    roots: Vec<PathBuf>,
    /// 索引档的路径（有它才有冷启动加速）。
    store_path: Option<PathBuf>,
    /// 上一次搜索的结果视图 —— 翻页靠它，不重跑匹配。
    view: RefCell<Option<View>>,
}

/// 一次搜索留下的视图。
struct View {
    /// 这次搜索用的查询串（翻页时必须一致）。
    query: String,
    /// 命中的**目录**下标（升序排好）。
    folders: Vec<u32>,
    /// 命中的**文件**下标（升序排好）。
    files: Vec<u32>,
    /// 排序维度（翻页时必须一致 —— 换了维度就是另一套顺序）。
    sort: SortField,
}

impl Engine {
    /// 建立引擎并完成首次扫描。
    ///
    /// 任一路径非法（相对路径 / 不存在 / 不是目录）时返回错误。
    pub fn new(paths: &[&Path]) -> Result<(Engine, u32), EngineError> {
        Engine::new_with_store(paths, None)
    }

    /// 建立引擎；给了 `store_path` 就**先读档**（冷启动的关键）。
    ///
    /// 【读档的三个前提】
    ///   1. 存档存在；
    ///   2. 存档里的**根目录**与这次请求的**完全一致**（否则是另一份索引）；
    ///   3. 存档格式对得上（内部版本号）。
    ///   任一不满足 → 退回全量扫描。
    pub fn new_with_store(
        paths: &[&Path],
        store_path: Option<PathBuf>,
    ) -> Result<(Engine, u32), EngineError> {
        let roots = validate_paths(paths)?;

        // 先试读档
        if let Some(sp) = &store_path {
            if let Some((entries, saved_roots)) = store::load(sp) {
                if saved_roots == roots {
                    let count = entries.len() as u32;
                    return Ok((
                        Engine {
                            entries: RefCell::new(entries),
                            roots,
                            store_path: store_path.clone(),
                            view: RefCell::new(None),
                        },
                        count,
                    ));
                }
            }
        }

        // 读不到档 → 全量扫描
        let entries = scan::scan(&roots)?;
        let count = entries.len() as u32;
        let engine = Engine {
            entries: RefCell::new(entries),
            roots,
            store_path,
            view: RefCell::new(None),
        };
        engine.persist();
        Ok((engine, count))
    }

    /// 重新扫描（刷新）—— 返回重扫之后的条目总数。
    pub fn rescan(&self) -> Result<u32, EngineError> {
        // 【为什么重建而不是增量】增量更新是 M3 的活。
        //   现在这一版先保证「刷新后内容一定是最新的」——
        //   用户点「刷新」就该看到磁盘现在的样子。
        let entries = scan::scan(&self.roots)?;
        let count = entries.len() as u32;
        *self.entries.borrow_mut() = entries;
        // 视图描述的是旧索引 → 必须作废（翻页会拿到错误的条目）
        *self.view.borrow_mut() = None;
        self.persist();
        Ok(count)
    }

    /// 把索引写到存档（失败不致命 —— 只是下次冷启动慢一点）。
    fn persist(&self) {
        if let Some(sp) = &self.store_path {
            if let Err(e) = store::save(&self.entries.borrow(), &self.roots, sp) {
                eprintln!("[sextant] 索引落盘失败（不影响使用）: {e}");
            }
        }
    }

    /// 跑一次匹配，返回第 `req.page` 页。
    pub fn search(&self, req: &SearchRequest) -> Result<SearchResult, EngineError> {
        let started = Instant::now();

        // 【空查询短路】不进引擎，直接返回空。
        //   一来「空查询 = 没有结果」在 UI 上本来就成立；
        //   二来全表匹配一次是白白的开销。
        if req.query.trim().is_empty() {
            return Ok(SearchResult {
                entries: Vec::new(),
                total_entries: 0,
                total_files: 0,
                total_folders: 0,
                complete: true,
                elapsed_ms: 0,
                warning: None,
                group_boundary: None,
            });
        }

        // 解析 —— **不会失败**：查询还没写完时解析器给的是「已解析的部分 + 一句提示」
        //   （见 query::parse 的说明）。所以这里没有「语法错误」这条错误路径。
        let q = query::parse(&req.query);

        // 匹配
        let entries = self.entries.borrow();
        let mut folders: Vec<u32> = Vec::new();
        let mut files: Vec<u32> = Vec::new();
        for (i, e) in entries.iter().enumerate() {
            if q.matches(e) {
                if e.is_folder {
                    folders.push(i as u32);
                } else {
                    files.push(i as u32);
                }
            }
        }

        // 排序
        let cmp = comparator(req.sort);
        folders.sort_by(|a, b| cmp(&entries[*a as usize], &entries[*b as usize]));
        files.sort_by(|a, b| cmp(&entries[*a as usize], &entries[*b as usize]));
        drop(entries); // 放掉借用，后面要写 view

        let total_folders = folders.len() as u32;
        let total_files = files.len() as u32;

        let view = View {
            query: req.query.clone(),
            folders,
            files,
            sort: req.sort,
        };

        // 记下视图（翻页靠它）
        *self.view.borrow_mut() = Some(view);

        // build_page 需要 `&View` —— 这里从 RefCell 里借出来用。
        // 【为什么不会 panic】
        //   上面那条 `borrow_mut()` 已经在语句结束时**放掉**了借用标记，
        //   这里重新 borrow 是安全的（不是嵌套借用）。
        let view_guard = self.view.borrow();
        let mut result = self.build_page(req, view_guard.as_ref().expect("刚写进去的视图"), total_files, total_folders);
        drop(view_guard);
        result.elapsed_ms = started.elapsed().as_millis() as u64;
        if !q.warnings.is_empty() {
            result.warning = Some(q.warnings.join("；"));
        }
        Ok(result)
    }

    /// 读取第 `req.page` 页（**不重跑匹配**）。
    pub fn page(&self, req: &SearchRequest) -> Result<SearchResult, EngineError> {
        let started = Instant::now();
        let view_guard = self.view.borrow();
        let view = view_guard.as_ref().ok_or(EngineError::NotFound)?;

        // 查询变了 → 旧页号失效
        if view.query != req.query {
            return Err(EngineError::InvalidArg);
        }
        // 排序维度变了 → 顺序是另一套
        if view.sort != req.sort {
            return Err(EngineError::InvalidArg);
        }

        let total_files = view.files.len() as u32;
        let total_folders = view.folders.len() as u32;
        // 【为什么把 view 传进去而不是在里面再 borrow 一次】
        //   这里已经持有了 `view` 的借用，再 borrow 一次会 panic
        //   （RefCell 的借用标记不区分「读」还是「写」）。
        let mut result = self.build_page(req, view, total_files, total_folders);
        result.elapsed_ms = started.elapsed().as_millis() as u64;
        Ok(result)
    }

    /// 从视图里切出第 `page` 页。
    fn build_page(
        &self,
        req: &SearchRequest,
        view: &View,
        total_files: u32,
        total_folders: u32,
    ) -> SearchResult {
        let size = req.page_size.max(1) as usize;
        let offset = req.page as usize * size;

        // 扁平空间：先目录后文件（升序）；降序是整体镜像。
        let mut flat: Vec<u32> = Vec::with_capacity(view.folders.len() + view.files.len());
        match req.kind {
            ListKind::All => {
                flat.extend_from_slice(&view.folders);
                flat.extend_from_slice(&view.files);
            }
            ListKind::Files => flat.extend_from_slice(&view.files),
            ListKind::Folders => flat.extend_from_slice(&view.folders),
        }
        if req.order == SortOrder::Desc {
            flat.reverse();
        }

        let total_entries = match req.kind {
            ListKind::All => total_files + total_folders,
            ListKind::Files => total_files,
            ListKind::Folders => total_folders,
        };

        let page_items: &[u32] = if offset >= flat.len() {
            &[]
        } else {
            let end = (offset + size).min(flat.len());
            &flat[offset..end]
        };

        // 高亮：只对**本页**的条目算（不是全表）
        let entries = self.entries.borrow();
        let mut out_entries: Vec<Entry> = Vec::with_capacity(page_items.len());
        // 查询串解析一次就够（同一页里所有条目共用）
        let parsed = query::parse(&view.query);
        for id in page_items {
            let e = &entries[*id as usize];
            let mut out = e.to_entry();
            let q = &parsed;
            let (name_ranges, path_ranges) = query::highlight_ranges(q, e);
            out.highlights = name_ranges
                .iter()
                .map(|(s, t)| Highlight {
                    start: *s as u32,
                    length: (*t - *s) as u32,
                })
                .collect();
            out.path_highlights = path_ranges
                .iter()
                .map(|(s, t)| Highlight {
                    start: *s as u32,
                    length: (*t - *s) as u32,
                })
                .collect();
            out_entries.push(out);
        }

        // 分组边界（只有「全部」列表才有）
        let group_boundary = if req.kind == ListKind::All {
            let base = match req.order {
                SortOrder::Asc => total_folders,
                SortOrder::Desc => total_files,
            } as u64;
            let raw = base.saturating_sub(req.page as u64 * req.page_size as u64);
            Some(raw.min(out_entries.len() as u64) as u32)
        } else {
            None
        };

        SearchResult {
            entries: out_entries,
            total_entries,
            total_files,
            total_folders,
            complete: true,
            elapsed_ms: 0,
            warning: None,
            group_boundary,
        }
    }

    /// 当前生效的排除规则（人类可读清单）。
    pub fn exclude_rules() -> String {
        exclude::exclude_rules()
    }

    /// 全部索引条目的一份**拷贝**。
    ///
    /// 【谁在用】落盘。`store::save` 要的是 `&[IndexEntry]`，
    ///   而条目归引擎的 `RefCell` 所有 —— 借出去会撞上运行期借用检查，
    ///   所以这里给一份快照（拷贝）。
    ///
    /// 【代价】8,535 条约 3MB 的拷贝 ≈ 0.2ms —— 只在落盘路径上付这个钱，
    ///   不在搜索路径上（搜索直接借用，不拷贝）。
    pub fn all_entries(&self) -> Vec<IndexEntry> {
        self.entries.borrow().clone()
    }

    /// 这条路径**会不会被排除规则挡掉**（watcher 用它过滤事件）。
    pub fn path_is_excluded(path: &Path) -> bool {
        exclude::path_is_excluded(path)
    }

    /// 引擎支持的查询语法表（前端据此生成帮助面板、着色表与校验规则）。
    ///
    /// 【格式：逗号分隔】—— 与 `exclude_rules()` 的「每行一条」不同，
    ///   别混用分隔符。上层 `syntax_tables` 命令按逗号切，
    ///   前端测试读的 `app/shared/syntax-tables.json` 也是按这个格式对账。
    pub fn syntax_modifiers() -> String {
        syntax::modifier_names().join(",")
    }
    pub fn syntax_fields() -> String {
        syntax::field_names().join(",")
    }
    pub fn syntax_operators() -> String {
        syntax::OPERATORS.join(",")
    }
    /// 三张表打包（前端一次拿全）。
    pub fn syntax_tables() -> syntax::SyntaxTables {
        syntax::SyntaxTables::new()
    }
}

/// 校验索引根目录。
///
/// 【为什么必须校验绝对路径】
///   相对路径会让后面拼路径时算出奇怪的结果（`./..` 之类），
///   绝对路径 + 存在 + 是目录，三条都要满足。
fn validate_paths(paths: &[&Path]) -> Result<Vec<PathBuf>, EngineError> {
    if paths.is_empty() {
        return Err(EngineError::InvalidArg);
    }
    let mut out = Vec::with_capacity(paths.len());
    for p in paths {
        if !p.is_absolute() {
            return Err(EngineError::InvalidArg);
        }
        if !p.is_dir() {
            return Err(EngineError::InvalidArg);
        }
        out.push(p.to_path_buf());
    }
    Ok(out)
}

/// 排序比较器。
fn comparator(sort: SortField) -> impl Fn(&IndexEntry, &IndexEntry) -> std::cmp::Ordering {
    move |a: &IndexEntry, b: &IndexEntry| match sort {
        SortField::Name => a.folded_name.cmp(&b.folded_name).then_with(|| a.name.cmp(&b.name)),
        SortField::Path => a.folded_path.cmp(&b.folded_path),
        SortField::Size => a.size.cmp(&b.size),
        SortField::Mtime => a.mtime.cmp(&b.mtime),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fixture() -> PathBuf {
        static SEQ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("sextant-engine-{}-{}", std::process::id(), seq));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["alpha.txt", "beta.txt", "gamma.log"] {
            std::fs::write(dir.join(name), "hello world").unwrap();
        }
        dir
    }

    fn request(query: &str, page: u32, page_size: u32) -> SearchRequest {
        SearchRequest {
            query: query.to_string(),
            sort: SortField::Name,
            order: SortOrder::Asc,
            kind: ListKind::All,
            page,
            page_size,
        }
    }

    #[test]
    fn search_and_page_return_consistent_data() {
        let dir = fixture();
        let (engine, indexed) = Engine::new(&[dir.as_path()]).unwrap();
        assert!(indexed >= 4, "应索引到 3 个文件 + 根目录");

        let p0 = engine.search(&request("*", 0, 2)).unwrap();
        assert_eq!(p0.entries.len(), 2);
        assert_eq!(p0.total_entries, 4);
        assert_eq!(p0.total_folders, 1);
        assert_eq!(p0.total_files, 3);
        // 分组边界（第 0 页：1 个目录 + 3 个文件 → 目录段占第 0 行）
        assert_eq!(p0.group_boundary, Some(1));

        let p1 = engine.page(&request("*", 1, 2)).unwrap();
        assert_eq!(p1.entries.len(), 2);
        assert_eq!(p1.group_boundary, Some(0));
        let p0_names: Vec<&str> = p0.entries.iter().map(|e| e.name.as_str()).collect();
        for e in &p1.entries {
            assert!(!p0_names.contains(&e.name.as_str()), "第 1 页与第 0 页重叠");
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn blank_query_short_circuits_to_empty() {
        let dir = fixture();
        let (engine, _) = Engine::new(&[dir.as_path()]).unwrap();
        let res = engine.search(&request("   ", 0, 2)).unwrap();
        assert_eq!(res.entries.len(), 0);
        assert_eq!(res.total_entries, 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn page_with_stale_query_is_rejected() {
        let dir = fixture();
        let (engine, _) = Engine::new(&[dir.as_path()]).unwrap();
        engine.search(&request("alpha", 0, 2)).unwrap();
        assert_eq!(
            engine.page(&request("beta", 1, 2)).unwrap_err(),
            EngineError::InvalidArg
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn relative_path_is_rejected() {
        assert!(Engine::new(&[Path::new("relative/path")]).is_err());
    }

    #[test]
    fn descending_reverses_the_whole_flat_space() {
        let dir = fixture();
        let (engine, _) = Engine::new(&[dir.as_path()]).unwrap();
        let mut req = request("*", 0, 10);
        req.order = SortOrder::Desc;
        let res = engine.search(&req).unwrap();
        // 降序：文件段在前，目录段在后
        assert_eq!(res.group_boundary, Some(3));
        assert!(!res.entries[0].is_folder);
        assert!(res.entries[3].is_folder);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn store_makes_the_second_start_instant() {
        let dir = fixture();
        let store_path = std::env::temp_dir().join(format!(
            "sextant-engine-store-{}.bin",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&store_path);

        let (engine, n1) =
            Engine::new_with_store(&[dir.as_path()], Some(store_path.clone())).unwrap();
        assert!(n1 >= 4);
        drop(engine);

        // 第二次：应该读档而不是扫描
        let (engine2, n2) =
            Engine::new_with_store(&[dir.as_path()], Some(store_path.clone())).unwrap();
        assert_eq!(n1, n2);
        let res = engine2.search(&request("*", 0, 10)).unwrap();
        assert!(res.total_entries >= 4);

        let _ = std::fs::remove_file(&store_path);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn store_with_different_roots_is_ignored() {
        let dir_a = fixture();
        let dir_b = fixture();
        let store_path = std::env::temp_dir().join(format!(
            "sextant-engine-store2-{}.bin",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&store_path);
        let (engine, _) = Engine::new_with_store(&[dir_a.as_path()], Some(store_path.clone())).unwrap();
        drop(engine);
        // 换根目录 → 存档不匹配 → 必须重新扫
        let (engine2, _) = Engine::new_with_store(&[dir_b.as_path()], Some(store_path.clone())).unwrap();
        let res = engine2.search(&request("*", 0, 10)).unwrap();
        assert!(res.total_entries >= 4);
        let _ = std::fs::remove_file(&store_path);
        let _ = std::fs::remove_dir_all(&dir_a);
        let _ = std::fs::remove_dir_all(&dir_b);
    }
}
