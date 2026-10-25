# AGENTS.md —— Lodestar macOS

> 面向任何 AI agent / 新会话。**先读这份。**

## 这是什么

**Lodestar** —— 一个 macOS / Apple Silicon 原生的**文件搜索工具**。

- 菜单栏常驻（托盘图标）+ 全局快捷键唤起（默认 `⌥⌘Space`）
- 输入即搜 → 虚拟滚动结果表 → 预览卡 / 右键菜单 → 访达中定位 / 打开
- **搜索引擎完全自研**（`app/src-tauri/search-core/`，纯 Rust）
- GUI 是自己写的 **Tauri v2（Rust）+ Vue 3 + TypeScript**

> **历史**：这个项目最早是「上游 FSearch 0.3.2 的 macOS 版」（GPL-2.0-or-later，
> 引擎是上游的 C 代码）。2026-10-23 起**自研引擎替换掉了上游**，
> 仓库里现在**一行上游代码都没有**，GPL 义务随之消失。
> 旧仓库留在 `/Users/dongjiahao/Downloads/fsearch-0.3.2`，仅作上游行为对照。

## 目录结构

```
lodestar/
├── app/
│   ├── src/                    Vue 前端：13 组件 + 15 个 lib 模块 ≈ 4,400 行
│   ├── src-tauri/
│   │   ├── src/                Rust 后端 11 模块，9,432 行
│   │   ├── search-core/        ★ 自研搜索引擎内核（纯 Rust，不依赖 Tauri）
│   │   │   ├── scan.rs           并行遍历器 + 排除规则（目录级剪枝）
│   │   │   ├── store.rs          索引落盘（冷启动不用重扫）
│   │   │   ├── query.rs          查询语法：词法 / 语法 / 匹配（自己写的）
│   │   │   ├── fold.rs           Unicode 折叠（索引期做，查询期只折一次）
│   │   │   ├── exclude.rs        排除规则（唯一真相，watcher 也问它）
│   │   │   ├── syntax.rs         语法三张表（前端共用同一份）
│   │   │   ├── engine.rs         索引 + 排序 + 分页 + 高亮
│   │   │   ├── incremental.rs    ★ 增量更新（脏路径 → 就地改索引）
│   │   │   └── worker.rs         串行搜索线程
│   │   └── tests/              5 个护栏集成测试
│   ├── shared/                 syntax-tables.json / cheatsheet.json
│   └── scripts/                bundle-app.sh（打包唯一入口）等 12 个脚本
├── docs/
│   ├── USER-GUIDE.md           用户手册（应用内「帮助 → 完整手册」渲染它）
│   ├── PROJECT-HANDOFF.md      项目交接文档（背景与决策）
│   └── superpowers/{specs,plans}/  设计与实施计划
├── prototype/                  HTML 原型（⌘K 命令面板等交互的现成货源）
├── LICENSE (MIT)  THIRD-PARTY-NOTICES.md  README.md
```

## 硬约束（违反即任务失败）

1. **删文件必须用 `mavis-trash <绝对路径>`** —— `rm -rf`、glob 删除会被安全网关拦。
2. **未经明确同意不得 `brew install`。**
3. **打包只能用 `bash app/scripts/bundle-app.sh`** —— 它带 `tauri/custom-protocol`
   feature 重编 + 一整套自检。**看到 `✓ 前端已嵌进二进制` 才算成功。**
   ⚠️ `cargo build --release` 的产物**没有**该 feature → 前端不内嵌 →
   **白屏但引擎照跑**（看起来像「启动了」，实则完全不可用）。
   ⚠️ 用 `cp` 覆盖 bundle 里的二进制 = 制造白屏。
4. **Rust 侧搜索线程必须串行**（`search-core/src/worker.rs`：单线程 + 单 mpsc）。
   搜索 / 翻页 / 重扫全部排队 —— 这是**正确性**要求（视图有状态）。
   状态栏的 `elapsed_ms` 是**引擎内部**耗时、**不含排队** ——
   症状是「数字显示很快，但感觉很慢」，容易误判成搜索算法慢。
5. **Tauri 事件不是免费的**：`app.emit()` = 在 webview 里执行一段 JS（≈7ms CPU）。
   想用事件传状态，先问「**能不能用查询代替**」——
   让前端 ack，后端只在「实际 ≠ 认知」时推 → 稳态零 IPC，且事件丢了能自愈。
6. **窗口隐藏 ≠ 前端定时器停了**（macOS 只限流到 ~1Hz）。
   隐藏时必须**主动停**前端定时器；可见性要问**窗口服务器的真相**（`win.is_visible()`），
   不是我们自己的标志。猜错的方向要一致：**宁可性能出问题，也不要功能出问题**。
7. **CSS 颜色只能用 `:root` 里已有的令牌**；`z-index` 走 `--z-*` 那一族。
   （`app/src-tauri/tests/z_index.rs` 是护栏，违反会红。）
8. **README 同时发 GitHub + Gitee，被内链引用的标题一律【不带标点】**（两平台 slug 规则不同），
   不能用 `{#custom-id}`（GitHub 不开 attributes 扩展，会原样显示）。
9. **别只抽查就下结论**：报一个数字之前先把它**算全**
   （例：统计索引里有哪些目录要遍历整棵树，不能只看三个就说「只剩这三个」）。
10. **同一台机器可能有并发会话在跑自己的冒烟测试**：别用 `pkill -f`（会杀掉执行它的那条命令自己），
    用 `pkill -x`；验收实例要**改名 + 换 bundle id**。
11. **增量更新不能被记成「一趟全量扫描」**：`watcher` 的「覆盖」闸靠
    `IndexState::last_scan_completed_at` 判断「这次变化是不是已经被扫过了」。
    增量更新**只覆盖它处理的那几个路径**，不是整棵树 ——
    把它记进那个时刻 → 之后的文件变化会被误判成「已覆盖」→ **被跳过** →
    索引悄悄过期、用户搜不到新文件。
    → 全量重扫走 `index_state::run_scan_blocking`，
      增量更新走 `index_state::run_incremental_blocking`（**刻意不记**完成时刻）。
    护栏：`index_state.rs` 的 `an_incremental_update_does_not_count_as_a_completed_scan`。

## 动手前先知道的几件事

- **索引不落盘 = 冷启动慢** → 已经修好：`search-core/src/store.rs` 把索引写成二进制档，
  启动时读档（目标 ≤15ms），扫描结果变化时重写。
- **排除规则是「目录级剪枝」** —— 规则匹配的是**目录本身**，扫描器**根本不进那个目录**，
  里面的文件从没被访问过。
  → 判断某个**文件**会不会被索引，必须**连祖先一起查**（`exclude::path_is_excluded` 就是这么做的）。
  只查文件本身会全部返回 false。
  → ⚠️ **规则必须逐层生效，包括顶层**（2026-10-23 实测踩过：
  顶层漏判时条目数从 8,320 变成 273,415、耗时从 0.49s 变成 10.7s）。
- **`~/Library` = 264,800 条目**，是可见目录总和（8,320）的 32 倍 —— 排除规则默认生效是性能命脉。
- **匹配用的是「索引期折叠好」的字符串**（`fold.rs`）：
  上游每次查询都折一遍（ICU），真实数据比合成数据慢 18 倍；我们折一次存起来，查询只折查询串。
- **Unicode 归一化用 NFD**（不是 NFC）：macOS 的 HFS+ 历史上是 NFD，APFS 不归一化 ——
  同一文件可能是两种形态，用 NFD 才能让它们互相匹配。
- **查询语法的三张表只有一份**（`search-core/src/syntax.rs`）→ 导出给前端 →
  手册 / 速查卡 / 着色表 / 校验规则共用同一份真相，不会漂移。
  （护栏：`app/src-tauri/tests/syntax_tables.rs`）
- **速查卡里的每条 query 都必须被引擎真认**（护栏：
  `app/src-tauri/search-core/tests/cheatsheet_queries.rs`）——
  2026-10-23 重建这道护栏时，它当场抓出 `dm:pastmonth` 不被认的缺口。
- **正则用 Rust 的 `regex` 不用 PCRE2** —— 它保证线性时间，没有灾难性回溯。
  用户能直接输入查询串，必须防这一手。
- **别用 `cargo build --release` 的产物去覆盖 bundle**（见硬约束 3）。
- **索引落盘很贵，写盘那一段尤其贵**（实测 8,541 项 / 3MB）：
  序列化（纯 CPU）**1.8ms** + 写盘（fsync + rename）**13.3ms**。
  → 落盘**不能**放在增量更新的关键路径上（会把「几毫秒」毁掉）→
    由 `worker.rs` 的**后台落盘线程**做（搜索线程只负责交出字节流）。
  → 顺带一提：不 fsync 的话写盘只要 **1.3ms** —— fsync 一个人占掉 12ms。
- **FSEvents 给的是「每个文件」的事件，不是「某个目录变了」**（实测，2026-10-24）。
  → 这正是能做增量更新的前提：单个文件的变化可以精确到那一条。
  → 但它**会丢事件**（队列满时）→ notify 把它翻译成 `Flag::Rescan` 事件
    → 收到它必须**全量重扫**兜底（增量更新依赖「事件没丢」）。

## 性能基线（实测，探针方法见设计文档 §2）

> 2026-10-24 优化后重测。**A/B 是同进程交替跑改前/改后两个版本**（消除机器负载波动），
> 探针在 `/tmp/abprobe`（改前版副本在 `/tmp/sc-before`）。

| 指标 | 旧引擎（上游 C） | 优化前 | **Lodestar 现在** | 备注 |
|---|---|---|---|---|
| 热缓存全量扫描（8,550 项） | 74.6ms | 62.8ms | **21.1ms**（快 66%，2.98×） | 顶层子树分区 + 原子游标 |
| 冷启动 → 可搜 | 300~450ms（每次全量扫描） | — | **2.7ms**（读索引档） | 达标 |
| **实时更新（改 1 个文件）** | 300ms+（上游 inotify 增量） | — | **0.01~0.05ms**（增量更新本身） | M3 |
| **实时更新（改 100 个文件）** | 300ms+ × N | — | **0.5~2.7ms**（**一次**更新） | M3 |
| **实时更新（端到端可见）** | — | — | **407ms**（含事件投递与去抖） | M3 |
| 查询（8,948 项，命中 55%） | 2.6ms | 1.43ms | **0.38ms**（快 73%） | 只排当前页需要的那一段 |
| 查询（全量匹配 `*`） | 2.06ms | 1.00ms | **0.47ms**（快 53%） | 同上 |
| 查询（`ext:pdf` / `size:>1M`） | ~1.08ms | 0.06ms | **0.05ms** | 字段索引期算好 |
| 查询（100,002 项） | 0.99ms | — | ≤1ms | 线性匹配，**不要造倒排索引** |
| 索引内存 | 4.3MB / 8,119 项 | 3.2MB | **3.2MB** | ⚠️ 目标 ≤1.5MB **未达标** |
| `.app` 体积 | 60M | 5.8M | **5.8M** | ICU 34M + GPL 源码包 13M 都消失了 |
| 后台常驻 CPU | 0.04% | 0.04% | 0.04% | 已达标 |

**⚠️ 表里的绝对值会随机器状态漂移** —— 2026-10-25 复测时，同机同树的
「热缓存全量扫描」是 **109~113ms**（表里记的 21.1ms 复现不出来）。
**别拿绝对值当基线，要比就做同机同树的交替 A/B**
（基线提交 vs 当前分支，轮流各测一轮）—— 单次测量在这台机器上噪声能到 4 倍。
详见 `docs/PROJECT-HANDOFF.md` 的「2026-10-25 性能复测」。

**2026-10-25 另两项实测 —— 别再重复排查这两件事**：

| 指标 | 实测 | 说明 |
|---|---|---|
| **唤起到窗口上屏** | **342~373ms**（中位 359） | **Rust 侧只占 2ms**（ensure 66µs + show 903µs + focus 883µs）→ 348ms 全在系统侧：窗口服务器合成 + **WKWebView 恢复渲染**（窗口隐藏时 WebKit 停止绘制）。与**窗口面积无关**（600×400 与 1040×700 一样）、与 **`set_focus` 无关**（跳过 focus 后 352.9ms vs 默认 353.0ms） |
| **运行时内存** | **≈95MB**（四进程 footprint 之和） | 主进程 30MB（**引擎+索引只占 3.2MB**）/ WebKit GPU 21MB / 渲染 40~45MB / 网络 5.9MB。窗口显隐不影响，反复开关面板也不累积（连开 5 次稳定在 92~94MB） |

**唤起延迟在 WKWebView 架构下无解** —— 曾试过「别 orderOut，把窗口挪到屏幕外」
（窗口仍 visible → WebKit 继续渲染 → 实测只要 **5~12ms**，快 30 倍），
但 **AppKit 会钳制窗口位置**（标题栏必须留一部分在屏幕上）：
`set_position((-20000,-20000))` 返回 `Ok(())` 而**位置根本没变**。
绕钳制的三条路都不通：私有符号 `CGSSetWindowOrigin` 在 macOS 15 已不存在、
AX 移动要辅助功能权限、挪出去 + `alpha=0` 会挡屏幕边缘 40px（Dock 放左边时会被挡）。
**要 10ms 级别的唤起，只能换掉 WebView**（= 重写整个前端）。

**内存同理**：约 70MB 是 WKWebView 底座，改配置改不动 ——
试过关掉窗口透明 + 窗口级毛玻璃，反而多占 5~10MB（已回退）。

**量内存用 `footprint -p <pid>`**，不要用 `ps -o rss` 求和（会把 WebKit 共享页重复计）。
WebKit 的三个 XPC 进程**不是主进程的子进程**（ppid=1），
`pgrep -P` 认不出来，得**按启动时刻**认。

**⚠️ 这张表以前有个错数字**：优化前写的「热缓存全量扫描 16~25ms，自研用 4 线程并行」是**错的** ——
并行**从来没生效过**（扫描器按「根目录之间」并行，而默认索引根就是 `$HOME` 这一个 → 线程数被压成 1）。
真实值是 62.8ms。现在改成「顶层子树分区 + 原子游标」才真的并行起来（21.1ms）。

**两条实测结论，别再走回头路**：

1. **分区赢，窃取输** —— 子树分区 2.4~3.2×；共享目录队列（工作窃取式）**零收益**。
2. **别抄上游的 `fstatat(dir_fd)`** —— 实测比 std 的路径式 stat **慢 34%**（69.5ms vs 51.8ms）。

**量 CPU 只有 `ps -o time` 的增量法可信**：`top -l N -pid <pid>` 的过滤模式不可信
（实测对真值 20% 占空比的 burner 连续 6 个采样点全报 0.0%），
`ps -o %cpu` 是衰减均值会严重低估。解析 `ps -o time` 要**按冒号个数分支**
（`MM:SS.ss` vs `HH:MM:SS`），否则会算出离谱的值。

## 引擎内部的三条不变量（2026-10-24 优化后新增，改引擎前先读）

1. **排序只排「当前页需要的窗口」，不是全排**
   → `View` 里存的是**未排序**的命中 id；`build_page` 里用 `sorted_window()`
   取 `[lo, hi)` 这一段（`select_nth_unstable_by` 分区 + 小窗口排序）。
   实测：全排序 0.56ms vs 只排 500 条 0.089ms → **6.3×**。
   ⚠️ **分区必须作用在子切片上**（`head[..lo]` / `head[lo..hi]`）——
   分区整个数组会把窗口外的元素换进窗口，**窗口的元素集合就错了**。
   护栏：`engine::tests::partial_window_matches_a_full_sort`。

2. **排序比较器必须是「全序」**
   → 顺序 = 主键 → 二级键 → **path** → id。
   - 二级键不能丢（名字 = 折叠名 + 原名）—— 丢了同名条目的相对顺序就变；
   - `path` 是为了**确定性**：并行扫描之后索引顺序是不确定的，
     不加 path 的话同样的磁盘内容重扫一次列表顺序会「跳」；
   - `id` 兜底（`select_nth_unstable_by` 不稳定 → 比较器必须让顺序唯一）。
   护栏：`engine::tests::equal_keys_are_ordered_by_path_then_id`。

3. **`find_folded` 的 needle 必须是「已折叠」的**
   → 调用方传的是 `Pattern::Plain.folded`，**不是**原样查询串。
   本函数只折叠 haystack（逐字符），不折叠 needle → 传原串进来，
   非 ASCII 的 needle 永远匹配不上。
   护栏：`fold::tests::find_folded_needs_a_folded_needle`。

**翻页的成本 100% 在高亮计算**（实测：翻一页 0.19ms，flat 重建几乎为 0）
→ 想再快要优化 `find_folded` 的 O(n·m)，不是去动分页逻辑。

## 正在做的工作

**外接卷索引已完成并合入 master**（2026-10-25，合并提交 `611cf67`，
分支 `feat/external-volume-index` 共 32 个提交）——
插上 U 盘自动进索引、拔掉即移除、插回复用旧索引。
**真机验收 8/9 通过**（#1 #2 #3 #4 #5 #6 #8 #9，在用户真实外接盘
`/Volumes/软件` 上跑的，97,460 项），逐条证据见
[`docs/superpowers/plans/2026-10-24-external-volume-indexing.md`](docs/superpowers/plans/2026-10-24-external-volume-indexing.md)
的「真机验收清单」表。

**改外接卷相关代码前必知的三个坑**：

- **卷枚举用 `getfsstat` 的 macOS 3 参数形式** —— `getmntinfo` 在 macOS 上**必崩**。
  判据是内核的 **`MNT_LOCAL` 位**（网络盘 smbfs/nfs 没有这个位），
  比按类型字符串匹配可靠。
- **索引档按根分文件**（`store.rs` v3，每根一个 `.bin`）——
  插拔卷**不会**把家目录的档作废。
- **卷的扫描绝不在搜索 worker 上做** —— `search-core/src/worker.rs` 是
  单线程串行的（视图有状态），卷扫描会堵住搜索；
  只有 `$HOME` 的全量重扫才留在 worker 上（21 ms）。

**M3 已完成并合回 master**（2026-10-24，提交 `dd651eb`）——
实时更新从「全量重扫」改成「脏路径 → 就地改索引」。
剩下的是 **M4（收口）**，清单见 [`docs/PROJECT-HANDOFF.md`](docs/PROJECT-HANDOFF.md)：

- UX 缺口：⌘K 命令面板、浅色主题、QuickLook、`~` 展开、MiB/GiB、可配置排除规则。
- ~~致谢面板还写着上游 FSearch 与 GPL 的说明~~ → **已完成**（现在写的是
  「引擎完全自研，早期版本用的上游 C 引擎已整体替换」，第三方清单另附）。
- ~~`app/README.md` 里还有旧的上游构建步骤~~ → **已完成**（现在全是 Tauri/Vue 的）。
- **README 重写** → **已完成**（按旧骨架重写 + 4 张真机截图重拍 +
  性能数字逐项实测取证 —— 详见下面的「性能基线」）。
- 打包验证 → **已完成**（合并后 `cargo test` 249 + `vitest` 278 全绿，
  `bundle-app.sh` 自检全过）。**CI 没做**（仓库无远端）。

**M2 的「补齐查询语法」已完成** —— 见下面的「语法表 = 实现」。

## 语法表 = 实现（2026-10-24 起）

前端判断「引擎认不认某项语法」的**唯一依据**是「它在这张表里」
（`app/src/lib/syntax-help.ts` 的 `isRowSupported`）→ **表里有 = 引擎必须真认**。

- `search-core/src/syntax.rs` 的 `FIELD_TABLE` / `MODIFIER_TABLE`
  **既是实现、也是导出的表** —— 表里不可能出现没实现的项（结构上防漂移）。
- 以前这里是两份名单（`FIELDS` 常量 + `field_supported()` 名单），
  两份会漂移 → 表里有、实现没有 → 前端说「支持」、引擎报警告。
- 护栏：`app/src-tauri/tests/syntax_tables.rs` 会拿**真引擎**去问表里的每一项
  （字段挨个试候选值，修饰符挨个试 `<mod>:probe`）。
- **加一个语法要同步三处**：`syntax.rs` 的表 → `app/shared/cheatsheet.json`
  → `docs/USER-GUIDE.md`。速查卡的每条 query 会被真引擎验一遍
  （`search-core/tests/cheatsheet_queries.rs`），写错直接红。

## 查询语法**永远不会**让引擎报错（2026-10-24 起）

用户**打字过程中**的输入天然不完整（`alpha AND`、单独一个 `(`、括号没闭合）。
以前这些会让 `query::parse()` 返回 `None` → 引擎 `Err(SearchFailed)` →
界面只显示一句英文 "search failed"，用户既不知道发生了什么也不知道怎么改。

- 现在 `query::parse()` **永不失败**：缺条件就**结束当前分支 + 给一句中文提示**，
  已经写好的条件照常出结果（`alpha AND` 仍然显示 alpha 的结果）。
- 提示走 `Query.warnings` → `SearchResult.warning` → 状态栏左下角（`StatusBar.vue`）。
- 护栏：`search-core/tests/incomplete_queries.rs` 钉住
  「不完整 → 不报错 + 有提示」「合法 → 不报错 + 无提示」两边。
- ⚠️ **别再引入 `?` 把「缺一个 term」升级成整条查询失败** —— 那就是根因本身。

## 关键文件

| 文件 | 作用 |
|---|---|
| `app/src-tauri/search-core/src/query.rs` | 查询引擎（**最容易改坏的地方**，改完必须跑速查卡护栏） |
| `app/src-tauri/search-core/src/syntax.rs` | 语法表（**表 = 实现**，改它等于改引擎能力） |
| `app/src-tauri/search-core/src/mime.rs` | 扩展名 → MIME（`contenttype:` 字段用） |
| `app/src-tauri/search-core/src/scan.rs` | 并行遍历器 + 排除剪枝 |
| `app/src-tauri/search-core/src/engine.rs` | 索引 + 排序 + 分页 + **分组边界公式**（前端 `paging.ts` 依赖它） |
| `app/src-tauri/src/commands.rs` | 17 个 command —— 换引擎只改这里 3 个函数 |
| `app/src-tauri/search-core/src/incremental.rs` | **增量更新内核**（脏路径 → 就地改索引）—— M3 新增 |
| `app/src-tauri/src/watcher.rs` | FSEvents 监控 + 排除判断缓存 + **脏路径集合** |
| `app/src-tauri/src/settings.rs` | 设置读写 + **identifier 迁移链** |
| `app/src-tauri/src/lifecycle.rs` | 窗口可见性真相 / ack 驱动事件 |
| `app/scripts/bundle-app.sh` | 打包 + 自检（唯一入口） |
| `app/scripts/make-icon-master.swift` | 生成**母版** `assets/icon-1024.png`（六分仪图形，程序绘制） |
| `app/scripts/make-icon.swift` | 母版 → macOS squircle（吃白边 + 投影） |
| `app/scripts/gen-icons.sh` | 母版 → `icons/` 下全部成品（32 / 128 / 256 / .icns） |
| `docs/USER-GUIDE.md` | 用户手册（改界面要同步改） |

## 改 identifier = 换设置目录，必须配迁移

`tauri-plugin-store` 把 `settings.json` 落在**以 identifier 命名的目录**里
（`~/Library/Application Support/<identifier>/`）→ 改 identifier 会让老用户的
书签 / 最近搜索 / 索引目录**全部读不到**。

迁移在 `settings::migrate_legacy_store()`（`app/src-tauri/src/settings.rs`），
跑在 `setup()` 的**第一件事**（必须早于任何一次 `settings::load()`）。
它是**链式**的（`com.fsearch.macos` → `com.lodestar.macos` → `com.lodestar.macos`），
旧目录改名成 `.bak`（**不删** —— 万一迁移出问题还能找回）。
**下次再改 identifier 时，照这个模式再加一条。**
