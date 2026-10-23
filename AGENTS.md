# AGENTS.md —— Sextant macOS

> 面向任何 AI agent / 新会话。**先读这份。**

## 这是什么

**Sextant** —— 一个 macOS / Apple Silicon 原生的**文件搜索工具**。

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
sextant/
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

| 指标 | 旧引擎（上游 C） | Sextant 自研 | 备注 |
|---|---|---|---|
| 热缓存全量扫描（8,352 项） | 74.6ms | **16~25ms** | 自研用 4 线程并行 |
| 冷启动 → 可搜 | 300~450ms（每次全量扫描） | **≤15ms**（读索引档） | 目标值 |
| **实时更新（改 1 个文件）** | 300ms+（上游 inotify 增量） | **0.05ms**（增量更新本身） | M3 |
| **实时更新（改 100 个文件）** | 300ms+ × N | **2.66ms**（一次更新） | M3 |
| **实时更新（端到端可见）** | — | **407ms**（含事件投递与去抖） | M3 |
| 查询（8,352 项，命中 55%） | 2.6ms | ≤1ms | 目标值 |
| 查询（100,002 项） | 0.99ms | ≤1ms | 线性匹配，**不要造倒排索引** |
| 索引内存 | 4.3MB / 8,119 项 | ≤1.5MB | 目标值 |
| `.app` 体积 | 60M | **≤12M** | ICU 34M + GPL 源码包 13M 都消失了 |
| 后台常驻 CPU | 0.04% | 0.04% | 已达标 |

**量 CPU 只有 `ps -o time` 的增量法可信**：`top -l N -pid <pid>` 的过滤模式不可信
（实测对真值 20% 占空比的 burner 连续 6 个采样点全报 0.0%），
`ps -o %cpu` 是衰减均值会严重低估。解析 `ps -o time` 要**按冒号个数分支**
（`MM:SS.ss` vs `HH:MM:SS`），否则会算出离谱的值。

## 正在做的工作

**M2/M3 还没做完**，具体清单见 [`docs/PROJECT-HANDOFF.md`](docs/PROJECT-HANDOFF.md)：

- 查询语法只实现了常用子集；`childcount`/`contenttype` 等字段还在表里但**未实现**
  （引擎会给「暂未实现」的警告，不会静默返回 0 条）。
- ~~**实时更新还是全量重扫**~~ → **M3 已完成**：改成「脏路径 → 就地改索引」。
  实测改 1 个文件 **0.05ms**、100 个文件 **2.66ms**（一次更新）、端到端可见 **407ms**。
- UX 缺口：⌘K 命令面板、浅色主题、QuickLook、`~` 展开、MiB/GiB、可配置排除规则。

## 关键文件

| 文件 | 作用 |
|---|---|
| `app/src-tauri/search-core/src/query.rs` | 查询引擎（**最容易改坏的地方**，改完必须跑速查卡护栏） |
| `app/src-tauri/search-core/src/scan.rs` | 并行遍历器 + 排除剪枝 |
| `app/src-tauri/search-core/src/engine.rs` | 索引 + 排序 + 分页 + **分组边界公式**（前端 `paging.ts` 依赖它） |
| `app/src-tauri/src/commands.rs` | 17 个 command —— 换引擎只改这里 3 个函数 |
| `app/src-tauri/search-core/src/incremental.rs` | **增量更新内核**（脏路径 → 就地改索引）—— M3 新增 |
| `app/src-tauri/src/watcher.rs` | FSEvents 监控 + 排除判断缓存 + **脏路径集合** |
| `app/src-tauri/src/settings.rs` | 设置读写 + **identifier 迁移链** |
| `app/src-tauri/src/lifecycle.rs` | 窗口可见性真相 / ack 驱动事件 |
| `app/scripts/bundle-app.sh` | 打包 + 自检（唯一入口） |
| `docs/USER-GUIDE.md` | 用户手册（改界面要同步改） |

## 改 identifier = 换设置目录，必须配迁移

`tauri-plugin-store` 把 `settings.json` 落在**以 identifier 命名的目录**里
（`~/Library/Application Support/<identifier>/`）→ 改 identifier 会让老用户的
书签 / 最近搜索 / 索引目录**全部读不到**。

迁移在 `settings::migrate_legacy_store()`（`app/src-tauri/src/settings.rs`），
跑在 `setup()` 的**第一件事**（必须早于任何一次 `settings::load()`）。
它是**链式**的（`com.fsearch.macos` → `com.lodestar.macos` → `com.sextant.macos`），
旧目录改名成 `.bak`（**不删** —— 万一迁移出问题还能找回）。
**下次再改 identifier 时，照这个模式再加一条。**
