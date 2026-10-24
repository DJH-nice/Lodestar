//! 搜索线程 —— 所有请求串行执行。
//!
//! 【为什么必须串行】
//!   引擎的「视图」（上一次搜索命中的条目列表）是**有状态**的：
//!   `page()` 读的是 `search()` 留下的那份。并发请求会读到别人的视图。
//!
//! 【为什么搜索与重扫也共用这一个线程】
//!   它们改的是同一份索引 —— 重扫期间搜索必然读到半成品。
//!   → 排队是**正确性**要求，不是性能取舍。
//!
//! 【代价】一次全量重扫（$HOME 量级 300~450ms）会把排在它后面的搜索全堵住。
//!   状态栏那个 `elapsed_ms` 是**引擎内部**耗时、**不含排队** ——
//!   症状是「数字显示很快，但感觉很慢」，很容易误判成搜索算法慢。
//!   → 这正是 M3 要治的病：**增量更新**只要几毫秒，堵不住谁。

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, RecvTimeoutError, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::engine::{path_in_root, Engine};
use crate::incremental::{self, ChangeSet, UpdateReport};
use crate::store;
use crate::types::{EngineError, IndexEntry, SearchRequest, SearchResult};

/// 两次**落盘**之间至少隔这么久。
///
/// 【为什么是 2s】
///   落盘 = 写 3MB + fsync + rename，实测 **9.6ms**（写盘那一段）。
///   不设这道闸，「持续小改动」会把后台线程**一直刷爆**。
///   2 秒一次 = 每秒最多 0.5 次写盘 —— 后台占用可以忽略。
///
/// 【为什么这不是「丢掉变化」】
///   引擎内存里的索引**立刻**就更新了（那才是用户看得见的东西）；
///   索引档只是**冷启动的缓存**，落后 2 秒不影响使用。
///   （顺带一提：改动前的实现里，watcher 本身就最少 5 秒才重扫一次 ——
///   索引档的滞后从 5 秒缩到 2 秒，其实是**变好了**。）
const PERSIST_INTERVAL: Duration = Duration::from_secs(2);

enum Job {
    Search {
        request: SearchRequest,
        reply: Sender<Result<SearchResult, EngineError>>,
    },
    Page {
        request: SearchRequest,
        reply: Sender<Result<SearchResult, EngineError>>,
    },
    Rescan {
        reply: Sender<Result<u32, EngineError>>,
    },
    /// 增量更新：把一批变化就地反映进索引。
    ApplyChanges {
        changes: ChangeSet,
        reply: Sender<Result<UpdateReport, EngineError>>,
    },
    /// 附加一个根目录（条目由调用方**在 worker 之外**扫好）。
    AttachRoot {
        root: PathBuf,
        entries: Vec<IndexEntry>,
        reply: Sender<Result<u32, EngineError>>,
    },
    /// 分离一个根目录 —— 条目立即消失。
    ///
    /// 【⚠️ 路径必须与 attach 时**逐字相同**】
    ///   引擎比的是**索引空间写法**（不是规范化后的真实路径）——
    ///   拼写不同 → **静默无效** → 条目留着 = **幽灵条目**
    ///   （用户最不能接受的那种：盘都拔了，搜得到它的文件）。
    DetachRoot {
        root: PathBuf,
        reply: Sender<Result<u32, EngineError>>,
    },
    Shutdown,
}

/// 落盘槽 —— **每个根只要最新的那一份**。
///
/// 【为什么是「覆盖」而不是「排队」】
///   变化来得比落盘快时（比如一次 `git checkout` 触几百个事件），
///   排队的写法会写好几次**内容几乎一样**的档 —— 白白烧 I/O。
///   覆盖掉旧的 → 只写最后那一份 ✓（内容是最新的 ✓）。
///
/// 【为什么是 map 而不是一份】
///   索引档改成**每根一个文件** → 挂载的根不止一个（\$HOME + 若干外接卷）
///   → 每个根各要一份最新快照。
struct PersistSlot {
    /// 根的档路径 → 该根最新的字节流。
    jobs: Mutex<HashMap<PathBuf, Vec<u8>>>,
    /// 【为什么用原子量而不是 Mutex】
    ///   落盘线程等在条件变量上时**不能**再持有别的锁 ——
    ///   `Condvar::wait` 只释放它自己那个 Mutex。
    ///   之前 `stopped` 用 Mutex 的话，等待期间会一直攥着它，
    ///   `shutdown()` 就永远拿不到 → **死锁**。
    stopped: AtomicBool,
    cv: Condvar,
}

/// 落盘线程 —— **把写档这件慢活挪出关键路径**。
///
/// 【为什么要单独一个线程】
///   写档实测 9.6ms（新建 inode 的 fsync 很贵）。
///   放在搜索线程里 → 每次落盘都把排在后面的搜索堵住近 10ms；
///   放在增量更新的同步路径里 → 「改一个文件」要多等 10ms。
///   → 挪到独立线程：搜索线程只负责「把字节流交出去」（约 1ms 的序列化）。
struct Persister {
    slot: Arc<PersistSlot>,
    handle: Option<JoinHandle<()>>,
}

impl PersistSlot {
    /// 交出一个根的快照（**覆盖**掉那个根还没写的那份）。
    fn submit(&self, path: PathBuf, bytes: Vec<u8>) {
        let mut g = self.jobs.lock().unwrap();
        g.insert(path, bytes);
        self.cv.notify_one();
    }

    /// 丢掉某个根**还没写**的那份快照（摘根时用）。
    ///
    /// 【为什么摘根要丢掉它】
    ///   落盘槽是「每根覆盖最新字节流」—— 摘根**之前**提交的那份快照
    ///   还在排队 → 摘完根它仍会落盘 → 把那个根的档重写成**摘根前**的内容
    ///   （陈旧档）。那个根已经不在索引里 → 档不会被读 → 不致命，
    ///   但它属于同一族「陈旧档」问题 → 顺手修掉。
    fn discard(&self, path: &Path) {
        let mut g = self.jobs.lock().unwrap();
        g.remove(path);
    }
}

impl Persister {
    fn new() -> Persister {
        let slot = Arc::new(PersistSlot {
            jobs: Mutex::new(HashMap::new()),
            stopped: AtomicBool::new(false),
            cv: Condvar::new(),
        });
        let thread_slot = Arc::clone(&slot);
        let handle = std::thread::Builder::new()
            .name("lodestar-persist".into())
            .spawn(move || {
                loop {
                    let jobs = {
                        let mut g = thread_slot.jobs.lock().unwrap();
                        while g.is_empty() && !thread_slot.stopped.load(Ordering::Relaxed) {
                            g = thread_slot.cv.wait(g).unwrap();
                        }
                        std::mem::take(&mut *g)
                    };
                    if jobs.is_empty() {
                        break;
                    }
                    for (path, bytes) in jobs {
                        if let Err(e) = store::save_bytes(&bytes, &path, true) {
                            eprintln!("[lodestar] 索引落盘失败（不影响使用）: {e}");
                        }
                    }
                }
            })
            .expect("启动落盘线程失败");
        Persister {
            slot,
            handle: Some(handle),
        }
    }

    /// 停掉落盘线程（**先把排着的写掉**再走）。
    fn shutdown(&mut self) {
        self.slot.stopped.store(true, Ordering::Relaxed);
        self.slot.cv.notify_all();
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// 搜索线程句柄。
pub struct Searcher {
    tx: Sender<Job>,
    handle: Option<JoinHandle<()>>,
    persister: Persister,
}

impl Searcher {
    /// 起一个搜索线程，把引擎交给它独占。
    pub fn new(engine: Engine) -> Searcher {
        let persister = Persister::new();
        // 落盘槽要与搜索线程**共享**（所以是 Arc）——
        //   `Persister` 本身（含线程句柄）留在 `Searcher` 里，退出时用来收尾。
        let persist_slot = Arc::clone(&persister.slot);
        let (tx, rx) = channel::<Job>();
        let handle = std::thread::Builder::new()
            .name("lodestar-search".into())
            .spawn(move || {
                // 当前 query —— 翻页请求必须与之一致，
                // 否则它读到的其实是上一次搜索的视图。
                let mut current_query: Option<String> = None;
                // **哪些根的档落后了**（索引空间写法）。
                //
                // 【为什么是「按根」而不是一个 bool】
                //   索引档现在是**每根一个文件** → 挂载的根不止一个
                //   （$HOME + 若干外接卷）→ 落盘要能**只重写变了的那几个**：
                //   插着 10 万文件的 U 盘时，每次都全量重写要动十几 MB，
                //   而 $HOME 那份**根本没变** → 白烧 I/O 与 CPU。
                let mut dirty_roots: HashSet<PathBuf> = HashSet::new();
                // 上次交出去落盘的时刻（「两次落盘至少隔 2s」那道闸）。
                let mut last_persist_at = Instant::now();

                loop {
                    // 【为什么要带超时】不能光 `recv()`：
                    //   「索引档落后了要补写」这件事没有任何请求会来触发它，
                    //   光等请求的话，索引档可能永远不更新。
                    //   → 每隔一会儿醒一次，把欠下的落盘补上。
                    let job = match rx.recv_timeout(PERSIST_INTERVAL) {
                        Ok(job) => Some(job),
                        Err(RecvTimeoutError::Timeout) => None,
                        Err(RecvTimeoutError::Disconnected) => break,
                    };

                    if let Some(job) = job {
                        match job {
                            Job::Search { request, reply } => {
                                let res = engine.search(&request);
                                if res.is_ok() {
                                    current_query = Some(request.query.clone());
                                }
                                let _ = reply.send(res);
                            }
                            Job::Page { request, reply } => {
                                let res = match &current_query {
                                    Some(q) if *q == request.query => engine.page(&request),
                                    // query 变了 → 这个翻页请求已经过期，直接拒
                                    _ => Err(EngineError::InvalidArg),
                                };
                                let _ = reply.send(res);
                            }
                            Job::Rescan { reply } => {
                                let res = engine.rescan();
                                if res.is_ok() {
                                    current_query = None; // 视图已被替换
                                }
                                let _ = reply.send(res);
                            }
                            Job::ApplyChanges { changes, reply } => {
                                let res = engine.apply_changes(&changes);
                                if let Ok(report) = &res {
                                    current_query = None; // 视图已被替换
                                    // 【退化成全量重扫时，引擎自己已经把所有根写过档了】
                                    //   （`Engine::apply_changes` → `rescan()` → `persist()`）
                                    //   → 不要再标脏（否则白写一遍）。
                                    if !report.fell_back {
                                        mark_changed_roots(&mut dirty_roots, &engine, &changes.paths);
                                    }
                                }
                                let _ = reply.send(res);
                            }
                            Job::AttachRoot { root, entries, reply } => {
                                let count = engine.attach_root(&root, entries);
                                // 【为什么先问一句「挂上了吗」】引擎会**静默拒绝**
                                //   与已有根重叠的根（不变式：根之间不嵌套）——
                                //   那种情况下这个根**不在**引擎里 → 没有它的档可写，
                                //   而且引擎**根本没动**它的视图 → 翻页状态也不该作废。
                                let attached = engine.roots().iter().any(|r| r == &root);
                                if attached {
                                    current_query = None; // 视图已被替换
                                    // 新挂上的根 → **它那份档**要写（别的根不用动）。
                                    dirty_roots.insert(root.clone());
                                }
                                let _ = reply.send(if attached {
                                    Ok(count)
                                } else {
                                    // 引擎拒了（与已有根重叠）→ 上层要能知道「没挂上」
                                    Err(EngineError::InvalidArg)
                                });
                            }
                            Job::DetachRoot { root, reply } => {
                                // 【为什么要先问一句「它在吗」】
                                //   引擎比的是**索引空间写法** —— 拼写不同（或者压根没挂过）
                                //   → 摘根**静默无效** → 条目留着 = **幽灵条目**
                                //   （用户最不能接受的那种：盘都拔了还搜得到它的文件）。
                                //   → 上层必须能分辨「没摘掉」→ 回 Err（与 attach 的拒绝语义对称）。
                                //   → **查不到就不去摘**：让引擎去摘一个没挂过的路径，
                                //   它会按路径前缀把**别的根**的条目一并删掉（误伤）。
                                let known = engine.roots().iter().any(|r| r == &root);
                                if known {
                                    let count = engine.detach_root(&root);
                                    current_query = None; // 视图已被替换
                                    // 摘掉的根**不该**再写档 → 从待写名单里去掉；
                                    // 也把落盘槽里**已经排着**的那份丢掉（否则会落一份陈旧档）。
                                    dirty_roots.remove(&root);
                                    if let Some(dir) = engine.store_dir() {
                                        persist_slot.discard(&store::root_archive_path(&dir, &root));
                                    }
                                    let _ = reply.send(Ok(count));
                                } else {
                                    // 没挂过（或拼写与 attach 时不同）→ 什么都没摘掉
                                    let _ = reply.send(Err(EngineError::InvalidArg));
                                }
                            }
                            Job::Shutdown => break,
                        }
                    }

                    // ── 落盘：欠了就补 ──
                    //
                    //   【为什么放在回复之后】回复先发出去 → 调用方立刻拿到结果，
                    //   落盘的序列化（约 1ms）不会算进调用方的等待时间。
                    //   【为什么还要「至少隔 2s」】写一次 3MB 的档要 9.6ms ——
                    //   不设闸的话，持续小改动会把后台线程刷爆。
                    //   【为什么只写变了的根】插着 U 盘时，全量重写要动十几 MB，
                    //   而 $HOME 那份**根本没变** → 只写**真的变了**的那几个根。
                    //   （「每根一个档」的**读侧**隔离在 Task 1 就做到了，
                    //     **写侧**隔离在这里补齐 —— 两边合起来才是「插拔卷不牵连 $HOME」。）
                    if !dirty_roots.is_empty() && last_persist_at.elapsed() >= PERSIST_INTERVAL {
                        // 引擎里已经不存在的根（被摘掉了）→ 不用再写
                        let live: HashSet<PathBuf> = engine.roots().into_iter().collect();
                        let wanted: Vec<PathBuf> = dirty_roots
                            .iter()
                            .filter(|r| live.contains(*r))
                            .cloned()
                            .collect();
                        // 【只序列化「要」的那几个根】—— 没被挑中的根，
                        //   它磁盘上的档**本来就是对的** ✓（不提交 ≠ 删掉）。
                        for (path, bytes) in engine.serialize_roots_for_store(&wanted) {
                            persist_slot.submit(path, bytes);
                        }
                        // 【写过的就不再欠账】—— ⚠️ 不摘掉的话，它会**每 2 秒**被重写一次
                        //   （无限循环：序列化还发生在**搜索线程**上 → 搜索被堵）。
                        //   死根（被摘掉的）同样不欠账 → 脏集合清空。
                        dirty_roots.clear();
                        // 【为什么要看 wanted】这一拍**什么都没写**（脏的根都死了）→
                        //   闸门不必重起算：脏集合已清空 → 不会有下一轮。
                        if !wanted.is_empty() {
                            last_persist_at = Instant::now();
                        }
                    }
                }
                // 线程退出时 engine 被 drop → 索引释放。
            })
            .expect("启动搜索线程失败");

        Searcher {
            tx,
            handle: Some(handle),
            persister,
        }
    }

    /// 提交一个 search/page 请求并等待结果。
    ///
    /// 每个请求自带一对 channel —— 回复通道与请求一一对应，不能共用（否则会串台）。
    fn submit_search(
        &self,
        request: SearchRequest,
        is_page: bool,
    ) -> Result<SearchResult, EngineError> {
        let (reply_tx, reply_rx) = channel();
        let job = if is_page {
            Job::Page { request, reply: reply_tx }
        } else {
            Job::Search { request, reply: reply_tx }
        };
        self.tx
            .send(job)
            .map_err(|_| EngineError::Internal("搜索线程已退出".into()))?;
        reply_rx
            .recv()
            .map_err(|_| EngineError::Internal("搜索线程异常退出".into()))?
    }

    pub fn search(&self, request: SearchRequest) -> Result<SearchResult, EngineError> {
        self.submit_search(request, false)
    }

    pub fn page(&self, request: SearchRequest) -> Result<SearchResult, EngineError> {
        self.submit_search(request, true)
    }

    /// 重新扫描（刷新）—— 返回重扫之后的条目总数。
    pub fn rescan(&self) -> Result<u32, EngineError> {
        let (reply_tx, reply_rx) = channel();
        self.tx
            .send(Job::Rescan { reply: reply_tx })
            .map_err(|_| EngineError::Internal("搜索线程已退出".into()))?;
        reply_rx
            .recv()
            .map_err(|_| EngineError::Internal("搜索线程异常退出".into()))?
    }

    /// 增量更新：把一批变化就地反映进索引。
    ///
    /// 【为什么走同一个线程】它改的是同一份索引 ——
    ///   与搜索并发 = 搜索读到半成品。排队是**正确性**要求。
    pub fn apply_changes(&self, changes: ChangeSet) -> Result<UpdateReport, EngineError> {
        let (reply_tx, reply_rx) = channel();
        self.tx
            .send(Job::ApplyChanges {
                changes,
                reply: reply_tx,
            })
            .map_err(|_| EngineError::Internal("搜索线程已退出".into()))?;
        reply_rx
            .recv()
            .map_err(|_| EngineError::Internal("搜索线程异常退出".into()))?
    }

    /// 附加一个根目录。**条目由调用方扫好** —— 扫描绝不在这个线程上做
    /// （30 万文件的盘要扫几十秒 → 搜索会被堵死）。
    ///
    /// 【返回值】附加之后的**索引条目总数**。
    ///   引擎若因「与已有根重叠」**拒绝**附加（不变式：根之间不嵌套），
    ///   这里返回 `Err(InvalidArg)` —— 上层要能知道「**没挂上**」。
    ///
    /// 【⚠️ 路径拼写】`root` 要与之后 [`Searcher::detach_root`] 用的
    ///   **逐字相同**（索引空间写法）—— 拼写不同 → 摘根静默无效 → 幽灵条目。
    pub fn attach_root(
        &self,
        root: &Path,
        entries: Vec<IndexEntry>,
    ) -> Result<u32, EngineError> {
        let (reply_tx, reply_rx) = channel();
        self.tx
            .send(Job::AttachRoot {
                root: root.to_path_buf(),
                entries,
                reply: reply_tx,
            })
            .map_err(|_| EngineError::Internal("搜索线程已退出".into()))?;
        reply_rx
            .recv()
            .map_err(|_| EngineError::Internal("搜索线程异常退出".into()))?
    }

    /// 分离一个根目录 —— 条目**立即**从索引里消失。
    ///
    /// 【⚠️ 路径必须与 attach 时**逐字相同**】
    ///   引擎比的是**索引空间写法**（`self.roots` 与条目路径），
    ///   **不是**规范化后的真实路径 ——
    ///   拼写不同 → **静默无效** → 条目留着 = **幽灵条目**
    ///   （用户最不能接受的那种：盘都拔了，搜得到它的文件）。
    ///
    /// 【返回值】分离之后的**索引条目总数**。
    pub fn detach_root(&self, root: &Path) -> Result<u32, EngineError> {
        let (reply_tx, reply_rx) = channel();
        self.tx
            .send(Job::DetachRoot {
                root: root.to_path_buf(),
                reply: reply_tx,
            })
            .map_err(|_| EngineError::Internal("搜索线程已退出".into()))?;
        reply_rx
            .recv()
            .map_err(|_| EngineError::Internal("搜索线程异常退出".into()))?
    }
}

/// 把一批变化路径映射回它所属的根 —— **只**把这些根标脏。
///
/// 【为什么要有这一层】落盘是**每 2 秒**一次的后台动作。
///   插着 U 盘时，每次落盘周期都要重写 $HOME 的 3.4MB **加上** U 盘那份十几 MB ——
///   而 $HOME 那份**根本没变** → 白烧 I/O 与 CPU。
///   → 只重写**真的变了**的那个根 ✓。
///
/// 【路径为什么要映射】FSEvents 报的是**真实路径**，
///   索引里的根是按**索引空间**拼的 —— 两边要靠 `root_pairs()` 对上。
///
/// 【找不到归属根怎么办】宁可多写，不能写错 → **所有**根都标脏
///   （路径落在所有根之外，或者它的根刚被摘掉了 → 说不清是哪份档该更新）。
fn mark_changed_roots(dirty: &mut HashSet<PathBuf>, engine: &Engine, paths: &[PathBuf]) {
    let pairs = engine.root_pairs();
    let mut unknown = false;
    for p in paths {
        match owning_root(&pairs, p) {
            Some(root) => {
                dirty.insert(root);
            }
            None => unknown = true,
        }
    }
    if unknown {
        dirty.extend(engine.roots());
    }
}

/// 这条变化路径属于哪个根（**索引空间**写法）。
///
/// 【为什么不自己写一遍前缀判定】
///   - 真实路径 → 索引空间：用增量更新的**同一份**映射
///     （`incremental::map_to_index_space`）；
///   - 索引空间路径 → 它的根：用引擎的**同一份** helper（`engine::path_in_root`）。
///   → 「路径属于哪个根」这件事**只有一份真相**（全局约束 C10）。
fn owning_root(pairs: &[(PathBuf, PathBuf)], raw: &Path) -> Option<PathBuf> {
    // 真实路径 → 索引空间
    let mapped = incremental::map_to_index_space(pairs, raw)?;
    // 索引空间路径 → 它的根（组件级判定）
    pairs
        .iter()
        .find(|(indexed, _)| path_in_root(&mapped, indexed))
        .map(|(indexed, _)| indexed.clone())
}

impl Drop for Searcher {
    fn drop(&mut self) {
        let _ = self.tx.send(Job::Shutdown);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
        // 搜索线程退了 → 把还没落盘的补上（**不能丢**）。
        self.persister.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 摘根时丢掉的那个快照，**真的不会再被写出去**（要求 ④）。
    ///
    /// 【为什么要有这条】`PersistSlot` 是「每根覆盖最新字节流」——
    ///   摘根**之前**提交的那份快照还在排队 → 摘完根它仍会落盘 →
    ///   把那个根的档重写成**摘根前**的内容（陈旧档）。
    ///   → 摘根要把那份快照丢掉。
    ///
    /// 【为什么用白盒】这个状态在**落盘线程**手里 —— 想在端到端里稳定地
    ///   观测到「提交了但还没写」那个窗口，得跟落盘线程赛跑（会 flaky）。
    ///   → 直接测落盘槽本身：提交 → 丢掉 → 槽里空了 ✓。
    #[test]
    fn a_discarded_snapshot_is_not_written_anymore() {
        let slot = PersistSlot {
            jobs: Mutex::new(HashMap::new()),
            stopped: AtomicBool::new(false),
            cv: Condvar::new(),
        };
        let path = PathBuf::from("/Volumes/U盘/archive.bin");
        slot.submit(path.clone(), vec![1, 2, 3]);
        assert_eq!(slot.jobs.lock().unwrap().len(), 1, "提交后槽里有一份");

        slot.discard(&path);
        assert!(
            slot.jobs.lock().unwrap().is_empty(),
            "丢掉之后槽里不该还留着它（否则会落一份陈旧档）"
        );
    }

    /// 丢掉**别的**根的快照，不该影响这一个。
    #[test]
    fn discarding_one_root_keeps_the_others() {
        let slot = PersistSlot {
            jobs: Mutex::new(HashMap::new()),
            stopped: AtomicBool::new(false),
            cv: Condvar::new(),
        };
        let a = PathBuf::from("/a.bin");
        let b = PathBuf::from("/b.bin");
        slot.submit(a.clone(), vec![1]);
        slot.submit(b.clone(), vec![2]);

        slot.discard(&a);
        let jobs = slot.jobs.lock().unwrap();
        assert_eq!(jobs.len(), 1);
        assert!(jobs.contains_key(&b), "别的根的快照不该被丢掉");
    }

    /// 变化路径 → 它所属的根（**索引空间**写法）。
    ///
    /// 【为什么要钉住】FSEvents 报的是**真实路径**，
    ///   索引里的根是按**索引空间**拼的 —— 两边对不上，
    ///   就会把变化算到**错的根**头上 → 改一份档、漏一份档。
    #[test]
    fn a_change_path_maps_back_to_its_root() {
        let pairs = vec![
            (PathBuf::from("/tmp/home"), PathBuf::from("/private/tmp/home")),
            (PathBuf::from("/Volumes/U盘"), PathBuf::from("/Volumes/U盘")),
        ];
        // 真实路径（macOS 上 /tmp 是 /private/tmp 的链接）→ 索引空间的根
        assert_eq!(
            owning_root(&pairs, Path::new("/private/tmp/home/sub/a.txt")),
            Some(PathBuf::from("/tmp/home"))
        );
        // 监听器报的就是原样路径 → 也要对得上
        assert_eq!(
            owning_root(&pairs, Path::new("/tmp/home/sub/a.txt")),
            Some(PathBuf::from("/tmp/home"))
        );
        // 另一个根（外接卷）
        assert_eq!(
            owning_root(&pairs, Path::new("/Volumes/U盘/x.log")),
            Some(PathBuf::from("/Volumes/U盘"))
        );
        // 所有根之外 → 找不到（→ 保守处理：全部重写）
        assert_eq!(owning_root(&pairs, Path::new("/elsewhere/x.log")), None);
    }
}
