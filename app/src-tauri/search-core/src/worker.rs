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

use std::sync::mpsc::{channel, Sender};
use std::thread::JoinHandle;

use crate::engine::Engine;
use crate::types::{EngineError, SearchRequest, SearchResult};

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
    Shutdown,
}

/// 搜索线程句柄。
pub struct Searcher {
    tx: Sender<Job>,
    handle: Option<JoinHandle<()>>,
}

impl Searcher {
    /// 起一个搜索线程，把引擎交给它独占。
    pub fn new(engine: Engine) -> Searcher {
        let (tx, rx) = channel::<Job>();
        let handle = std::thread::Builder::new()
            .name("lodestar-search".into())
            .spawn(move || {
                // 当前 query —— 翻页请求必须与之一致，
                // 否则它读到的其实是上一次搜索的视图。
                let mut current_query: Option<String> = None;

                while let Ok(job) = rx.recv() {
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
                        Job::Shutdown => break,
                    }
                }
                // 线程退出时 engine 被 drop → 索引释放。
            })
            .expect("启动搜索线程失败");

        Searcher { tx, handle: Some(handle) }
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
}

impl Drop for Searcher {
    fn drop(&mut self) {
        let _ = self.tx.send(Job::Shutdown);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}
