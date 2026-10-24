use std::sync::{Arc, Mutex};

use search_core::types::{ListKind, SearchRequest, SortField, SortOrder};
use search_core::Searcher;
use serde::Serialize;
use tauri::{Manager, State};

use crate::index_state::IndexState;

#[derive(Debug, Clone, Serialize)]
pub struct IndexStatus {
    /// 正在建索引
    pub scanning: bool,
    /// 已索引条目数
    pub indexed: u32,
    /// 索引的目录
    pub paths: Vec<String>,
    /// 最近一次错误（比如某个路径不可读）
    pub error: Option<String>,
    /// **窗口现在可见吗**（窗口服务器的真相）。
    ///
    /// 【为什么索引状态里要带这个】前端有个 800ms 的状态轮询，
    ///   而窗口隐藏时那个轮询是纯浪费（用户看不见状态栏）。
    ///   → 前端靠这个字段决定「要不要继续轮询」。
    ///
    /// 【谁负责填】**只有真正知道窗口的人能填** ——
    ///   `IndexState` 不知道窗口的事，所以它给的是**占位值 `true`**，
    ///   由拿到 `AppHandle` 的调用点覆盖成真实值
    ///   （见 `commands::index_status` 与 `watcher::auto_rescan`）。
    ///   占位值取 `true` 是刻意的：**猜「不可见」会让状态栏停止更新**
    ///   （用户看得见的故障）；猜「可见」只是多跑一点轮询（性能问题，不是故障）。
    pub visible: bool,
    /// 当前插着的**外接卷**及各自的状态。
    ///
    /// 【为什么是可选的】和 `visible` 一样：**填不出来的调用点一律给空列表** ——
    ///   缺省 = 「没有卷」= 状态栏不显示卷那一行（不是故障）。
    /// 【谁负责填】拿着 AppHandle 的调用点（`index_status` / `rescan` / `reindex` /
    ///   `watcher::auto_rescan` / `volumes::notify_frontend`）——
    ///   `IndexState` 自己不知道卷的事（那是 `VolumeState` 的职责）。
    pub volumes: Vec<crate::volumes::VolumeStatus>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SyntaxTables {
    pub modifiers: Vec<String>,
    pub fields: Vec<String>,
    pub operators: Vec<String>,
}

fn parse_sort(s: &str) -> SortField {
    match s {
        "path" => SortField::Path,
        "size" => SortField::Size,
        "mtime" => SortField::Mtime,
        _ => SortField::Name,
    }
}

fn parse_order(s: &str) -> SortOrder {
    match s {
        "desc" => SortOrder::Desc,
        _ => SortOrder::Asc,
    }
}

fn parse_kind(s: &str) -> ListKind {
    match s {
        "files" => ListKind::Files,
        "folders" => ListKind::Folders,
        _ => ListKind::All,
    }
}

/// 从状态里取出引擎句柄。
///
/// 【为什么是 Arc<Searcher> 而不是 Searcher】：命令是 async 的，
/// 而 `std::sync::MutexGuard` **不是 Send**。如果把 guard 拿进
/// `spawn_blocking` 的闭包（哪怕只是借用），会同时踩两个编译错误：
///   1. `spawn_blocking` 要求闭包 `'static`，借用局部 guard 过不了；
///   2. guard 跨越 `.await` 存活 → 「future is not Send」。
/// 所以这里只在锁内 clone 出一个 Arc，把锁立刻放掉，再把 Arc 搬进闭包。
pub(crate) fn take_searcher(
    state: &Mutex<Option<Arc<Searcher>>>,
) -> Result<Arc<Searcher>, String> {
    let guard = state.lock().map_err(|e| e.to_string())?;
    guard
        .as_ref()
        .cloned()
        .ok_or_else(|| "索引尚未就绪".to_string())
}

#[tauri::command]
pub async fn search(
    searcher: State<'_, Mutex<Option<Arc<Searcher>>>>,
    query: String,
    sort: String,
    order: String,
    kind: String,
    page: u32,
    page_size: u32,
) -> Result<search_core::types::SearchResult, String> {
    let q_log = query.clone();
    crate::logging::log_line(&format!(
        "search query={q_log:?} sort={sort} order={order} kind={kind} page={page} size={page_size}"
    ));
    let req = SearchRequest {
        query,
        sort: parse_sort(&sort),
        order: parse_order(&order),
        kind: parse_kind(&kind),
        page,
        page_size: if page_size == 0 { 500 } else { page_size },
    };

    let engine = take_searcher(searcher.inner())?;

    // 阻塞调用包在 spawn_blocking 里 —— Tauri 的 async command 不该占住运行时线程
    //
    // 【为什么要量「往返」和「引擎」两个数】
    //   引擎自己算的 `elapsed_ms` 只有几毫秒（实测 3~4ms），
    //   但**用户等的是「往返」** —— 搜索请求要先进 worker 的队列，
    //   而搜索与重扫共用**同一个 worker 线程、同一条通道**（严格串行）。
    //   所以一次重扫（$HOME 量级 0.7~1.9 秒）会把排在它后面的搜索全堵住，
    //   而状态栏显示的 `elapsed_ms` **看不到这段等待**。
    //   → 「引擎几毫秒、往返几秒」= 用户在等重扫让出 worker。
    let t_round = std::time::Instant::now();
    let res = tauri::async_runtime::spawn_blocking(move || {
        if req.page == 0 {
            engine.search(req)
        } else {
            engine.page(req)
        }
    })
    .await
    .map_err(|e| e.to_string())?;
    let round_ms = t_round.elapsed().as_millis() as u64;
    let engine_ms = res.as_ref().map(|r| r.elapsed_ms).unwrap_or(0);

    crate::logging::log_line(&format!(
        "search done query={q_log:?} ok={} totals={:?} 往返={round_ms}ms 引擎={engine_ms}ms 排队≈{}ms",
        res.is_ok(),
        res.as_ref()
            .ok()
            .map(|r| (r.total_entries, r.total_files, r.total_folders, r.entries.len())),
        round_ms.saturating_sub(engine_ms)
    ));
    res.map_err(|e| e.to_string())
}

/// 重扫当前引擎的索引目录 —— **「刷新」按钮与「自动刷新」共用同一条路**。
///
/// 【为什么抽出来】—— 简报 §2.3
///   「刷新」按钮（[`rescan`]）与「文件变化时自动刷新」（`watcher.rs`）
///   要保证三件事一致：**状态更新一致、引擎操作串行、失败处理一致**。
///   复制一份迟早会走偏 —— 而这两条路**同时**能触发
///   （用户点「刷新」的同时 watcher 也发现了文件变化），
///   走偏的后果是「状态与引擎错位」（状态栏报旧引擎、结果表是新引擎）。
///
/// 【为什么是同步的】
///   watcher 跑在自己的**后台线程**上（不是 async）——
///   同步函数两边都能调；Tauri 命令那条路自己包一层 `spawn_blocking`。
///
/// 【为什么路径取自状态而不是引擎】
///   引擎自己记住了路径（`fs_engine_rescan` 用它们重建 store），
///   但 Rust 侧要更新状态（条目数、路径）就得有一份 —— 状态里那份
///   正是**当前活着的那个引擎**的路径（不变式见 `index_state.rs`）。
///
/// 【M1：为什么提交前要校验「引擎代次」】
///   重扫是**慢**的（`$HOME` 量级实测 ≈1.7 秒 CPU / 2~3 秒墙钟，比 reindex 还慢）。
///   它在**开始时**快照路径、在**结束时**提交 —— 这期间若 `reindex`
///   装上了新引擎并提交了新状态，这次重扫的旧快照就会把新状态覆盖掉：
///   状态栏报「已索引 20001 项」，结果表里却是新引擎的 2 条
///   （正是 Task 6 fix round 1 修掉的同一类「引擎 ≠ 界面」三方错位）。
///
///   修法 = **引擎代次号**：换上新引擎时代次 +1（同一个引擎上重扫不加），
///   提交时校验「代次还是我快照的那个吗」——
///   不是就**丢弃**这次结果（状态保持 reindex 提交的那份，正确的）。
///
///   【为什么快照（代次 + 路径）要排在取引擎【前面】】
///     反过来（先取引擎、后快照）会开一个「旧引擎 + 新代次」的窗口：
///     那期间它拿着**旧引擎**却读到**新代次** → 校验通过 →
///     把旧引擎的结果写进代表新引擎的状态 → **反向错位**，比原 bug 更糟。
///     （代次与路径现在是**一次锁内**读出来的 —— 它们之间没有窗口了；
///       但这一对整体仍然要在取引擎**之前**拿。）
///
/// 【扫描标志（scanning）的生命周期】交给 [`index_state::run_scan_blocking`] ——
///   begin / 收尾都在它里面，任何提前 return 都绕不过去。
///   （`scanning` 卡在 true 的后果见 `index_state.rs` 的说明：
///     状态栏永远「正在扫描索引…」，前端的状态翻转判断永不成立。）
///
/// 【`take_engine` 为什么是个闭包，而不是直接给引擎】——
///   「快照代次」与「取引擎」这两步的**先后顺序**是有讲究的（见上面），
///   而「顺序对不对」只有在「这两步之间恰好换了一次引擎」时才看得出来。
///   闭包让测试能**确定性地**制造那个交错 —— 不靠 sleep 撞时序
///   （重扫只要 1ms，撞时序根本撞不准）。
///   生产里传「从引擎槽位里取」那个闭包。
///
/// 【返回值】重扫之后的条目数。
pub fn do_rescan<F>(state: &IndexState, take_engine: F) -> Result<u32, String>
where
    F: FnOnce() -> Result<Arc<Searcher>, String>,
{
    // ① 【一次锁内读出「代次 + 路径」】—— 两者必然是**同一时刻**的，
    //   不用再靠「先取代次、后取路径」那条顺序约定兜着
    //   （那条约定太隐蔽，迟早有人写反 → 已由 `engine_snapshot()` 取代）。
    //
    //   【顺序要紧】这一步必须在②【之前】—— 见上面「为什么快照要排在取引擎前面」。
    let (gen, paths) = state.engine_snapshot();
    // ② 后取引擎（生产：从槽位里取；测试：可以在这里制造交错）
    let engine = take_engine()?;
    // ③ 【走 run_scan_blocking】—— scanning 的 begin/收尾由它管，
    //   中间任何提前 return（含线程 panic）都绕不过去。
    let indexed = crate::index_state::run_scan_blocking(state, || {
        engine.rescan().map_err(|e| e.to_string())
    })?;
    // ④ 用**开始时**的代次提交
    if !state.commit_if_current(gen, paths, indexed) {
        // 期间换了引擎 —— 这次重扫的是【旧引擎】，
        // 它的结果描述的是旧路径 / 旧计数，不能写进代表新引擎的状态。
        // 【丢弃它】→ 状态保持换引擎那一方提交的那份（正确的）。
        crate::logging::log_line("[rescan] 期间索引目录已更换，丢弃本次刷新结果");
    }
    Ok(indexed)
}

/// 把一批变化**增量地**应用到索引 —— 与「刷新」按钮**不同**的一条路。
///
/// 【为什么不复用 `do_rescan`】
///   重扫是**全量**的（`$HOME` 量级实测 300ms+），而且会把排在后面的搜索全堵住。
///   增量是**局部的**（毫秒级）—— 这正是 M3 要做的事。
///   但两者必须保证同一件事：**状态更新一致、引擎操作串行、失败处理一致**
///   → 所以借用同一套「**代次守卫**」骨架（快照代次 → 取引擎 → 做 → 按代次提交）。
///
/// 【与 `do_rescan` 的两处不同】
///   ① 走 [`index_state::run_incremental_blocking`] 而不是 `run_scan_blocking`
///      —— 增量更新**不能**被记成「一趟全量扫描」
///      （那会让之后的文件变化被误判成「已覆盖」而跳过）。
///   ② 失败时**不清**索引（增量更新失败 = 什么都没改）。
pub fn do_apply_changes<F>(
    state: &IndexState,
    take_engine: F,
    changes: search_core::incremental::ChangeSet,
) -> Result<u32, String>
where
    F: FnOnce() -> Result<Arc<Searcher>, String>,
{
    // ① 【一次锁内读出「代次 + 路径」】—— 理由与 `do_rescan` 完全一样：
    //    两者必须来自**同一时刻**，否则会出现「旧引擎的结果写进新引擎的状态」。
    let (gen, paths) = state.engine_snapshot();
    // ② 取引擎（生产：从槽位里取；测试：可以在这里制造交错）
    let engine = take_engine()?;
    // ③ 做增量更新
    let indexed = crate::index_state::run_incremental_blocking(state, || {
        engine
            .apply_changes(changes)
            .map(|r| r.indexed)
            .map_err(|e| e.to_string())
    })?;
    // ④ 用**开始时**的代次提交
    if !state.commit_if_current(gen, paths, indexed) {
        // 期间换了引擎 —— 这次更新动的是**旧引擎**，
        // 它的结果描述的是旧路径 / 旧计数，不能写进代表新引擎的状态。
        crate::logging::log_line("[incremental] 期间索引目录已更换，丢弃本次更新结果");
    }
    Ok(indexed)
}

/// 重新扫描索引（标题栏的「刷新」）。
///
/// 【为什么现在会更新状态】
///   以前这个命令**完全不碰 IndexState**：
///     - `scanning` 恒为 false → 前端 `pollStatus` 的
///       「wasScanning && !scanning → refreshKey++」**永不成立**
///       → 结果表里留着 rescan 之前的行，状态栏的「已索引 N 项」也不更新；
///     - 而且 800ms 的轮询也永远撞不上（wasScanning 一直是 false）。
///   现在：**begin → 重扫 → finish（带回新的条目数）→ 把新状态返回给前端**，
///   前端据此**立刻重搜一次**（不靠轮询撞状态翻转）。
///
/// 【路径从哪来 / 代次守卫 / scanning 的生命周期】—— 全在 [`do_rescan`] 里，
///   那里有完整的说明（这个命令与「文件变化时自动刷新」共用那一个函数）。
///
/// 【实现】整个重扫走 [`do_rescan`] —— **与「文件变化时自动刷新」同一条路**
///   （见 `do_rescan` 的说明：两条路必须保证状态更新一致、失败处理一致）。
///
/// 【为什么整趟丢到后台线程】
///   重扫是**阻塞的**（`$HOME` 量级实测 ≈1.7 秒 CPU / 2~3 秒墙钟）——
///   Tauri 的 async command 不该占住运行时线程。
///   `do_rescan` 自己管 scanning 的 begin/收尾，所以这里只需要把
///   「重扫 + 提交状态」整块搬到 `spawn_blocking` 里。
#[tauri::command]
pub async fn rescan<R: tauri::Runtime>(app: tauri::AppHandle<R>) -> Result<IndexStatus, String> {
    // 【为什么要 AppHandle】——
    //   `do_rescan` 要 `&IndexState`，而 `spawn_blocking` 的闭包必须 `'static`
    //   —— 借来的 `State<'_, IndexState>` 过不了这一关。
    //   `AppHandle` 是 Clone + Send + 'static：在闭包里 `handle.state::<IndexState>()`
    //   重新取一份就行（`State` 的借用只活在那一条语句里）。
    let handle = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let state = handle.state::<IndexState>();
        // 【取引擎放在闭包里】——
        //   `do_rescan` 内部是「先快照代次、后取引擎」，
        //   这个顺序不能乱（见 `do_rescan` 的说明）。
        do_rescan(state.inner(), || {
            take_searcher(handle.state::<Mutex<Option<Arc<Searcher>>>>().inner())
        })
    })
    .await
    .map_err(|e| e.to_string())??;
    // 【窗口可见性也要是真值】—— snapshot() 给的是占位值 `true`。
    //   前端拿这个返回值更新状态栏，占位值会让状态栏里的「可见性」对不上真相。
    let mut st = app.state::<IndexState>().snapshot();
    st.visible = crate::lifecycle::window_visible(&app);
    // 【卷的状态也要是真值】—— snapshot() 给的是空列表（它不知道卷的事）。
    st.volumes = crate::volumes::volume_statuses(&app);
    Ok(st)
}

/// **前端告诉我们「我现在认为窗口可见吗」** —— 它的认知。
///
/// 【为什么要有这个命令】看门狗每秒要知道「要不要给前端推可见性事件」——
///   判据是「**实际状态 ≠ 前端的认知**」。
///
///   - 「每次都推」→ 太贵：Tauri v2 的事件 = **在 webview 里执行一段 JS**，
///     窗口隐藏时 webview 睡着 → 每推一次叫醒一次 → 实测每次约 7ms CPU。
///   - 「跟上次推的不一样才推」→ 会漏：事件万一丢了（比如发生在前端
///     `listen` 注册之前），Rust 以为「已经说过了」→ **永远不推** → 状态栏冻住。
///
///   → 前端每次**真正改变**认知时调这个命令告诉 Rust。
///     稳态下两边一致 → **零 IPC**；
///     事件丢了 → 前端没 ack → Rust 知道「它还不知道」→ 继续推 → **自愈**。
#[tauri::command]
pub fn ack_visibility(state: State<'_, crate::lifecycle::Lifecycle>, visible: bool) {
    state.ack_visibility(visible);
}

#[tauri::command]
pub fn syntax_tables() -> SyntaxTables {
    // 三张表在 shim 里都是【逗号分隔】的（SYNTAX_MODIFIERS / FIELDS / OPERATORS），
    // 与 exclude_rules 的【每行一条】不同，别混用分隔符。
    let split = |s: &str| -> Vec<String> {
        s.split(',')
            .map(|x| x.trim().to_string())
            .filter(|x| !x.is_empty())
            .collect()
    };
    SyntaxTables {
        // 闭包收的是 &str，而这三个函数返回 String —— 必须借一下，
        // 否则报 error[E0308]: mismatched types（expected `&str`, found `String`）
        modifiers: split(&search_core::Engine::syntax_modifiers()),
        fields: split(&search_core::Engine::syntax_fields()),
        operators: split(&search_core::Engine::syntax_operators()),
    }
}

#[tauri::command]
pub fn exclude_rules() -> Vec<String> {
    search_core::Engine::exclude_rules()
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect()
}

#[tauri::command]
pub fn index_status<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: State<'_, IndexState>,
) -> IndexStatus {
    let mut s = state.snapshot();
    // 【把窗口可见性填成真值】—— snapshot() 给的是占位值 `true`（它不知道窗口）。
    //   前端靠这个字段决定要不要继续跑那个 800ms 的轮询，
    //   所以这里必须是**窗口服务器的真相**，不能是我们自己的标志
    //   （`⌘H` 收走窗口时我们的标志不知道，窗口却真的看不见了）。
    s.visible = crate::lifecycle::window_visible(&app);
    // 【卷的状态也要是真值】—— snapshot() 给的是空列表（它不知道卷的事）。
    s.volumes = crate::volumes::volume_statuses(&app);
    s
}

/// 用系统默认程序打开一个文件/目录。
///
/// 前端也可以用 opener 插件的 JS API，但 `revealItemInDir` 之外的动作
/// （比如「用别的程序打开」）走这里更统一。
///
/// 【runtime 要泛型化】跟 `get_settings` / `set_settings` 一个理由：
///   写死 `tauri::AppHandle`（= `AppHandle<Wry>`）的话，
///   测试用的 `MockRuntime` 就传不进来 ——
///   报 `the trait bound AppHandle: CommandArg<'_, MockRuntime> is not satisfied`。
#[tauri::command]
pub fn open_path<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    path: String,
) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;
    app.opener()
        .open_path(path, None::<&str>)
        .map_err(|e| e.to_string())
}

/// 读全部设置。
/// 【runtime 要泛型化】写死 `tauri::AppHandle`（= `AppHandle<Wry>`）的话，
/// 测试用的 `MockRuntime` 就传不进来 —— 报
/// `the trait bound AppHandle: CommandArg<'_, MockRuntime> is not satisfied`。
/// 泛型化之后，Wry 和 MockRuntime 都能用。
#[tauri::command]
pub fn get_settings<R: tauri::Runtime>(app: tauri::AppHandle<R>) -> crate::settings::Settings {
    crate::settings::load(&app)
}

/// 书签文件的大小上限 —— 1MB。
///
/// 【为什么要限】导入的文件是用户自己挑的，但万一挑了个巨大的文件，
/// `read_to_string` 会把它整个读进内存 → 内存被吃满。
/// 超了直接报错，不读。
const MAX_BOOKMARKS_FILE: u64 = 1024 * 1024;

/// 把书签写到用户选的文件里。
///
/// 【为什么不让前端直接写文件】前端没有文件系统的能力
/// （`@tauri-apps/plugin-fs` 不在这个项目里，也**不打算为此加依赖**）——
/// 路径由前端用已装的 `@tauri-apps/plugin-dialog` 拿，
/// 落盘这件事交给 Rust（`std::fs`，**零新依赖**）。
///
/// 【写什么】由前端拼好 JSON 传进来 —— 格式的真相在前端（它也要读回来校验）。
#[tauri::command]
pub fn write_bookmarks_file(path: String, contents: String) -> Result<(), String> {
    std::fs::write(&path, contents).map_err(|e| format!("写入 {path} 失败：{e}"))
}

/// 读一个书签文件的内容（由前端解析与校验）。
///
/// 【为什么只回原始文本、不在 Rust 侧解析】
///   格式的真相在前端（导入时要校验 `format`/`version`/`kind`/`value`），
///   Rust 侧只管「把字节取回来」—— 解析规则只留一份。
///
/// 【大小上限】超过 [`MAX_BOOKMARKS_FILE`] 直接报错，不读进内存。
#[tauri::command]
pub fn read_bookmarks_file(path: String) -> Result<String, String> {
    let meta = std::fs::metadata(&path).map_err(|e| format!("读不到 {path}：{e}"))?;
    if meta.len() > MAX_BOOKMARKS_FILE {
        return Err(format!(
            "{path} 太大了（{} 字节），上限 {MAX_BOOKMARKS_FILE} 字节",
            meta.len()
        ));
    }
    std::fs::read_to_string(&path).map_err(|e| format!("读 {path} 失败：{e}"))
}

/// 写全部设置。
///
/// 【注意】这是**整体替换**，不是增量改 —— 调用方必须先拿到完整对象再改一个字段。
/// 前端的所有写入都走「先 load → 改一个字段 → 整体 save」这条路，
/// 所以不会把别的字段写没。
#[tauri::command]
pub fn set_settings<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    settings: crate::settings::Settings,
) -> Result<(), String> {
    // 【走 `settings::update` —— 拿写锁】（终审 M-10）
    //   这条命令是**整体替换**（前端那条路要传完整对象）。
    //   整体替换同样是一次「写」→ 必须跟别的写者**串行**，
    //   否则它会把 Rust 侧刚写进去的字段**抹掉**。
    //   （前端现在改用 `patch_settings`（差量合并），
    //     这条命令留着给「一次性写整份设置」的场景。）
    crate::settings::update(&app, |cur| {
        *cur = settings;
        Ok(())
    })
    .map(|_| ())
}

/// 换索引目录：重建引擎。
///
/// 【为什么不写设置】写设置由前端统一做（`set_settings` 整体替换）。
/// 命令只管「把引擎换成这几个路径」，副作用最小。
///
/// 【为什么先重建、后保存】
///   前端的顺序是：先 `reindex`（成功才继续）→ 再 `saveSettings`。
///   这样路径非法时 `Engine::new` 失败 → 旧引擎原封不动、设置也没被改，
///   不会出现「设置说有 3 个目录、引擎只有旧的 1 个」的错位。
///
/// 【为什么整个过程在 spawn_blocking 里】
///   1. `Engine::new` + 首次扫描是阻塞的（`$HOME` 量级实测 1.1s）
///   2. 换掉旧 `Arc<Searcher>` 会 drop 旧对象 → join 旧搜索线程
///      → 旧线程若正在跑宽查询（实测 2.4s）要等它跑完
///   两样都不能占住 async 运行时线程。
///
/// 【空列表 = 索引家目录】
///   与启动路径同一套回退（`settings::effective_paths` 是唯一回退点）。
///   不套的话 `Engine::new(&[])` → shim 返回 NULL（`path_count == 0`）
///   → 用户看到 "invalid argument"；而且「移除最后一个目录」后
///   UI 就**永远回不到家目录模式**（唯一出路是重启应用）。
///
/// 【状态只在成功时提交】
///   `state.begin()` 只置 scanning，paths 与 indexed 等提交才写。
///   失败时 `state.fail()` 不碰它们 —— 旧引擎原封不动，
///   状态就必须还报旧引擎的那组路径与条目数，
///   否则状态栏说「已索引 0 项」而结果表里还是旧结果（反向错位）。
///
/// 【提交在哪儿做】在 `settings::rebuild_engine` 里 ——
///   **装引擎 + 递增引擎代次 + 提交状态，三步在【同一把锁内】**。
///   （代次必须在装引擎【之后】才递增 —— 理由见 `index_state.rs`。）
#[tauri::command]
pub async fn reindex<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: State<'_, IndexState>,
    paths: Vec<String>,
) -> Result<IndexStatus, String> {
    // 【先解析空列表】—— 解析之后 paths 就是「引擎真正要索引的那组路径」，
    //   rebuild_engine 提交的也是同一份解析结果，两边不会错位。
    let paths = crate::settings::effective_paths(&paths);

    let handle = app.clone();
    let for_build = paths.clone();
    // 【走 run_scan】—— scanning 的 begin/收尾由它管，
    //   线程 panic 的 JoinError 那条路也绕不过去。
    //   （状态提交在 rebuild_engine 里 —— 装引擎 + 递增代次 + 提交 = 同锁内。）
    let _indexed = crate::index_state::run_scan(&state, move || {
        crate::settings::rebuild_engine(&handle, &for_build)
    })
    .await?;
    // 这里只把当前状态回给前端。
    // 【窗口可见性也要是真值】—— 同 `rescan`：snapshot() 给的是占位值 `true`。
    let mut st = state.snapshot();
    st.visible = crate::lifecycle::window_visible(&app);
    // 【卷的状态也要是真值】—— 同 `rescan`：snapshot() 给的是空列表。
    st.volumes = crate::volumes::volume_statuses(&app);
    Ok(st)
}

/// 快捷键变更的结果 —— **总是 Ok**，成败和警告都在字段里。
///
/// 【为什么不用 `Result<(), String>`】
///   「注销失败」时**状态已经变了**（生效的键不是「用户请求的那个」）——
///   这不是「操作失败」，而是一次**部分成功**。
///   用 `Err` 表达会丢掉「实际生效的是哪个键」这个信息，
///   前端只能显示一句笼统的「操作失败」，或者骗人说「已切换」。
#[derive(Debug, Clone, Serialize)]
pub struct ShortcutChange {
    /// **实际生效**的快捷键（`None` = 确实没有键生效）。
    pub effective: Option<String>,
    /// 警告文案（`None` = 一切正常）。
    pub warning: Option<String>,
}

/// 改全局快捷键 —— **先改实际状态，再写盘**。
///
/// 【为什么不在前端直接改设置】
///   注册可能失败（快捷键被别的软件占用）。
///   如果先写盘再注册，设置里就会记着一个**没生效**的快捷键 ——
///   用户按了没反应，还不知道是为什么。
///   → 顺序必须是：注册新的 → 注销旧的 → 成功才写盘。
///
/// 【为什么这里直接写盘而不走前端的 `patchSettings`】
///   `set_settings` 是**整体替换**（前端那条路径要传完整对象）。
///   这里在 Rust 侧做「读 → 改一个字段 → 整体写回」，
///   跟 Rust 侧存窗口位置是同一套做法，不会把别的字段写没。
///
/// 【为什么是【同步】命令】
///   这个命令会调 `tray::set_warning` —— 它要**重建托盘菜单**，
///   而 muda 在 macOS 上**只能在进程主线程**建菜单
///   （muda 0.20 `platform_impl/macos/mod.rs:121` 直接 panic：
///   「`muda::Menu` can only be created on the main thread」——
///   **崩的是整个进程**）。
///   Tauri 的 **async 命令会被 spawn 到 tokio 线程**（`tauri` 的 `ipc/mod.rs`），
///   → 那里调 `set_warning` = 整个应用崩。
///   同步命令在 IPC 线程（= 主线程）上跑 → 安全。
///
/// 【硬规则：设置 = **实际生效**的键】
///   不问「用户请求了什么」，只问「现在生效的是哪个」。
///   - 成功        → 生效的 = 用户请求的那个
///   - 注册失败    → **什么都没变** → 生效的仍是旧键
///   - 注销失败    → 生效的是**错误里带回来的那个**
///     （换键 = 新键；「关掉快捷键」= 没注销掉的旧键 —— 它还在生效！）
///   这条规则不需要任何额外判断，也不会出现「设置说 A、实际 B 在唤起」。
///
/// 【返回的永远是 `Ok`】
///   「注册失败」与「注销失败」**后果不同**，不能都塞进 `Err`：
///   前者**什么都没变**，后者**状态变了**。
///   → 成败与警告都放在 [`ShortcutChange`] 的字段里。
#[tauri::command]
pub fn set_global_shortcut<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    spec: Option<String>,
) -> Result<ShortcutChange, String> {
    let s = crate::settings::load(&app);
    let old = s.global_shortcut.clone();
    // ① 先改**实际状态**（注册 / 注销）
    let (effective, mut warning, mut notice) =
        match crate::shortcut::apply(&app, old.as_deref(), spec.as_deref()) {
            Ok(()) => (spec.clone(), None, TrayNotice::None),
            Err(e) => {
                let msg = e.message();
                // 【失败时「生效的是哪个键」由错误带出来】——
                //   注销失败时它可能仍是**旧键**（「关掉快捷键」那条路径），
                //   猜错 = 面板显示「未设置」而旧键还在唤起。
                let effective = match &e {
                    crate::shortcut::ShortcutError::UnregisterFailed { effective, .. } => {
                        effective.clone()
                    }
                    // 注册失败 / 规格非法 → **什么都没变** → 生效的仍然是旧键
                    _ => old.clone(),
                };
                // 【托盘要说的是哪一句】—— 按**实际后果**分，不看文案里有没有某个词
                let notice = match &e {
                    crate::shortcut::ShortcutError::UnregisterFailed { .. } => {
                        TrayNotice::OldStillActive
                    }
                    _ => TrayNotice::NotEffective,
                };
                (effective, Some(msg), notice)
            }
        };
    // ② 【硬规则】设置 = **实际生效**的键
    //
    //   【为什么用 `settings::update` 而不是自己 load→改→save】（终审 M-10）
    //   「读-改-写」必须**原子** —— 否则前端同时在写设置时，
    //   两边会互相覆盖（后写的那份把它没读到的字段写回旧值）。
    //   `update` 把整个序列放进**同一把进程内写锁**。
    if s.global_shortcut != effective {
        let want = effective.clone();
        let save_result = crate::settings::update(&app, |cur| {
            if cur.global_shortcut != want {
                cur.global_shortcut = want.clone();
            }
            Ok(())
        });
        if let Err(e) = save_result {
            // 【快捷键已经生效了】—— 别让面板显示成「操作失败」：
            //   那会让用户以为没换成功，而实际已经换成功了。
            //   → 报成「部分成功」：生效是真的，只是没存住。
            warning = Some(match warning {
                Some(w) => format!("{w}；另外，设置没有保存：{e}"),
                None => format!("快捷键已生效，但设置没有保存：{e}"),
            });
            // 【托盘那句也要跟着改】——
            //   键**确实生效了**（只是没存住）→ 说「未生效」就是**骗人**，
            //   而且与面板说的**相反**（面板说「已生效，但没存住」）。
            //   → 换成「设置没存住」。
            //
            //   【但不能把更危险的那条覆盖掉】（终审 I-3）
            //   这一段只在「生效的键变了」时才会进来 ——
            //   而「生效的键变了」有两种可能：
            //     ① 换键成功（旧键注销成功了）→ 「设置没存住」是对的那句
            //     ② 换键成功但**旧键没注销掉** → 「旧键还在唤起」才是要紧的那句
            //   → 用 `worse()` 留**更危险**的那条，别把 ② 吞成 ①。
            notice = notice.worse(TrayNotice::SettingNotSaved);
        }
    }
    // ③ 托盘：一切正常 → 撤掉警告项；有警告 → **摆一句短的**上去。
    //    【为什么是短的】macOS 的菜单项过长会被**截断**显示；
    //    详情留在设置面板里（那里有完整的句子）。
    crate::tray::set_warning(&app, tray_warning_text(notice));
    Ok(ShortcutChange { effective, warning })
}

/// 快捷键的**运行时真相** —— 前端打开设置面板时问它。
///
/// 【为什么要有这个命令】
///   `get_settings` 返回的是**用户的偏好**（设置文件里存的那个键），
///   而面板要显示的是「**此刻按下去会不会唤起 Lodestar**」。
///   两者会分叉 —— 最典型的一条：
///   **启动时** `⌥⌘Space` 被别的软件占着 → 注册失败 →
///   设置里**一个字都没改**（偏好还在，好让用户重试）→
///   面板若直接显示设置里的值，就等于骗人说「它已生效」。
///   （`commands.rs` 里那条硬规则「设置 = 实际生效的键」
///     只管住了「改键」那条路径，**启动路径**不走它。）
///
/// 【为什么不返回「偏好」】
///   偏好前端自己就有（`get_settings` 已经读过一遍了）。
///   这个命令只回答**事实**：生效的是哪个键、有没有问题。
#[derive(Debug, Clone, Serialize)]
pub struct ShortcutRuntimeState {
    /// 本进程**实际注册着**的快捷键（`None` = 没有键生效）。
    pub effective: Option<String>,
    /// 出问题的原因（`None` = 一切正常）。
    pub error: Option<String>,
}

/// 问「此刻生效的是哪个快捷键」—— **真相查询**。
///
/// 【为什么读不到设置也没关系】
///   这个命令**只**读运行时状态（进程内的一份），
///   不碰 store —— 所以它不会因为配置文件坏了就失败，
///   也让它的 IPC 往返测试不必隔离 `$HOME`。
#[tauri::command]
pub fn get_global_shortcut<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
) -> ShortcutRuntimeState {
    ShortcutRuntimeState {
        effective: crate::shortcut::effective_shortcut(&app),
        error: crate::shortcut::runtime_error(),
    }
}

/// 托盘要通报的**实际后果** —— 决定托盘上写哪一句短话。
///
/// 【为什么要按「后果」分，而不是看文案里有没有某个词】
///   托盘与面板必须说**同一件事**。
///   写盘失败时面板说「已生效，但没存住」，托盘要是还说「未生效」，
///   两处就**说的相反**了 —— 用户看哪边都不对。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TrayNotice {
    /// 一切正常 —— 没有警告项。
    None,
    /// 键**没生效**（注册失败 / 规格非法）—— 旧键还在。
    NotEffective,
    /// 旧键**没注销掉**（新键生效了，但两个都会唤起）。
    OldStillActive,
    /// 键**生效了**，但**设置没存住**（下次启动会回到旧键）。
    SettingNotSaved,
}

impl TrayNotice {
    /// 危险程度 —— **数字越大越危险**。
    ///
    /// 【为什么要排这个】
    ///   两条警告可能**同时成立**（注销失败 + 写盘失败）。
    ///   托盘上只能摆一条 → 必须留**更危险**的那条。
    ///
    ///   排序的理由（终审 I-3）：
    ///   - `OldStillActive` —— 旧键**还在唤起 Lodestar**。
    ///     用户以为已经关掉了，其实它还在生效 → 最危险。
    ///   - `NotEffective` —— 用户以为配好了，按下去没反应。
    ///   - `SettingNotSaved` —— 键**生效了**，只是**设置没存住**
    ///     （下次启动会回到旧键 —— 而那个旧键本来就是用户自己选的）→ 最轻。
    fn severity(self) -> u8 {
        match self {
            TrayNotice::None => 0,
            TrayNotice::SettingNotSaved => 1,
            TrayNotice::NotEffective => 2,
            TrayNotice::OldStillActive => 3,
        }
    }

    /// 两条都成立时 —— **留更危险的那条**。
    ///
    /// 【终审 I-3 的原始病灶】
    ///   「注销失败」+「写盘失败」同时成立时，
    ///   代码**无条件**把托盘文案覆盖成「⚠️ 设置没有保存」→
    ///   「旧键可能仍在唤起 Lodestar」这句话被**吞掉**了
    ///   （面板的 `warning` 里两句都有 → 两个入口说的**不等价**，
    ///     用户只看托盘就会以为一切正常）。
    fn worse(self, other: TrayNotice) -> TrayNotice {
        if other.severity() > self.severity() {
            other
        } else {
            self
        }
    }
}

/// 托盘警告项的**短文案** —— 详情在设置面板里。
///
/// 【为什么要短】macOS 的菜单项太长会被**截断**（Task 3 复审的 Minor-2）。
///   托盘上留个「出问题了」的提示就够，具体原因去设置面板看。
///
/// 【为什么每种后果都有一句自己的话】
///   托盘与面板**必须说同一件事**（见 [`TrayNotice`]）——
///   面板说「已生效但没存住」时，托盘不能还说「未生效」。
fn tray_warning_text(notice: TrayNotice) -> Option<&'static str> {
    match notice {
        TrayNotice::None => None,
        TrayNotice::NotEffective => Some("⚠️ 快捷键未生效"),
        TrayNotice::OldStillActive => Some("⚠️ 旧快捷键未注销"),
        TrayNotice::SettingNotSaved => Some("⚠️ 设置没有保存"),
    }
}

/// 开机自启的**真实**状态（问系统，不是问设置文件）。
///
/// 【为什么要问系统而不是问设置】
///   设置里的值是「用户上次的选择」，而开关点下去改的是**系统**。
///   用户手动删掉登录项、或在「系统设置 → 通用 → 登录项」里关掉之后，
///   两者就不一致了 —— 而面板显示的是「用户的操作对象」，必须显示**真值**。
///   （显示设置里的旧值 = 面板说「开着」而实际上没有 = 骗人。）
///
/// 【为什么读失败要【报错】，而不是当成「没开」】
///   「读失败」与「确实是关的」是**两回事**：
///   吞掉错误 → 面板显示「关」，而系统里其实**开着** ——
///   用户看着一个假的开关去改设置，改的还是设置文件里的旧值。
///   → 读失败就**报错**，由面板决定怎么办（退回设置里的值 + 提示）。
#[tauri::command]
pub fn get_launch_at_login<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
) -> Result<bool, String> {
    use tauri_plugin_autostart::ManagerExt;
    app.autolaunch()
        .is_enabled()
        .map_err(|e| format!("读开机自启状态失败：{e}"))
}

/// 改开机自启 —— **先改系统，再写盘**。
///
/// 【为什么要先改系统】跟快捷键同理：
///   写盘成功但系统改失败 → 设置说「开机自启开着」而实际上没有 —— 骗人。
///
/// 【为什么是【同步】命令】
///   `auto-launch` 在 macOS 上只是**写一个 plist 文件**
///   （`auto-launch-0.6.0/src/macos.rs` 的 `enable_launch_agent`：
///   `fs::File::create(...).write(...)` —— **不 spawn 进程**），
///   几毫秒就完事，同步做不会卡住界面。
///   （这条命令**不碰托盘菜单** —— 它的线程约束是自己的，
///     与 `set_global_shortcut` 那条无关。）
#[tauri::command]
pub fn set_launch_at_login<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    enabled: bool,
) -> Result<(), String> {
    use tauri_plugin_autostart::ManagerExt;
    let mgr = app.autolaunch();
    if enabled {
        mgr.enable()
            .map_err(|e| format!("开启开机自启失败：{e}"))?;
    } else {
        mgr.disable()
            .map_err(|e| format!("关闭开机自启失败：{e}"))?;
    }
    // 【写盘走 `settings::update`】（终审 M-10）
    //   「读-改-写」要**原子** —— 否则前端同时在写设置时两边互相覆盖。
    crate::settings::update(&app, |cur| {
        cur.launch_at_login = enabled;
        Ok(())
    })?;
    Ok(())
}

/// 改设置里的**一个字段**（差量）—— **原子**。
///
/// 【为什么前端要有一条这样的命令】（终审 M-10）
///   前端原来自己「读一份 → 改一个字段 → 整体写回」——
///   读与写之间是**两次 IPC**，中间 Rust 侧写一次就被覆盖回去
///   （典型症状：刚改完快捷键，自动刷新开关的改动被抹掉）。
///   → 差量交给 Rust：**在锁内**读盘上的真值、合并、写回。
///   两个写者（前端 / Rust）就真正串行了。
///
/// 【差量是什么】
///   一个 JSON 对象，键是设置里的**顶层字段名**（camelCase），
///   值是要写进去的新值。**没提到的字段一律保留。**
///
/// 【为什么不是「整体替换」】
///   整体替换 = 前端手里那份**可能已经旧了**
///   （设置面板改了索引目录之后，书签面板里那份拷贝就没索引目录了）→
///   拿它写回去就是把索引目录抹掉。
#[tauri::command]
pub fn patch_settings<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    delta: serde_json::Value,
) -> Result<crate::settings::Settings, String> {
    crate::settings::patch(&app, delta)
}

#[cfg(test)]
mod tests {
    //! `commands::rescan` 的 IPC 往返测试 —— 在此之前 rescan **零单测**（B2'）。
    //!
    //! 【为什么要走真实 IPC】
    //!   「命令定义了」不等于「注册了」——
    //!   忘了写进 `invoke_handler` 的话编译期不报错，
    //!   只有前端真调的时候才报 `command rescan not found`。
    //!   这里用 `tauri::test::get_ipc_response` 按命令名调，把它验掉。
    //!
    //! 【为什么这个模块不隔离 HOME】
    //!   这些用例**不碰设置**（没注册 store 插件、也不读盘）→
    //!   改 HOME 没有必要 → 也就不会跟 `settings.rs` 里那些改 HOME 的用例打架
    //!   （cargo test 默认并行跑同进程内的测试）。
    //!   引擎只索引**临时目录**，不依赖 `$HOME`。

    use super::*;
    use std::path::PathBuf;
    use tauri::ipc::{CallbackFn, InvokeBody};
    use tauri::test::{mock_context, noop_assets, INVOKE_KEY};
    use tauri::webview::InvokeRequest;
    use tauri::Manager;

    fn invoke(cmd: &str, body: serde_json::Value) -> InvokeRequest {
        InvokeRequest {
            cmd: cmd.into(),
            callback: CallbackFn(0),
            error: CallbackFn(1),
            url: "tauri://localhost".parse().unwrap(),
            body: InvokeBody::Json(body),
            headers: Default::default(),
            invoke_key: INVOKE_KEY.to_string(),
        }
    }

    fn build_app() -> tauri::App<tauri::test::MockRuntime> {
        tauri::test::mock_builder()
            // 【引擎句柄 + 索引状态必须 manage】
            //   `rebuild_engine` 要从 app 里取这两样，mock app 没 manage 的话
            //   `app.state::<…>()` 直接 panic（state not managed for field …）。
            .manage(std::sync::Mutex::new(Option::<Arc<Searcher>>::None))
            .manage(crate::index_state::IndexState::new())
            // 【命令表用生产的那一份】—— 不是抄一份。
            //   抄的话，「生产忘了注册」这件事测试是发现不了的
            //   （验的是抄的那份，不是生产在用的那份）。
            .invoke_handler(crate::app_commands_handler::<tauri::test::MockRuntime>())
            .build(mock_context(noop_assets()))
            .expect("构建 mock app")
    }

    /// 【窗口可见性必须是【真值】，不能是 `snapshot()` 的占位值】
    ///   前端靠这个字段决定「要不要继续跑那个 800ms 的状态轮询」——
    ///   窗口看不见的时候用户看不见状态栏，那个轮询是纯浪费
    ///   （实测每次约 3.4ms CPU → 后台常驻时它就是 CPU 的大头）。
    ///
    ///   【这条测试能验到什么、验不到什么】
    ///   - 能验：返回值与「直接问 `window_visible`」的答案一致（契约）。
    ///   - 验不到：占位值和真值在 mock 下**都是 true**（mock 里没有窗口）——
    ///     所以「占位漏出去了」这件事**在单测里测不出来**。
    ///     → 它的真正保障是**真机实测**（窗口可见与收起来的 CPU 差好几倍：
    ///       字段要是恒为 true，两种状态下的 CPU 会一样）。
    #[test]
    fn index_status_reports_the_window_visibility() {
        let app = build_app();
        let st = crate::commands::index_status(app.handle().clone(), app.state());
        assert_eq!(
            st.visible,
            crate::lifecycle::window_visible(&app.handle()),
            "index_status 的 visible 必须与窗口的真实可见性一致"
        );
    }

    /// 造一个临时目录，里面放 n 个文件。
    fn make_dir_with_files(tag: &str, n: usize) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "fsearch-rescan-test-{tag}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for i in 0..n {
            std::fs::write(dir.join(format!("marker{i}.txt")), b"hello").unwrap();
        }
        dir
    }

    /// 直接问引擎：「你索引了多少条」—— 用来跟状态栏报的数对账。
    fn engine_total_entries(app: &tauri::App<tauri::test::MockRuntime>) -> u32 {
        let st = app.state::<Mutex<Option<Arc<Searcher>>>>();
        let g = st.lock().unwrap();
        let s = g.as_ref().expect("引擎应在 state 里");
        let req = search_core::types::SearchRequest {
            // 【`*` = 匹配全部】空查询会被 shim 短路成 0 条
            query: "*".to_string(),
            sort: search_core::types::SortField::Name,
            order: search_core::types::SortOrder::Asc,
            kind: search_core::types::ListKind::All,
            page: 0,
            page_size: 50,
        };
        s.search(req).unwrap().total_entries
    }

    /// rescan 的 IPC 往返：
    /// 返回的 `IndexStatus.indexed` 必须与**引擎真实条目数**一致，
    /// 且 `index_status` 命令读到的是同一份。
    #[test]
    fn rescan_command_is_registered_and_round_trip() {
        let dir = make_dir_with_files("roundtrip", 3);
        let dir_str = dir.to_string_lossy().into_owned();

        let app = build_app();
        let webview = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
            .build()
            .expect("创建 webview");

        // 1) 先装一个引擎（走真实 IPC 的 reindex）
        let body = tauri::test::get_ipc_response(
            &webview,
            invoke("reindex", serde_json::json!({ "paths": [dir_str.clone()] })),
        )
        .expect("reindex 应返回 Ok（命令没注册的话这里会 Err）");
        let val: serde_json::Value = body.deserialize().unwrap();
        assert!(
            val["indexed"].as_u64().unwrap_or(0) >= 3,
            "3 个文件应至少索引到 3 项，实际: {val}"
        );
        assert_eq!(val["paths"][0], dir_str, "IndexStatus.paths 应反映新目录");

        // 2) 引擎自己说它有多少条
        let engine_total = engine_total_entries(&app);

        // 3) rescan（走真实 IPC）
        let body =
            tauri::test::get_ipc_response(&webview, invoke("rescan", serde_json::json!({})))
                .expect("rescan 应返回 Ok（命令没注册的话这里会 Err）");
        let val: serde_json::Value = body.deserialize().unwrap();
        assert_eq!(val["scanning"], false, "rescan 完不该还在 scanning");
        assert_eq!(
            val["indexed"].as_u64().unwrap(),
            engine_total as u64,
            "rescan 返回的条目数必须与引擎真实条目数一致，实际: {val}"
        );
        assert_eq!(val["paths"][0], dir_str, "rescan 后路径不该变");
        assert_eq!(val["error"], serde_json::Value::Null);

        // 4) index_status 读到的是同一份
        let body = tauri::test::get_ipc_response(
            &webview,
            invoke("index_status", serde_json::json!({})),
        )
        .expect("index_status 应返回 Ok");
        let val2: serde_json::Value = body.deserialize().unwrap();
        assert_eq!(val2["indexed"].as_u64().unwrap(), engine_total as u64);
        assert_eq!(val2["paths"][0], dir_str);
        assert_eq!(val2["scanning"], false);
    }

    /// 【M1】reindex 必须**递增引擎代次**；
    /// 用旧代次去提交（= 模拟「跨越了换引擎的旧 rescan」）必须被拒，
    /// 状态仍然报**新引擎**那份。
    ///
    /// 【为什么不靠 sleep 撞时序】
    ///   竞态本身靠**代次守卫的逻辑**保证（不是靠时序测试碰运气）。
    ///   这里把那条逻辑**确定性地**验掉：
    ///   手动拿着旧代次去提交 → 必须被拒、状态不许变。
    #[test]
    fn reindex_bumps_the_generation_and_stale_commit_is_rejected() {
        let dir_a = make_dir_with_files("gen-a", 2);
        let dir_b = make_dir_with_files("gen-b", 1);
        let a = dir_a.to_string_lossy().into_owned();
        let b = dir_b.to_string_lossy().into_owned();

        let app = build_app();
        let webview = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
            .build()
            .expect("创建 webview");
        let index_state = app.state::<crate::index_state::IndexState>();

        // ① 第一代引擎（目录 A）
        let body = tauri::test::get_ipc_response(
            &webview,
            invoke("reindex", serde_json::json!({ "paths": [a.clone()] })),
        )
        .expect("reindex 应返回 Ok");
        let val: serde_json::Value = body.deserialize().unwrap();
        let indexed_a = val["indexed"].as_u64().unwrap() as u32;
        assert_eq!(index_state.engine_snapshot().0, 1, "装上第一个引擎后代次应是 1");
        let gen1 = index_state.engine_snapshot().0;

        // ② 换引擎（目录 B）→ 代次 +1
        let body = tauri::test::get_ipc_response(
            &webview,
            invoke("reindex", serde_json::json!({ "paths": [b.clone()] })),
        )
        .expect("reindex 应返回 Ok");
        let val: serde_json::Value = body.deserialize().unwrap();
        let indexed_b = val["indexed"].as_u64().unwrap() as u32;
        assert_eq!(
            index_state.engine_snapshot().0,
            2,
            "换引擎后代次必须是 2（换引擎才 +1）"
        );

        // ③ 跨越了换引擎的旧 rescan 来提交 → 必须被拒
        let accepted = index_state.commit_if_current(gen1, vec![a.clone()], indexed_a);
        assert!(
            !accepted,
            "旧代次的提交必须被拒（否则旧结果会覆盖新状态）"
        );

        // ④ 状态仍然是【新引擎】那份
        let snap = index_state.snapshot();
        assert_eq!(snap.paths, vec![b.clone()], "路径不该被旧结果覆盖");
        assert_eq!(snap.indexed, indexed_b, "条目数不该被旧结果覆盖");
        assert!(!snap.scanning, "scanning 不该卡在 true");
    }

    // ── do_rescan 的「次序不变式」──

    /// 【次序不变式】`do_rescan` 必须是「**先快照代次、后取引擎**」。
    ///
    /// 【这条顺序为什么有讲究】
    ///   `do_rescan` 内部是四步：
    ///     ① 快照（代次, 路径） ② 取引擎 ③ 重扫 ④ 用①的代次提交
    ///   若 ②③ 之间换了引擎（用户改了索引目录 → reindex）：
    ///   - **正确顺序**：② 拿到的是**新**引擎；④ 的代次校验**不通过**
    ///     → 丢弃这次结果 → 状态保持 reindex 提交的那份（正确的）✓
    ///   - **错误顺序**（先取引擎、后快照）：② 拿到的是**旧**引擎，
    ///     而 ① 读到的是**新**代次 → 校验**通过**
    ///     → 把**旧引擎**的结果写进代表新引擎的状态
    ///     → **反向错位**（状态栏报新目录、条目数却是旧引擎的）✗
    ///
    /// 【怎么确定性地测】不靠 sleep 撞时序（重扫只要 1ms，撞不准）：
    ///   取引擎那一步是个**闭包** —— 闭包是在**快照之后**才被调用的
    ///   （这正是要钉死的次序）。测试在闭包里制造「换引擎」这个交错，
    ///   然后返回**旧引擎** —— 于是：
    ///     正确顺序 → 代次校验拒掉 → 状态仍是新引擎那份 ✓
    ///     错误顺序 → 闭包在快照之前被调用 → 交错没被制造出来 → 状态被旧结果覆盖 ✗
    ///
    /// 【这条同时钉住「代次守卫」】——
    ///   把 `commit_if_current` 换成无条件提交 → 状态会被旧结果覆盖 → 这条也会红。
    #[test]
    fn do_rescan_takes_the_engine_after_the_generation_snapshot() {
        // 目录 A 有 5 个文件、目录 B 有 1 个 —— 条目数必须**明显不同**，
        // 否则「A 的结果写进了状态」看不出来。
        let dir_a = make_dir_with_files("order-a", 5);
        let dir_b = make_dir_with_files("order-b", 1);
        let a = dir_a.to_string_lossy().into_owned();
        let b = dir_b.to_string_lossy().into_owned();

        let state = crate::index_state::IndexState::new();

        // 装上【引擎 A】并提交状态（代次 1）
        let engine_a = {
            let refs: Vec<&std::path::Path> = vec![dir_a.as_path()];
            let (e, n) = search_core::Engine::new(&refs).expect("建引擎 A");
            state.commit_new_engine(vec![a.clone()], n);
            std::sync::Arc::new(search_core::Searcher::new(e))
        };
        assert_eq!(state.engine_snapshot().0, 1, "装上第一个引擎后代次应是 1");

        // 【在闭包里制造交错】——
        //   闭包被调用的那一刻，正是「快照之后、��擎之前」——
        //   所以在这里换引擎，模拟「重扫期间换了索引目录」。
        let indexed_old = do_rescan(&state, || {
            // 换引擎：装上 B 并提交状态（代次 +1 → 2）
            let refs: Vec<&std::path::Path> = vec![dir_b.as_path()];
            let (_e, n) = search_core::Engine::new(&refs).expect("建引擎 B");
            state.commit_new_engine(vec![b.clone()], n);
            // 但这次重扫用的仍然是【旧引擎 A】
            Ok(engine_a)
        })
        .expect("重扫不该失败");

        // 旧引擎 A 的条目数（目录 + 5 个文件）
        assert!(
            indexed_old >= 5,
            "旧引擎 A 至少索引到 5 项，实际 {indexed_old}"
        );

        // 状态必须是【新引擎 B】那份 —— 不能被旧引擎的结果覆盖
        let snap = state.snapshot();
        assert_eq!(
            snap.paths,
            vec![b.clone()],
            "路径不该被旧引擎的结果覆盖（反向错位）"
        );
        assert!(
            snap.indexed < indexed_old,
            "条目数不该是旧引擎的 {}（新引擎只索引了 1 个文件）",
            indexed_old
        );
        assert!(!snap.scanning, "scanning 不该卡在 true");
    }

    // ── 快捷键的「真相查询」──

    /// 【I-1】面板必须能问到「**此刻生效**的是哪个键」。
    ///
    /// 【为什么要有这条】
    ///   终审 I-1：启动时注册失败 → 设置里**一个字都没改** →
    ///   面板显示设置里的值 → 面板说「⌥⌘Space 已生效」、
    ///   用户按了没反应 = **骗人**。
    ///   这个命令就是面板问「真相」的那条路。
    ///
    /// 【为什么要走真实 IPC】
    ///   命令「定义了」不等于「注册了」——
    ///   忘了写进 `invoke_handler` 的话编译期不报错，
    ///   只有前端真调的时候才报 `command get_global_shortcut not found`。
    ///   （命令表是**生产的那一份**，见 `build_app` 的注释。）
    #[test]
    fn get_global_shortcut_is_registered_and_reports_the_truth() {
        // 【先把进程级的运行时状态清空】——
        //   它是 `static`（进程里就一份），cargo test 在同一进程里并行跑用例 →
        //   不清空的话会被别的用例污染（谁先跑完谁说了算 → 偶发红）。
        crate::shortcut::reset_runtime();

        let app = build_app();
        let webview = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
            .build()
            .expect("创建 webview");

        let body = tauri::test::get_ipc_response(
            &webview,
            invoke("get_global_shortcut", serde_json::json!({})),
        )
        .expect("get_global_shortcut 应返回 Ok（命令没注册的话这里会 Err）");
        let val: serde_json::Value = body.deserialize().unwrap();
        // 此刻**一个键都没注册**（单测里没有真的注册过）→
        //   真相必须是「没有键生效」，不能是设置里存着的那个偏好。
        assert_eq!(
            val["effective"], serde_json::Value::Null,
            "没注册过就不能说有键生效（面板会拿它当显示值）"
        );
        // 也没出过错 → 没有警告。
        assert_eq!(
            val["error"], serde_json::Value::Null,
            "没失败过就不该有警告"
        );
    }

    // ── 书签导入 / 导出用的两个命令 ──

    /// 造一个临时目录（书签文件的落脚点）。
    fn temp_bookmarks_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "fsearch-bookmarks-file-test-{tag}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// 书签导入 / 导出的两个命令：**注册了** + **能往返**。
    ///
    /// 【为什么要这条】命令「定义了」不等于「注册了」——
    ///   忘了写进 `invoke_handler` 的话**编译期不报错**，
    ///   只有前端真调的时候才报 `command read_bookmarks_file not found`。
    ///   （这里用的命令表是**生产的那一份**，见 `build_app` 的注释。）
    #[test]
    fn bookmarks_file_commands_are_registered_and_round_trip() {
        let dir = temp_bookmarks_dir("roundtrip");
        let file = dir.join("fsearch-bookmarks.json");
        let path = file.to_string_lossy().into_owned();
        let contents = r#"{"format":"fsearch-bookmarks","version":1,"bookmarks":[{"kind":"query","value":"java"}]}"#;

        let app = build_app();
        let webview = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
            .build()
            .expect("创建 webview");

        // 1) 写（走真实 IPC —— 命令没注册的话这里会 Err）
        tauri::test::assert_ipc_response(
            &webview,
            invoke(
                "write_bookmarks_file",
                serde_json::json!({ "path": path.clone(), "contents": contents }),
            ),
            Ok(()),
        );
        assert!(file.exists(), "文件应该真的写出来了: {}", file.display());

        // 2) 读回来 —— 内容必须一字不差
        let body = tauri::test::get_ipc_response(
            &webview,
            invoke("read_bookmarks_file", serde_json::json!({ "path": path })),
        )
        .expect("read_bookmarks_file 应返回 Ok（命令没注册的话这里会 Err）");
        let val: serde_json::Value = body.deserialize().unwrap();
        assert_eq!(
            val.as_str().unwrap(),
            contents,
            "读回来的内容必须与写进去的一致"
        );
    }

    /// 读**超大**文件必须报错 —— 不能把内存吃满。
    #[test]
    fn read_bookmarks_file_rejects_a_file_over_the_size_limit() {
        let dir = temp_bookmarks_dir("oversize");
        let file = dir.join("big.json");
        // 1MB + 1 字节 —— 刚好越过上限
        let big = vec![b'x'; (1024 * 1024 + 1) as usize];
        std::fs::write(&file, &big).unwrap();

        let err = crate::commands::read_bookmarks_file(file.to_string_lossy().into_owned())
            .expect_err("超过 1MB 的文件必须报错");
        assert!(
            err.contains("太大"),
            "错误里要说清是文件太大，实际: {err}"
        );
    }

    /// 读一个【不存在】的文件必须报错（而不是返回空串）。
    #[test]
    fn read_bookmarks_file_reports_a_missing_file() {
        let missing = std::env::temp_dir().join("fsearch-bookmarks-does-not-exist-xyz.json");
        let err = crate::commands::read_bookmarks_file(missing.to_string_lossy().into_owned())
            .expect_err("读不到的文件必须报错");
        assert!(err.contains("读不到"), "实际: {err}");
    }

    // ── 快捷键变更结果的【线上格式】 ──

    /// 快捷键变更结果序列化后的形状 —— 前端就按这两个字段名读。
    ///
    /// 【为什么要测这个】前端拿到的是 JSON：
    ///   字段名对不上 → `effective` 读成 `undefined` → 面板显示「未设置」，
    ///   而实际生效的快捷键还在跑（面板与实际状态对不上）。
    ///   Rust 侧改了字段名、或忘了序列化某个字段，这里就会红。
    ///
    /// 【为什么能在单测里做】`ShortcutChange` 是个**纯数据**结构，
    ///   序列化它不需要 `AppHandle`、也不碰系统。
    #[test]
    fn the_shortcut_change_serializes_to_the_shape_the_frontend_reads() {
        let v = serde_json::to_value(ShortcutChange {
            effective: Some("Option+Cmd+Space".to_string()),
            warning: Some("旧快捷键注销失败（…）：…".to_string()),
        })
        .expect("序列化不该失败");
        assert_eq!(v["effective"], "Option+Cmd+Space", "字段名/effective");
        assert!(v["warning"].is_string(), "warning 要是字符串");

        // 「没有」的值必须是 **null**（而不是「缺这个键」）——
        //   前端是直接 `if (res.warning)` 判空的，缺键会变成 undefined，
        //   行为一样但形状不一致（前后端各写一份约定时最容易出这种岔）。
        let none = serde_json::to_value(ShortcutChange {
            effective: None,
            warning: None,
        })
        .expect("序列化不该失败");
        assert!(none["effective"].is_null(), "没有值时要是 null");
        assert!(none["warning"].is_null(), "没有值时要是 null");
    }

    // ── 托盘警告项的短文案 ──

    /// 托盘上摆的必须是**短**句子 —— macOS 菜单项过长会被截断。
    ///   详情留在设置面板里。
    #[test]
    fn the_tray_warning_is_short() {
        use crate::commands::{tray_warning_text, TrayNotice};
        for notice in [
            TrayNotice::NotEffective,
            TrayNotice::OldStillActive,
            TrayNotice::SettingNotSaved,
        ] {
            let t = tray_warning_text(notice).expect("有警告时应该有文案");
            assert!(
                t.chars().count() <= 12,
                "托盘文案太长会被截断（{} 字）：{t}",
                t.chars().count()
            );
        }
    }

    /// 托盘与面板**必须说同一件事**。
    ///
    /// 【为什么要钉】写盘失败时键**确实生效了**（只是没存住）——
    ///   面板说「已生效，但没存住」，托盘要是还说「未生效」，
    ///   两处就**说的相反**了：用户看哪边都不对。
    #[test]
    fn the_tray_warning_never_says_not_effective_when_the_key_took_effect() {
        use crate::commands::{tray_warning_text, TrayNotice};
        let t = tray_warning_text(TrayNotice::SettingNotSaved).expect("该有文案");
        assert!(
            !t.contains("未生效"),
            "键**确实生效了**，托盘不能说「未生效」：{t}"
        );
        assert!(
            t.contains("设置"),
            "要说清是「设置没存住」：{t}"
        );
    }

    /// 【终审 I-3】两条警告同时成立时，托盘上要留**更危险**的那条。
    ///
    /// 【为什么这条重要】
    ///   「注销失败」（旧键还在唤起）+「写盘失败」（设置没存住）同时成立时，
    ///   以前代码**无条件**把托盘文案覆盖成「⚠️ 设置没有保存」→
    ///   「旧键可能仍在唤起 Lodestar」被**吞掉**了
    ///   （面板的 `warning` 里两句都有 → 两个入口说的不等价，
    ///     只看托盘的用户会以为一切正常）。
    #[test]
    fn the_tray_keeps_the_more_dangerous_warning() {
        use crate::commands::{tray_warning_text, TrayNotice};
        // 「旧键没注销掉」比「设置没存住」危险 → 留前者。
        let notice = TrayNotice::OldStillActive.worse(TrayNotice::SettingNotSaved);
        assert_eq!(
            notice,
            TrayNotice::OldStillActive,
            "旧键还在唤起比设置没存住更危险 —— 不能被覆盖掉"
        );
        let t = tray_warning_text(notice).expect("该有文案");
        assert!(
            t.contains("旧快捷键"),
            "托盘上必须还看得到「旧键没注销掉」这句话：{t}"
        );
        // 「设置没存住」比「键没生效」轻 → 留「键没生效」。
        assert_eq!(
            TrayNotice::NotEffective.worse(TrayNotice::SettingNotSaved),
            TrayNotice::NotEffective
        );
        // 换键成功（本来没警告）+ 写盘失败 → 变成「设置没存住」。
        assert_eq!(
            TrayNotice::None.worse(TrayNotice::SettingNotSaved),
            TrayNotice::SettingNotSaved
        );
    }

    /// 每种**实际后果**都有自己那一句短话。
    #[test]
    fn each_outcome_has_its_own_tray_sentence() {
        use crate::commands::{tray_warning_text, TrayNotice};
        assert_eq!(tray_warning_text(TrayNotice::None), None, "正常 → 没有警告项");
        assert_eq!(
            tray_warning_text(TrayNotice::NotEffective),
            Some("⚠️ 快捷键未生效"),
            "注册失败 → 键没生效"
        );
        assert_eq!(
            tray_warning_text(TrayNotice::OldStillActive),
            Some("⚠️ 旧快捷键未注销"),
            "注销失败 → 旧键可能还在"
        );
        assert_eq!(
            tray_warning_text(TrayNotice::SettingNotSaved),
            Some("⚠️ 设置没有保存"),
            "键生效了但没存住"
        );
    }
}
