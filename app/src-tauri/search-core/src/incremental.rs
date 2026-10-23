//! 增量更新 —— 把「哪些路径变了」变成「索引变了」。
//!
//! 【为什么要有这一层】
//!   以前的实时更新是**全量重扫**：收到文件变化 → 把整棵子树重新扫一遍
//!   （`$HOME` 量级实测 300ms+），而且会把排在后面的搜索全堵住。
//!   Linux 上游 FSearch 走 inotify 是**增量增删** ——
//!   这是本项目当时**唯一弱于上游**的地方。
//!
//! 【这一层做什么】
//!   输入：一批「哪些路径变了」（watcher 从 FSEvents 收到的）。
//!   输出：索引被就地更新（新增 / 删除 / 改 size·mtime / 目录整棵换掉）。
//!
//! 【为什么「目录」要整棵换掉，而「文件」只改自己】
//!   FSEvents 给的是**每个文件**的事件（实测，见 `watcher.rs` 的说明），
//!   所以单个文件的变化可以精确到那一条 —— 不用碰它的任何邻居。
//!   但**目录**不一样：它可能被整体创建（连内容一起）、
//!   或者被改名 —— 这时候按路径去猜「里面有哪些文件」是不可靠的，
//!   直接把它的子树重新扫一遍最稳。
//!
//! 【为什么按「两阶段」做】
//!   第一阶段只**读**（分类、算增量），第二阶段才**改**（删旧的、加新的）。
//!   理由：删除会搬动下标（`swap_remove`）—— 如果边判边改，
//!   后面那些「已经算好的下标」就全错了。
//!   → **先把整批变化算清楚，再一次性落地**。

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::exclude;
use crate::scan;
use crate::types::{EngineError, IndexEntry};

/// 一批变化 —— 「哪些路径变了」。
#[derive(Debug, Clone, Default)]
pub struct ChangeSet {
    /// 变化过的路径。**顺序无关**：引擎会自己去重、按深度排序。
    pub paths: Vec<PathBuf>,
}

/// 一次增量更新的结果。
#[derive(Debug, Clone, Default)]
pub struct UpdateReport {
    /// 更新之后的索引条目总数。
    pub indexed: u32,
    /// 这一批里**实际生效**的路径数（被排除、被祖先覆盖的不算）。
    pub applied: u32,
    /// 是不是退化成了「全量重扫」。
    ///
    /// 【什么时候会退化】索引与磁盘的关系对不上时
    ///   （比如某个文件的父目录不在索引里）——
    ///   这时**宁可慢也不能错**：全量重扫一定是对的。
    pub fell_back: bool,
}

/// 一个目录的**子项计数**增量。
///
/// 【为什么用增量而不是「重算」】
///   重算要枚举那个目录的全部子项（一个大目录可能有上万个）——
///   而增删一两个文件只需要 ±1。
///
/// 【⚠️ 合并 M2 时要补一处】
///   M2 给 `IndexEntry` 加了 `child_file_count` / `child_folder_count`
///   两个字段（`childfilecount:` / `childfoldercount:` 字段查询用）。
///   合并时**这里要一并维护那两个计数** ——
///   否则增量出来的索引与全量重扫对不上。
///   **护栏会当场抓到它**：`search-core/tests/incremental.rs` 里
///   「增量之后的索引与全量重扫等价」那条断言会变红 ✓
///   （所以这个坑**不会**悄悄溜过去）。
#[derive(Debug, Clone, Copy, Default)]
struct ChildDelta {
    /// 子项**总数**（文件 + 目录）。
    total: i64,
}

/// 路径的变化类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// 不在了（被删了 / 被改名了）。
    Gone,
    /// 是个文件（新建或改了内容）。
    File,
    /// 是个目录（新建 / 改名 / 内容变了）。
    Dir,
}

/// 把一批变化应用到索引上。
///
/// 【返回 `Result` 的理由】退化不是错误 —— 退化是**正确的兜底**，
///   所以用 `fell_back` 表达，而不是 `Err`。
pub fn apply(
    entries: &mut Vec<IndexEntry>,
    by_path: &mut HashMap<Box<str>, u32>,
    roots: &[(PathBuf, PathBuf)],
    changes: &ChangeSet,
) -> Result<UpdateReport, EngineError> {
    // ───────────────────────── ① 映射 + 去重 + 排序 ─────────────────────────
    //
    // 【为什么要映射】FSEvents 报的是**真实路径**（`/private/tmp/...`），
    //   索引里存的是**按根目录原样拼出来的路径** —— 两边对不上就认不出来。
    let mut targets: Vec<PathBuf> = Vec::new();
    let mut seen: HashSet<PathBuf> = HashSet::new();
    for raw in &changes.paths {
        let Some(mapped) = map_to_index_space(roots, raw) else {
            continue;
        };
        if seen.insert(mapped.clone()) {
            targets.push(mapped);
        }
    }
    // 【为什么要排序】**父目录必须先于它的子项被处理**：
    //   新建一个目录时，目录本身的事件与里面文件的事件会**同时**到达；
    //   目录先处理 → 它的子树被整棵换掉 → 里面那些文件的事件就被**覆盖**了
    //   （不会重复处理，也不会打架）。
    targets.sort_by(|a, b| {
        component_count(a)
            .cmp(&component_count(b))
            .then_with(|| a.cmp(b))
    });

    let mut report = UpdateReport::default();
    let mut removals: HashSet<u32> = HashSet::new();
    let mut insertions: Vec<IndexEntry> = Vec::new();
    let mut deltas: HashMap<PathBuf, ChildDelta> = HashMap::new();
    // 被整棵换掉/删掉的目录 → 它的后代要一并处理。
    let mut removed_prefixes: Vec<String> = Vec::new();
    // 已经被祖先目录整体处理掉的目录 → 里面的路径不用再单独处理。
    let mut covered: Vec<PathBuf> = Vec::new();
    let mut applied = 0u32;

    // ───────────────────────── ② 分类 + 算清楚要做什么 ─────────────────────────
    for target in &targets {
        // 被祖先目录整棵处理掉了 → 跳过（避免重复处理）
        if covered.iter().any(|c| target.starts_with(c)) {
            continue;
        }
        // 【排除规则】引擎根本不索引的路径 → 这条变化**与索引无关**。
        //
        //   ⚠️ 必须是**连祖先一起查**（`path_is_excluded` 就是这么做的）：
        //   排除是**目录级剪枝** —— `~/Library/...` 底下的一切
        //   压根没进过索引，变化了也不该动索引。
        if exclude::path_is_excluded(target) {
            continue;
        }

        let key = target.to_string_lossy().into_owned();
        match classify(target) {
            Kind::Gone => {
                // 不在了 → 从索引里拿掉
                let Some(&slot) = by_path.get(key.as_str()) else {
                    continue;
                };
                let entry = entries[slot as usize].clone();
                removals.insert(slot);
                bump(&mut deltas, Path::new(&entry.dir), -1);
                if entry.is_folder {
                    // 目录没了 → 整棵子树都没了
                    removed_prefixes.push(format!("{key}/"));
                    covered.push(target.clone());
                }
                applied += 1;
            }
            Kind::File => {
                let parent = target.parent().map(Path::to_path_buf);
                match by_path.get(key.as_str()).copied() {
                    Some(slot) => {
                        if entries[slot as usize].is_folder {
                            // 【类型变了】目录 → 文件（`rm -rf d && touch d`）
                            //   旧的那条要删掉，旧的子树也没了。
                            removals.insert(slot);
                            removed_prefixes.push(format!("{key}/"));
                            covered.push(target.clone());
                            // 总数不变（1 个目录 → 1 个文件）→ 父目录的计数不动
                            let depth = parent_depth(entries, by_path, parent.as_deref());
                            let Some(depth) = depth else {
                                return Ok(fallback());
                            };
                            insertions.push(scan::make_entry(target, parent.as_deref().unwrap(), depth, false));
                            applied += 1;
                        } else {
                            // 已索引 → 只改 size / mtime（**不碰它的任何邻居**）
                            if let Ok(meta) = std::fs::symlink_metadata(target) {
                                let (size, mtime) = scan::size_and_mtime(&meta);
                                let e = &mut entries[slot as usize];
                                e.size = size;
                                e.mtime = mtime;
                            }
                            applied += 1;
                        }
                    }
                    None => {
                        // 新增的文件 → 父目录必须在索引里（否则算不出深度）
                        let Some(parent) = parent else {
                            return Ok(fallback());
                        };
                        let Some(depth) = parent_depth(entries, by_path, Some(&parent)) else {
                            return Ok(fallback());
                        };
                        insertions.push(scan::make_entry(target, &parent, depth, false));
                        bump(&mut deltas, &parent, 1);
                        applied += 1;
                    }
                }
            }
            Kind::Dir => {
                // 目录 → 整棵子树换掉
                let existing = by_path.get(key.as_str()).copied();
                let base_depth = match existing {
                    Some(slot) => entries[slot as usize].depth,
                    None => {
                        // 新目录 → 深度 = 父目录 + 1
                        let Some(parent) = target.parent().map(Path::to_path_buf) else {
                            return Ok(fallback());
                        };
                        let Some(depth) = parent_depth(entries, by_path, Some(&parent)) else {
                            return Ok(fallback());
                        };
                        depth
                    }
                };

                // 新的子树（深度从 0 起 —— 待会儿整体偏移）
                let fresh = match scan::scan(&[target.clone()]) {
                    Ok(f) => f,
                    Err(_) => return Ok(fallback()),
                };

                match existing {
                    Some(slot) => {
                        // 旧的子树要删掉
                        removals.insert(slot);
                        removed_prefixes.push(format!("{key}/"));
                        covered.push(target.clone());
                        // 父目录的子项总数：删了又加回来 → **净变化 0**（不动）
                    }
                    None => {
                        // 新目录 → 父目录的子项数 +1
                        if let Some(parent) = target.parent() {
                            bump(&mut deltas, parent, 1);
                        }
                    }
                }

                for mut e in fresh {
                    e.depth += base_depth;
                    insertions.push(e);
                }
                applied += 1;
            }
        }
    }

    // ── 被删掉的目录的后代 —— 一次扫过全表捞出来 ──
    //
    // 【为什么要单独扫】删一个目录 = 删掉它底下**所有**条目。
    //   一次线性扫（O(n × 目录数)）比「每个后代各查一次」划算得多。
    //   【判据】路径以「目录/」开头 —— 带尾斜杠是为了不误伤
    //   同前缀的兄弟目录（`/a/bc` 不该被 `/a/b/` 命中）。
    if !removed_prefixes.is_empty() {
        for (i, e) in entries.iter().enumerate() {
            if removed_prefixes.iter().any(|p| e.path.starts_with(p.as_str())) {
                removals.insert(i as u32);
            }
        }
    }

    // ───────────────────────── ③ 落地：先删，再加 ─────────────────────────
    //
    // 【为什么先删后加】插入是**追加**到末尾的（不动别人的下标），
    //   删除会**搬动**下标 —— 所以删除必须一次做完。
    //
    // 【为什么从大到小删】`swap_remove(i)` 会把**最后一条**搬到 `i`。
    //   从大到小删 → 每次被搬动的都是「还没轮到要删的那条」以外的东西，
    //   剩下的待删下标**不会**被搬歪。
    let mut slots: Vec<u32> = removals.into_iter().collect();
    slots.sort_unstable_by(|a, b| b.cmp(a));
    for slot in slots {
        let i = slot as usize;
        if i >= entries.len() {
            continue;
        }
        by_path.remove(entries[i].path.as_str());
        entries.swap_remove(i);
        if i < entries.len() {
            // 被搬上来的是原来的最后一条 → 它的下标变了，要改表
            let moved = entries[i].path.as_str().into();
            by_path.insert(moved, i as u32);
        }
    }

    for e in insertions {
        let slot = entries.len() as u32;
        by_path.insert(e.path.as_str().into(), slot);
        entries.push(e);
    }

    // ───────────────────────── ④ 修子项计数 + 刷新目录自己的元信息 ─────────────────────────
    //
    // 【为什么还要刷新目录的 size / mtime】
    //   目录**自己**的大小与修改时间会随内容变化：
    //   加一个文件 → 目录的 mtime 变成「刚才」→ `dm:today` 这类查询会认它。
    //   全量扫描每次都会重新 stat 目录（所以它是对的），
    //   增量更新不刷新就会**留下过期的值** —— 索引与磁盘对不上。
    for (dir, d) in deltas {
        let key = dir.to_string_lossy();
        let Some(&slot) = by_path.get(key.as_ref()) else {
            continue;
        };
        let meta = std::fs::symlink_metadata(dir).ok();
        let e = &mut entries[slot as usize];
        e.child_count = (e.child_count as i64 + d.total).max(0) as u32;
        if let Some(m) = meta {
            let (size, mtime) = scan::size_and_mtime(&m);
            e.size = size;
            e.mtime = mtime;
        }
    }

    report.indexed = entries.len() as u32;
    report.applied = applied;
    Ok(report)
}

/// 「退化」的报告 —— 引擎会改走全量重扫。
fn fallback() -> UpdateReport {
    UpdateReport {
        indexed: 0,
        applied: 0,
        fell_back: true,
    }
}

/// 父目录的深度 + 1（父目录不在索引里 → `None`）。
fn parent_depth(
    entries: &[IndexEntry],
    by_path: &HashMap<Box<str>, u32>,
    parent: Option<&Path>,
) -> Option<u32> {
    let parent = parent?;
    let key = parent.to_string_lossy();
    let slot = *by_path.get(key.as_ref())?;
    Some(entries[slot as usize].depth + 1)
}

/// 记一笔子项计数增量。
fn bump(deltas: &mut HashMap<PathBuf, ChildDelta>, dir: &Path, by: i64) {
    deltas.entry(dir.to_path_buf()).or_default().total += by;
}

/// 这个路径**是什么**（文件 / 目录 / 不在了）。
///
/// 【为什么用 `symlink_metadata`】**不跟随符号链接** ——
///   与扫描器（`scan.rs`）保持一致：macOS 上 /var → /private/var
///   这种链接到处都是，跟进去就可能绕成环；
///   链接本身按「文件」索引。
fn classify(p: &Path) -> Kind {
    match std::fs::symlink_metadata(p) {
        Ok(m) => {
            if m.is_dir() && !m.file_type().is_symlink() {
                Kind::Dir
            } else {
                Kind::File
            }
        }
        Err(_) => Kind::Gone,
    }
}

/// 把事件报的路径**映射回索引空间**。
///
/// 【为什么要映射】FSEvents 报的是**真实路径**：
///   监听 `/tmp/x` 时，事件报的是 `/private/tmp/x/...`
///   （macOS 的 `/tmp` 是指向 `/private/tmp` 的符号链接）。
///   索引里的路径却是按根目录**原样**拼出来的 —— 两边对不上，
///   增量更新就会「认不出变了的东西」。
///
/// 【两根都试】先按真实路径试，再按原样路径试 ——
///   万一监听器报的就是原样路径，也不至于漏。
fn map_to_index_space(roots: &[(PathBuf, PathBuf)], raw: &Path) -> Option<PathBuf> {
    for (indexed, real) in roots {
        if let Ok(rel) = raw.strip_prefix(real) {
            return Some(indexed.join(rel));
        }
    }
    for (indexed, _) in roots {
        if raw.starts_with(indexed) {
            return Some(raw.to_path_buf());
        }
    }
    None
}

/// 路径有多少层（用来排序：**浅的先处理**）。
fn component_count(p: &Path) -> usize {
    p.components().count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_symlink_to_a_directory_counts_as_a_file() {
        // 与扫描器一致：链接按「文件」索引（不跟随）
        let dir = std::env::temp_dir().join(format!("sextant-inc-link-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("real")).unwrap();
        std::fs::write(dir.join("real/inside.txt"), "x").unwrap();
        std::os::unix::fs::symlink(dir.join("real"), dir.join("link")).unwrap();

        assert_eq!(classify(&dir.join("link")), Kind::File);
        assert_eq!(classify(&dir.join("real")), Kind::Dir);
        assert_eq!(classify(&dir.join("nope")), Kind::Gone);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_real_path_is_mapped_back_to_the_index_space() {
        let roots = vec![(
            PathBuf::from("/tmp/watched"),
            PathBuf::from("/private/tmp/watched"),
        )];
        let got = map_to_index_space(&roots, Path::new("/private/tmp/watched/a/b.txt"));
        assert_eq!(got, Some(PathBuf::from("/tmp/watched/a/b.txt")));
    }

    #[test]
    fn a_path_outside_every_root_is_not_mapped() {
        let roots = vec![(
            PathBuf::from("/tmp/watched"),
            PathBuf::from("/private/tmp/watched"),
        )];
        assert_eq!(
            map_to_index_space(&roots, Path::new("/elsewhere/x.txt")),
            None
        );
    }

    #[test]
    fn the_same_path_mapped_twice_gives_the_same_answer() {
        let roots = vec![(
            PathBuf::from("/tmp/watched"),
            PathBuf::from("/private/tmp/watched"),
        )];
        let a = map_to_index_space(&roots, Path::new("/private/tmp/watched/x.txt"));
        let b = map_to_index_space(&roots, Path::new("/tmp/watched/x.txt"));
        assert_eq!(a, b);
    }
}
