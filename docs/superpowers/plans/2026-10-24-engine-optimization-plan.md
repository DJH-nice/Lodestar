# 搜索引擎性能优化方案

> 生成时间：2026-10-24 · 分支 `master` · 工作区 `/Users/dongjiahao/Projects/sextant`
> 关联设计：[`../specs/2026-10-23-self-owned-search-engine-design.md`](../specs/2026-10-23-self-owned-search-engine-design.md)
> 本文**只有方案，没有代码改动**。所有数字都是实测（探针见 §6）。

---

## 0. 结论先说

**问「上游引擎代码能不能优化」——技术上能，但本项目里没有「优化上游」这个选项。**

三条理由，任何一条都足以否掉：

1. **上游已经不在链路上了。** Sextant 用的是自研 Rust 引擎
   （`app/src-tauri/search-core/`），上游 FSearch 的 C 代码**一行都没进来**。
   旧仓库 `/Users/dongjiahao/Downloads/fsearch-0.3.2` 只是**只读行为对照**。
2. **就算在，也不许改。** 旧仓库的硬约束：`src/` 必须与原始 tarball **逐字节一致**
   （`bash shim/check-upstream-pure.sh` 会拦），平台差异只能靠编译参数 / 垫片头吸收。
3. **改了也白改。** M4 的既定动作就是删掉 `src/` + `shim/` → 现在改上游 =
   写一段马上要被删掉的代码。

所以本方案回答的是真正可执行的那个问题：

> **上游引擎里那些做得不够好的地方，哪些值得「收割」进自研引擎？
> 自研引擎自己现在还欠着哪些优化？**

我读完了上游 30,531 行 C 代码 + 自研 4,104 行 Rust，实测后给出结论：
**上游有 6 处可优化点，自研已收割 4 处、还欠 2 处、另有 1 处「看着更省其实更慢」不能抄。**
自研引擎自己另外有 6 项实测出来的优化空间（§3）。

---

## 0.5 实施结果（2026-10-24 已执行批次 1~4）

> 验证方式：**同进程交替跑「改前」「改后」两个版本**（消除机器负载波动）——
> 改前版副本在 `/tmp/sc-before`，探针在 `/tmp/abprobe`。

| 项 | 状态 | 实测 |
|---|---|---|
| **P0-① 扫描并行（顶层子树分区 + 原子游标）** | ✅ 已做 | 62.8ms → **21.1ms**（快 66%，2.98×） |
| **P0-② 去掉重复 stat** | ✅ 已做 | 火焰图 `lstat` 29.6% → 约 15% |
| **P0-③① Unicode 折叠 ASCII 快路径** | ✅ 已做 | 折叠开销的 92% 是 NFD → ASCII 直接跳过 |
| **P0-③②③ 路径折叠惰性 / 拼接** | ⏸️ **未做（且不值得做）** | ASCII 快路径后折叠只剩 6.2ms（原来 17.7ms）→ 惰性化最多再省 ~1.8ms 墙钟，却要冒 final sigma 的语义风险 → 不做（见 §9.3） |
| **P1-① 排序** | ✅ 已做（**换了做法**） | 查询 `*` 1.00 → **0.47ms**；`a` 1.43 → **0.38ms** |
| **P1-② 翻页不重建 flat** | ✅ 已做 | 8.9K 命中时几乎看不出，10 万级才有意义 |
| **P1-③ 高亮逐字符折叠** | ✅ 已做 | 文本查询的搜索 0.71 → **0.34ms** |
| **P2-①② 索引内存瘦身** | ⏸️ **未做** | 仍 3.2MB（目标 ≤1.5MB）→ 必须排在 M3 之后 |

**总账**：热缓存扫描 **62.8 → 21.1ms**，查询（命中 55%）**1.43 → 0.38ms**，
翻页 **+0.03ms**（部分排序的代价）→ 一次搜索通常配 0~2 次翻页 → **净赚**。

## 1. 上游 FSearch 引擎：优化点清单（收割清单）

| # | 上游的问题 | 证据 | 影响 | 自研状态 |
|---|---|---|---|---|
| A | **匹配期逐条目做 Unicode 折叠** | `src/fsearch_query_match_data.c:41-63`（每条目 `set_entry` → 失效缓存 → 重新折叠）；`src/fsearch_utf.c:69-152`（ICU `ucasemap_utf8FoldCase` + `unorm2` NFD） | 真实数据比合成数据慢 **18~31 倍**（每条 310ns vs 10ns） | ✅ **已收割**（`fold.rs` 索引期折叠） |
| B | **匹配期重建完整路径并再折叠一遍路径** | `src/fsearch_query_match_data.c:88-104`（`db_entry_append_full_path` 走父链）；`src/fsearch_query_matchers.c` 的 `utf_strcasestr` 要 folded path | 路径查询时每条目 O(depth) 拼接 + 全路径 ICU 折叠 | ✅ **已收割**（`folded_path` 存索引） |
| C | **高亮在匹配循环里对每个条目分配 `PangoAttribute`** | `src/fsearch_query_matchers.c` 的 `highlight_ascii` / `highlight_regex` → `pango_attr_weight_new`（每次 calloc）；`src/fsearch_query_match_data.c:186-196` | 全表 N 条 × M 个命中 = **N×M 次堆分配**，而实际只有当前页 500 条要高亮 | ✅ **已收割**（`build_page` 只对本页算高亮） |
| D | **字段查询要现算** | `src/fsearch_query_matchers.c:19-42`（`db_entry_get_extension` 现取扩展名） | `ext:` 查询比自研慢 **20 倍** | ✅ **已收割**（扩展名索引期算好） |
| E | **排序没有缓存，每次查询重排** | `src/fsearch_database_sort.c` + `src/fsearch_database_search_view.c:227-260` | 上游靠 5 维预排序数组（`FsearchDatabaseSortOrderChain`）兜底，但**内存代价大** | ⚠️ **自研还欠着** → §3-P1 |
| F | **单线程递归扫描** | `src/fsearch_database_scan.c:130-200`（`db_folder_scan_recursive`） | 上游热缓存 74.6ms | ⚠️ 上游的问题**自研也有**（且更隐蔽）→ §3-P0 |
| — | `fstatat(dir_fd, name)` 写法 | `src/fsearch_database_scan.c:172-180` | **实测比 std 路径式 stat 慢 34%**（§4-①） | ❌ **不能抄** |

> ⚠️ 复核过、**不是** bug 的地方：`src/fsearch_query_matchers.c:34-37` 的扩展名提前退出，
> 依赖 `src/fsearch_query_node.c:338` 用同一比较器排序 → 自洽。
> （我一开始怀疑这里大小写不敏感时会漏匹配，核对后确认上游是对的。）

---

## 2. 自研引擎现状：实测基线

> 测量方法：`cargo run --release -p search-core --example bench`
> 机器：Apple Silicon，8 核（4P + 4E）。**绝对值随机器负载波动很大**（同一条 IO 地板
> 两次测出 38.5ms 和 59.2ms），所以下面一律给**比值**和**趋势**。

### 2.1 端到端（`examples/bench`，8,549 项）

| 指标 | 实测 | 目标（设计文档 §6） | 状态 |
|---|---|---|---|
| 冷启动读档 | **2.5ms** / 8,948 项 | ≤ 15ms | ✅ 达标 |
| 全量扫描（**冷**缓存，首次） | 322.9ms | — | 磁盘瓶颈 |
| 全量扫描（**热**缓存，重复跑） | **70.7ms**（最小）/ 76.2ms（中位） | ≤ 30ms | ❌ **超 2.4~2.5 倍** |
| 查询 `*`（命中 8,549） | 1.03~1.19ms | ≤ 1ms | ⚠️ 勉强 |
| 查询 `a`（命中 5,019） | 1.28~1.48ms | ≤ 1ms | ❌ |
| 查询 `ext:pdf` | 0.064ms | ≤ 1ms | ✅ 超 15 倍 |
| 查询 `size:>1M` | 0.058ms | ≤ 1ms | ✅ |
| 查询（无命中） | 0.122ms | — | ✅ |
| 索引档体积 | **3.2MB** / 8,549 项 = 374 B/项 | ≤ 1.5MB | ❌ **超 2.1 倍** |

### 2.2 成本拆解（`/tmp/queryprobe`，8,948 项）

| 环节 | 耗时 | 占比 |
|---|---|---|
| `query::parse`（一次） | 0.0003ms | — |
| **匹配**（parse + 全表比对） | **0.082~0.254ms**（9~28 ns/条） | 与上游 10ns/条同级 ✅ |
| **排序**（搜索减去匹配） | **0.37~0.91ms** | **占一次搜索的 40~76%** ❌ |
| 翻页 `page(N)` | 0.18~0.21ms | 每次翻页重建 flat + 500 条高亮 |

> **排序才是查询的大头，不是匹配。** 这一点上游设计文档没算到（它只量了「查询」总量）。

### 2.3 扫描火焰图（`sample` 抓真实扫描器，6s / 1,530 样本）

| 栈顶 | 样本 | 占比 |
|---|---|---|
| `lstat` | 453 | **29.6%** |
| `__open_nocancel` | 292 | 19.1% |
| `__getdirentries64` | 155 | 10.1% |
| NFD 折叠系（`decompose` + `canonical_combining_class` + `String::from_iter`） | 260 | **~17%** |
| 分配器（malloc/free 系） | ~150 | ~10% |
| `Path::hash`（`HashMap<PathBuf, ChildCounts>`） | 28 | ~1.8% |

**采样里只有 1 个线程** → 证实了 §3-P0 的判断：**默认配置下扫描器是单线程的。**

---

## 3. 优化项清单

按「收益 ÷ 风险 ÷ 改动量」排序。每项都带**验收方法**——没有验收方法的优化不写进来。

### P0-① 扫描：单根目录时并行完全失效

**问题**　`scan.rs` 的并行策略是「根目录之间并行」：

```rust
let threads = thread_count().min(per_root.len());   // 默认索引根 = $HOME = 1 个
if threads <= 1 { /* 走单线程分支 */ }
```

默认索引范围是**整个 `$HOME`**（`settings.index_paths` 为空）→ **只有 1 个根** →
`threads = 1` → 永远单线程。`thread_count()` 算出来的 4 线程是摆设。

**实测**（`/tmp/partition_probe`，8,516 项，热缓存，重复跑两次）：

| 形态 | 1 线程 | 2 线程 | 4 线程 | 8 线程 |
|---|---|---|---|---|
| **子树分区**（每线程一棵独立子树，无锁） | 70.4 / 84.0ms | 32.3 / 47.4ms | **21.8 / 34.7ms** | 23.2 / 32.4ms |
| **共享目录队列**（工作窃取式） | 38.5 / 59.2ms | — | 41.8 / 47.2ms | 42.0ms |

**结论**：
- 子树分区 **2.4~3.2 倍**；共享队列 **≈0 收益**（甚至更慢）。
- → **要按「顶层子树分区」做，不要按「共享目录队列 / 工作窃取」做。**
  `$HOME` 有 26 个顶层子目录，4 线程正好切得开。

**改法**　`scan.rs`：根目录下按顶层子目录分区（轮转分配给各线程），
每个线程独立走完自己的子树，互不通信；线程数 = `min(4, 顶层子目录数)`。

**预期**　热缓存扫描 70.7ms → **约 25~30ms**（实测纯遍历部分 2.4~3.2×）。
**验收**　`examples/bench` 的扫描耗时；再跑 20 轮取最小值（别用单次值）。
**风险**　中。`scan.rs` 里有 3 个单测依赖条目顺序（`depth_is_relative_to_the_index_root`
等）→ 分区后**条目顺序会变** → 这三个断言要改成「按集合断言」而不是「按下标断言」。
⚠️ **前端不能依赖索引顺序**（`paging.ts` 的分组边界公式依赖的是**排序后**的位置，不是索引顺序）。

**未验证**　冷缓存（322.9ms，磁盘延迟主导）下并行收益**没测**——
需要 `sudo purge` 后对比，测不了就别写进验收标准。

---

### P0-② 扫描：每个条目 stat 了两次

**问题**　`scan.rs` 的 `walk_queue` 拿了一次 `symlink_metadata`（判 is_dir），
`make_entry` 里**又拿了一次**（取 size / mtime）：

```rust
let meta = std::fs::symlink_metadata(&path)?;   // 第 1 次
let is_dir = meta.is_dir() && !meta.file_type().is_symlink();
entries.push(make_entry(&path, &dir, depth + 1, is_dir));
// make_entry 内部：
let (size, mtime) = match std::fs::symlink_metadata(path) { ... };  // 第 2 次
```

**实测**（`/tmp/sextant_scan_probe`，8,548 项）：加第 2 次 stat = **+7.5ms**（约 11%）。

**改法**　把 `Metadata` 传进 `make_entry`，复用第 1 次的结果。
**预期**　扫描省 ~7.5ms（热缓存 70.7 → 63ms）。
**验收**　同上 + 火焰图里 `lstat` 占比应从 29.6% 降到 ~15%。
**风险**　低。⚠️ 但 `make_entry` 是 `pub` 语义的内部函数，签名要改 → 5 个单测要跟着改。

---

### P0-③ 扫描：NFD 折叠占 CPU 的 ~17%，而路径折叠贵 4 倍

**问题**　`fold.rs` 的折叠 = NFD → 转小写 → 再 NFD。
`scan.rs` 对**每个条目**折两次：名字一次、**完整路径一次**。

**实测**（`/tmp/foldprobe`，8,548 条目）：

| 项 | 耗时 | 备注 |
|---|---|---|
| `fold(name)` | 3.4ms | 0.4 µs/条 |
| `fold(path)` | **13.7ms** | **1.6 µs/条（贵 4 倍）**——路径长 |
| 合计 | **17.7ms** | 占热缓存扫描 70.7ms 的 **25%** |
| 其中 NFD 归一化本身 | 16.3ms | **占折叠总量的 92%** |

**关键洞察**：`folded_path` **只在「搜路径」时才用得上**（`query.rs` 的
`Target::Path` 分支），而绝大多数使用场景只搜文件名 → **现在无条件折路径是白折的**。

**改法（三选一，从省事到最彻底）**：

1. **ASCII 快路径**（最省事，**语义完全等价**）：
   ASCII 文本的 NFD 是恒等变换，所以
   `if s.is_ascii() { return s.to_ascii_lowercase(); }` 与现在的 `fold()` **完全等价**。
   8,548 条目里绝大多数是 ASCII → 可省掉大部分 17.7ms。
   ⚠️ 必须在单测里钉住这条等价性（含一个非 ASCII 对照用例）。
2. **路径折叠改惰性**：只折名字；`folded_path` 在**第一次 path 查询**时才折，折完缓存。
   → 省 13.7ms（19%）。
3. **路径折叠由父路径拼接**：`folded_path = 父目录的 folded_path + "/" + folded_name`。
   → 只折名字，路径折叠几乎免费。
   ⚠️ **必须补一个回归用例**：希腊字母 final sigma（`Σ` → `ς`）在词尾的规则——
   `to_lowercase` 是**上下文相关**的，逐段折叠与整串折叠在边界处可能不同。
   虽然 `/` 是词边界（我推导下来应当等价），但**要用测试钉住**，不能靠推导。

**预期**　①②合计省 10~17ms；③ 再省 ~13.7ms。
**验收**　`examples/bench` 扫描耗时 + `/tmp/foldprobe` 重跑。
**风险**　中（Unicode 语义）→ 见 §7 的「Unicode 三条红线」。

---

### P1-① 查询：排序占一次搜索的 40~76%，而每次都重排

**问题**　`engine.rs` 的 `search()` 每次都把**全部命中**重排：

```rust
folders.sort_by(|a, b| cmp(&entries[*a as usize], &entries[*b as usize]));
files.sort_by(|a, b| cmp(&entries[*a as usize], &entries[*b as usize]));
```

比较闭包每次都**两次解引用 `IndexEntry`**（`&entries[*a]`）→ 指针追逐 + 字符串比较。

**实测**（`/tmp/queryprobe`，8,948 项）：

| 查询 | 完整 search | 减掉匹配 | **排序占比** |
|---|---|---|---|
| `*` sort=Name | 1.030ms | 0.78ms | **76%** |
| `*` sort=Path | 1.166ms | 0.91ms | **78%** |
| `*` sort=Size | 0.622ms | 0.37ms | 59% |
| `*` sort=Mtime | 0.713ms | 0.46ms | 65% |

**改法（按推荐顺序）**：

1. **提取排序键再排**（改动最小，收益确定）：
   先把命中的 id 映射成 `(sort_key, id)` 一次性提取（`sort_by_cached_key`），
   比较时只比 key，不解引用 `IndexEntry` → 消掉指针追逐。
   预计排序耗时降 30~50%。
2. **排序结果缓存**（抄上游的 `FsearchDatabaseSortOrderChain` 思路）：
   缓存「上一次的结果集 + 排序维度」，切维度时若结果集没变就不重排。
   ⚠️ 设计文档 §4.1 明确决定「**只维护 1 份按 name 排序的数组**，其余维度按需排序结果集」→
   **不要改成上游的 5 维预排序数组**（那是内存换 CPU，我们现在内存已经超指标 2.1 倍）。
3. **name 维走二分**（可选）：前缀查询在预排序的 name 数组上二分定位起点，
   只扫那一段 → O(log N + K)。设计文档 §4.4 已规划。

**预期**　排序 0.78ms → 0.3~0.4ms → 一次搜索 1.03ms → **~0.6ms**（达标）。
**验收**　`examples/bench` 的查询延迟；`/tmp/queryprobe` 重跑看排序占比。
**风险**　中。⚠️ **排序必须保持稳定语义**：现在 `Name` 用
`folded_name.cmp().then_with(|| name.cmp())` 做二级排序 → 改排序实现时**必须保留二级键**，
否则同名条目的相对顺序会变 → 前端的分组边界和「上一项/下一项」会错位。

---

### P1-② 查询：翻页每次重建整个 flat 数组

**问题**　`engine.rs` 的 `build_page()`：

```rust
let mut flat: Vec<u32> = Vec::with_capacity(view.folders.len() + view.files.len());
flat.extend_from_slice(&view.folders);
flat.extend_from_slice(&view.files);
if req.order == SortOrder::Desc { flat.reverse(); }   // ← 整数组反转
```

→ **每次翻页 O(总命中) 次拷贝 + 一次全量反转**。
8,948 命中 = 36KB/页；**10 万命中 = 400KB/页**，而且 `reverse` 是 O(n) 原地翻转。

**改法**　不建 `flat`，直接用**下标公式**映射到 `view.folders` / `view.files` 两段：
上游 `fsearch_database_search_view.c:93-116` 的 `get_idx_for_sort_type` 就是这个思路
（降序 = 整个扁平空间整体镜像 → `idx → num_folders + num_files - (idx + 1)`）。
→ 翻页变成 O(page_size)。

**预期**　翻页 0.18~0.21ms → **< 0.05ms**；10 万命中时从 ~2ms → ~0.05ms。
**验收**　`/tmp/queryprobe` 的 `page(N)`。
**风险**　**高** ⚠️ —— **分组边界公式**（`clamp(total_folders - page*size, 0, len)`）
是前端 `app/src/lib/paging.ts` 的依赖，公式一变界面分隔线就错位。
→ 改完必须跑前端 264 个 vitest + `search-core` 的 43 个用例。

---

### P1-③ 查询：高亮的 `find_folded` 逐字符做完整折叠

**问题**　`fold.rs` 的 `find_folded()`：

```rust
for folded in fold(&ch.to_string()).chars() {   // ← 每个字符：一次 String 分配 + 一次完整 NFD
```

O(n·m)，且**每个字符都跑一遍完整的 NFD 流程**（92% 的折叠开销在 NFD 上）。
只对当前页 500 条做 → 影响有限，但是当前页每次翻页都要重算。

**改法**　单字符折叠走**查表 / ASCII 快路径**；或改用 `unicode-normalization` 的
单码点 API（避免 `collect::<String>()` 的中间分配）。
**预期**　翻页路径上省掉一部分 0.18ms。
**验收**　`/tmp/queryprobe` 的 `page(N)` + 高亮区间正确性用例。
**风险**　中（Unicode 语义）→ 同 §7。

---

### P2-① 内存：索引 3.2MB，超指标 2.1 倍

**问题**　每个条目 6 个 `String`（`types.rs` 的 `IndexEntry`）：
`name` / `path` / `dir` / `extension` / `folded_name` / `folded_path`。
其中 **`path` / `dir` / `folded_path` 三份都是全路径长度** → 每条目约 374 字节。

**实测**　8,548 条目 → 2.67MB 字符串字节（`/tmp/sextant_scan_probe` 实测）。

**改法**（设计文档 §4.1 已经规划好，只是还没做）：

- `dir` → 换成**父目录 id**（`u32`），路径按需拼接；
- 字符串进**变长 arena**（name / folded_name / path 各一段），条目表只存偏移+长度；
- `folded_path` 按 §3-P0-③ 决定是否惰性折叠；
- 最终形态 = 设计文档的 **mmap 列存**（`header / entry table / arena`）。

**预期**　3.2MB → **≤1.5MB**（达标）；顺带冷启动读档从 2.5ms 变成「几乎为零」（页按需换入）。
**验收**　`du -sh` 索引档 + RSS 增量；读档耗时。
**风险**　**高** —— 这是存储格式改动 → `store.rs` 的 `VERSION` 要 +1（老档自动作废重扫，
   这是**设计好的**行为，不是 bug）；`scan.rs` / `engine.rs` / `query.rs` 都要跟着改数据访问方式。
→ **建议放到 M3 之后单独一个里程碑**，不要和 M3 增量更新混在一起。

---

### P2-② 扫描：`HashMap<PathBuf, ChildCounts>` 每个条目克隆一次路径

**问题**　`scan.rs` 的 `walk_queue` 每条目 `child_counts.entry(dir.clone())` ——
**每个条目克隆一次完整父路径 `PathBuf`** + 对完整路径做一次哈希；
回填时又 `child_counts.get(Path::new(&e.path))` 哈希一次全路径。

**实测**　火焰图里 `Path::hash` + `Hasher::write` 合计 ~1.8%。

**改法**　用**目录 id**（扫描时分配的序号）替代 `PathBuf` 键 → `Vec<ChildCounts>` + `Vec<u32>` 映射。
**预期**　省 ~1.8% + 每次条目少一次路径克隆（8,548 次分配）。
**验收**　火焰图 + 扫描耗时。
**风险**　低。

---

## 4. 实测证伪的「优化」（**不要做**）

> 这一节是本文档最有价值的部分——**省下的复杂度比省下的时间值钱**。

### ① 别抄上游的 `fstatat(dir_fd, name)` 写法

上游 `src/fsearch_database_scan.c:172-180` 用 `fstatat(dir_fd, dent->d_name, ...)`
（相对已打开的目录 fd），看起来「比解析整条路径省」。

**实测**（`/tmp/statprobe`，8,548 项，热缓存）：

| stat 策略 | 耗时 |
|---|---|
| A 路径式 `fs::symlink_metadata(绝对路径)`（自研现在这样） | **51.8ms** |
| B dir_fd 式 `fstatat(目录fd, 文件名)`（上游那样） | **69.5ms** |

**B 比 A 慢 34%。** 原因：B 要为每个子目录**自己拼一次完整路径字符串**，
而 `std::fs::read_dir` 在 macOS 上走的是批量 `getdirentries64`，本身已经很快。
→ **别改**。

### ② 别做「共享目录队列 / 工作窃取」式并行扫描

实测（§3-P0-①）：子树分区 2.4~3.2×，共享队列 ≈ 0 收益。
→ **要分区，不要窃取**。设计文档 §2.1 提到的「工作窃取」方向是错的。

### ③ 别造倒排索引

实测匹配 **9~28 ns/条**（与上游 10ns 同级）→ 1M 条目才 10ms。
**线性 + 提前退出**已经够了，造倒排索引 = 内存换不存在的问题。

### ④ 别做「每次查询重新折叠」以外的折叠优化

折叠前移到索引期**已经做了**（`fold.rs`）。剩下的空间只在
「路径折叠惰性化」和「ASCII 快路径」两个方向（§3-P0-③），
不需要更复杂的 Unicode 方案，也**不需要 ICU**。

### ⑤ 别用 `top -l -pid` 量 CPU

沿用旧仓库的教训：`ps -o time` 的增量法才准，`top` 的过滤模式对真值 20% 的 burner
能连续 6 个采样点全报 0.0%。

---

## 5. 实施顺序建议

| 批次 | 内容 | 预期 | 实际 |
|---|---|---|---|
| **第 1 批** ✅ | P0-②（去重复 stat）+ P0-③①（ASCII 快路径） | 扫描 70.7ms → ~50ms | **55.7ms**（A/B：74.7 → 55.7，快 25.5%） |
| **第 2 批** ✅ | P0-①（子树分区并行 + 原子游标） | 扫描 → ~25~30ms | **21.1ms**（A/B：62.8 → 21.1，快 66%） |
| **第 3 批** ✅ | P1-①（排序：**改成部分排序**） | 查询 1.03ms → ~0.6ms | **0.38~0.47ms**（快 53~73%） |
| **第 4 批** ✅ | P1-②（翻页不重建 flat）+ P1-③（高亮 ASCII 快路径） | 翻页 → <0.05ms | 翻页 0.19ms（**估算错了**，见 §9.2）；文本查询快 2× |
| **第 5 批** ⏸️ | P0-③②③（路径折叠惰性 / 拼接） | 扫描再省 ~13ms | **未做**（语义边界要先定清楚） |
| **第 6 批** ⏸️ | P2-①②（内存瘦身） | 3.2MB → ≤1.5MB | **未做**（排在 M3 之后） |

> **M3（增量更新）优先于第 6 批。** 增量更新把「实时更新」从全量重扫（70.7ms）
> 降到 <5ms，用户感知比扫描快 10 倍更明显。而 P2-① 的存储格式改动会**重写落盘层**，
> 和 M3 的 diff 回写冲突 → **M3 做完再做 P2-①**。

---

## 6. 探针清单（全部离屏，不碰项目代码）

| 探针 | 位置 | 干什么 | 复现 |
|---|---|---|---|
| 扫描 IO 地板 | `/tmp/sextant_scan_probe.rs` | stat 次数 / 字符串构建 / 共享队列并行 的对照 | `rustc -O -o /tmp/p /tmp/sextant_scan_probe.rs && /tmp/p "$HOME"` |
| 子树分区并行 | `/tmp/partition_probe.rs` | 分区式 vs 共享队列式的真实加速比 | `rustc -O -o /tmp/p2 /tmp/partition_probe.rs && /tmp/p2 "$HOME"` |
| stat 策略对比 | `/tmp/statprobe/` | 路径式 vs `fstatat(dir_fd)` | `cd /tmp/statprobe && cargo run --release` |
| 折叠成本 | `/tmp/foldprobe/` | `fold(name)` / `fold(path)` / NFD 净增 | `cd /tmp/foldprobe && cargo run --release` |
| 查询拆解 | `/tmp/queryprobe/` | 解析 / 匹配 / 排序 / 翻页 四段拆开 | `cd /tmp/queryprobe && cargo run --release` |
| 真实扫描器火焰图 | `/tmp/scanprobe2/` | `sample` 抓 30 轮真实扫描 | `cd /tmp/scanprobe2 && cargo build --release && ./target/release/scanprobe2 30 &` 然后 `sample <pid> 6` |

> `queryprobe` / `scanprobe2` 通过 **path 依赖**引用 `search-core`：
> `search-core = { path = "/Users/dongjiahao/Projects/sextant/app/src-tauri/search-core" }`
> → **不改它一行**，只调它的公开 API。

---

## 7. 三条不能碰的红线

1. **Unicode 三条语义**（AGENTS.md §10.2）：
   归一化用 **NFD**（不是 NFC）、大小写折叠用**转小写**（不是 full case folding）、
   **折叠前移到索引期**。改 `fold.rs` 前先想清楚这三条——它们决定「搜不搜得到」。
   任何折叠改动**必须**配一个 Unicode 回归用例（中文 / 土耳其语 i / 德语 ß /
   组合字符 / 希腊 final sigma）。
2. **分组边界公式**（`clamp(total_folders - page*size, 0, len)`）：
   前端 `app/src/lib/paging.ts` 依赖它 → 改分页实现前先跑前端 vitest 摸底，
   改完必须 264 个 vitest + 43 个 `search-core` 用例全绿。
3. **排序的二级键**：`Name` 排序是 `folded_name` + `name` 两级 → 同名条目的相对顺序
   变了，界面上的分隔线和「上一项/下一项」就会错位。

---

## 8. 一句话总结

> 上游引擎**技术上**有优化空间，但在这个项目里「改上游」从来不是选项——
> 正确姿势是**把上游做对的地方（折叠前移、高亮只算当前页、字段索引期算好）
> 和做错的地方（每次重排、单线程扫描、匹配期建路径）一起收割进自研引擎**。
>
> 自研引擎现在的欠账，按实测收益排序是：
> **扫描并行没生效（70.7→25ms）> 排序占查询 76%（1.03→0.6ms）> 索引内存超 2.1 倍（3.2→1.5MB）**
> —— 而**别做的**比该做的更长：别抄 `fstatat`、别做工作窃取、别造倒排索引。

---

## 9. 实施后的复盘（方案与现实不符的地方）

### 9.1 「提取排序键」这条是错的 —— 实测只快 19%

方案 §3-P1-① originally 建议「提取排序键再排」（`sort_by_cached_key` 思路）。
实测（`/tmp/sortprobe`，n=8,948）：

| 策略 | 耗时 | 相对现役 |
|---|---|---|
| 现役（比较时解引用 entries） | 0.702ms | — |
| 提取排序键（紧凑数组） | 0.569ms | **只快 19%** |
| n=2,237 时 | 0.092 vs 0.094ms | **-2%（更慢）** |

→ **大头是字符串比较本身，不是指针解引用。**
→ 真正的大头是「**根本不用排那么多**」：只排当前页需要的 500 条 = **0.089ms（6.3×）**。
→ 所以最终实现是「**部分排序**」，不是「提取键」。
（另外提一句：提取键要额外分配一个 ~500KB 的键数组 —— 那是**搜索热路径**上的大额分配，不划算。）

### 9.2 「翻页 <0.05ms」这个估算也错了

方案 §3-P1-② 估计翻页成本主要在「重建 flat + 全量 reverse」。
实测：**flat 重建在 8.9K 命中时几乎为 0**，翻页的成本 **100% 在高亮计算**
（500 条 × `find_folded` 的 O(n·m)）。
→ 去掉 flat 重建在当前规模下**看不出差别**（收益要到 10 万命中才显现：400KB/页）。
→ 真正的提速来自 P1-③（高亮的 ASCII 快路径）。

### 9.2.5 优化前后的火焰图对比（真实扫描器，`sample` 抓）

| 栈顶 | 优化前 | 优化后 | 变化 |
|---|---|---|---|
| `lstat` | 29.6% | 19%（工作量口径 ~33%） | 每条目少一次 stat ✅ |
| `__open_nocancel` | 19.1% | 17% | 固有成本（每目录一次 open） |
| `__getdirentries64` | 10.1% | 12% | 固有成本 |
| **NFD 折叠系** | **~17%** | **~3.5%** | **ASCII 快路径 ✅**（折叠总耗时 17.7ms → 6.2ms） |
| 分配器 | ~10% | ~4% | 少一半的 String 分配 |
| `Path::hash`（`HashMap<PathBuf>`） | ~1.8% | ~2% | 仍未处理（P2-②） |

> 优化前的采样里**只有一个线程** → 证实「并行从来没生效过」；
> 优化后能看到多个工作线程在跑。

### 9.3 路径折叠惰性化：收益不够，风险不小

方案 §3-P0-③②③ 提到「`folded_path` 由父目录 folded path + 折叠名拼出来」。
实现时发现一个必须先钉死的语义问题：
**`to_lowercase` 是上下文相关的**（希腊字母 final sigma：`Σ` 在词尾 → `ς`）。
逐段折叠与整串折叠在边界处是否等价，**推导不能代替测试**。
→ 这一项**留到有明确用例时再做**，不在本轮冒这个险。

**而且收益已经被 ASCII 快路径吃掉了**：折叠总耗时从 17.7ms 降到 **6.2ms**
（`fold(name)` 3.4→0.7ms，`fold(path)` 13.7→5.4ms）。
这部分工作**也会被并行化** → 墙钟贡献约 2.1ms（占 21.1ms 的 10%），
路径折叠惰性化最多再省 **~1.8ms** → **不值得为一个 8% 的收益去冒 Unicode 语义风险**。

### 9.4 排序语义有一处**故意的**变化

比较器从「主键 → 二级键」变成「主键 → 二级键 → **path** → id」。

- 二级键**没丢**（名字仍是 折叠名 + 原名）→ 方案里点名要保的那条保住了；
- 加 `path` 是因为**并行扫描之后索引顺序不确定了** —— 不加 path 的话，
  同样的磁盘内容重扫一次，列表的相对顺序会变 → 界面上的列表会「跳」；
- 加 `id` 是因为 `select_nth_unstable_by` 不稳定 → 比较器必须让顺序**唯一**。

护栏：`engine::tests::partial_window_matches_a_full_sort`（部分排序 == 全排序）
与 `engine::tests::equal_keys_are_ordered_by_path_then_id`（等值条目按 path、id 定序）。

### 9.5 验证纪律（这一轮最值钱的东西）

- **同进程交替跑改前/改后**（`/tmp/abprobe` + `/tmp/sc-before` 改前版副本）
  → 消掉了「同机不同时刻」的负载波动（那台机器同一条 IO 地板两次测出 38.5ms 和 59.2ms）。
- **每个性能结论都有探针**，没有一条是「读代码觉得快」。
- **护栏先写**：部分排序 == 全排序、ASCII 快路径 == 通用路径、needle 必须已折叠 ——
  三条都是「优化本身可能悄悄改语义」的坑，各配了一条钉死它的用例。
