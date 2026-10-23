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

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, RecvTimeoutError, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::engine::Engine;
use crate::incremental::{ChangeSet, UpdateReport};
use crate::store;
use crate::types::{EngineError, SearchRequest, SearchResult};

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
    Shutdown,
}

/// 落盘线程的活儿：一份已经序列化好的索引字节流 + 目标路径。
struct PersistJob {
    bytes: Vec<u8>,
    path: std::path::PathBuf,
}

/// 落盘槽 —— **只要最新的那一份**。
///
/// 【为什么是「覆盖」而不是「排队」】
///   变化来得比落盘快时（比如一次 `git checkout` 触几百个事件），
///   排队的写法会写好几次**内容几乎一样**的档 —— 白白烧 I/O。
///   覆盖掉旧的 → 只写最后那一份 ✓（内容是最新的 ✓）。
struct PersistSlot {
    job: Mutex<Option<PersistJob>>,
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
    /// 交出一份快照（**覆盖**掉还没写的那份 —— 只要最新的）。
    fn submit(&self, job: PersistJob) {
        let mut g = self.job.lock().unwrap();
        *g = Some(job);
        self.cv.notify_one();
    }
}

impl Persister {
    fn new() -> Persister {
        let slot = Arc::new(PersistSlot {
            job: Mutex::new(None),
            stopped: AtomicBool::new(false),
            cv: Condvar::new(),
        });
        let thread_slot = Arc::clone(&slot);
        let handle = std::thread::Builder::new()
            .name("sextant-persist".into())
            .spawn(move || {
                loop {
                    let job = {
                        let mut g = thread_slot.job.lock().unwrap();
                        while g.is_none() && !thread_slot.stopped.load(Ordering::Relaxed) {
                            g = thread_slot.cv.wait(g).unwrap();
                        }
                        g.take()
                    };
                    let Some(job) = job else { break };
                    if let Err(e) = store::save_bytes(&job.bytes, &job.path, true) {
                        eprintln!("[sextant] 索引落盘失败（不影响使用）: {e}");
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
            .name("sextant-search".into())
            .spawn(move || {
                // 当前 query —— 翻页请求必须与之一致，
                // 否则它读到的其实是上一次搜索的视图。
                let mut current_query: Option<String> = None;
                // 索引档是不是落后于内存里的索引。
                let mut store_dirty = false;
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
                                if res.is_ok() {
                                    current_query = None; // 视图已被替换
                                    store_dirty = true;
                                }
                                let _ = reply.send(res);
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
                    if store_dirty && last_persist_at.elapsed() >= PERSIST_INTERVAL {
                        if let Some(bytes) = engine.serialize_for_store() {
                            if let Some(path) = engine.store_path() {
                                persist_slot.submit(PersistJob { bytes, path });
                            }
                            store_dirty = false;
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
