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
    if !store.exists() {
        println!("  （还没有索引档：{store:?}）");
    } else {
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
                    // 【为什么可能读不出来】索引档有**格式版本**：
                    //   版本对不上（别的版本写过的档）→ 读档失败 → 退回全量扫描。
                    //   这是**设计如此**，不是错误。
                    println!("  ✗ 读档失败（版本对不上？→ 会退回全量扫描，这是安全的）");
                    best = 0.0;
                    break;
                }
            }
        }
        if best > 0.0 {
            println!("  读档 {count} 项：{best:.1} ms");
        }
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

    println!("\n=== 5. 索引落盘（写索引档的耗时）===");
    // 【为什么要单独量】落盘是「把内存里的索引写成文件」。
    //   增量更新若要**顺手**落盘，这个耗时就在增量更新的关键路径上 ——
    //   量清楚才知道能不能放在同步路径里。
    let probe_store = std::env::temp_dir().join("sextant-bench-persist.bin");
    let entries = engine.all_entries();
    let mut best = f64::MAX;
    for _ in 0..3 {
        let t = Instant::now();
        if let Err(e) = search_core::store::save(&entries, &[home_path.clone()], &probe_store) {
            println!("  ✗ 落盘失败：{e}");
            return;
        }
        let d = ms_since(t);
        if d < best {
            best = d;
        }
    }
    println!("  落盘 {indexed} 项：{best:.1} ms（临时文件 + fsync + rename）");

    // 读回刚写的这一份 → **冷启动的读档耗时**（自己写的档，版本一定对得上）
    let mut best_load = f64::MAX;
    for _ in 0..3 {
        let t = Instant::now();
        match search_core::store::load(&probe_store) {
            Some((e, _)) => {
                let d = ms_since(t);
                if d < best_load {
                    best_load = d;
                }
                std::hint::black_box(e.len());
            }
            None => {
                println!("  ✗ 读档失败");
                best_load = 0.0;
                break;
            }
        }
    }
    if best_load > 0.0 {
        println!("  读回 {indexed} 项：{best_load:.1} ms（**冷启动的关键**）");
    }

    // 拆开看：落盘是「序列化（CPU）」+「写盘（I/O）」两段。
    // 哪一段贵，决定了它能不能放在增量更新的关键路径上。
    let mut best_ser = f64::MAX;
    for _ in 0..3 {
        let t = Instant::now();
        let buf = search_core::store::serialize(&entries, &[home_path.clone()]);
        let d = ms_since(t);
        if d < best_ser {
            best_ser = d;
        }
        std::hint::black_box(&buf);
    }
    println!("  其中 序列化（纯 CPU）：{best_ser:.1} ms");

    // 写盘那一段：fsync 与不 fsync 的差别
    let buf = search_core::store::serialize(&entries, &[home_path.clone()]);
    for sync in [true, false] {
        let mut best_w = f64::MAX;
        for _ in 0..3 {
            let t = Instant::now();
            search_core::store::save_bytes(&buf, &probe_store, sync).unwrap();
            let d = ms_since(t);
            if d < best_w {
                best_w = d;
            }
        }
        println!(
            "  其中 写盘（fsync={sync}）：{best_w:.1} ms"
        );
    }

    let mut best_copy = f64::MAX;
    for _ in 0..3 {
        let t = Instant::now();
        let c = engine.all_entries();
        let d = ms_since(t);
        if d < best_copy {
            best_copy = d;
        }
        std::hint::black_box(&c);
    }
    println!("  其中 拷一份条目（落盘线程要用的）：{best_copy:.1} ms");

    let _ = std::fs::remove_file(&probe_store);

    // ─────────────────────────────────────────────────────────────
    // 6. 增量更新（M3）—— 「改文件 → 索引跟上」到底要多久
    // ─────────────────────────────────────────────────────────────
    //
    // 【为什么要在临时目录里造一份同样大的索引】
    //   增量更新的耗时**取决于索引有多大**（有几处线性扫）。
    //   要跟「改一个真实文件」比，就得有**同样规模**的索引。
    //   用 `$HOME` 当基准可以，但**不能往用户的家目录里造测试文件** ——
    //   所以这里造一份**同样条目数**的临时索引。
    println!("\n=== 6. 增量更新（改文件 → 索引跟上）===");
    let bench_dir = std::env::temp_dir().join("sextant-bench-incremental");
    let _ = std::fs::remove_dir_all(&bench_dir);
    for d in 0..40 {
        let sub = bench_dir.join(format!("d{d:02}"));
        std::fs::create_dir_all(&sub).unwrap();
        for f in 0..200 {
            std::fs::write(sub.join(format!("f{f:03}.txt")), "x").unwrap();
        }
    }
    let (inc_engine, inc_indexed) = Engine::new(&[bench_dir.as_path()]).unwrap();
    println!("  基准索引：{inc_indexed} 项（与 $HOME 同量级）");

    // ① 改**一个**文件 —— 最常见的场景
    let one = bench_dir.join("d00/f000.txt");
    let mut content = std::fs::read(&one).unwrap();
    content.push(b'x');
    let t = Instant::now();
    std::fs::write(&one, &content).unwrap();
    let write_done = ms_since(t);
    let changes = search_core::incremental::ChangeSet {
        paths: vec![one.clone()],
    };
    let t = Instant::now();
    let report = inc_engine.apply_changes(&changes).unwrap();
    let one_ms = ms_since(t);
    println!(
        "  改 1 个文件：更新耗时 {one_ms:.2} ms（落盘另算）+ 写文件 {write_done:.2} ms；生效 {} 个路径",
        report.applied
    );

    // ② 改 **100** 个文件 —— 「批量不该变成 100 次重扫」
    let mut paths = Vec::new();
    for d in 0..10 {
        for f in 0..10 {
            let p = bench_dir.join(format!("d{d:02}/f{f:03}.txt"));
            std::fs::write(&p, "changed").unwrap();
            paths.push(p);
        }
    }
    let changes = search_core::incremental::ChangeSet { paths };
    let t = Instant::now();
    let report = inc_engine.apply_changes(&changes).unwrap();
    let batch_ms = ms_since(t);
    println!(
        "  改 100 个文件：更新耗时 {batch_ms:.2} ms（**一次**更新，生效 {} 个路径）",
        report.applied
    );

    // ③ 对照：全量重扫要多久（同一份索引）
    let t = Instant::now();
    let _ = inc_engine.rescan().unwrap();
    let full_ms = ms_since(t);
    println!("  对照 · 全量重扫：{full_ms:.2} ms");
    println!(
        "  → 增量比全量快 {:.0} 倍",
        full_ms / one_ms.max(0.001)
    );

    let _ = std::fs::remove_dir_all(&bench_dir);
}
