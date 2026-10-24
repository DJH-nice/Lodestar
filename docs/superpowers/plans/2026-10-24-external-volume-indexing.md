# 外接卷索引 实施计划（插上 U 盘就能搜到里面的文件）

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让 Lodestar 在插入外接盘（U 盘 / 移动硬盘）后自动把盘上的文件纳入索引，拔盘后立刻从索引里移除，重新插入时复用旧索引并后台刷新。

**Architecture:** 把「索引根目录」从建引擎时定死的常量变成**运行时可增删**的一等公民 —— 引擎新增 `attach_root` / `detach_root`，索引档改成**每个根一个文件**（插拔卷不再作废 $HOME 的档），新增 `src/volumes.rs` 用 `getmntinfo` 每秒枚举 `/Volumes` 下的本地外接卷并驱动附加/分离。

**Tech Stack:** Rust 1.90 · `libc`（`getmntinfo`，已在 lock 文件里，不新增下载）· Tauri v2 · Vue 3 + TypeScript · `cargo test` / `vitest`

**配套设计文档：** [`docs/superpowers/specs/2026-10-24-external-volume-indexing-design.md`](../specs/2026-10-24-external-volume-indexing-design.md)

---

## Global Constraints

以下每条都是硬约束，**每个任务都隐含包含**：

| # | 约束 |
|---|---|
| C1 | **删文件必须用 `mavis-trash <绝对路径>`** —— `rm -rf` / glob 删除会被安全网关拦 |
| C2 | **未经明确同意不得 `brew install`** |
| C3 | **Rust 侧搜索线程必须串行**（`search-core/src/worker.rs` 单线程 + 单 mpsc 通道）—— 视图有状态，排队是**正确性**要求 |
| C4 | **扫描绝不在搜索 worker 上做** —— worker 只做「整份替换」这种微秒级操作。30 万文件的盘扫一次要几十秒，堵在 worker 上 = 几十秒搜不动 |
| C5 | **路径归属判定必须用 `Path::starts_with`（组件级）** —— 字符串前缀会让 `/a/bc` 被 `/a/b` 命中 → 漏索引 |
| C6 | **CSS 颜色只能用 `:root` 里已有的令牌**（`--fg-3` / `--warn` / `--success` / `--accent` …）；`z-index` 走 `--z-*` 那一族 |
| C7 | **打包只能用 `bash app/scripts/bundle-app.sh`** —— 自检里看到 `✓ 前端已嵌进二进制` 才算成功 |
| C8 | **新增 Rust→前端事件，必须同时加进 `src/event_names.rs` 的 `EVENT_NAMES` 并把数量断言从 7 改成 8** —— 那里有护栏测试，改漏了会红 |
| C9 | **报错方向要一致：宁可性能出问题，也不要功能出问题** —— 漏索引是用户看得见的故障，多扫一次只是慢 |
| C10 | **同一个东西只有一份真相** —— 排除规则、语法表、事件名清单都遵守这条（新增判定逻辑不要抄第二份） |

---

## 与设计文档的三处偏差（合并 M3 之后必须改）

设计文档写于 M3 合并**之前**。M3 把「全量重扫」换成了「增量更新」，有三处要按现实调整：

| 设计文档 | 现实 | 怎么改 |
|---|---|---|
| §3.6 watcher 改成「脏根集合 + 按根重扫」 | **不需要了** —— M3 的增量更新已经只碰「真的变了的那几个路径」，U 盘上一个文件变动不会牵连 $HOME | **删掉这一节**，Task 3~6 都不做它。省掉一个子系统 |
| §3.2「按路径过滤维护条目归属」 | M3 给引擎加了 `by_path: HashMap<Box<str>, u32>`（O(1) 定位路径）→ 增删根时**必须同步维护它**，否则增量更新认不出路径 | Task 2 里加一条 |
| §3.3 索引档按根分文件 | M3 加了**后台落盘线程**（`PersistSlot` 持有「要写的那一份字节流」）→ 按根分档后，它得从「存一份」变成「存一份 per 根」 | Task 3 里改 `PersistSlot` |
| §3.4「扫描永远在 worker 之外」 | **只对卷成立**。设计文档写的时候，全量重扫是 **300ms~2.4s** → 挪出 worker 值得；M2 把它优化到 **21.1ms** → 挪出去不划算（还要多一套「谁负责扫」的编排） | **卷的扫描**在卷任务线程上（Task 5）；**$HOME 的全量重扫**仍在 worker 上（Task 3 不动它） |

> ⚠️ 这四处偏差**都要在动手前读完本计划** —— 照设计文档原样实现会做出一个已经被 M3/M2 取代的子系统。

---

## 文件结构

```
app/src-tauri/
├── search-core/src/
│   ├── store.rs          改：每根一个档（root_archive_path / save_root / load_root）
│   ├── engine.rs         改：roots 可增删（attach_root / detach_root）、按根序列化
│   └── worker.rs         改：AttachRoot / DetachRoot 两个 Job、落盘槽改成 per 根
├── src/
│   ├── volumes.rs        ★ 新建：卷枚举 + 监控线程 + 附加/分离编排
│   ├── settings.rs       改：索引档改成目录、重建引擎时带上已附加的卷
│   ├── index_state.rs    改：卷状态（VolumeState）
│   ├── commands.rs       改：IndexStatus 加 volumes 字段
│   └── event_names.rs    改：加 index-volumes-changed + 数量断言 7 → 8
└── ../src/
    ├── App.vue           改：监听 index-volumes-changed → 重搜
    ├── components/StatusBar.vue  改：显示外接卷状态
    └── lib/types.ts      改：IndexStatus 加 volumes
```

**新增测试文件：**

| 文件 | 测什么 |
|---|---|
| `search-core/tests/root_mutation.rs` | 引擎的根目录增删（Task 2） |
| `app/src-tauri/tests/volumes.rs` | 卷枚举的纯函数 + 差分逻辑（Task 4、5） |

---

## Task 1: 索引档按根分文件

**Files:**
- Modify: `app/src-tauri/search-core/src/store.rs`
- Test: `app/src-tauri/search-core/src/store.rs`（文件末尾的 `mod tests`）

**Interfaces:**
- Consumes: 无（这是最底层，谁都不依赖）
- Produces:
  ```rust
  pub fn root_archive_path(dir: &Path, root: &Path) -> PathBuf;
  pub fn serialize_root(root: &Path, entries: &[IndexEntry]) -> Vec<u8>;
  pub fn save_root(dir: &Path, root: &Path, entries: &[IndexEntry]) -> std::io::Result<()>;
  pub fn load_root(dir: &Path, root: &Path) -> Option<Vec<IndexEntry>>;
  ```
  （Task 2 的引擎、Task 3 的 worker、Task 5 的卷任务都用这几个）

**为什么先做这个**：索引档现在是「一份档 + roots 全等校验」（`store.rs:96` 那条校验在 `engine.rs` 里）。
插拔一次 U 盘 → roots 变了 → 整份档作废 → **$HOME 全量重扫**（实测 300ms~2.4s）。
按根分档之后，$HOME 的档与卷的档**互不影响**。

- [ ] **Step 1: 写失败的测试**

追加到 `app/src-tauri/search-core/src/store.rs` 的 `mod tests` 里（`sample()` 这个已有的辅助函数直接复用）：

```rust
    #[test]
    fn a_root_archive_round_trips() {
        let dir = std::env::temp_dir().join(format!("lodestar-store-root-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let root = PathBuf::from("/Volumes/U盘");
        let entries = sample();
        save_root(&dir, &root, &entries).expect("应该能写档");
        let got = load_root(&dir, &root).expect("应该能读档");
        assert_eq!(got.len(), entries.len());
        assert_eq!(got[0].path, entries[0].path);
        assert_eq!(got[0].name, entries[0].name);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn two_roots_never_share_an_archive() {
        let dir = std::env::temp_dir().join(format!("lodestar-store-two-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let home = PathBuf::from("/Users/me");
        let usb = PathBuf::from("/Volumes/U盘");
        save_root(&dir, &home, &sample()).expect("应该能写档");
        assert!(
            load_root(&dir, &usb).is_none(),
            "另一个根不该读到这份档 —— 这正是「插拔卷不作废 \$HOME 的档」的前提"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_archive_name_is_stable_per_root() {
        let dir = Path::new("/tmp/archives");
        let a = root_archive_path(dir, Path::new("/Users/me"));
        let b = root_archive_path(dir, Path::new("/Volumes/U盘"));
        assert_eq!(a, root_archive_path(dir, Path::new("/Users/me")), "同一个根 → 同一个文件名");
        assert_ne!(a, b, "不同的根 → 不同的文件名");
        assert_eq!(a.extension().unwrap(), "bin");
    }

    #[test]
    fn a_missing_archive_returns_none() {
        assert!(load_root(Path::new("/nonexistent/lodestar"), Path::new("/Users/me")).is_none());
    }
```

- [ ] **Step 2: 跑测试确认它失败**

```bash
cd app/src-tauri && cargo test -p search-core store::tests 2>&1 | tail -20
```

Expected: 编译错误 —— `cannot find function save_root in this scope`（函数还不存在）。

- [ ] **Step 3: 实现**

在 `app/src-tauri/search-core/src/store.rs` 里，**保留**现有的 `serialize` / `save` / `load`（Task 2 才会切过去），**新增**下面这些：

```rust
/// 格式版本。
///
/// 【v3】改成**每个根一个档** —— 档里不再有 roots 列表，只有「自己这个根」。
///   为什么要拆：插拔一次外接卷 → roots 集合变了 → v2 的「roots 全等校验」
///   会把**整份档**（包括 \$HOME 的）判成「不是这份索引」→ \$HOME 全量重扫。
///   拆开之后，卷的档与 \$HOME 的档**互不影响**。
///   v2 的老档直接作废 → 一次性全量重扫（可接受）。
const VERSION: u32 = 3;

/// 根路径 → 它的档在 `dir` 里的文件名。
///
/// 【为什么用哈希而不是把路径编码进文件名】
///   路径里有 `/` 和中文，长度也不封顶 → 文件名会不合法或超长。
///   哈希是定长十六进制 → 永远是合法文件名。
///
/// 【碰撞了会怎样】
///   档里**记着自己的根路径**（见 [`parse_root`]）→ 读出来对不上就当没有档
///   （退回全量扫描）→ 碰撞最多浪费一次扫描，**不会读错数据**。
pub fn root_archive_path(dir: &Path, root: &Path) -> PathBuf {
    dir.join(format!("{:016x}.bin", fnv1a64(&root.to_string_lossy())))
}

/// FNV-1a 64 位 —— 自己写，不依赖 `DefaultHasher`
/// （后者的种子不保证跨 Rust 版本稳定，不能拿来当持久化文件名）。
fn fnv1a64(s: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// 把**一个根**的条目序列化成字节流。
///
/// 【与 v2 的 `serialize` 有什么不同】
///   v2：MAGIC + 版本 + **roots 列表** + 全部条目（所有根混在一起）。
///   v3：MAGIC + 版本 + **这个根的路径** + 这个根的条目。
pub fn serialize_root(root: &Path, entries: &[IndexEntry]) -> Vec<u8> {
    let mut cap = 64usize + entries.len() * 96;
    for e in entries {
        cap += 4 + e.name.len()
            + 4 + e.path.len()
            + 4 + e.dir.len()
            + 4 + e.extension.len()
            + 4 + e.folded_name.len()
            + 4 + e.folded_path.len()
            + 8 + 8 + 1 + 4 + 4 + 4 + 4;
    }
    let mut buf: Vec<u8> = Vec::with_capacity(cap);
    buf.extend_from_slice(MAGIC);
    buf.extend_from_slice(&VERSION.to_le_bytes());
    write_str(&mut buf, &root.to_string_lossy());
    buf.extend_from_slice(&(entries.len() as u32).to_le_bytes());
    for e in entries {
        write_str(&mut buf, &e.name);
        write_str(&mut buf, &e.path);
        write_str(&mut buf, &e.dir);
        write_str(&mut buf, &e.extension);
        write_str(&mut buf, &e.folded_name);
        write_str(&mut buf, &e.folded_path);
        buf.extend_from_slice(&e.size.to_le_bytes());
        buf.extend_from_slice(&e.mtime.to_le_bytes());
        let mut flags = 0u8;
        if e.is_folder {
            flags |= 1;
        }
        buf.push(flags);
        buf.extend_from_slice(&e.depth.to_le_bytes());
        buf.extend_from_slice(&e.child_count.to_le_bytes());
        buf.extend_from_slice(&e.child_file_count.to_le_bytes());
        buf.extend_from_slice(&e.child_folder_count.to_le_bytes());
    }
    buf
}

/// 把一个根的条目写到 `dir` 里它自己的档（原子替换）。
pub fn save_root(dir: &Path, root: &Path, entries: &[IndexEntry]) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let path = root_archive_path(dir, root);
    save_bytes(&serialize_root(root, entries), &path, true)
}

/// 读某个根的档。**读不出来就返回 `None`**（调用方退回全量扫描）。
///
/// 【为什么「读不出来」不是错误】
///   没有档、格式版本变了、文件被截断、哈希碰撞对不上根 ——
///   这四种都该走「重新扫一遍」，而不是让应用起不来。
pub fn load_root(dir: &Path, root: &Path) -> Option<Vec<IndexEntry>> {
    let path = root_archive_path(dir, root);
    let mut buf = Vec::new();
    std::fs::File::open(path).ok()?.read_to_end(&mut buf).ok()?;
    let (saved_root, entries) = parse_root(&buf)?;
    if saved_root != root.to_string_lossy() {
        // 哈希碰撞 → 这份档不是这个根的 → 当没有档
        return None;
    }
    Some(entries)
}

/// 解析 v3 档 → (根路径, 条目)。
fn parse_root(buf: &[u8]) -> Option<(String, Vec<IndexEntry>)> {
    let mut r = Reader { buf, pos: 0 };
    if r.take(4)? != MAGIC {
        return None;
    }
    if r.u32()? != VERSION {
        return None;
    }
    let root = r.string()?;
    let count = r.u32()? as usize;
    let mut entries = Vec::with_capacity(count);
    for _ in 0..count {
        let name = r.string()?;
        let path = r.string()?;
        let dir = r.string()?;
        let extension = r.string()?;
        let folded_name = r.string()?;
        let folded_path = r.string()?;
        let size = r.i64()?;
        let mtime = r.i64()?;
        let flags = r.u8()?;
        let depth = r.u32()?;
        let child_count = r.u32()?;
        let child_file_count = r.u32()?;
        let child_folder_count = r.u32()?;
        entries.push(IndexEntry {
            name, path, dir, extension, folded_name, folded_path,
            size, mtime,
            is_folder: flags & 1 != 0,
            depth, child_count, child_file_count, child_folder_count,
        });
    }
    Some((root, entries))
}
```

- [ ] **Step 4: 跑测试确认通过**

```bash
cd app/src-tauri && cargo test -p search-core store::tests 2>&1 | tail -15
```

Expected: `test result: ok. ... passed`（含新增的 4 条）。

⚠️ 如果 `parse_root` 报「找不到 `Reader`」—— `Reader` 是 `store.rs` 里已有的私有结构体（`store.rs:144`），直接用即可，不用重写。

- [ ] **Step 5: 提交**

```bash
git add app/src-tauri/search-core/src/store.rs
git commit -m "feat(store): 索引档改成每根一个文件

插拔一次外接卷 → roots 集合变了 → v2 的「roots 全等校验」会把**整份档**
（包括 \$HOME 的）判成「不是这份索引」→ \$HOME 全量重扫（实测 300ms~2.4s）。
拆成每根一个档之后，卷的档与 \$HOME 的档互不影响。

- root_archive_path：FNV-1a 64 位哈希做文件名（自写，不用 DefaultHasher ——
  它的种子不保证跨版本稳定，不能当持久化文件名）
- 档里记着自己的根路径 → 哈希碰撞能检出（当没有档 → 重扫，不会读错数据）
- 格式 v2 → v3（老档作废 → 一次性全量重扫）
- 旧的 serialize/save/load 先留着，Task 2 切过去之后再删"
```

---

## Task 2: 引擎支持根目录增删

**Files:**
- Modify: `app/src-tauri/search-core/src/engine.rs`
- Test: `app/src-tauri/search-core/tests/root_mutation.rs`（新建）

**Interfaces:**
- Consumes: Task 1 的 `store::save_root` / `store::load_root` / `store::root_archive_path`
- Produces:
  ```rust
  // Engine
  pub fn attach_root(&self, root: &Path, entries: Vec<IndexEntry>) -> u32;
  pub fn detach_root(&self, root: &Path) -> u32;
  pub fn serialize_for_store(&self) -> Vec<(PathBuf, Vec<u8>)>;
  pub fn new_with_store(paths: &[&Path], store_dir: Option<PathBuf>) -> Result<(Engine, u32), EngineError>;
  pub fn store_dir(&self) -> Option<PathBuf>;
  ```

- [ ] **Step 1: 写失败的测试**

新建 `app/src-tauri/search-core/tests/root_mutation.rs`：

```rust
//! 引擎的「根目录可增删」—— 外接卷索引的地基。
//!
//! 【为什么要有这个能力】
//!   插上 U 盘 → 把它加进索引；拔掉 → 把它从索引里摘掉。
//!   而**不能**用「重建整个引擎」实现：
//!   重建要重扫所有根（新盘首扫 30 秒 → 整个索引空窗 30 秒）。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use search_core::types::{ListKind, SearchRequest, SortField, SortOrder};
use search_core::Engine;

fn fixture(tag: &str) -> PathBuf {
    static SEQ: AtomicU32 = AtomicU32::new(0);
    let seq = SEQ.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "lodestar-roots-{tag}-{}-{seq}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    std::fs::write(dir.join("alpha.txt"), "hello").unwrap();
    std::fs::write(dir.join("sub/beta.log"), "x").unwrap();
    dir
}

fn request(query: &str) -> SearchRequest {
    SearchRequest {
        query: query.to_string(),
        sort: SortField::Name,
        order: SortOrder::Asc,
        kind: ListKind::All,
        page: 0,
        page_size: 50,
    }
}

#[test]
fn attaching_a_root_makes_its_files_searchable() {
    let home = fixture("home");
    let usb = fixture("usb");
    let (engine, before) = Engine::new(&[home.as_path()]).unwrap();
    let entries = search_core::scan::scan(&[usb.clone()]).unwrap();
    let after = engine.attach_root(&usb, entries);
    assert!(after > before, "附加后条目数应该变多（{before} → {after}）");
    let res = engine.search(&request("beta")).unwrap();
    assert_eq!(res.total_files, 1, "U 盘上的文件要能搜到");
    assert!(
        res.entries.iter().any(|e| e.path.ends_with("beta.log")),
        "结果里要有 U 盘上的那个文件：{:?}",
        res.entries.iter().map(|e| &e.path).collect::<Vec<_>>()
    );
}

#[test]
fn detaching_a_root_removes_its_files_immediately() {
    let home = fixture("home");
    let usb = fixture("usb");
    let (engine, _) = Engine::new(&[home.as_path()]).unwrap();
    let entries = search_core::scan::scan(&[usb.clone()]).unwrap();
    engine.attach_root(&usb, entries);
    assert_eq!(engine.search(&request("beta")).unwrap().total_files, 1);

    let after = engine.detach_root(&usb);
    let res = engine.search(&request("beta")).unwrap();
    assert_eq!(res.total_files, 0, "拔盘后不该还搜得到（**不留幽灵条目**）");
    assert!(after > 0, "本机的条目还在");
}

#[test]
fn attaching_the_same_root_twice_replaces_instead_of_duplicating() {
    let home = fixture("home");
    let usb = fixture("usb");
    let (engine, _) = Engine::new(&[home.as_path()]).unwrap();
    let entries = search_core::scan::scan(&[usb.clone()]).unwrap();
    let once = engine.attach_root(&usb, entries.clone());
    let twice = engine.attach_root(&usb, entries);
    assert_eq!(once, twice, "重复附加 = 替换，不是叠加");
    assert_eq!(engine.search(&request("beta")).unwrap().total_files, 1, "不该出现两条");
}

#[test]
fn detaching_an_unknown_root_changes_nothing() {
    let home = fixture("home");
    let (engine, before) = Engine::new(&[home.as_path()]).unwrap();
    let after = engine.detach_root(Path::new("/Volumes/根本没插过"));
    assert_eq!(before, after);
}

#[test]
fn a_missing_root_is_skipped_but_the_others_survive() {
    let home = fixture("home");
    // 一个不存在的根（拔掉的卷正好撞在这个窗口里）
    let (engine, n) = Engine::new(&[home.as_path(), Path::new("/nonexistent/lodestar-xyz")]).unwrap();
    assert!(n > 0, "存在的那个根要留下");
    assert_eq!(engine.search(&request("alpha")).unwrap().total_files, 1);
}
```

- [ ] **Step 2: 跑测试确认它失败**

```bash
cd app/src-tauri && cargo test -p search-core --test root_mutation 2>&1 | tail -20
```

Expected: 编译错误 —— `no method named attach_root found for struct Engine`。

- [ ] **Step 3: 实现**

改 `app/src-tauri/search-core/src/engine.rs`：

**(a) 结构体：roots / real_roots 变成可改的，store_path 变成 store_dir**

```rust
pub struct Engine {
    /// 全部索引条目。
    entries: RefCell<Vec<IndexEntry>>,
    /// 路径 → 条目在 `entries` 里的下标。
    by_path: RefCell<HashMap<Box<str>, u32>>,
    /// 索引根目录。
    ///
    /// 【为什么是 `RefCell`】根目录现在**运行时可增删**（插上 U 盘加一个、拔掉删一个）。
    roots: RefCell<Vec<PathBuf>>,
    /// 索引根目录的**真实路径**（解过符号链接）—— 与 `roots` **一一对应**（同序同长）。
    real_roots: RefCell<Vec<PathBuf>>,
    /// 索引档的**目录**（每根一个文件）。
    store_dir: Option<PathBuf>,
    /// 上一次搜索的结果视图。
    view: RefCell<Option<View>>,
}
```

**(b) `new_with_store`：按根读档 / 按根扫**

```rust
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
                        let e = scan::scan(&[root.clone()])?;
                        // 写档失败不致命（下次冷启动慢一点而已）
                        if let Err(e) = store::save_root(dir, root, &e) {
                            eprintln!("[lodestar] 索引档写入失败（不影响使用）: {e}");
                        }
                        e
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
```

⚠️ **注意**：落盘有**两条路**，别以为 worker 的后台线程包办了：
- **建引擎时**（`new_with_store` 扫完）→ 同步写一次 —— 见 (f) 里的理由；
- **有变化之后** → worker 的后台落盘线程写（不占关键路径，Task 3 改）。

**(c) `validate_paths`：容忍不存在的根**

```rust
/// 校验索引根目录。
///
/// 【为什么改成「跳过不存在的」】
///   以前是「有任何一个不存在 → 整个引擎建不起来」（`InvalidArg`）。
///   但**外接卷会拔掉** —— 枚举到卷的那一刻它还在，几毫秒后就没了。
///   那时若因为它建不起引擎 → **整个应用没有索引**。
///   → 改成：跳过不存在的；**一个都不剩**才报错（那才是真的配错了）。
fn validate_paths(paths: &[&Path]) -> Result<Vec<PathBuf>, EngineError> {
    let mut out = Vec::with_capacity(paths.len());
    for p in paths {
        if !p.is_absolute() {
            return Err(EngineError::InvalidArg);
        }
        // 不存在（或已经不是目录了）→ 跳过
        if !p.is_dir() {
            continue;
        }
        out.push(p.to_path_buf());
    }
    if out.is_empty() {
        return Err(EngineError::InvalidArg);
    }
    Ok(out)
}
```

**(d) `attach_root` / `detach_root`**

```rust
    /// 附加一个根目录（条目由调用方**在 worker 之外**扫好）。
    ///
    /// 【幂等】重复附加 = **替换**（先摘掉旧的再挂新的）。
    ///
    /// 【为什么 `by_path` 必须一起维护】
    ///   M3 的增量更新靠 `by_path` O(1) 定位「这条路径在索引里的哪」。
    ///   挂了新条目却没登记 → 增量更新认不出它 → 索引悄悄过期。
    pub fn attach_root(&self, root: &Path, entries: Vec<IndexEntry>) -> u32 {
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
    /// 【它的档不删】—— 插回来时要复用（用户要的是「插回来立刻能搜」）。
    /// 档不动是因为落盘只写「当前在索引里的根」→ 拔掉的卷那份文件原地不动 ✓。
    pub fn detach_root(&self, root: &Path) -> u32 {
        let mut all = self.entries.borrow_mut();
        // 【组件级前缀】字符串前缀会让 `/a/bc` 被 `/a/b` 命中 → 漏索引
        all.retain(|e| !Path::new(&e.path).starts_with(root));
        *self.by_path.borrow_mut() = build_by_path(&all);
        let count = all.len() as u32;
        drop(all);

        // roots 与 real_roots 一一对应 → 按**同一个下标**删
        {
            let mut roots = self.roots.borrow_mut();
            let mut real_roots = self.real_roots.borrow_mut();
            if let Some(pos) = roots.iter().position(|r| r == root) {
                roots.remove(pos);
                if pos < real_roots.len() {
                    real_roots.remove(pos);
                }
            }
        }

        *self.view.borrow_mut() = None;
        count
    }
```

**(e) `serialize_for_store`：按根切分**

```rust
    /// 把索引按根切成「每根一份字节流」。
    ///
    /// 【为什么按根切】每根一个档 → 拔掉一个卷只影响它自己那份，
    ///   \$HOME 那份**不用重写**（省掉几 MB 的写盘）。
    pub fn serialize_for_store(&self) -> Vec<(PathBuf, Vec<u8>)> {
        let Some(dir) = self.store_dir.as_ref() else {
            return Vec::new();
        };
        let entries = self.entries.borrow();
        let roots = self.roots.borrow();
        roots
            .iter()
            .map(|root| {
                let part: Vec<IndexEntry> = entries
                    .iter()
                    .filter(|e| Path::new(&e.path).starts_with(root))
                    .cloned()
                    .collect();
                (store::root_archive_path(dir, root), store::serialize_root(root, &part))
            })
            .collect()
    }

    /// 索引档的**目录**。
    pub fn store_dir(&self) -> Option<PathBuf> {
        self.store_dir.clone()
    }
```

**(f) `persist()` 改成按根写；`store_path()` 改成 `store_dir()`**

`persist()` 现在要写**每个根一个档**（不是一个大档）：

```rust
    /// 把索引写到档（每根一个文件）。
    ///
    /// 【什么时候调】
    ///   - 建引擎时（全量扫完）—— 见上面 `new_with_store` 里的理由；
    ///   - 全量重扫后（`rescan()` 原来就调它）。
    ///   其余时候由 worker 的后台落盘线程负责（不占关键路径）。
    fn persist(&self) {
        let Some(dir) = self.store_dir.as_ref() else {
            return;
        };
        for (path, bytes) in self.serialize_for_store() {
            if let Err(e) = store::save_bytes(&bytes, &path, true) {
                eprintln!("[lodestar] 索引档写入失败（不影响使用）: {e}");
            }
        }
    }
```

⚠️ **`store::save` / `store::load` / `store::serialize`（v2 那套）现在没有调用方了** ——
但 **worker 还在用 `store::serialize`**（Task 3 才改）→ **本任务先留着它们**，
Task 3 的 Step 5 删。

⚠️ **`VERSION` 必须拆成两个** —— v2 的 `serialize` 与 v3 的 `serialize_root` 共用一个版本号
会写出「版本号是 3、布局却是 v2」的档 → 读回来错位：

```rust
/// v2 格式的版本号 —— **只给还没切走的旧代码用**（Task 3 会删掉那些代码）。
const VERSION_V2: u32 = 2;
/// v3 格式的版本号（当前）。
const VERSION: u32 = 3;
```

v2 的 `serialize` 用 `VERSION_V2`，v3 的 `serialize_root` 用 `VERSION` ✓ 两边互不干扰。

- [ ] **Step 4: 跑测试确认通过**

```bash
cd app/src-tauri && cargo test -p search-core 2>&1 | tail -15
```

Expected: 全绿（含新增的 `root_mutation` 5 条）。

⚠️ 如果 `Engine::new` 报「`store_path` 字段不存在」→ 说明还有别处引用它，
`grep -n "store_path" app/src-tauri/search-core/src/` 找出来改掉。

- [ ] **Step 5: 提交**

```bash
git add app/src-tauri/search-core/src/engine.rs app/src-tauri/search-core/src/store.rs app/src-tauri/search-core/tests/root_mutation.rs
git commit -m "feat(engine): 索引根目录可运行时增删（attach_root / detach_root）

插上 U 盘 → 加一个根；拔掉 → 摘一个根。**不用重建引擎**：
重建要重扫所有根（新盘首扫 30 秒 → 整个索引空窗 30 秒）。

- roots / real_roots 改成 RefCell（运行时可改），两者一一对应、同序同长
- by_path 跟着一起维护 —— M3 的增量更新靠它 O(1) 定位路径
- 归属判定用 Path::starts_with（组件级）—— 字符串前缀会漏索引
- validate_paths 改成「跳过不存在的根」—— 枚举到卷的那一刻它还在，
  几毫秒后就没了；那时建不起引擎 = 整个应用没有索引
- new_with_store 改成按根读档/按根扫；索引档从「一个文件」变成「一个目录」
- 格式版本拆成 VERSION_V2 / VERSION（v2 的那几个函数还在被 worker 用）"
```

---

## Task 3: worker 的附加/分离 + 按根落盘

**Files:**
- Modify: `app/src-tauri/search-core/src/worker.rs`
- Modify: `app/src-tauri/search-core/src/store.rs`（删掉 v2 那套）

**Interfaces:**
- Consumes: Task 2 的 `Engine::attach_root` / `detach_root` / `serialize_for_store`
- Produces:
  ```rust
  // Searcher
  pub fn attach_root(&self, root: &Path, entries: Vec<IndexEntry>) -> Result<u32, EngineError>;
  pub fn detach_root(&self, root: &Path) -> Result<u32, EngineError>;
  ```

- [ ] **Step 1: 写失败的测试**

在 `app/src-tauri/search-core/tests/root_mutation.rs` 末尾追加：

```rust
#[test]
fn the_searcher_can_attach_and_detach_a_root() {
    use search_core::Searcher;
    let home = fixture("w-home");
    let usb = fixture("w-usb");
    let (engine, _) = Engine::new(&[home.as_path()]).unwrap();
    let searcher = Searcher::new(engine);
    let entries = search_core::scan::scan(&[usb.clone()]).unwrap();
    let n = searcher.attach_root(&usb, entries).expect("应该能附加");
    assert!(n > 0);
    let res = searcher.search(&request("beta")).expect("应该能搜");
    assert_eq!(res.total_files, 1);
    searcher.detach_root(&usb).expect("应该能分离");
    let res = searcher.search(&request("beta")).expect("应该还能搜");
    assert_eq!(res.total_files, 0, "拔盘后不该还搜得到");
}
```

- [ ] **Step 2: 跑测试确认它失败**

```bash
cd app/src-tauri && cargo test -p search-core --test root_mutation 2>&1 | tail -15
```

Expected: 编译错误 —— `no method named attach_root found for struct Searcher`。

- [ ] **Step 3: 实现 —— worker 加两个 Job**

在 `app/src-tauri/search-core/src/worker.rs` 的 `enum Job` 里加：

```rust
    /// 附加一个根目录（条目由调用方**在 worker 之外**扫好）。
    AttachRoot {
        root: PathBuf,
        entries: Vec<IndexEntry>,
        reply: Sender<Result<u32, EngineError>>,
    },
    /// 分离一个根目录 —— 条目立即消失。
    DetachRoot {
        root: PathBuf,
        reply: Sender<Result<u32, EngineError>>,
    },
```

（`use std::path::PathBuf;` 与 `use crate::types::IndexEntry;` 要补上。）

在 `while let Ok(job) = rx.recv()` 的 match 里加两个分支：

```rust
                            Job::AttachRoot { root, entries, reply } => {
                                let res = engine.attach_root(&root, entries);
                                if res.is_ok() {
                                    current_query = None; // 视图已被替换
                                    store_dirty = true;
                                }
                                let _ = reply.send(res);
                            }
                            Job::DetachRoot { root, reply } => {
                                let res = engine.detach_root(&root);
                                if res.is_ok() {
                                    current_query = None; // 视图已被替换
                                    store_dirty = true;
                                }
                                let _ = reply.send(res);
                            }
```

在 `impl Searcher` 里加两个方法：

```rust
    /// 附加一个根目录。**条目由调用方扫好** —— 扫描绝不在这个线程上做
    /// （30 万文件的盘要扫几十秒 → 搜索会被堵死）。
    pub fn attach_root(
        &self,
        root: &Path,
        entries: Vec<IndexEntry>,
    ) -> Result<u32, EngineError> {
        let (reply_tx, reply_rx) = channel();
        self.tx
            .send(Job::AttachRoot { root: root.to_path_buf(), entries, reply: reply_tx })
            .map_err(|_| EngineError::Internal("搜索线程已退出".into()))?;
        reply_rx
            .recv()
            .map_err(|_| EngineError::Internal("搜索线程异常退出".into()))?
    }

    /// 分离一个根目录。
    pub fn detach_root(&self, root: &Path) -> Result<u32, EngineError> {
        let (reply_tx, reply_rx) = channel();
        self.tx
            .send(Job::DetachRoot { root: root.to_path_buf(), reply: reply_tx })
            .map_err(|_| EngineError::Internal("搜索线程已退出".into()))?;
        reply_rx
            .recv()
            .map_err(|_| EngineError::Internal("搜索线程异常退出".into()))?
    }
```

- [ ] **Step 4: 实现 —— 落盘槽改成 per 根**

M3 的 `PersistSlot` 现在存的是「一份字节流 + 一个路径」。按根分档后要存「每个根一份」：

```rust
/// 落盘槽 —— **每个根只要最新的那一份**。
///
/// 【为什么是「覆盖」而不是「排队」】
///   变化来得比落盘快时（一次 `git checkout` 触几百个事件），
///   排队的写法会写好几次**内容几乎一样**的档 —— 白白烧 I/O。
///   覆盖掉旧的 → 只写最后那一份 ✓（内容是最新的 ✓）。
///
/// 【为什么是 map 而不是一份】
///   索引档改成**每根一个文件** → 挂载的根不止一个（\$HOME + 若干外接卷）
///   → 每个根各要一份最新快照。
struct PersistSlot {
    /// 根的档路径 → 该根最新的字节流。
    jobs: Mutex<HashMap<PathBuf, Vec<u8>>>,
    stopped: AtomicBool,
    cv: Condvar,
}
```

`submit` 改成：

```rust
impl PersistSlot {
    /// 交出一个根的快照（**覆盖**掉那个根还没写的那份）。
    fn submit(&self, path: PathBuf, bytes: Vec<u8>) {
        let mut g = self.jobs.lock().unwrap();
        g.insert(path, bytes);
        self.cv.notify_one();
    }
}
```

落盘线程改成：

```rust
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
                            eprintln!("[lodestar] 索引档写入失败（不影响使用）: {e}");
                        }
                    }
                }
            })
```

⚠️ **注意线程名的旧名**：`worker.rs` 里还有 `.name("sextant-persist".into())` 和 `eprintln!("[sextant] ...")`
（那是 M3 合并时带回来的旧名）→ 一并改成 `lodestar-persist` / `[lodestar]`。

worker 的落盘触发那段改成：

```rust
                    if store_dirty && last_persist_at.elapsed() >= PERSIST_INTERVAL {
                        let jobs = engine.serialize_for_store();
                        if !jobs.is_empty() {
                            for (path, bytes) in jobs {
                                persist_slot.submit(path, bytes);
                            }
                            store_dirty = false;
                            last_persist_at = Instant::now();
                        }
                    }
```

- [ ] **Step 5: 删掉 v2 那套 store 函数**

`app/src-tauri/search-core/src/store.rs` 里，`serialize` / `save` / `load` / `parse`（v2 的）
现在**没有任何调用方**了 → 删掉（连同它们的测试 `round_trip_preserves_everything` 等，
但**保留** `garbage_input_is_rejected_not_panicked` 与 `an_older_archive_version_is_rejected`
并改成针对 v3 的版本）。

保留 `save_bytes`（worker 用）✓。

⚠️ 删完跑一次 `cargo build -p search-core` —— 如果报「unused function」，
说明还有调用方没切过去 → `grep -n "store::save\|store::load\|store::serialize" app/src-tauri/` 找出来。

- [ ] **Step 6: 跑测试确认通过**

```bash
cd app/src-tauri && cargo test -p search-core 2>&1 | tail -15
```

Expected: 全绿。

- [ ] **Step 7: 提交**

```bash
git add app/src-tauri/search-core/src/worker.rs app/src-tauri/search-core/src/store.rs app/src-tauri/search-core/tests/root_mutation.rs
git commit -m "feat(worker): 支持附加/分离根目录 + 落盘改成 per 根

- 新增 Job::AttachRoot / Job::DetachRoot（保持串行不变式）
- 落盘槽从「存一份」改成「每个根一份」→ 拔掉一个卷只重写它自己那份，
  \$HOME 那份不用动
- 删掉 v2 那套 store 函数（没有调用方了）
- 顺手修 M3 合并时带回来的旧名（sextant-persist / [sextant] 前缀）"
```

---

## Task 4: 卷枚举

**Files:**
- Create: `app/src-tauri/src/volumes.rs`
- Modify: `app/src-tauri/Cargo.toml`（加 `libc`）
- Modify: `app/src-tauri/src/lib.rs`（`mod volumes;`）
- Test: `app/src-tauri/src/volumes.rs`（内联单元测试）

**Interfaces:**
- Consumes: 无（纯枚举，不依赖引擎）
- Produces:
  ```rust
  // src/volumes.rs
  pub fn mounted_local_volumes() -> Vec<PathBuf>;
  pub fn filter_local_volumes(mounts: &[(PathBuf, u32)]) -> Vec<PathBuf>;   // 纯函数，可测
  ```
  （Task 5 用它们）

- [ ] **Step 1: 加依赖**

`app/src-tauri/Cargo.toml` 的 `[dependencies]` 里加一行：

```toml
# 【为什么加它】枚举挂载点要用 `getmntinfo`（macOS 的标准系统调用）。
# `libc` 已经在 lock 文件里（search-core 用了）→ **不新增下载**。
libc = "0.2"
```

- [ ] **Step 2: 写失败的测试**

新建 `app/src-tauri/src/volumes.rs`：

```rust
//! 外接卷的枚举与监控 —— **插上 U 盘就能搜到里面的文件**。
//!
//! 【为什么用 `getmntinfo` 而不是别的】
//!   macOS 的外接盘挂在 `/Volumes/<卷名>`。系统调用 `getmntinfo` 能一次列出
//!   **所有**挂载点 + 它们的标志位 —— 一次调用、零依赖、不需要回调。
//!   （`NSFileManager` 要 objc 绑定；`Disk Arbitration` 是回调式的，
//!   要挂 RunLoop —— 对「每秒轮询」这个形态都不划算。）

use std::path::{Path, PathBuf};

/// 挂载点的标志位：本地磁盘（网络盘 smbfs/nfs **没有**这个位）。
///
/// 【为什么靠它排除网络盘】
///   网络共享也挂在 `/Volumes/` 下（`/Volumes/公司共享`）——
///   用户说的是「插硬盘」，网络盘不在范围内，而且网络盘扫描慢、容易断。
const MNT_LOCAL: u32 = libc::MNT_LOCAL as u32;

/// 当前挂载的**本地外接卷**。
///
/// 【判据（三条全中才算）】
///   1. `MNT_LOCAL` —— 本地磁盘（排除 smbfs / nfs / afpfs / webdav）
///   2. 挂载点是 `/Volumes` 的**直接子目录**（排除 `/` 与 `/System/Volumes/*`）
///   3. 不是 `/Volumes/Macintosh HD` 那个指向 `/` 的符号链接
pub fn mounted_local_volumes() -> Vec<PathBuf> {
    // 【getmntinfo 的两段式调用】第一次只问「有多少个」，
    // 第二次才把内容填进我们给的数组里。
    unsafe {
        let count = libc::getmntinfo(std::ptr::null_mut(), libc::MNT_NOWAIT);
        if count <= 0 {
            return Vec::new();
        }
        let mut buf = vec![std::mem::zeroed::<libc::statfs>(); count as usize];
        let count = libc::getmntinfo(buf.as_mut_ptr() as *mut *mut libc::statfs, libc::MNT_NOWAIT);
        if count <= 0 {
            return Vec::new();
        }
        let mounts: Vec<(PathBuf, u32)> = buf[..count as usize]
            .iter()
            .map(|s| {
                // 【为什么用 `symlink_metadata` 的等价物】这里要的是挂载点**本身**，
                // 不是它指向的东西 —— 用 CStr 读原始字节。
                let name = std::ffi::CStr::from_ptr(s.f_mntonname.as_ptr());
                (
                    PathBuf::from(name.to_string_lossy().into_owned()),
                    s.f_flags,
                )
            })
            .collect();
        filter_local_volumes(&mounts)
    }
}

/// 从挂载点里挑出「本地外接卷」—— **纯函数**（好测）。
pub fn filter_local_volumes(mounts: &[(PathBuf, u32)]) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = mounts
        .iter()
        .filter(|(_, flags)| flags & MNT_LOCAL != 0)
        .filter(|(p, _)| p.parent() == Some(Path::new("/Volumes")))
        .map(|(p, _)| p.clone())
        .collect();
    out.sort();
    out.dedup();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_root_volume_is_not_an_external_volume() {
        let mounts = vec![(PathBuf::from("/"), MNT_LOCAL)];
        assert!(filter_local_volumes(&mounts).is_empty(), "根卷不是外接盘");
    }

    #[test]
    fn system_volumes_are_not_external_volumes() {
        let mounts = vec![
            (PathBuf::from("/System/Volumes/Data"), MNT_LOCAL),
            (PathBuf::from("/System/Volumes/Preboot"), MNT_LOCAL),
        ];
        assert!(filter_local_volumes(&mounts).is_empty());
    }

    #[test]
    fn a_usb_stick_under_volumes_is_picked_up() {
        let mounts = vec![(PathBuf::from("/Volumes/U盘"), MNT_LOCAL)];
        assert_eq!(
            filter_local_volumes(&mounts),
            vec![PathBuf::from("/Volumes/U盘")]
        );
    }

    #[test]
    fn network_shares_are_skipped() {
        // 网络盘的标志位里**没有** MNT_LOCAL
        let mounts = vec![(PathBuf::from("/Volumes/公司共享"), 0)];
        assert!(filter_local_volumes(&mounts).is_empty(), "网络盘不自动索引");
    }

    #[test]
    fn nested_paths_under_volumes_are_not_picked_up() {
        let mounts = vec![(PathBuf::from("/Volumes/U盘/子目录"), MNT_LOCAL)];
        assert!(
            filter_local_volumes(&mounts).is_empty(),
            "只认 /Volumes 的直接子目录"
        );
    }

    #[test]
    fn the_result_is_sorted_and_deduplicated() {
        let mounts = vec![
            (PathBuf::from("/Volumes/B"), MNT_LOCAL),
            (PathBuf::from("/Volumes/A"), MNT_LOCAL),
            (PathBuf::from("/Volumes/A"), MNT_LOCAL),
        ];
        assert_eq!(
            filter_local_volumes(&mounts),
            vec![PathBuf::from("/Volumes/A"), PathBuf::from("/Volumes/B")]
        );
    }
}
```

- [ ] **Step 3: 跑测试确认它失败**

```bash
cd app/src-tauri && cargo test -p search-core volumes 2>&1 | tail -10
cd app/src-tauri && cargo test --lib volumes 2>&1 | tail -20
```

Expected: 第一次报「没有 search-core 里的 volumes」（跳过），第二次编译错误 ——
`could not find libc in dependencies`（还没加依赖）或 `unresolved import`。

- [ ] **Step 4: 加 `mod volumes;`**

`app/src-tauri/src/lib.rs` 的模块声明区（`mod watcher;` 那一排）加一行：

```rust
mod volumes;
```

- [ ] **Step 5: 跑测试确认通过**

```bash
cd app/src-tauri && cargo test --lib volumes 2>&1 | tail -15
```

Expected: 6 条全绿。

⚠️ `libc::statfs` 的 `f_flags` 字段类型在 macOS 上是 `u32`，
`f_mntonname` 是 `[c_char; 1024]` —— 上面代码里的 `s.f_mntonname.as_ptr()` 依赖这个布局。
若编译报类型不匹配 → 用 `s.f_mntonname.as_ptr() as *const std::os::raw::c_char` 显式转一下。

- [ ] **Step 6: 提交**

```bash
git add app/src-tauri/src/volumes.rs app/src-tauri/src/lib.rs app/src-tauri/Cargo.toml app/src-tauri/Cargo.lock
git commit -m "feat(volumes): 用 getmntinfo 枚举本地外接卷

- 判据三条：MNT_LOCAL（排除网络盘）+ /Volumes 的直接子目录 + 不是符号链接
- 纯函数 filter_local_volumes 拆出来 → 6 条单测覆盖各种挂载表
- libc 已在 lock 文件里（search-core 用了）→ 不新增下载"
```

---

## Task 5: 监控线程与附加/分离编排

**Files:**
- Modify: `app/src-tauri/src/volumes.rs`
- Modify: `app/src-tauri/src/settings.rs`（索引档改成目录 + 重建引擎时带上卷）
- Modify: `app/src-tauri/src/index_state.rs`（卷状态）
- Modify: `app/src-tauri/src/lib.rs`（`setup()` 里起监控线程）
- Test: `app/src-tauri/src/volumes.rs`（内联单元测试）+ `app/src-tauri/tests/volumes.rs`（集成）

**Interfaces:**
- Consumes: Task 3 的 `Searcher::attach_root` / `detach_root`；Task 4 的 `mounted_local_volumes`
- Produces:
  ```rust
  // src/volumes.rs
  pub struct VolumeStatus { pub path: String, pub label: String, pub phase: VolumePhase, pub indexed: u32 }
  pub enum VolumePhase { Indexing, Ready, Failed(String) }
  pub struct VolumeState { /* 内部 Mutex<Vec<VolumeStatus>> */ }
  impl VolumeState { pub fn snapshot(&self) -> Vec<VolumeStatus>; }
  pub fn diff_volumes(known: &HashSet<PathBuf>, now: &HashSet<PathBuf>) -> (Vec<PathBuf>, Vec<PathBuf>);
  pub fn spawn_volume_monitor<R: Runtime>(app: AppHandle<R>);
  ```

- [ ] **Step 1: 写失败的测试（纯函数部分）**

追加到 `app/src-tauri/src/volumes.rs` 的 `mod tests` 里：

```rust
    #[test]
    fn newly_mounted_volumes_are_detected() {
        let known: HashSet<PathBuf> = HashSet::new();
        let now: HashSet<PathBuf> = [PathBuf::from("/Volumes/U盘")].into_iter().collect();
        let (appeared, disappeared) = diff_volumes(&known, &now);
        assert_eq!(appeared, vec![PathBuf::from("/Volumes/U盘")]);
        assert!(disappeared.is_empty());
    }

    #[test]
    fn unplugged_volumes_are_detected() {
        let known: HashSet<PathBuf> = [PathBuf::from("/Volumes/U盘")].into_iter().collect();
        let now: HashSet<PathBuf> = HashSet::new();
        let (appeared, disappeared) = diff_volumes(&known, &now);
        assert!(appeared.is_empty());
        assert_eq!(disappeared, vec![PathBuf::from("/Volumes/U盘")]);
    }

    #[test]
    fn an_unchanged_set_produces_nothing() {
        let s: HashSet<PathBuf> = [PathBuf::from("/Volumes/U盘")].into_iter().collect();
        let (appeared, disappeared) = diff_volumes(&s, &s);
        assert!(appeared.is_empty() && disappeared.is_empty());
    }
```

- [ ] **Step 2: 跑测试确认它失败**

```bash
cd app/src-tauri && cargo test --lib volumes 2>&1 | tail -15
```

Expected: 编译错误 —— `cannot find function diff_volumes`。

- [ ] **Step 3: 实现 —— 卷状态与差分**

在 `app/src-tauri/src/volumes.rs` 里加：

```rust
use std::collections::HashSet;
use std::sync::Mutex;
use std::time::Duration;
use tauri::{AppHandle, Manager, Runtime};

/// 每隔多久枚举一次挂载点。
const POLL_INTERVAL: Duration = Duration::from_secs(1);

/// 一个卷现在的状态。
#[derive(Debug, Clone, serde::Serialize)]
pub struct VolumeStatus {
    /// 挂载点路径（如 `/Volumes/U盘`）。
    pub path: String,
    /// 卷名（挂载点的最后一段）。
    pub label: String,
    /// 处在哪一步。
    pub phase: VolumePhase,
    /// 已经索引进去多少条。
    pub indexed: u32,
}

/// 卷的状态机。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "state", content = "detail")]
pub enum VolumePhase {
    /// 正在扫（第一次插上，或插回来后的后台刷新）。
    Indexing,
    /// 就绪。
    Ready,
    /// 出错了（reason 是原因）。
    Failed(String),
}

/// 卷状态 —— Tauri 管理的全局状态。
///
/// 【为什么单独一个状态而不是塞进 `IndexState`】
///   `IndexState` 讲的是「引擎索引了什么」；
///   这里讲的是「哪些外接卷插着、扫到哪一步了」—— 两件事，分开放。
#[derive(Default)]
pub struct VolumeState {
    inner: Mutex<Vec<VolumeStatus>>,
}

impl VolumeState {
    /// 当前所有卷的状态（给状态栏看）。
    pub fn snapshot(&self) -> Vec<VolumeStatus> {
        self.inner.lock().unwrap().clone()
    }

    fn set(&self, path: &Path, phase: VolumePhase, indexed: u32) {
        let mut g = self.inner.lock().unwrap();
        let label = path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        match g.iter_mut().find(|v| v.path == path.to_string_lossy()) {
            Some(v) => {
                v.phase = phase;
                v.indexed = indexed;
            }
            None => g.push(VolumeStatus {
                path: path.to_string_lossy().into_owned(),
                label,
                phase,
                indexed,
            }),
        }
    }

    fn remove(&self, path: &Path) {
        let mut g = self.inner.lock().unwrap();
        g.retain(|v| v.path != path.to_string_lossy());
    }
}

/// 比较「上次知道的」与「现在的」→ (新出现的, 消失的)。
///
/// 【为什么差分而不是直接对着当前集合做】
///   差分能明确回答两件事：**谁来了**（要附加）、**谁走了**（要分离）。
///   直接对当前集合做的话，「走了」这件事就没人管了。
pub fn diff_volumes(
    known: &HashSet<PathBuf>,
    now: &HashSet<PathBuf>,
) -> (Vec<PathBuf>, Vec<PathBuf>) {
    let mut appeared: Vec<PathBuf> = now.difference(known).cloned().collect();
    let mut disappeared: Vec<PathBuf> = known.difference(now).cloned().collect();
    appeared.sort();
    disappeared.sort();
    (appeared, disappeared)
}
```

- [ ] **Step 4: 实现 —— 附加/分离的编排**

```rust
/// 起监控线程：每秒枚举一次挂载点，驱动附加/分离。
pub fn spawn_volume_monitor<R: Runtime>(app: AppHandle<R>) {
    let spawned = std::thread::Builder::new()
        .name("lodestar-volumes".into())
        .spawn(move || {
            let mut known: HashSet<PathBuf> = HashSet::new();
            loop {
                std::thread::sleep(POLL_INTERVAL);
                let now: HashSet<PathBuf> = mounted_local_volumes().into_iter().collect();
                let (appeared, disappeared) = diff_volumes(&known, &now);
                for v in disappeared {
                    detach_volume(&app, &v);
                    known.remove(&v);
                }
                for v in appeared {
                    attach_volume(&app, &v);
                    known.insert(v);
                }
                // 【卷被改名了会怎样】
                //   macOS 改名 = 卸载 + 以新名字挂载 → 这里看到「旧的消失 + 新的出现」
                //   → 旧的那个被分离、新的那个当**新盘**处理（没有档 → 全量扫）。
                //   旧盘留下的档会变成孤儿文件（**暂不清理**，见设计文档 §8）。
            }
        });
    if let Err(e) = spawned {
        eprintln!("[lodestar] 起卷监控线程失败，外接盘不会被索引: {e}");
    }
}

/// 把一个卷从索引里摘掉。
fn detach_volume<R: Runtime>(app: &AppHandle<R>, volume: &Path) {
    let searcher = match app.state::<std::sync::Mutex<Option<std::sync::Arc<search_core::Searcher>>>>().inner() {
        Ok(s) => s.clone(),
        Err(_) => return,
    };
    let Some(searcher) = searcher else { return };
    match searcher.detach_root(volume) {
        Ok(indexed) => {
            crate::logging::log_line(&format!("[volumes] 已摘掉 {}（剩 {indexed} 项）", volume.display()));
            app.state::<VolumeState>().remove(volume);
            // 路径也要从状态里去掉 —— 否则 watcher 会继续监听已消失的路径
            remove_path(app, volume);
            // 索引变了 → 前端要重搜
            notify_frontend(app);
        }
        Err(e) => eprintln!("[lodestar] 摘掉卷失败 {}: {e}", volume.display()),
    }
}

/// 把一个卷加进索引。
///
/// 【为什么要开一个自己的线程】
///   第一次插上要**全量扫**这个卷（可能几十万文件 → 几十秒）。
///   那个扫描绝不能占着监控线程（否则拔盘都检测不到），
///   更不能占着搜索 worker（否则搜索全堵死）。
fn attach_volume<R: Runtime>(app: &AppHandle<R>, volume: &Path) {
    let app = app.clone();
    let volume = volume.to_path_buf();
    let spawned = std::thread::Builder::new()
        .name("lodestar-volume-attach".into())
        .spawn(move || {
            app.state::<VolumeState>().set(&volume, VolumePhase::Indexing, 0);
            match scan_volume(&app, &volume) {
                Ok(entries) => {
                    let Some(searcher) = current_searcher(&app) else { return };
                    // 【拔盘竞态】扫的这几十秒里用户可能把盘拔了 →
                    //   复查一次，没挂载就丢弃结果（不把死盘的条目塞进索引）。
                    if !mounted_local_volumes().contains(&volume) {
                        crate::logging::log_line(&format!("[volumes] {} 扫描期间被拔掉，丢弃", volume.display()));
                        app.state::<VolumeState>().remove(&volume);
                        return;
                    }
                    match searcher.attach_root(&volume, entries) {
                        Ok(indexed) => {
                            crate::logging::log_line(&format!("[volumes] 已挂上 {}（{indexed} 项）", volume.display()));
                            app.state::<VolumeState>().set(&volume, VolumePhase::Ready, indexed);
                            commit_paths(&app, &volume, indexed);
                            notify_frontend(app);
                        }
                        Err(e) => {
                            app.state::<VolumeState>()
                                .set(&volume, VolumePhase::Failed(e.to_string()), 0);
                        }
                    }
                }
                Err(e) => {
                    app.state::<VolumeState>()
                        .set(&volume, VolumePhase::Failed(e.to_string()), 0);
                }
            }
        });
    if let Err(e) = spawned {
        eprintln!("[lodestar] 起卷附加线程失败: {e}");
    }
}
```

再加三个小helper：

```rust
/// 拿当前活着的引擎句柄。
fn current_searcher<R: Runtime>(app: &AppHandle<R>) -> Option<std::sync::Arc<search_core::Searcher>> {
    app.state::<std::sync::Mutex<Option<std::sync::Arc<search_core::Searcher>>>>()
        .inner()
        .ok()
        .and_then(|s| s.clone())
}

/// 扫一个卷的全部条目。
///
/// 【为什么要先读档】插回来的盘**上次扫过** → 档还在 → 读档就是毫秒级，
/// 用户立刻能搜到；然后再后台刷一遍（见 `refresh_volume`）。
fn scan_volume<R: Runtime>(app: &AppHandle<R>, volume: &Path) -> Result<Vec<search_core::IndexEntry>, String> {
    let store_dir = app.path().app_config_dir().ok().map(|d| d.join("index"));
    if let Some(dir) = &store_dir {
        if let Some(entries) = search_core::store::load_root(dir, volume) {
            crate::logging::log_line(&format!("[volumes] {} 命中旧索引档，秒开", volume.display()));
            return Ok(entries);
        }
    }
    search_core::scan::scan(&[volume.to_path_buf()]).map_err(|e| e.to_string())
}

/// 把「引擎索引了哪些目录」更新到状态里（卷加进来了）。
fn commit_paths<R: Runtime>(app: &AppHandle<R>, volume: &Path, indexed: u32) {
    let state = app.state::<crate::index_state::IndexState>();
    let (gen, mut paths) = state.engine_snapshot();
    if !paths.iter().any(|p| p == &volume.to_string_lossy().to_string()) {
        paths.push(volume.to_string_lossy().into_owned());
    }
    // 【条件提交】期间要是换过引擎 → 这次结果描述的是旧引擎 → 丢弃
    state.commit_if_current(gen, paths, indexed);
}

/// 把一个目录从「引擎索引了哪些目录」里去掉（卷拔掉了）。
///
/// 【为什么必须去掉】
///   `watcher::desired_watch_dirs` 读的就是这份列表 ——
///   不去掉的话 watcher 会**继续监听一个已经不存在的路径**，
///   每秒对账一次都会失败一次（还会在日志里刷屏）。
fn remove_path<R: Runtime>(app: &AppHandle<R>, volume: &Path) {
    let state = app.state::<crate::index_state::IndexState>();
    let (gen, mut paths) = state.engine_snapshot();
    paths.retain(|p| p != &volume.to_string_lossy().to_string());
    let indexed = state.snapshot().indexed;
    state.commit_if_current(gen, paths, indexed);
}

/// 通知前端「索引变了，重搜一次」。
fn notify_frontend<R: Runtime>(app: &AppHandle<R>) {
    use tauri::Emitter;
    let mut st = app.state::<crate::index_state::IndexState>().snapshot();
    st.visible = crate::lifecycle::window_visible(app);
    st.volumes = app.state::<VolumeState>().snapshot();
    if let Err(e) = app.emit(crate::volumes::VOLUMES_CHANGED_EVENT, st) {
        eprintln!("[lodestar] 通知前端失败: {e}");
    }
}
```

⚠️ **`search_core::store` 与 `search_core::scan` 要是 `pub mod`** ——
`search-core/src/lib.rs` 里已经是 `pub mod scan;` / `pub mod store;` ✓
但 `IndexEntry` 要从 `search_core::types::IndexEntry` 导出 ——
`lib.rs` 里有 `pub use types::{..., IndexEntry, ...}` ✓

- [ ] **Step 5: 索引档改成目录 + 重建引擎时带上卷**

`app/src-tauri/src/settings.rs` 的 `build_engine`：

```rust
    let store_dir = app
        .path()
        .app_config_dir()
        .ok()
        .map(|dir| dir.join("index"));
    let (engine, indexed) =
        search_core::Engine::new_with_store(&refs, store_dir).map_err(|e| e.to_string())?;
```

`rebuild_engine`（用户手动换索引目录时）要把**已挂载的卷**也带上，
否则换一次目录就把 U 盘踢出索引了：

```rust
pub fn rebuild_engine<R: Runtime>(
    app: &AppHandle<R>,
    paths: &[String],
) -> Result<u32, String> {
    let mut resolved = effective_paths(paths);
    // 【已挂载的卷要一起带上】换索引目录不该把 U 盘踢出索引
    for v in crate::volumes::mounted_local_volumes() {
        let s = v.to_string_lossy().into_owned();
        if !resolved.contains(&s) {
            resolved.push(s);
        }
    }
    ...
}
```

⚠️ `rebuild_engine` 里后面那几行用的是 `resolved`（`build_engine(app, &resolved)` 与
`index_state.commit_new_engine(resolved, indexed)`）→ 变量名保持叫 `resolved` 就不用改其他行，
但它现在得是 `mut`。

- [ ] **Step 6: `setup()` 里起监控线程**

`app/src-tauri/src/lib.rs` 的 `setup` 里，`watcher::spawn_watcher(app.handle());` 那一行旁边加：

```rust
            // 外接卷监控（插上 U 盘就把它加进索引）。
            crate::volumes::spawn_volume_monitor(app.handle());
```

并且要在 `setup` 里注册状态（这样 `app.state::<VolumeState>()` 才拿得到）：

```rust
        app.manage(crate::volumes::VolumeState::default());
```

（`manage` 要在 `setup` 的开头，与其他 `manage` 调用放一起。）

- [ ] **Step 7: 跑测试确认通过**

```bash
cd app/src-tauri && cargo test --lib volumes 2>&1 | tail -15
cd app/src-tauri && cargo test 2>&1 | tail -15
```

Expected: 全绿。

- [ ] **Step 8: 提交**

```bash
git add app/src-tauri/src/volumes.rs app/src-tauri/src/settings.rs app/src-tauri/src/index_state.rs app/src-tauri/src/lib.rs
git commit -m "feat(volumes): 监控线程 + 附加/分离编排

- 每秒枚举一次挂载点 → 差分出「新出现的」与「消失的」
- 附加：先读档（插回来的盘秒开）→ 没有档就全量扫
  ⚠️ 扫描在**自己的线程**上做 —— 几十万文件的盘要扫几十秒，
  绝不能占着监控线程（拔盘都检测不到）或搜索 worker（搜索全堵死）
- 拔盘竞态：扫完复查挂载表，没挂载就丢弃结果
- 分离：条目立即消失，**档留着**（插回来要复用）
- 索引档从「一个文件」改成「一个目录」
- 重建引擎时带上已挂载的卷 —— 换索引目录不该把 U 盘踢出去"
```

---

## Task 6: 状态与前端

**Files:**
- Modify: `app/src-tauri/src/commands.rs`（`IndexStatus.volumes`）
- Modify: `app/src-tauri/src/index_state.rs`（`snapshot()` 填占位值）
- Modify: `app/src-tauri/src/event_names.rs`（加事件名 + 数量 7 → 8）
- Modify: `app/src-tauri/src/volumes.rs`（加 `VOLUMES_CHANGED_EVENT` 常量）
- Modify: `app/src/lib/types.ts`（`IndexStatus.volumes`）
- Modify: `app/src/App.vue`（监听事件 → 重搜）
- Modify: `app/src/components/StatusBar.vue`（显示卷状态）
- Test: `app/src/components/StatusBar.test.ts`（已有文件，追加）

**Interfaces:**
- Consumes: Task 5 的 `VolumeState::snapshot()` / `VolumeStatus`
- Produces: 前端能看见「插了哪些卷、扫到哪一步」，并在卷变化时自动重搜

- [ ] **Step 1: Rust 侧 —— 状态加字段**

`app/src-tauri/src/commands.rs` 的 `IndexStatus`：

```rust
    /// 当前插着的**外接卷**及各自的状态。
    ///
    /// 【为什么是可选的】和 `visible` 一样：**填不出来的调用点一律给空列表** ——
    ///   缺省 = 「没有卷」= 状态栏不显示卷那一行（不是故障）。
    pub volumes: Vec<crate::volumes::VolumeStatus>,
```

`app/src-tauri/src/index_state.rs` 的 `snapshot()` 里加一行：

```rust
            // 【占位值】`IndexState` 不知道卷的事（那是 `VolumeState` 的职责）
            //   → 这里给空列表，由 `commands::index_status` 与 `volumes::notify_frontend`
            //   覆盖成真实值。
            volumes: Vec::new(),
```

`app/src-tauri/src/commands.rs` 的 `index_status` 命令里覆盖它：

```rust
    st.volumes = app.state::<crate::volumes::VolumeState>().snapshot();
```

⚠️ `commands.rs` 里**每一个**返回 `IndexStatus` 的地方都要检查一遍
（`index_status` / `rescan` / `reindex` / `watcher::auto_rescan`）→ 凡是返回给前端的，
都补上 `st.volumes = ...`（漏一个 → 那个路径上卷的状态是空的，状态栏不显示）。

- [ ] **Step 2: Rust 侧 —— 新事件**

`app/src-tauri/src/volumes.rs` 顶部加：

```rust
/// 「外接卷变化了」事件 —— 载荷是 `IndexStatus`。
///
/// 【为什么需要它】
///   前端靠 800ms 轮询里的「scanning 从 true 翻到 false」来触发重搜。
///   卷的附加/分离**不会**让 `scanning` 翻转 → 轮询撞不出来 →
///   结果表里留着已经拔掉的盘的条目。
///   → 必须显式发事件。
pub const VOLUMES_CHANGED_EVENT: &str = "index-volumes-changed";
```

`app/src-tauri/src/event_names.rs`：

```rust
pub const EVENT_NAMES: &[&str] = &[
    crate::lifecycle::AWAKE_EVENT,
    crate::lifecycle::OPEN_SETTINGS_EVENT,
    crate::lifecycle::VISIBILITY_EVENT,
    crate::watcher::AUTO_REFRESH_EVENT,
    crate::volumes::VOLUMES_CHANGED_EVENT,   // ← 新增
    crate::OPEN_USER_GUIDE_EVENT,
    crate::OPEN_SYNTAX_CHEATSHEET_EVENT,
    crate::OPEN_CREDITS_EVENT,
];
```

**并且把数量断言从 7 改成 8**：

```rust
        assert_eq!(
            EVENT_NAMES.len(),
            8,
            "事件表少了一条或多了一条 —— \
             新增 Rust→前端 的事件时，记得把它加进 EVENT_NAMES 并更新这个数字"
        );
```

⚠️ 这个文件里还有一条测试会**扫前端源码**，确认「表里的每个事件名前端都在听」
→ 前端没加 `listen` 之前，这条测试会红 → 所以 Step 3 与 Step 2 必须一起做。

- [ ] **Step 3: 前端 —— 类型**

`app/src/lib/types.ts`：

```ts
/** 一个外接卷的状态。 */
export interface VolumeStatus {
  /** 挂载点路径（如 `/Volumes/U盘`）。 */
  path: string;
  /** 卷名。 */
  label: string;
  /** 处在哪一步。 */
  phase:
    | { state: "Indexing" }
    | { state: "Ready" }
    | { state: "Failed"; detail: string };
  /** 已经索引进去多少条。 */
  indexed: number;
}
```

并在 `IndexStatus` 里加：

```ts
  /** 当前插着的**外接卷**及各自的状态。 */
  volumes?: VolumeStatus[];
```

- [ ] **Step 4: 前端 —— 监听事件 → 重搜**

`app/src/App.vue`：在别的 `listen(...)` 旁边加一个（用 `onUnmounted` 收尾，与现有的写法一致）：

```ts
/* 外接卷变化（插上 / 拔掉 / 扫完）→ 重搜一次。
 *
 *  【为什么不能靠轮询撞】
 *    前端靠「scanning 从 true 翻到 false」触发重搜，
 *    而卷的附加/分离**不会**让 scanning 翻转 → 撞不出来 →
 *    结果表里会留着已经拔掉的盘的条目。 */
const un = await listen("index-volumes-changed", (e) => {
  status.value = e.payload as IndexStatus;
  refreshKey.value += 1;
});
onUnmounted(() => un());
```

⚠️ `App.vue` 里 `listen` 的用法要照抄现有的（它们在 `onMounted` 里、`await` 后存进变量、
`onUnmounted` 里调 `un()`）→ **放在同一个 `onMounted` 里**。

- [ ] **Step 5: 前端 —— 状态栏显示卷**

`app/src/components/StatusBar.vue`：

```vue
<template>
  <footer class="statusbar">
    <span class="sb-left">{{ hitText }}</span>
    <span class="sb-right">
      <!-- 【外接卷】
           用户插了 U 盘就得看得见「盘上的文件进索引了没有」——
           首扫可能要几十秒，没有反馈的话用户会以为功能坏了。 -->
      <span
        v-for="v in status?.volumes ?? []"
        :key="v.path"
        class="sb-volume"
        :title="v.path"
        >{{ volumeText(v) }}</span
      >
      <span v-if="status?.scanning">正在扫描索引…</span>
      <span v-else-if="status">已索引 {{ status.indexed }} 项</span>
      ...
```

script 里加：

```ts
import type { IndexStatus, SearchResult, VolumeStatus } from "../lib/types";

/** 一个卷在状态栏上怎么写。 */
function volumeText(v: VolumeStatus): string {
  switch (v.phase.state) {
    case "Indexing":
      return `${v.label} · 正在索引…`;
    case "Failed":
      return `${v.label} · 索引失败`;
    default:
      return `${v.label} · ${v.indexed.toLocaleString()} 项`;
  }
}
```

样式（**只能用 `:root` 里已有的令牌**）：

```css
.sb-volume {
  color: var(--fg-3);
  margin-right: 8px;
}
.sb-volume:has(+ *) { /* 不用动 */ }
```

⚠️ 别写新颜色值 —— 用 `var(--fg-3)` / `var(--warn)` / `var(--danger)` 这些现成的。

- [ ] **Step 6: 前端 —— 写测试**

`app/src/components/StatusBar.test.ts` 末尾追加：

```ts
it("状态栏会显示外接卷的状态", async () => {
  const status: IndexStatus = {
    scanning: false,
    indexed: 120,
    paths: ["/Users/me"],
    error: null,
    visible: true,
    volumes: [
      { path: "/Volumes/U盘", label: "U盘", phase: { state: "Ready" }, indexed: 42 },
      { path: "/Volumes/移动硬盘", label: "移动硬盘", phase: { state: "Indexing" }, indexed: 0 },
    ],
  };
  const { container } = render(StatusBar, { props: { result: null, status, loading: false } });
  expect(container.textContent).toContain("U盘");
  expect(container.textContent).toContain("42");
  expect(container.textContent).toContain("正在索引");
});
```

（测试文件里已有的 `render` / `IndexStatus` 构造方式照抄现有的，别自己造。）

- [ ] **Step 7: 跑测试**

```bash
cd app/src-tauri && cargo test 2>&1 | tail -15
cd app && pnpm vitest run 2>&1 | tail -12
```

Expected: Rust 全绿（含事件表那条 —— 前端已经 `listen` 了 `index-volumes-changed`）；
前端全绿。

⚠️ Rust 的事件表测试会扫前端源码找 `listen("index-volumes-changed")` ——
**Step 4 没做完就 Step 7，那条测试会红**。

- [ ] **Step 8: 提交**

```bash
git add app/src-tauri/src/commands.rs app/src-tauri/src/index_state.rs app/src-tauri/src/event_names.rs app/src-tauri/src/volumes.rs app/src/lib/types.ts app/src/App.vue app/src/components/StatusBar.vue app/src/components/StatusBar.test.ts
git commit -m "feat(ui): 状态栏显示外接卷 + 卷变化自动重搜

- IndexStatus 加 volumes 字段（IndexState 填占位空列表，由调用点覆盖）
- 新事件 index-volumes-changed —— 卷的附加/分离**不会**让 scanning 翻转，
  靠轮询撞不出来 → 必须显式发事件
- 事件名加进 EVENT_NAMES，数量断言 7 → 8
- 状态栏显示「U盘 · 12,433 项」/「U盘 · 正在索引…」
- 前端监听该事件 → refreshKey++ → 重搜"
```

---

## 真机验收清单（M6）

**这些必须插着真 U 盘跑**，不能只在模拟环境里过。

| # | 场景 | 期望 | 怎么验 |
|---|---|---|---|
| 1 | 插入 U 盘（第一次） | 3 秒内状态栏出现「U盘 · 正在索引…」；扫完后搜得到盘上的文件 | 真插盘 + `log stream`/应用日志 |
| 2 | 拔掉 U 盘 | 2 秒内该卷的条目从结果里消失（**不留幽灵**） | 插着时搜一个盘上的独有文件名，拔掉后同一个查询应 0 命中 |
| 3 | 重新插入 | 1 秒内可搜到（旧索引复用）→ 随后后台刷新完成 | 日志里应出现「命中旧索引档，秒开」 |
| 4 | 插拔各一次 | **$HOME 没有发生全量重扫** | 日志里数「全量扫描」的次数，与不插盘时对比 |
| 5 | 插着 U 盘搜本机文件 | 延迟与不插时一致（不被卷的扫描拖累） | 搜 `readme` 记 `elapsed_ms` |
| 6 | 冷启动（U 盘已插） | 启动读档 ≤15ms | `open` 一下看日志 |
| 7 | 挂一个网络盘 | **不自动索引** | `mount_smbfs` 一个 → 状态栏不该出现它 |
| 8 | 拔盘状态下重启应用 | 正常启动、能搜本机文件，**不出现 "invalid argument"** | 拔盘 → 重启 → 看日志 |
| 9 | U 盘上有同名文件 | 结果里能按路径区分 | 盘上放一个与本机同名的文件 → 结果里两条路径都在 |

**打包验证**（Task 6 之后）：

```bash
bash app/scripts/bundle-app.sh
```

必须看到 `✓ 前端已嵌进二进制` 与 `✅ 全部自检通过`。

---

## 回滚

每个 Task 独立提交 → 任何一个 Task 出问题都可以单独回退：

```bash
git revert <commit>          # 回退某个 Task
git revert --no-commit 8463dd7..HEAD   # 回退从改名之后的所有
```

**没跑通之前不要动 `settings.rs` 的 `LEGACY_CONFIG_DIR_NAMES`** ——
那是设置目录迁移链（`com.sextant.macos` → `com.lodestar.macos`），动它会让用户的书签/快捷键读不到。
