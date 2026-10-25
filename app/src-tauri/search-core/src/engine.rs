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
use std::cmp::Ordering;
use std::collections::HashMap;
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
    /// 路径 → 条目在 `entries` 里的下标。
    ///
    /// 【为什么要有它】增量更新要能**O(1)** 找到「这条路径在索引里的哪」——
    ///   线性找的话，100 个变化就是 100 × 8,535 次字符串比较（约 10ms）。
    ///   有了它，查找是一次哈希。
    ///
    /// 【为什么键是 `Box<str>`】比 `String` 少 8 字节（没有 capacity 字段）。
    by_path: RefCell<HashMap<Box<str>, u32>>,
    /// 索引根目录。
    ///
    /// 【为什么是 `RefCell`】根目录现在**运行时可增删**（插上 U 盘加一个、拔掉删一个）。
    roots: RefCell<Vec<PathBuf>>,
    /// 索引根目录的**真实路径**（解过符号链接）—— 与 `roots` **一一对应**（同序同长）。
    ///
    /// 【为什么需要它】FSEvents 报的是**真实路径**：
    ///   监听 `/tmp/x` 时，事件报的是 `/private/tmp/x/...`（macOS 的 /tmp 是链接）。
    ///   索引里的路径却是按根目录**原样**拼出来的 —— 两边对不上，
    ///   增量更新就会把「变了的东西」认不出来。
    ///   → 用这张表把真实路径**映射回**索引空间。
    real_roots: RefCell<Vec<PathBuf>>,
    /// 索引档的**目录**（每根一个文件）。
    ///
    /// 【为什么是目录而不是「一个文件」】插拔一次外接卷 → 根的集合变了 →
    ///   「一份档装所有根」的布局会把它整份判成「不是这份索引」→ $HOME 全量重扫。
    ///   拆成「每根一个档」之后，卷的档与 $HOME 的档**互不影响**。
    store_dir: Option<PathBuf>,
    /// 上一次搜索的结果视图 —— 翻页靠它，不重跑匹配。
    view: RefCell<Option<View>>,
}

/// 一次搜索留下的视图。
struct View {
    /// 这次搜索用的查询串（翻页时必须一致）。
    query: String,
    /// 这次搜索**已经解析好**的查询 —— 存下来，翻页时不用再解析一遍
    /// （解析一次约 0.3µs，但每页都解析就是白花）。
    parsed: query::Query,
    /// 命中的**目录**下标 —— **未排序**。
    ///
    /// 【为什么不在这里就排好】
    ///   一页只看得到 `page_size` 条（默认 500），
    ///   而命中可能有几万个 → 把全部排一遍是纯浪费。
    ///   实测（n=8,948，探针）：全排序 0.56ms vs 只排当前页需要的 500 条 0.089ms
    ///   → **6.3×**。排序因此推迟到 `build_page` 里按需做。
    folders: Vec<u32>,
    /// 命中的**文件**下标 —— 未排序（同上）。
    files: Vec<u32>,
    /// 排序维度（翻页时必须一致 —— 换了维度就是另一套顺序）。
    sort: SortField,
}

impl Engine {
    /// 建立引擎并完成首次扫描。
    ///
    /// 【路径规则】相对路径 → 报错；**不存在的目录会被跳过**（外接卷随时会拔）；
    ///   一个都不剩才报错（那才是真的配错了）。
    pub fn new(paths: &[&Path]) -> Result<(Engine, u32), EngineError> {
        Engine::new_with_store(paths, None)
    }

    /// 建立引擎；给了 `store_dir` 就**先读档**（冷启动的关键）。
    ///
    /// 【读档的前提】（**每个根各读各的档** —— 见 `store::load_root`）
    ///   1. 这个根的档存在；
    ///   2. 档里记的**根路径**与这次请求的**完全一致**（哈希碰撞保护）；
    ///   3. 档的格式版本对得上。
    ///   任一不满足 → **只重扫这一个根**（别的根的档不受影响）。
    pub fn new_with_store(
        paths: &[&Path],
        store_dir: Option<PathBuf>,
    ) -> Result<(Engine, u32), EngineError> {
        let roots = validate_paths(paths)?;
        let mut all: Vec<IndexEntry> = Vec::new();
        for root in &roots {
            // 每个根各读各的档 —— 有档就秒开，没档就扫这一个根。
            let entries = match &store_dir {
                Some(dir) => match store::load_root(dir, root) {
                    Some(e) => e,
                    None => {
                        // 【为什么不在这儿写档】末尾的 `persist()` 会把**所有**根的档
                        //   写一遍 —— 在这儿再写一遍就是**同一个根写两次**
                        //   （$HOME 量级多写一次 3.4MB ≈ 10ms，纯浪费）。
                        //   → 这里只扫，末尾统一落盘。
                        scan::scan(&[root.clone()])?
                    }
                },
                None => scan::scan(&[root.clone()])?,
            };
            all.extend(entries);
        }
        let count = all.len() as u32;
        let by_path = build_by_path(&all);
        let real_roots = resolve_real_roots(&roots);
        let engine = Engine {
            entries: RefCell::new(all),
            by_path: RefCell::new(by_path),
            roots: RefCell::new(roots),
            real_roots: RefCell::new(real_roots),
            store_dir,
            view: RefCell::new(None),
        };
        // 【为什么这里要落盘】worker 的后台落盘线程只在「**有变化**」时才写档
        //   （`store_dirty` 由增量更新/重扫置位）。
        //   启动这趟全量扫描**不算变化** → 不落盘的话，
        //   「这次扫出来的结果」永远存不下来 → **每次冷启动都要全量重扫**。
        //   → 启动时同步写一次（$HOME 量级约 9.6ms，在 worker 起来之前，不影响搜索）。
        engine.persist();
        Ok((engine, count))
    }

    /// 重新扫描（刷新）—— 返回重扫之后的条目总数。
    pub fn rescan(&self) -> Result<u32, EngineError> {
        // 【为什么重建而不是增量】增量更新是 M3 的活。
        //   现在这一版先保证「刷新后内容一定是最新的」——
        //   用户点「刷新」就该看到磁盘现在的样子。
        let roots = self.roots.borrow();
        if roots.is_empty() {
            // 【为什么根被摘光时返回空索引，而不是报错】
            //   根被摘光是一个**合法状态**（用户把索引目录全删了 / 外接卷全拔了）
            //   → 索引就该是空的。`scan::scan(&[])` 报的是 InvalidArg，
            //   那是「参数非法」，不是这里的情况 → 不能把错误传给上层。
            drop(roots);
            *self.entries.borrow_mut() = Vec::new();
            *self.by_path.borrow_mut() = HashMap::new();
            // 视图描述的是旧索引 → 必须作废
            *self.view.borrow_mut() = None;
            // 【档不动】根被摘掉时它那份档原地留着 —— 插回来时要复用
            return Ok(0);
        }
        let entries = scan::scan(roots.as_slice())?;
        drop(roots);
        let count = entries.len() as u32;
        *self.by_path.borrow_mut() = build_by_path(&entries);
        *self.entries.borrow_mut() = entries;
        // 视图描述的是旧索引 → 必须作废（翻页会拿到错误的条目）
        *self.view.borrow_mut() = None;
        self.persist();
        Ok(count)
    }

    /// 把索引写到档（每根一个文件）。
    ///
    /// 【什么时候调】
    ///   - 建引擎时（全量扫完）—— 见上面 `new_with_store` 里的理由；
    ///   - 全量重扫后（`rescan()` 原来就调它）。
    ///   其余时候由 worker 的后台落盘线程负责（不占关键路径）。
    fn persist(&self) {
        // 【没有索引档就没什么可写的】
        if self.store_dir.is_none() {
            return;
        }
        for (path, bytes) in self.serialize_for_store() {
            if let Err(e) = store::save_bytes(&bytes, &path, true) {
                eprintln!("[lodestar] 索引档写入失败（不影响使用）: {e}");
            }
        }
    }

    /// 附加一个根目录（条目由调用方**在 worker 之外**扫好）。
    ///
    /// 【幂等】重复附加 = **替换**（先摘掉旧的再挂新的）。
    ///
    /// 【为什么 `by_path` 必须一起维护】
    ///   M3 的增量更新靠 `by_path` O(1) 定位「这条路径在索引里的哪」。
    ///   挂了新条目却没登记 → 增量更新认不出它 → 索引悄悄过期。
    pub fn attach_root(&self, root: &Path, entries: Vec<IndexEntry>) -> u32 {
        // 【为什么不许与已有的根重叠】见 [`drop_nested_roots`] 的四条坑 ——
        //   运行期附加的根（外接卷）也要守**同一条**规则，
        //   否则「建引擎时守住了、附加时没守住」→ 坑照样会踩。
        //
        // ⚠️ **同一个根重复附加不算「重叠」** —— 那是「替换」（幂等），
        //   外接卷重新扫描时要靠它把旧条目换成新的。
        let clash = {
            let roots = self.roots.borrow();
            roots
                .iter()
                .find(|r| {
                    let r = r.as_path();
                    r != root && (root.starts_with(r) || r.starts_with(root))
                })
                .cloned()
        };
        if let Some(other) = clash {
            eprintln!("[lodestar] 根 {root:?} 与已有的根 {other:?} 重叠 → 不附加");
            // 什么都没变 → 返回**当前**的条目数
            return self.entries.borrow().len() as u32;
        }

        // 先摘旧的（幂等）
        self.detach_root(root);

        let mut all = self.entries.borrow_mut();
        let start = all.len() as u32;
        all.extend(entries);
        {
            let mut by_path = self.by_path.borrow_mut();
            for (i, e) in all[start as usize..].iter().enumerate() {
                by_path.insert(e.path.clone().into_boxed_str(), start + i as u32);
            }
        }
        let count = all.len() as u32;
        drop(all);

        self.roots.borrow_mut().push(root.to_path_buf());
        self.real_roots
            .borrow_mut()
            .push(std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf()));

        // 视图描述的是旧索引 → 必须作废
        *self.view.borrow_mut() = None;
        count
    }

    /// 分离一个根目录 —— 条目**立即**从索引里消失。
    ///
    /// 【⚠️ 路径必须与 attach 时**逐字相同**】
    ///   这里比的是**索引空间写法**（`self.roots` 与条目路径），
    ///   **不是**规范化后的真实路径 ——
    ///   拼写不同（例如 attach 用 `/Volumes/U盘`、detach 用它的真实路径）
    ///   → **静默无效** → 条目留着 = **幽灵条目**（用户最不能接受的那种）。
    ///   → 摘根时要用与挂载时**完全一样**的那个字符串。
    ///
    /// 【不变式：只摘挂过的根】
    ///   **挂过** → 摘；**没挂过** → 原样返回、**一条条目都不删**。
    ///   （以前是无条件按路径前缀删 → 传一个没挂过的路径，
    ///     就会连带删掉**别的根**的条目 —— 这把枪一直架在这个公开 API 上。
    ///     现在这条不变式**下沉到引擎层**守住，
    ///     worker 侧那个「查不到就不去摘」的检查就变成双保险而不是唯一防线。）
    ///
    /// 【它的档不删】—— 插回来时要复用（用户要的是「插回来立刻能搜」）。
    /// 档不动是因为落盘只写「当前在索引里的根」→ 拔掉的卷那份文件原地不动 ✓。
    pub fn detach_root(&self, root: &Path) -> u32 {
        // 【先问一句「它挂过吗」】——
        //   没挂过就**什么都不做**（连别的根的条目都不能碰）。
        //   判定口径与下面删根时用的**同一份**（组件级前缀，见 `path_in_root`）。
        let attached = self.roots.borrow().iter().any(|r| path_in_root(r, root));
        if !attached {
            return self.entries.borrow().len() as u32;
        }
        let mut all = self.entries.borrow_mut();
        // 【组件级前缀】字符串前缀会让 `/a/bc` 被 `/a/b` 命中 → 漏索引
        all.retain(|e| !path_in_root(Path::new(&e.path), root));
        *self.by_path.borrow_mut() = build_by_path(&all);
        let count = all.len() as u32;
        drop(all);

        // 【为什么要按「包含」摘根，而不是按「相等」】
        //   条目是按**包含**删的（整个子树都没了）→ 落在那个子树里的**根**
        //   也必须一起从根列表里摘掉 —— 否则它会留在列表里、被算出一个
        //   **空 part** → 用空档**覆盖掉它那份好档** → 下次冷启动它的索引被永久抹掉。
        //   （口径必须与删条目的那一句一致 —— 同一份真相，见 [`path_in_root`]。）
        //
        // roots 与 real_roots 一一对应 → 按**同一个下标**删
        {
            let mut roots = self.roots.borrow_mut();
            let mut real_roots = self.real_roots.borrow_mut();
            // 从后往前删 → 删掉之后前面的下标不受影响
            let mut pos = roots.len();
            while pos > 0 {
                pos -= 1;
                if path_in_root(&roots[pos], root) {
                    roots.remove(pos);
                    if pos < real_roots.len() {
                        real_roots.remove(pos);
                    }
                }
            }
        }

        *self.view.borrow_mut() = None;
        count
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
        // 警告要在把 q 移进视图**之前**取出来（Query 是自有数据，move 之后就不能再读）
        let warning = if q.warnings.is_empty() {
            None
        } else {
            Some(q.warnings.join("；"))
        };

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
        drop(entries); // 放掉借用（排序已推迟到出页时）

        let total_folders = folders.len() as u32;
        let total_files = files.len() as u32;

        let view = View {
            query: req.query.clone(),
            parsed: q,
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
        result.warning = warning;
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

        let lf = view.folders.len();
        let lfi = view.files.len();

        // ── 算出本页要取的是哪一小段 ────────────────────────────
        //
        // 【扁平空间的形状】（前端 `paging.ts` 依赖它，**不能改**）
        //   升序：目录段 ++ 文件段
        //   降序：整个扁平空间镜像 → 文件段在前
        //
        // 【不再重建整个 flat】
        //   以前每次翻页都 `Vec::with_capacity(命中总数)` + 全量拷贝 + 全量 reverse
        //   → 8,948 命中 = 36KB/页，10 万命中 = 400KB/页。
        //   现在只取**本页需要的那一段**，代价 O(页大小)。
        let (f_lo, f_hi, x_lo, x_hi) = match req.kind {
            ListKind::All => {
                let (lo, hi) = page_window(offset, size, lf + lfi, req.order);
                (
                    lo.min(lf),
                    hi.min(lf),
                    lo.saturating_sub(lf),
                    hi.saturating_sub(lf),
                )
            }
            ListKind::Files => {
                let (lo, hi) = page_window(offset, size, lfi, req.order);
                (0, 0, lo, hi)
            }
            ListKind::Folders => {
                let (lo, hi) = page_window(offset, size, lf, req.order);
                (lo, hi, 0, 0)
            }
        };

        let entries = self.entries.borrow();
        let mut page_ids: Vec<u32> = Vec::with_capacity(size);
        if f_lo < f_hi {
            page_ids.extend(sorted_window(&entries, &view.folders, view.sort, f_lo, f_hi));
        }
        if x_lo < x_hi {
            page_ids.extend(sorted_window(&entries, &view.files, view.sort, x_lo, x_hi));
        }
        if req.order == SortOrder::Desc {
            // 降序 = 整个窗口镜像（升序窗口取出来再反转）
            page_ids.reverse();
        }

        let total_entries = match req.kind {
            ListKind::All => total_files + total_folders,
            ListKind::Files => total_files,
            ListKind::Folders => total_folders,
        };

        // 高亮：只对**本页**的条目算（不是全表）
        let mut out_entries: Vec<Entry> = Vec::with_capacity(page_ids.len());
        // 查询**已经解析好了**（存在视图里）—— 同一页里所有条目共用
        let parsed = &view.parsed;
        for id in &page_ids {
            let e = &entries[*id as usize];
            let mut out = e.to_entry();
            let q = parsed;
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

    /// 增量更新：把一批变化就地反映进索引。
    ///
    /// 【为什么不走全量重扫】全量重扫是 `$HOME` 量级 300ms+，
    ///   而且会把排在后面的搜索全堵住（搜索与重扫共用同一个 worker 线程）。
    ///   增量更新只碰**真的变了**的那些路径 —— 实测在毫秒级。
    pub fn apply_changes(
        &self,
        changes: &crate::incremental::ChangeSet,
    ) -> Result<crate::incremental::UpdateReport, EngineError> {
        let mut entries = self.entries.borrow_mut();
        let mut by_path = self.by_path.borrow_mut();
        let mut report = crate::incremental::apply(&mut entries, &mut by_path, &self.root_pairs(), changes)?;
        drop(by_path);
        drop(entries);
        if report.fell_back {
            // 【为什么退化时要作废视图】索引被整体换掉了 → 旧视图描述的是旧索引。
            *self.view.borrow_mut() = None;
            let count = self.rescan()?;
            // 【必须把新的条目数写回报告】——
            //   退化时 `apply` 返回的 `indexed` 是 0（它没真的更新索引）。
            //   上层拿这个数去提交状态（状态栏「已索引 N 项」）——
            //   不写回去的话，状态栏会显示「已索引 0 项」。
            report.indexed = count;
            return Ok(report);
        }
        // 视图描述的是**旧索引** → 必须作废（翻页会拿到错误的条目）
        *self.view.borrow_mut() = None;
        Ok(report)
    }

    /// （索引空间根, 真实路径根）的对照表。
    ///
    /// 【为什么 worker 需要它】FSEvents 报的是**真实路径**
    ///   （`/private/tmp/...`），索引里的路径是按**索引空间**拼的 ——
    ///   两边要靠这张表对上（「这条变化属于哪个根？」）。
    pub fn root_pairs(&self) -> Vec<(PathBuf, PathBuf)> {
        let roots = self.roots.borrow();
        let real_roots = self.real_roots.borrow();
        roots
            .iter()
            .zip(real_roots.iter())
            .map(|(a, b)| (a.clone(), b.clone()))
            .collect()
    }

    /// 引擎**实际**索引的根目录（索引空间写法）。
    ///
    /// 【为什么不能报「请求的那份」】引擎会**静默丢弃嵌套根**
    ///   （见 [`drop_nested_roots`]）—— 请求的路径表与实际根不一致 →
    ///   多一路冗余的 FSEvents 监听（`watcher.rs` 的 `desired_watch_dirs`
    ///   读的就是它）+ 状态栏显示的目录数也不对。
    ///   → 上层要报「引擎索引了哪些目录」，就报这一份。
    pub fn roots(&self) -> Vec<PathBuf> {
        self.roots.borrow().clone()
    }

    /// 索引档的**目录**（没有就说明**没开**落盘）。
    pub fn store_dir(&self) -> Option<PathBuf> {
        self.store_dir.clone()
    }

    /// 把索引按根切成「每根一份字节流」。
    ///
    /// 【这个函数返回的是「所有根」的快照】
    ///   引擎**不知道**哪个根变了 → 「只重写变了的根」是 **worker 侧**的职责
    ///   （worker 拿变化路径映射回根，见 `worker.rs` 的落盘那段）。
    ///   → 要只序列化某几个根，用 [`Engine::serialize_roots_for_store`]。
    ///
    /// 【为什么要按根切】每根一个档 → 插拔一个外接卷**只作废它自己那份**，
    ///   $HOME 的档**不受影响**（这一条是**读档侧**真的成立的，见 `store.rs` 的 v3 说明）。
    ///
    /// 【为什么要「序列化」而不是「给一份条目拷贝」】
    ///   条目拷贝要给 8,537 × 6 个 String 各分配一次（实测 **2.0ms**），
    ///   而序列化只是拼字节（实测约 **1ms**）—— 而且序列化本来就是
    ///   落盘必须做的那一步，不做就白做一遍。
    ///
    /// 【为什么放在后台线程】写盘那一段实测 **9.6ms**
    ///   （新建 inode 的 fsync 很贵）—— 放在关键路径上会毁掉增量更新的
    ///   「几毫秒」承诺，还会把排在后面的搜索堵住。
    pub fn serialize_for_store(&self) -> Vec<(PathBuf, Vec<u8>)> {
        let roots = self.roots.borrow().clone();
        self.serialize_roots_for_store(&roots)
    }

    /// **只**序列化指定的那几个根 —— worker 用它实现「只重写变了的根」。
    ///
    /// 【为什么要有这个「只挑几个」的版本】
    ///   落盘是**每 2 秒**一次的后台动作。插着一个 10 万文件的 U 盘时，
    ///   每次都把**所有**根重新序列化一遍 → 光 CPU 就是几毫秒到几十毫秒，
    ///   而 $HOME 那份**根本没变**。
    ///   → 只序列化**真的变了**的那个根 ✓。
    ///
    /// 【为什么「不写」不等于「删掉」】
    ///   没被挑中的根，它磁盘上的档**本来就是对的** ✓（不提交 ≠ 删掉）。
    pub fn serialize_roots_for_store(&self, wanted: &[PathBuf]) -> Vec<(PathBuf, Vec<u8>)> {
        // 没有索引档 → 没什么可落盘的
        let Some(dir) = self.store_dir.as_ref() else {
            return Vec::new();
        };
        let entries = self.entries.borrow();
        let roots = self.roots.borrow();
        roots
            .iter()
            // 【不在「要的」名单里 → 不碰】—— 档文件原地不动 ✓
            .filter(|root| wanted.contains(root))
            .map(|root| {
                let part: Vec<IndexEntry> = entries
                    .iter()
                    // 【组件级前缀】字符串前缀会让 `/a/bc` 被 `/a/b` 命中 → 漏索引
                    .filter(|e| path_in_root(Path::new(&e.path), root))
                    .cloned()
                    .collect();
                (store::root_archive_path(dir, root), store::serialize_root(root, &part))
            })
            .collect()
    }
    /// 全部索引条目的一份**拷贝**。
    ///
    /// 【谁在用】落盘。`store::serialize_root` 要的是 `&[IndexEntry]`，
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

/// 建「路径 → 下标」的表。
fn build_by_path(entries: &[IndexEntry]) -> HashMap<Box<str>, u32> {
    let mut map = HashMap::with_capacity(entries.len());
    for (i, e) in entries.iter().enumerate() {
        map.insert(e.path.as_str().into(), i as u32);
    }
    map
}

/// 根目录的**真实路径**（解符号链接）。
///
/// 【解不出来就原样用】权限不足之类的理由不该让整个引擎起不来。
fn resolve_real_roots(roots: &[PathBuf]) -> Vec<PathBuf> {
    roots
        .iter()
        .map(|r| std::fs::canonicalize(r).unwrap_or_else(|_| r.clone()))
        .collect()
}

/// 校验索引根目录。
///
/// 【为什么必须校验绝对路径】
///   相对路径会让后面拼路径时算出奇怪的结果（`./..` 之类），
///   绝对路径这条必须满足。
///
/// 【为什么改成「跳过不存在的】
///   以前是「有任何一个不存在 → 整个引擎建不起来」（`InvalidArg`）。
///   但**外接卷会拔掉** —— 枚举到卷的那一刻它还在，几毫秒后就没了。
///   那时若因为它建不起引擎 → **整个应用没有索引**。
///   → 改成：跳过不存在的；**一个都不剩**才报错（那才是真的配错了）。
///
/// 【为什么还要丢掉「被别的根包住」的根】见 [`drop_nested_roots`]。
fn validate_paths(paths: &[&Path]) -> Result<Vec<PathBuf>, EngineError> {
    let mut candidates: Vec<PathBuf> = Vec::with_capacity(paths.len());
    for p in paths {
        if !p.is_absolute() {
            return Err(EngineError::InvalidArg);
        }
        // 不存在（或已经不是目录了）→ 跳过
        if !p.is_dir() {
            continue;
        }
        candidates.push(p.to_path_buf());
    }
    if candidates.is_empty() {
        return Err(EngineError::InvalidArg);
    }
    // 【根之间不许嵌套】—— 一个不变式，消掉四个坑（见 drop_nested_roots）。
    let roots = drop_nested_roots(candidates);
    if roots.is_empty() {
        return Err(EngineError::InvalidArg);
    }
    Ok(roots)
}

/// 丢掉**被别的根包含**的根（`$HOME` 与 `$HOME/Documents` 里的后者）。
///
/// 【为什么必须丢掉，而不是报错】
///   用户可能误配（先加了 `$HOME`，后来又加了 `$HOME/Documents`）。
///   报错 = 整个应用没有索引 → 那是最糟的结果。
///   丢掉内层根 → **内容不丢**（那些文件仍在 `$HOME` 里）→
///   只是不再单独索引一遍。
///
/// 【为什么根之间不能嵌套】一旦嵌套，会同时踩四个坑：
///   1. 摘掉外层根 → 按路径前缀删条目 → 内层根的条目被**连带**删掉；
///   2. 摘掉外层根 → 内层根还留在根列表里 → 它的档被算出**空 part**
///      → 用空档**覆盖掉它那份好档** → 下次冷启动它的索引被永久抹掉；
///   3. 扫描时同一个文件的 `depth` 会随「扫一个根」还是「扫所有根」而变
///      （1 vs 2）→ `depth:` / `parents:` 是真实查询字段 →
///      **点一次刷新，前后结果就不一样**；
///   4. 同一条路径在索引里出现两次 → 搜索结果里同一个文件显示两遍。
///   → 一个不变式（**根之间不嵌套**）把这四个坑一起消掉。
///
/// 【为什么不能边加边判】
///   先收进来的那个根，可能被**后收进来的**那个包住
///   → 必须**先收集全部候选根，再两两判断**。
fn drop_nested_roots(candidates: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut kept: Vec<PathBuf> = Vec::with_capacity(candidates.len());
    for (i, cand) in candidates.iter().enumerate() {
        // 被**别的**候选根包住 → 丢弃。
        // （`j != i`：同一个路径配两次 → 第二个也在这里被丢掉 ✓）
        let outer = candidates
            .iter()
            .enumerate()
            .find(|(j, other)| *j != i && cand.starts_with(other))
            .map(|(_, other)| other);
        match outer {
            Some(outer) => eprintln!(
                "[lodestar] 索引根 {cand:?} 已被 {outer:?} 包含 → 忽略它（那些文件仍在 {outer:?} 里）"
            ),
            None => kept.push(cand.clone()),
        }
    }
    kept
}

/// 这条路径**是不是**在这个根底下（**组件级**）。
///
/// 【为什么不能用字符串前缀】`/a/bc` 会被 `/a/b` 命中 → 漏索引。
///
/// 【为什么是「一份真相」】归属判定有两处要用（摘根时删条目、切档时挑条目），
///   两处口径不一致的话，摘根会**连带删掉别的根的条目**（见 [`Engine::detach_root`]）。
///   现在**还有第三处**（worker 侧「这条变化属于哪个根？」）——
///   → 把它开放成 `pub(crate)`，让 worker 也用**这一份**，
///   而不是自己再写一遍 `starts_with`。
pub(crate) fn path_in_root(path: &Path, root: &Path) -> bool {
    path.starts_with(root)
}

/// 排序比较器（**全序** —— 任意两个不同条目都不相等）。
///
/// 【为什么要做成全序】
///   `select_nth_unstable_by` 是**不稳定**的 —— 如果比较器允许「相等」，
///   分区后的相对次序就不确定 → 同一份索引可能给出两种顺序。
///   → 最后一级用条目 id 兜底 → 顺序唯一 ✓。
///
/// 【比较顺序 = 主键 → 二级键 → path → id】
///   - 主键 / 二级键沿用旧语义（名字 = 折叠名 + 原名；路径 = 折叠路径）——
///     **不能丢**，丢了同名条目的相对顺序就会变。
///   - `path` 让结果**只由内容决定**：
///     并行扫描之后索引顺序是不确定的 → 不加 path 的话，
///     同样的磁盘内容重扫一次，列表的相对顺序可能变 → 界面上的列表会「跳」。
///   - `id` 只是兜底（理论上同 path 不会出现两次，除非索引目录互相重叠）。
fn entry_cmp(sort: SortField, entries: &[IndexEntry]) -> impl Fn(&u32, &u32) -> Ordering + '_ {
    move |a: &u32, b: &u32| {
        let ea = &entries[*a as usize];
        let eb = &entries[*b as usize];
        match sort {
            SortField::Name => ea
                .folded_name
                .cmp(&eb.folded_name)
                .then_with(|| ea.name.cmp(&eb.name))
                .then_with(|| ea.path.cmp(&eb.path))
                .then_with(|| a.cmp(b)),
            SortField::Path => ea
                .folded_path
                .cmp(&eb.folded_path)
                .then_with(|| ea.path.cmp(&eb.path))
                .then_with(|| a.cmp(b)),
            SortField::Size => ea
                .size
                .cmp(&eb.size)
                .then_with(|| ea.path.cmp(&eb.path))
                .then_with(|| a.cmp(b)),
            SortField::Mtime => ea
                .mtime
                .cmp(&eb.mtime)
                .then_with(|| ea.path.cmp(&eb.path))
                .then_with(|| a.cmp(b)),
        }
    }
}

/// 本页在「升序扁平空间」里对应的窗口 [lo, hi)。
///
/// 【降序为什么要换算】
///   降序 = 整个扁平空间的镜像 → 镜像空间的第 p 页 [offset, end)
///   对应升序空间的 [len-end, len-offset) → 取出来再整体反转即可。
fn page_window(offset: usize, size: usize, len: usize, order: SortOrder) -> (usize, usize) {
    match order {
        SortOrder::Asc => (offset.min(len), (offset + size).min(len)),
        SortOrder::Desc => (
            len.saturating_sub(offset + size),
            len.saturating_sub(offset),
        ),
    }
}

/// 取「按 sort 排好序」的 [lo, hi) 窗口。
///
/// 【为什么不整个排】
///   一页只看得到 `page_size` 条（默认 500），而命中可能有几万个。
///   实测（n=8,948，探针）：全排序 0.56ms vs 只排前 500 条 0.089ms → **6.3×**。
///   翻到很深的页时窗口变大，代价才接近全排序。
///
/// 【为什么结果与「全排序取同一段」逐条相同】
///   比较器是**全序**（见 [`entry_cmp`]）→ 谁在前完全由内容决定，
///   不依赖排序算法稳不稳定 → 部分排序与全排序给出**同一个**顺序 ✓。
///   护栏：`partial_window_matches_a_full_sort` 专门盯这一点。
///
/// 【⚠️ 分区必须作用在子切片上】
///   第一次分区把 `[0, hi)` 变成「最小的 hi 个」；
///   第二次分区**只能在这 hi 个里面**做（`head[..lo]` 与 `head[lo..hi]`）——
///   如果分区整个数组，hi 之外的元素会被换进窗口，窗口的**元素集合**就错了。
fn sorted_window(
    entries: &[IndexEntry],
    ids: &[u32],
    sort: SortField,
    lo: usize,
    hi: usize,
) -> Vec<u32> {
    let hi = hi.min(ids.len());
    if lo >= hi {
        return Vec::new();
    }
    let cmp = entry_cmp(sort, entries);
    let mut v = ids.to_vec();
    // 先分区到 hi：[0, hi) 就是「最小的 hi 个」
    if hi < v.len() {
        v.select_nth_unstable_by(hi, &cmp);
    }
    // 再在 hi 以内的头段里分区到 lo
    let head = &mut v[..hi];
    if lo > 0 {
        head.select_nth_unstable_by(lo, &cmp);
    }
    // 窗口内部还没排 → 排一下（窗口 ≤ 页大小，很便宜）
    head[lo..hi].sort_by(&cmp);
    v[lo..hi].to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// 临时目录/档目录的名字要带的序号。
    ///
    /// 【为什么不能只用 pid】同一个测试二进制**并发**跑两次会拿到同一个 pid
    ///   → 临时目录撞车 → 互相删对方的目录 → 测试结果随机。
    static SEQ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

    fn fixture() -> PathBuf {
        let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("lodestar-engine-{}-{}", std::process::id(), seq));
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

    // ── 排序护栏 ──────────────────────────────────────────────

    /// 造一条索引条目（测试用）。
    ///
    /// 【刻意造出「等值」】
    ///   同名、同大小的条目会很多 → 才能盯住排序的等值处理
    ///   （比较器是全序：主键 → 二级键 → path → id）。
    fn entry_at(id: u32, name: &str, size: i64) -> IndexEntry {
        let path = format!("/tmp/dir{}/{}", id % 3, name);
        IndexEntry {
            name: name.to_string(),
            path: path.clone(),
            dir: "/tmp".to_string(),
            extension: String::new(),
            size,
            mtime: 1_700_000_000 + id as i64,
            is_folder: false,
            depth: 1,
            child_count: 0,
            child_file_count: 0,
            child_folder_count: 0,
            folded_name: name.to_lowercase(),
            folded_path: path.to_lowercase(),
        }
    }

    /// 200 条含大量等值的条目。
    fn tie_heavy_entries() -> Vec<IndexEntry> {
        const NAMES: [&str; 5] = ["alpha.txt", "beta.txt", "gamma.log", "alpha.txt", "delta.bin"];
        (0..200u32)
            .map(|i| entry_at(i, NAMES[(i % 5) as usize], (i % 3) as i64 * 100))
            .collect()
    }

    #[test]
    fn partial_window_matches_a_full_sort() {
        // 【部分排序的核心不变量】
        //   引擎现在只排「本页需要的那一段」，而不是把全部命中排一遍。
        //   → 窗口结果必须与「全排序后取同一段」**逐条相同**。
        //   任何一条对不上 → 界面上的排序就会和以前不一样。
        let entries = tie_heavy_entries();
        let ids: Vec<u32> = (0..entries.len() as u32).collect();
        for sort in [
            SortField::Name,
            SortField::Path,
            SortField::Size,
            SortField::Mtime,
        ] {
            let cmp = entry_cmp(sort, &entries);
            let mut full = ids.clone();
            full.sort_by(&cmp);
            for (lo, hi) in [
                (0usize, 10usize),
                (0, 200),
                (7, 23),
                (50, 150),
                (190, 200),
                (0, 0),
                (100, 100),
                (200, 200),
            ] {
                let got = sorted_window(&entries, &ids, sort, lo, hi);
                let want: Vec<u32> = full[lo.min(full.len())..hi.min(full.len())].to_vec();
                assert_eq!(
                    got, want,
                    "sort={sort:?} 窗口[{lo},{hi}) 与全排序结果不一致"
                );
            }
        }
    }

    #[test]
    fn equal_keys_are_ordered_by_path_then_id() {
        // 【等值条目的顺序必须是确定的】
        //   并行扫描之后索引顺序是不确定的 → 排序必须自己给出确定顺序，
        //   否则同样的磁盘内容重扫一次，列表顺序会「跳」。
        let entries = tie_heavy_entries();
        let ids: Vec<u32> = (0..entries.len() as u32).collect();
        let got = sorted_window(&entries, &ids, SortField::Name, 0, ids.len());
        // 同名（同折叠名、同原名）的条目之间 → path 必须递增；
        // path 也相同 → id 递增。
        for w in got.windows(2) {
            let (a, b) = (w[0] as usize, w[1] as usize);
            let (ea, eb) = (&entries[a], &entries[b]);
            let key = (ea.folded_name.as_str(), ea.name.as_str());
            let key_b = (eb.folded_name.as_str(), eb.name.as_str());
            if key == key_b {
                assert!(
                    (ea.path.as_str(), a) < (eb.path.as_str(), b),
                    "等值条目没有按 (path, id) 排：{ea:?} 出现在 {eb:?} 之前"
                );
            } else {
                assert!(key < key_b, "排序键不对：{key:?} 出现在 {key_b:?} 之前");
            }
        }
    }

    #[test]
    fn page_window_mirrors_the_flat_space() {
        // 降序 = 整个扁平空间镜像 → 第 p 页(降序) = 第 (n-1-p) 页(升序) 反过来。
        let len = 7usize;
        let size = 3usize;
        let asc: Vec<(usize, usize)> = (0..3)
            .map(|p| page_window(p * size, size, len, SortOrder::Asc))
            .collect();
        let desc: Vec<(usize, usize)> = (0..3)
            .map(|p| page_window(p * size, size, len, SortOrder::Desc))
            .collect();
        // 升序第 2 页 = [6,7) → 降序第 0 页 = [7-3, 7-0) = [4,7) → 反过来 = [6,4)
        assert_eq!(asc[2], (6, 7));
        assert_eq!(desc[0], (4, 7));
        // 越界时不能 panic，窗口必须夹在 [0, len) 内
        assert_eq!(page_window(100, size, len, SortOrder::Asc), (len, len));
        assert_eq!(page_window(100, size, len, SortOrder::Desc), (0, 0));
    }

    #[test]
    fn descending_pages_mirror_the_ascending_pages() {
        // 端到端：降序的第 p 页 = 升序的第 (n-1-p) 页反过来。
        let dir = fixture();
        let (engine, _) = Engine::new(&[dir.as_path()]).unwrap();

        let size = 2u32;
        let total = engine.search(&request("*", 0, size)).unwrap().total_entries as usize;
        let pages = total.div_ceil(size as usize);
        assert!(pages >= 2, "fixture 至少要两页才测得出镜像关系");

        let fetch = |desc: bool| -> Vec<Vec<String>> {
            (0..pages)
                .map(|p| {
                    let mut req = request("*", p as u32, size);
                    req.order = if desc {
                        SortOrder::Desc
                    } else {
                        SortOrder::Asc
                    };
                    engine
                        .search(&req)
                        .unwrap()
                        .entries
                        .iter()
                        .map(|e| e.path.clone())
                        .collect()
                })
                .collect()
        };
        let asc_pages = fetch(false);
        let desc_pages = fetch(true);

        for p in 0..pages {
            let mirrored: Vec<String> = asc_pages[pages - 1 - p].iter().rev().cloned().collect();
            assert_eq!(
                desc_pages[p], mirrored,
                "降序第 {p} 页与升序第 {} 页不是镜像关系",
                pages - 1 - p
            );
        }
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

    /// 第二次启动必须是**读档**，不是「又扫了一遍」。
    ///
    /// 【为什么不能只断言「条目数相等」】
    ///   重扫出来的东西一样多 → 只比数字的话，「读档」与「重扫」分不开
    ///   （档读不出来时会静默退回扫描，测试照样绿）。
    ///   → 第一次落档后**往树里加一个文件**：重扫会多出一条，读档不会。
    #[test]
    fn the_second_start_reads_the_archive_instead_of_rescanning() {
        let dir = fixture();
        let store_dir = std::env::temp_dir().join(format!(
            "lodestar-engine-store-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&store_dir);

        let (engine, n1) =
            Engine::new_with_store(&[dir.as_path()], Some(store_dir.clone())).unwrap();
        assert!(n1 >= 4);
        drop(engine);

        // 树变了 → 「重扫」会多出一条，「读档」不会
        std::fs::write(dir.join("gamma.txt"), "new").unwrap();

        let (engine2, n2) =
            Engine::new_with_store(&[dir.as_path()], Some(store_dir.clone())).unwrap();
        assert_eq!(
            n1, n2,
            "第二次必须是读档 —— 树已经变了，重扫会多出一条"
        );
        let res = engine2.search(&request("*", 0, 10)).unwrap();
        assert!(res.total_entries >= 4);

        let _ = std::fs::remove_dir_all(&store_dir);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **每个根各有一份档** —— 换根不会作废别的根的档（v3 的核心语义）。
    ///
    /// 【为什么要有这条】v2 是「一份档装所有根」，换根 → 整份档作废 → 全量重扫。
    ///   v3 拆成「每根一个档」之后，$HOME 的档与卷的档**互不影响**。
    #[test]
    fn each_root_keeps_its_own_archive() {
        let dir_a = fixture();
        let dir_b = fixture();
        let store_dir = std::env::temp_dir().join(format!(
            "lodestar-engine-store2-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&store_dir);

        let (engine, n_a) =
            Engine::new_with_store(&[dir_a.as_path()], Some(store_dir.clone())).unwrap();
        drop(engine);

        // 换根 → B 没有档 → 扫 B；**A 的档不受影响**
        let (engine2, n_b) =
            Engine::new_with_store(&[dir_b.as_path()], Some(store_dir.clone())).unwrap();
        assert!(n_b >= 4);
        assert!(engine2.search(&request("*", 0, 10)).unwrap().total_entries >= 4);

        // 再单独读 A → 条目数与第一次一样（**A 的档没被 B 作废**）
        let (_engine3, n_a2) =
            Engine::new_with_store(&[dir_a.as_path()], Some(store_dir.clone())).unwrap();
        assert_eq!(n_a, n_a2, "换根不该作废别的根的档");

        // 档目录里应该有两个档（每根一个）
        let files = std::fs::read_dir(&store_dir).unwrap().count();
        assert_eq!(files, 2, "每个根一份档");

        let _ = std::fs::remove_dir_all(&store_dir);
        let _ = std::fs::remove_dir_all(&dir_a);
        let _ = std::fs::remove_dir_all(&dir_b);
    }

    /// **只**序列化「要」的那几个根 —— 别的根**不碰**。
    ///
    /// 【为什么要有这条】落盘是**每 2 秒**一次的后台动作：
    ///   插着一个 10 万文件的 U 盘时，每次都全量重写要动十几 MB，
    ///   而 $HOME 那份**根本没变** → 白烧 CPU 与 I/O。
    ///   → worker 只把**变了**的根交给这个函数 ✓。
    #[test]
    fn only_the_requested_roots_are_serialized() {
        let dir_a = fixture();
        let dir_b = fixture();
        let store_dir = std::env::temp_dir().join(format!(
            "lodestar-engine-store3-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&store_dir);
        let (engine, _) = Engine::new_with_store(
            &[dir_a.as_path(), dir_b.as_path()],
            Some(store_dir.clone()),
        )
        .unwrap();

        let only_a = engine.serialize_roots_for_store(&[dir_a.clone()]);
        assert_eq!(only_a.len(), 1, "只该序列化要的那个根");
        assert_eq!(
            only_a[0].0,
            store::root_archive_path(&store_dir, &dir_a),
            "交出去的档路径必须是那个根自己的档"
        );

        let all = engine.serialize_for_store();
        assert_eq!(all.len(), 2, "全部根都要在");

        let _ = std::fs::remove_dir_all(&store_dir);
        let _ = std::fs::remove_dir_all(&dir_a);
        let _ = std::fs::remove_dir_all(&dir_b);
    }

    /// 摘一个**从来没挂过**的根 → **一条条目都不能删**。
    ///
    /// 【为什么要有这条】`detach_root` 是 `pub` API ——
    ///   以前它**无条件**按路径前缀删条目 →
    ///   将来任何直接调它的调用点传一个没挂过的路径，
    ///   就会连带删掉**别的根**的条目（误伤，且很难查）。
    ///   → 这条不变式现在**下沉到引擎层**守住。
    #[test]
    fn detaching_a_root_that_was_never_attached_does_not_delete_anything() {
        let dir = fixture();
        let (engine, indexed) = Engine::new(&[dir.as_path()]).unwrap();

        // 摘一个**没挂过**的根（它连路径前缀都不沾）→ 什么都不该变
        let after = engine.detach_root(Path::new("/Volumes/从没挂过的盘"));
        assert_eq!(
            after, indexed,
            "摘一个没挂过的根不该动索引里的任何条目"
        );
        let res = engine.search(&request("*", 0, 50)).unwrap();
        assert_eq!(
            res.total_entries, indexed,
            "索引里的条目一个都不该少"
        );

        // 摘一个**路径前缀沾上**、但同样没挂过的根 → 同样什么都不该变
        // （这条最容易误伤：以前它会把那个前缀下的条目全删掉。）
        let nested = dir.join("alpha.txt");
        let after2 = engine.detach_root(nested.as_path());
        assert_eq!(
            after2, indexed,
            "摘一个没挂过的根（哪怕路径前缀沾上了）也不该删条目"
        );
        let res2 = engine.search(&request("*", 0, 50)).unwrap();
        assert_eq!(res2.total_entries, indexed, "条目一个都不该少");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 挂过的根照旧能摘掉（不变式不能把正常路径也堵死）。
    #[test]
    fn detaching_an_attached_root_still_removes_its_entries() {
        let dir = fixture();
        let (engine, indexed) = Engine::new(&[dir.as_path()]).unwrap();
        let after = engine.detach_root(dir.as_path());
        assert!(
            after < indexed,
            "挂过的根照旧能摘掉（{after} 应该小于 {indexed}）"
        );
        assert!(
            engine.roots().is_empty(),
            "摘掉的根要从根列表里消失"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 摘根之后，**那个根的档还在** —— 插回来才能秒开。
    ///
    /// 【为什么要有这条】「拔盘留档」是本特性明确要的行为
    ///   （用户要的是「插回来立刻能搜」）→ 摘根**不许**删档。
    ///   落盘只写「当前在索引里的根」→ 拔掉的卷那份文件原地不动 ✓。
    #[test]
    fn detaching_a_root_keeps_its_archive() {
        let dir = fixture();
        let store_dir = std::env::temp_dir().join(format!(
            "lodestar-engine-keep-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&store_dir);
        let (engine, indexed) =
            Engine::new_with_store(&[dir.as_path()], Some(store_dir.clone())).unwrap();
        assert!(
            store::load_root(&store_dir, &dir).is_some(),
            "建引擎时就要落档"
        );

        engine.detach_root(dir.as_path());

        let after = store::load_root(&store_dir, &dir);
        assert!(
            after.is_some(),
            "摘根之后那个根的档必须还在 —— 插回来要靠它**秒开**"
        );
        assert_eq!(
            after.unwrap().len() as u32,
            indexed,
            "档里的条目要完整（不能被清空）"
        );

        let _ = std::fs::remove_dir_all(&store_dir);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
