//! 性能基准 —— 把引擎的关键数字一次测清。
//!
//! 用法：
//! ```bash
//! cargo run --release -p search-core --example bench
//! ```
//!
//! 【测什么】
//!   1. 读档（冷启动的关键）：从索引档读出全部条目要多久
//!   2. 全量扫描：同样的目录，重新扫一遍要多久（读档 vs 扫描的差 = 冷启动省下的时间）
//!   3. 查询延迟：不同规模的索引上，查询要多久
//!
//! 【为什么要有这个】
//!   「冷启动从 300ms 降到多少」这种数字，**必须实测**——
//!   凭感觉估出来的数字在文档里活不过一次改动。

use std::path::PathBuf;
use std::time::Instant;

use search_core::engine::Engine;
use search_core::types::{ListKind, SearchRequest, SortField, SortOrder};

fn ms_since(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}

fn request(query: &str) -> SearchRequest {
    SearchRequest {
        query: query.to_string(),
        sort: SortField::Name,
        order: SortOrder::Asc,
        kind: ListKind::All,
        page: 0,
        page_size: 500,
    }
}

fn main() {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/".into());
    let home_path = PathBuf::from(&home);
    let store = PathBuf::from(&home)
        .join("Library/Application Support/com.sextant.macos/index.bin");

    println!("=== 1. 读档（冷启动的关键）===");
    if store.exists() {
        // 读三遍取最小 —— 第一次可能受磁盘缓存影响
        let mut best = f64::MAX;
        let mut count = 0;
        for _ in 0..3 {
            let t = Instant::now();
            match search_core::store::load(&store) {
                Some((entries, _)) => {
                    let d = ms_since(t);
                    if d < best {
                        best = d;
                    }
                    count = entries.len();
                }
                None => {
                    println!("  ✗ 读档失败");
                    break;
                }
            }
        }
        println!("  读档 {count} 项：{best:.1} ms");
    } else {
        println!("  （还没有索引档：{store:?}）");
    }

    println!("\n=== 2. 全量扫描（读档的对照组）===");
    let t = Instant::now();
    let (engine, indexed) = match Engine::new(&[home_path.as_path()]) {
        Ok(v) => v,
        Err(e) => {
            println!("  ✗ 建引擎失败：{e}");
            return;
        }
    };
    println!("  扫描 {indexed} 项：{:.1} ms", ms_since(t));

    println!("\n=== 3. 查询延迟（预热后取三次最小）===");
    for q in ["*", "a", "report", "ext:pdf", "size:>1M", "zzzznope"] {
        // 预热
        let _ = engine.search(&request(q));
        let mut best = f64::MAX;
        let mut hits = 0;
        for _ in 0..3 {
            let t = Instant::now();
            let r = engine.search(&request(q)).expect("查询失败");
            let d = ms_since(t);
            if d < best {
                best = d;
            }
            hits = r.total_entries;
        }
        println!("  q={:<12} 命中 {hits:<7} {best:.3} ms", format!("{q:?}"));
    }

    println!("\n=== 4. 索引内存 ====");
    let mem = std::process::Command::new("/usr/bin/du")
        .arg("-h")
        .arg(&store)
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.split_whitespace().next().unwrap_or("?").to_string())
        .unwrap_or_else(|| "?".into());
    println!("  索引档大小：{mem}（{indexed} 项）");
}
