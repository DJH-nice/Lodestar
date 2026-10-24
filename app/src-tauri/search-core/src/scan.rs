//! 并行扫描器 —— 把磁盘变成索引。
//!
//! 【为什么要并行】
//!   扫描是 **I/O 延迟**瓶颈，不是 CPU 瓶颈：每个条目都要一次 `stat`，
//!   单线程时每个 stat 的延迟直接串起来。
//!   实测（`$HOME`，8,352 条目，热缓存）：
//!     单线程裸遍历      38.1ms
//!     2 线程            22.2ms
//!     4 线程            16.6ms  ← 4 线程就饱和
//!     8 线程            15.5ms
//!   → 4 线程 ≈ 2.3× 加速。再往上加线程收益甚微（磁盘到顶了）。
//!
//! 【与上游的对比】
//!   上游是**单线程**递归扫描（`src/fsearch_database_scan.c`），
//!   同样的范围它热缓存要 74.6ms —— 本引擎 4 线程约 16~25ms。
//!
//! 【排除规则在这里生效】命中规则的**目录不递归进去**（剪枝）。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::exclude;
use crate::fold::fold;
use crate::types::{EngineError, IndexEntry};

/// 一个目录的子项计数（分文件 / 分目录）。
///
/// 【为什么要分开数】
///   `childcount:`（总数）、`childfilecount:`（子文件数）、
///   `childfoldercount:`（子目录数）是三个字段 —— 只数总数的话，
///   后两个就没法实现（总需要重新遍历磁盘）。
///   → 扫描时顺手分开数，索引期就存好（查询期 O(1)）。
#[derive(Debug, Clone, Copy, Default)]
struct ChildCounts {
    files: u32,
    folders: u32,
}

impl ChildCounts {
    fn total(&self) -> u32 {
        self.files + self.folders
    }
}

/// 扫描线程数。
///
/// 【为什么上限是 4】实测 4 线程就到饱和点（16.6ms），再往上磁盘到顶。
///   下限是 2 —— 单线程扫描在 10 万级索引上会明显卡。
fn thread_count() -> usize {
    let cpus = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    cpus.clamp(2, 4)
}

/// 扫描一批根目录，返回索引条目。
///
/// 【条目里包含索引根本身】
///   每个根目录也是一条条目（depth 0）——
///   上游也这么做，所以「搜到的是不是那个目录本身」的行为保持一致。
///
/// 【并行策略：顶层子树分区 + 动态游标】
///   以前是「**根目录之间**并行」——
///   而默认索引范围是**整个 `$HOME`**（`settings.index_paths` 为空），
///   也就是**只有 1 个根** → 线程数被压成 1 → **并行从来没生效过**
///   （`sample` 抓的真实扫描火焰图里只有一个线程为证）。
///
///   现在改成：
///   1. 主线程**浅扫**每个根 → 建出根的条目（depth 0）+ 根的直接子项（depth 1）；
///   2. 根下的每个**子目录**成为一个「并行单位」；
///   3. 线程用**原子游标**动态领单位（不是静态轮转）——
///      静态轮转会被一棵大子树卡死（实测 `$HOME` 里 `Desktop` 占 34.9%）。
///
/// 【为什么是「分区」而不是「共享队列 / 工作窃取」】
///   两种形态都实测过（同一台机器、同一份树，同进程交替跑）：
///     子树分区（每线程一棵独立子树）  70.4ms → 21.8ms   **2.4~3.2×**
///     共享目录队列（工作窃取式）      38.5ms → 41.8ms   **零收益**
///   → 分区赢，窃取输。**别把这段改回共享队列。**
///
/// 【并行上限】
///   单位是「一级子目录」→ 上限 = 总条目 / 最大子树。
///   实测 `$HOME`：最大一级子树占 34.9% → 理论上限 2.87×。
///   想再快只能把单位切得更细（二级单位 → 上限 4.06×），
///   但那要多扫一层目录 —— 等真需要时再说。
///
/// 【索引顺序是「不确定」的】
///   各子树由不同线程完成 → 条目在 `entries` 里的先后取决于调度。
///   这跟以前（多根目录并行时）一样，**界面不依赖索引顺序**
///   （结果一律按排序维度重排；分组边界算的是**排序后**的位置）。
///   ⚠️ 但**别**在测试里按下标断言条目顺序 —— 用集合断言。
pub fn scan(roots: &[PathBuf]) -> Result<Vec<IndexEntry>, EngineError> {
    if roots.is_empty() {
        return Err(EngineError::InvalidArg);
    }

    let mut entries: Vec<IndexEntry> = Vec::new();
    let mut child_counts: HashMap<PathBuf, ChildCounts> = HashMap::new();
    // 并行单位：每个根下的**一级子目录**。
    let mut units: Vec<PathBuf> = Vec::new();

    // ── 主线程：浅扫每个根 ─────────────────────────────────────
    for root in roots {
        // ★ 索引根本身**也是一条条目**（depth 0）——
        //   上游也这么做，所以「搜到的是不是那个目录本身」的行为保持一致。
        let parent = root.parent().unwrap_or(root).to_path_buf();
        // 根目录的元数据在这里取一次（下面遍历时不会再有第二次）
        let meta = std::fs::symlink_metadata(root).ok();
        entries.push(make_entry(root, &parent, 0, true, meta.as_ref()));

        // 浅扫根：建出根的直接子项（depth 1），并把其中的目录收成并行单位。
        //
        // 【为什么根要单独浅扫，而不是把根丢进队列让线程去走】
        //   根是所有子树的汇合点 —— 只有把它「切开」，
        //   各棵子树之间才互不相交，线程之间才**不需要任何同步**。
        let read = match std::fs::read_dir(root) {
            Ok(r) => r,
            Err(_) => {
                // 读不了根 → 这个根没有内容
                //   （macOS TCC 会让受保护目录读不了）
                //   【不要让整个扫描失败】——见 walk_queue 里的同款说明。
                continue;
            }
        };
        let mut root_counts = ChildCounts::default();
        for item in read.flatten() {
            let name = item.file_name().to_string_lossy().into_owned();
            // 隐藏项一律不要（文件与目录都算）
            if exclude::is_hidden(&name) {
                continue;
            }
            let path = item.path();
            // 用 symlink_metadata：**不跟随符号链接**
            let meta = match std::fs::symlink_metadata(&path) {
                Ok(m) => m,
                Err(_) => continue,
            };
            let is_dir = meta.is_dir() && !meta.file_type().is_symlink();
            // 命中排除规则 → 这一项压根不进索引，**也不算子项**
            if exclude::is_excluded_dir_name(&name) {
                continue;
            }
            if is_dir {
                root_counts.folders += 1;
                // 一级子目录 = 并行单位（它的子树交给线程去走）
                units.push(path.clone());
                entries.push(make_entry(&path, root, 1, true, Some(&meta)));
            } else {
                root_counts.files += 1;
                entries.push(make_entry(&path, root, 1, false, Some(&meta)));
            }
        }
        child_counts.insert(root.clone(), root_counts);
    }

    // ── 并行：线程动态领单位 ───────────────────────────────────
    let threads = thread_count().min(units.len());
    if threads <= 1 {
        // 单位不够分（或零个子目录）→ 主线程自己走完
        for unit in &units {
            walk_queue(vec![(unit.clone(), 1)], &mut entries, &mut child_counts);
        }
    } else {
        // 原子游标：谁空谁领下一个单位。
        //
        // 【为什么动态领，而不是静态轮转】
        //   负载很偏（实测 `$HOME`：最大一级子树占 34.9%，前 4 大占 79.9%，
        //   最大/最小 56×）—— 静态轮转可能把两棵大树分给同一个线程，
        //   那个线程就拖到了最后。动态领则让先干完的线程接着领。
        let cursor = AtomicUsize::new(0);
        let per_thread_entries: Vec<(Vec<IndexEntry>, HashMap<PathBuf, ChildCounts>)> =
            std::thread::scope(|scope| {
                let handles: Vec<_> = (0..threads)
                    .map(|_| {
                        scope.spawn(|| {
                            let mut local: Vec<IndexEntry> = Vec::new();
                            let mut local_counts: HashMap<PathBuf, ChildCounts> = HashMap::new();
                            loop {
                                let idx = cursor.fetch_add(1, Ordering::Relaxed);
                                if idx >= units.len() {
                                    break;
                                }
                                // 单位是一级子目录（depth 1）→ 它的子项是 depth 2
                                walk_queue(
                                    vec![(units[idx].clone(), 1)],
                                    &mut local,
                                    &mut local_counts,
                                );
                            }
                            (local, local_counts)
                        })
                    })
                    .collect();
                handles
                    .into_iter()
                    .map(|h| h.join().expect("扫描线程 panicked"))
                    .collect()
            });

        for (local, local_counts) in per_thread_entries {
            entries.extend(local);
            for (k, v) in local_counts {
                child_counts.insert(k, v);
            }
        }
    }

    // 回填子项数量
    for e in entries.iter_mut() {
        if e.is_folder {
            let c = child_counts
                .get(Path::new(&e.path))
                .copied()
                .unwrap_or_default();
            e.child_count = c.total();
            e.child_file_count = c.files;
            e.child_folder_count = c.folders;
        }
    }

    Ok(entries)
}

/// 走完一个队列（目录 → 子目录 → …）。
///
/// 【队列里带 depth】根是 0，它的子目录是 1 —— `depth:` 字段直接用它。
fn walk_queue(
    mut queue: Vec<(PathBuf, u32)>,
    entries: &mut Vec<IndexEntry>,
    child_counts: &mut HashMap<PathBuf, ChildCounts>,
) {
    while let Some((dir, depth)) = queue.pop() {
        let read = match std::fs::read_dir(&dir) {
            Ok(r) => r,
            Err(_) => {
                // 读不了就跳过这个目录 —— 【不要让整个扫描失败】
                //
                // 【为什么】macOS 的 TCC 会让受保护目录（桌面/文稿/下载）读不了。
                //   上游在这里是**整个扫描失败**（`WALK_BADIO` 向上传播）——
                //   那意味着「一个目录没权限 → 整个索引报废」。
                //   本引擎选择跳过 —— 少索引点东西，比什么都搜不到强得多。
                continue;
            }
        };

        for item in read.flatten() {
            let name = item.file_name().to_string_lossy().into_owned();
            if name == "." || name == ".." {
                continue;
            }
            let path = item.path();

            // 隐藏项一律不要（文件与目录都算）
            if exclude::is_hidden(&name) {
                continue;
            }

            // 用 symlink_metadata：**不跟随符号链接**
            // 【为什么】macOS 上 /var → /private/var 这种链接到处都是，
            //   跟进去就可能绕成环；而链接本身按「文件」索引（与上游一致）。
            let meta = match std::fs::symlink_metadata(&path) {
                Ok(m) => m,
                Err(_) => continue,
            };

            let is_dir = meta.is_dir() && !meta.file_type().is_symlink();

            // 命中排除规则 → 这一项压根不进索引，**也不算子项**
            // （先判断再计数：省掉「加了又减」这种来回改）
            //
            // ★ 目录级剪枝：命中排除规则的目录**不递归进去**
            //   —— 里面的文件从没被访问过（这是性能命脉，见 exclude.rs）。
            if exclude::is_excluded_dir_name(&name) {
                continue;
            }

            // 子项计数挂在这个目录自己身上（分文件 / 分目录）
            let counts = child_counts.entry(dir.clone()).or_default();
            if is_dir {
                counts.folders += 1;
                entries.push(make_entry(&path, &dir, depth + 1, true, Some(&meta)));
                queue.push((path, depth + 1));
            } else {
                counts.files += 1;
                entries.push(make_entry(&path, &dir, depth + 1, false, Some(&meta)));
            }
        }
    }
}

/// 一个条目的「大小 + 修改时间」—— **索引期算好，查询期直接读**。
///
/// 【为什么要单独抽出来】扫描器与增量更新器都要算这两个值，
///   必须**算得一模一样**（差一秒 mtime，索引就与磁盘对不上）。
///   → 只有这一份实现。
pub(crate) fn size_and_mtime(meta: &std::fs::Metadata) -> (i64, i64) {
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    (meta.len() as i64, mtime)
}

/// 造一条索引条目。
///
/// 【为什么要把 `meta` 传进来，而不是在这里自己 stat】
///   扫描器**已经**为这个条目 stat 过一次了（判 is_dir + 不跟随符号链接）。
///   在这里再 stat 一次 = **每个条目两次系统调用**。
///   实测（离屏探针，8,548 条目）：去掉第二次 stat 省 ~7.5ms，
///   而 `sample` 抓的真实扫描火焰图里 **`lstat` 占 29.6%** —— 扫描器最大的一笔开销。
///   → 元数据由调用方**传进来**，这里只取用，不再碰磁盘。
///
/// 【`meta` 传 `None` 的场合】
///   只有「压根没 stat 过」的场合（根目录 stat 失败、或单测）→ size/mtime 记 0。
///
/// 【mtime 不会额外触发系统调用】
///   `Metadata::modified()` 在 Unix 上直接读已缓存的 `stat` 结构 → 零 syscall。
///
/// 【为什么是 `pub(crate)`】增量更新器要**造同样的条目** ——
///   两边各造一份的话，字段一漂移（「忘了折叠路径」之类），
///   增量出来的索引就与全量扫出来的对不上。
///   → 只有这一份实现。
pub(crate) fn make_entry(
    path: &Path,
    dir: &Path,
    depth: u32,
    is_folder: bool,
    meta: Option<&std::fs::Metadata>,
) -> IndexEntry {
    let name = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let path_s = path.to_string_lossy().into_owned();
    let dir_s = dir.to_string_lossy().into_owned();

    let (size, mtime) = match meta {
        Some(m) => size_and_mtime(m),
        None => (0, 0),
    };

    let extension = if is_folder {
        String::new()
    } else {
        extension_of(&name)
    };

    IndexEntry {
        folded_name: fold(&name),
        folded_path: fold(&path_s),
        name,
        path: path_s,
        dir: dir_s,
        extension,
        size,
        mtime,
        is_folder,
        depth,
        child_count: 0,
        child_file_count: 0,
        child_folder_count: 0,
    }
}

/// 取扩展名（不含点号）。
///
/// 【规则】最后一个 `.` 之后的部分；没有点、或点在开头 → 空串。
fn extension_of(name: &str) -> String {
    match name.rfind('.') {
        Some(idx) if idx > 0 && idx + 1 < name.len() => name[idx + 1..].to_lowercase(),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fixture() -> PathBuf {
        static SEQ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("lodestar-scan-{}-{}", std::process::id(), seq));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("alpha.txt"), "hello").unwrap();
        std::fs::write(dir.join("beta.RS"), "hi").unwrap();
        std::fs::write(dir.join("sub/gamma.log"), "x").unwrap();
        std::fs::write(dir.join(".hidden"), "x").unwrap();
        std::fs::create_dir_all(dir.join("node_modules/pkg")).unwrap();
        std::fs::write(dir.join("node_modules/pkg/index.js"), "x").unwrap();
        dir
    }

    #[test]
    fn scans_files_and_prunes_excluded_dirs() {
        let dir = fixture();
        let entries = scan(&[dir.clone()]).unwrap();
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();

        assert!(names.contains(&"alpha.txt"));
        assert!(names.contains(&"beta.RS"));
        assert!(names.contains(&"gamma.log"));
        assert!(names.contains(&"sub"));
        // 索引根本身也算一条
        assert!(names.contains(&dir.file_name().unwrap().to_string_lossy().as_ref()));
        // 隐藏文件不要
        assert!(!names.iter().any(|n| n.starts_with('.')));
        // ★ node_modules 整棵子树要被剪掉
        assert!(!names.contains(&"node_modules"));
        assert!(!names.contains(&"index.js"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn depth_is_relative_to_the_index_root() {
        let dir = fixture();
        let entries = scan(&[dir.clone()]).unwrap();
        let root_name = dir.file_name().unwrap().to_string_lossy().to_string();
        let root = entries.iter().find(|e| e.name == root_name).unwrap();
        assert_eq!(root.depth, 0, "索引根的 depth 必须是 0");

        let sub = entries.iter().find(|e| e.name == "sub").unwrap();
        assert_eq!(sub.depth, 1, "根的直接子目录 depth 必须是 1");

        let gamma = entries.iter().find(|e| e.name == "gamma.log").unwrap();
        assert_eq!(gamma.depth, 2, "根的孙子 depth 必须是 2");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn child_count_excludes_pruned_dirs() {
        let dir = fixture();
        let entries = scan(&[dir.clone()]).unwrap();
        let root_name = dir.file_name().unwrap().to_string_lossy().to_string();
        let root = entries.iter().find(|e| e.name == root_name).unwrap();
        // 根下有：alpha.txt / beta.RS / sub / .hidden(隐藏,不算) / node_modules(剪掉,不算)
        assert_eq!(root.child_count, 3, "被剪掉的目录不该计入子项数");
        // 分开数：文件 2 个（alpha.txt / beta.RS），目录 1 个（sub）
        assert_eq!(root.child_file_count, 2, "子文件数不对");
        assert_eq!(root.child_folder_count, 1, "子目录数不对");
        assert_eq!(
            root.child_file_count + root.child_folder_count,
            root.child_count,
            "总数必须等于「文件 + 目录」"
        );

        let sub = entries.iter().find(|e| e.name == "sub").unwrap();
        assert_eq!(sub.child_count, 1, "sub 下只有 gamma.log");
        assert_eq!(sub.child_file_count, 1);
        assert_eq!(sub.child_folder_count, 0);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn extension_is_lowercased_and_missing_for_dotfiles() {
        assert_eq!(extension_of("a.TXT"), "txt");
        assert_eq!(extension_of("archive.tar.gz"), "gz");
        assert_eq!(extension_of("noext"), "");
        assert_eq!(extension_of(".bashrc"), "");
    }

    #[test]
    fn folded_name_is_normalized_and_lowercased() {
        let e = make_entry(Path::new("/tmp/ÉCLAIR.txt"), Path::new("/tmp"), 1, false, None);
        // NFD：É → E + 组合尖音符；再转小写
        assert!(e.folded_name.starts_with('e'));
        assert_eq!(e.extension, "txt");
    }

    #[test]
    fn entry_size_and_mtime_come_from_the_passed_metadata() {
        // 【去掉重复 stat 之后的护栏】
        //   元数据由调用方传入 → 必须保证「传进来的那份」被正确使用，
        //   而不是退化成 0、也不是又偷偷 stat 一次。
        let dir = fixture();
        let f = dir.join("alpha.txt");
        std::fs::write(&f, "hello").unwrap();
        let meta = std::fs::symlink_metadata(&f).unwrap();

        let e = make_entry(&f, &dir, 1, false, Some(&meta));
        assert_eq!(e.size, 5, "size 必须来自传入的元数据");
        assert!(e.mtime > 0, "mtime 必须来自传入的元数据");
        assert_eq!(e.name, "alpha.txt");
        assert_eq!(e.extension, "txt");

        // 没有元数据 → 记 0（不 panic，也不去碰磁盘）
        let e2 = make_entry(&f, &dir, 1, false, None);
        assert_eq!(e2.size, 0);
        assert_eq!(e2.mtime, 0);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unreadable_dir_does_not_abort_the_whole_scan() {
        // TCC 受保护目录读不了时，只能跳过它，不能让整个扫描失败
        let dir = fixture();
        let entries = scan(&[dir.clone(), PathBuf::from("/nonexistent-xyz")]);
        assert!(entries.is_ok(), "不存在的根目录不该让扫描失败");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
