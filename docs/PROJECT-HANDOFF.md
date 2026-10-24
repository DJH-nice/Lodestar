# Lodestar —— 项目交接文档（新会话必读）

> 生成时间：2026-10-23 · 分支 `master` · 工作区 `/Users/dongjiahao/Projects/sextant`
> 配套设计：[`docs/superpowers/specs/2026-10-23-self-owned-search-engine-design.md`](superpowers/specs/2026-10-23-self-owned-search-engine-design.md)
> 目标读者：**新会话的 AI agent / 接手的人**。读完这一份就能接着干。

---

## 1. 项目目标

**Sextant** = 一个 macOS / Apple Silicon 原生的**文件搜索工具**。

**它长什么样**：菜单栏常驻（托盘图标）+ 全局快捷键唤起（默认 `⌥⌘Space`）→
输入即搜 → 虚拟滚动结果表 → 预览卡 / 右键菜单 → 访达中定位 / 打开。

**三条硬指标**（用户原话）：

| 要求 | 含义 | 验收数字 |
|---|---|---|
| **轻量** | 包小、内存小、不拖机器 | `.app` ≤ 12M；索引内存 ≤ 1.5MB |
| **有可视化页面** | 不是命令行玩具，是正经 GUI | 保留现有 Vue 前端 |
| **性能只能强不能弱** | 至少不输上游 FSearch，最好赢 | 见 §8 基线表 |

**2026-10-23 的方向变更**：这个项目原本是「上游 FSearch 0.3.2 的 macOS 版」
（引擎是上游的 C 代码，GPL-2.0-or-later）。现在**自研引擎已落地并替换掉上游**：

- 仓库里**一行上游代码都没有** → GPL 义务消失
- 旧仓库留在 `/Users/dongjiahao/Downloads/fsearch-0.3.2`，仅作上游行为对照

---

## 2. 当前目录结构

```
sextant/                          ← 新仓库（git 已初始化，1 次提交）
├── app/                        主应用（Tauri v2 + Vue 3）
│   ├── src/                    Vue 前端：13 组件 + 15 个 lib 工具模块 ≈ 4,400 行
│   ├── src-tauri/
│   │   ├── src/                Rust 后端 11 个模块，9,432 行
│   │   │   ├── commands.rs         全部 #[tauri::command]（17 个）
│   │   │   ├── watcher.rs          FSEvents 监控 + 排除判断缓存（2,483 行）
│   │   │   ├── lifecycle.rs        窗口可见性真相 / ack 驱动事件
│   │   │   ├── settings.rs         settings.json 读写 + 迁移链
│   │   │   ├── index_state.rs      扫描代次令牌（防竞态）
│   │   │   ├── shortcut.rs  tray.rs  event_names.rs  logging.rs  lib.rs  main.rs
│   │   ├── search-core/       ★ 自研搜索引擎内核（纯 Rust，约 2,000 行）
│   │   │   ├── src/scan.rs          并行遍历器 + 排除规则（目录级剪枝）
│   │   │   ├── src/store.rs         索引落盘（冷启动读档）
│   │   │   ├── src/query.rs         查询语法：词法/语法/匹配/字段/日期
│   │   │   ├── src/fold.rs          Unicode 折叠（索引期做）
│   │ │   ├── src/exclude.rs        排除规则（唯一真相）
│   │   │   ├── src/syntax.rs        语法三张表（前端共用）
│   │   │   ├── src/engine.rs        排序/分页/高亮
│   │   │   ├── src/worker.rs        串行搜索线程
│   │   │   └── tests/cheatsheet_queries.rs  速查卡护栏
│   │   ├── tests/              5 个护栏集成测试
│   │   └── tauri.conf.json     productName=Sextant / identifier=com.sextant.macos
│   ├── shared/                 syntax-tables.json / cheatsheet.json（单一真相）
│   └── scripts/                bundle-app.sh（打包唯一入口）等 12 个脚本
├── prototype/                  HTML 原型（1,679 行）—— ⌘K 命令面板的现成货源
├── docs/
│   ├── USER-GUIDE.md           用户手册（应用内「帮助 → 完整手册」渲染它）
│   ├── PROJECT-HANDOFF.md      ← 本文件
│   └── superpowers/{specs,plans}/  设计与实施计划
├── AGENTS.md                   面向 agent 的项目规约（**新会话先读这个**）
├── README.md  THIRD-PARTY-NOTICES.md  LICENSE(MIT)  .gitignore
```

---

## 3. 已完成的功能（17 项界面功能 + 自研引擎）

| 功能 | 实现位置 | 状态 |
|---|---|---|
| 即时搜索 + 查询语法 | `commands.rs` + `tokenizer.ts` | ✅ |
| 命中高亮（name / path 两套坐标系） | `highlight.ts` + 引擎 | ✅ |
| 过滤（全部/文件/目录）+ 排序（名称/位置/大小/时间 × 升降） | `App.vue` + `SearchBar.vue` | ✅ |
| 分页 + 虚拟滚动 + 分组分隔行 | `paging.ts` + `ResultTable.vue` | ✅ |
| 预览卡 / 右键菜单（空白/输入框/结果行三种行为） | `PreviewCard.vue` / `ContextMenu.vue` | ✅ |
| 书签（query/path 两类）+ 导入导出 + 最近搜索 | `settings.rs` + `settings.ts` | ✅ |
| 设置面板（索引目录 / 自动刷新 / 快捷键 / 开机自启） | `SettingsPanel.vue` | ✅ |
| 帮助中心 + 语法速查表 + 完整手册内联 | `HelpPanel.vue` / `UserGuidePanel.vue` | ✅ |
| 致谢与许可面板 | `CreditsPanel.vue` | ✅ |
| 菜单栏托盘图标 / 全局快捷键唤起 / 开机自启 | `tray.rs` / `shortcut.rs` | ✅ |
| 后台常驻 + 唤醒 + 隐藏时停轮询（后台 CPU 0.04%） | `lifecycle.rs` | ✅ |
| 实时更新（FSEvents + 去抖 800ms + 最小间隔 5s） | `watcher.rs` | ✅ 但**是全量重扫**（M3 改） |
| 手动刷新 / 重建索引 / 在访达中定位 | `commands.rs` | ✅ |
| **自研引擎**：并行扫描 / 索引落盘 / 查询语法 / 排序分页 / 高亮 | `search-core/` | ✅ 43 个用例 |
| **设置迁移链**（fsearch → lodestar → sextant） | `settings.rs` | ✅ |

**测试规模**：264 个 vitest + 43 个 `search-core` 用例 + 20 项 Rust 护栏检查。

---

## 4. 正在开发的功能

**M2 / M3 / M4**，具体见 §9。

已拍板的四个决策（2026-10-23）：

1. **查询语法**：保留通用惯例（`AND/OR/NOT` 关键字、`ext:`/`size:` 这类字段写法、
   smart case、含 `/` 搜路径），**其余自定**。⚠️ 上游 parser 是 GPL 代码，**不能抄**。
2. **`src/` + `shim/`：从仓库删掉** → 已完成，GPL 义务已消失。
3. **顺带补 UX 缺口**（见 §7.2 的 8 项）。
4. **旧设置保留**：索引目录 / 书签 / 快捷键都留着（换引擎不换用户数据）→ 已完成。

---

## 5. 关键技术栈

| 层 | 选型 | 备注 |
|---|---|---|
| 外壳 | **Tauri v2**（Rust 后端 + 系统 WKWebView） | 不是 Electron，不带 Chromium |
| 前端 | **Vue 3 + TypeScript + Vite** | 13 组件 + 15 lib 模块 |
| 引擎 | **纯 Rust 自研**（`search-core` crate） | 不依赖 Tauri，可 `cargo test -p search-core` 单独测 |
| 依赖 | `regex` / `unicode-normalization` / `libc` / `notify 8.2.0` / `serde` | **没有 ICU、没有 GLib、没有 Pango、没有 PCRE2** |
| 监控 | `notify 8.2.0`（底层 FSEvents） | 已有 |
| 存储 | 自定义二进制索引档 `index.bin` | app config 目录里 |
| 打包 | `bash app/scripts/bundle-app.sh` | **唯一入口** |
| 环境 | Rust 1.98.1 aarch64 / Node 22.14.0 / pnpm 9.15.9 / Xcode 完整版 | |

---

## 6. 重要文件说明

| 文件 | 作用 | 备注 |
|---|---|---|
| `app/src-tauri/search-core/src/query.rs` | 查询引擎 | **最容易改坏的地方**，改完必须跑速查卡护栏 |
| `app/src-tauri/search-core/src/engine.rs` | 索引 + 排序 + 分页 + **分组边界公式** | 前端 `paging.ts` 依赖它，公式不能变 |
| `app/src-tauri/search-core/src/scan.rs` | 并行遍历 + 排除剪枝 | ⚠️ 排除规则必须逐层生效（含顶层） |
| `app/src-tauri/search-core/src/store.rs` | 索引落盘 | 冷启动的关键 |
| `app/src-tauri/src/commands.rs` | 17 个 command | 换引擎只改了这里 3 个函数 |
| `app/src-tauri/src/watcher.rs` | FSEvents + `ExcludedDirCache` | M3 增量更新从这里接 |
| `app/src-tauri/src/settings.rs` | 设置读写 + `migrate_legacy_store()` | 迁移链是**链式**的 |
| `app/src-tauri/tauri.conf.json` | 窗口/托盘/图标/打包配置 | **没有 frameworks 段了**（无外部 dylib） |
| `app/src/lib/api.ts` | 前端 ↔ Rust 调用封装 | 契约的单一入口 |
| `app/src/lib/paging.ts` | 分页 + 分组分隔行计算 | 依赖引擎的页内边界公式 |
| `app/src/lib/highlight.ts` | 高亮区间切分 | 引擎给的是精确长度，兼容旧的哨兵值逻辑 |
| `app/shared/syntax-tables.json` | 语法单一真相 | 与 `search-core/src/syntax.rs` 对账（护栏） |
| `app/scripts/bundle-app.sh` | 打包 + 自检 | 唯一打包入口 |
| `docs/USER-GUIDE.md` | 用户手册 | 改界面要同步改 |
| `prototype/index.html` | HTML 原型 | ⌘K 命令面板 / 三页签索引设置的现成货源 |

---

## 7. 已知问题

### 7.1 引擎层（M2/M3 要解决的）

| 问题 | 证据 | 影响 |
|---|---|---|
| **实时更新是全量重扫** | `watcher.rs` 只有 `rescan` 一条路 | **弱于 Linux 上游**（上游走 inotify 增量）→ M3 |
| 索引档是「整读整解析」 | `store.rs` | 8K 条目 ~1.1MB，够用；mmap 列存是后续优化项 |
| 并行只到「根目录级」 | `scan.rs` | 单根目录内部仍是串行；工作窃取是优化项 |

### 7.2 UX 缺口（顺带补）

命令面板 `⌘K` · 浅色主题 · QuickLook 预览面板 · 可配置排除规则 · `path:` 的 `~` 展开 ·
`size:` 认 MiB/GiB（现只认 k/M/G/T）· 排序偏好与列表种类偏好「存了但界面不用」· 多窗口/多标签页。

### 7.3 环境与工具链

- 无 CSP（`tauri.conf.json` `security.csp: null`）。
- `app/src-tauri/target/` 会涨到 1.9G（构建缓存，可清）。
- 首次构建要下载 crates（crates.io 可直连，实测 HTTP 200）。

---

## 8. 性能基线（自研引擎 vs 上游，全部实测）

> 测量方法：`cargo run --release -p search-core --example bench`
> （读档 / 扫描 / 查询延迟一次测清）。上游数字来自旧仓库的 C 探针实测。

> **2026-10-24 优化后重测**。A/B 是**同进程交替跑改前/改后两个版本**
> （消除机器负载波动）—— 探针 `/tmp/abprobe`，改前版副本在 `/tmp/sc-before`。

| 指标 | 上游基线 | 优化前 | **Sextant 现在** | 对比上游 |
|---|---|---|---|---|
| 冷启动：读索引档 | —（上游不落盘） | 2.6ms | **2.7ms** | 每次省 300ms |
| **热缓存：全量扫描** | 74.6ms | 62.8ms | **21.1ms**（快 66%） | **3.5× 快** |
| 冷缓存：全量扫描 | 300–450ms | 322.9ms | 112.1ms（磁盘已热） | 持平 |
| 字段查询 `ext:pdf` | ~1.08ms | 0.063ms | **0.052ms** | **20× 快** |
| 字段查询 `size:>1M` | ~1.08ms | 0.060ms | **0.059ms** | **20× 快** |
| 普通词（命中 55%） | 2.59ms | 1.43ms | **0.38ms**（快 73%） | **6.8× 快** |
| 全量匹配 `*` | 2.06ms | 1.00ms | **0.47ms**（快 53%） | **4.4× 快** |
| 无命中查询 | 0.14ms | 0.121ms | 0.121ms | 持平 |
| 翻页（每页 500 条） | — | 0.16ms | 0.19ms | ⚠️ 慢 0.03ms |
| 索引内存 | 4.3MB / 8,119 项 | 3.2MB | **3.2MB** / 8,550 项 | 持平 ⚠️ 目标 ≤1.5MB **未达标** |
| `.app` 体积 | 60M | 5.8M | **5.8M** | **10× 小** |
| DMG | 29M | **4.2M** | **4.2M** | 7× 小 |

**这次优化做了什么**（详见 [`superpowers/plans/2026-10-24-engine-optimization-plan.md`](superpowers/plans/2026-10-24-engine-optimization-plan.md)）：

1. **扫描并行真的生效了** —— 以前按「根目录之间」并行，而默认索引根就是 `$HOME` 这**一个**
   → 线程数被压成 1 → **并行从来没生效过**（`sample` 抓的火焰图只有一个线程为证）。
   现在改成「顶层子树分区 + 原子游标」→ 62.8ms → 21.1ms。
2. **每个条目少一次 `stat`** —— `scan.rs` 里 stat 了两次（判 is_dir 一次、取 size/mtime 一次）；
   火焰图 `lstat` 占 **29.6%**。
3. **Unicode 折叠加 ASCII 快路径** —— 折叠开销的 **92% 是 NFD**，而 ASCII 文本的 NFD 是恒等变换
   → 语义**完全等价**，但省掉绝大部分折叠开销。
4. **排序只排「当前页需要的窗口」** —— 以前每次查询把全部命中排一遍（占一次搜索的 40~76%），
   现在只排本页要的那 500 条 → 实测 6.3×。
5. **翻页不再重建整个 flat 数组** —— 以前每页拷贝一次全部命中的 id（10 万命中 = 400KB/页）。

**为什么字段查询快 20 倍**：上游的字段条件要**逐条目重算**（`ext` 要现取扩展名、
`size` 要现 stat），我们是**索引期算好、存进定长记录** → 查询期直接读字段。

**⚠️ 一笔要说清的账**：翻页慢了 0.03ms（部分排序的代价），换来的是每次搜索快 0.55ms。
一次搜索通常配 0~2 次翻页 → 净赚。另外**翻页的成本 100% 在高亮计算**，
不在 flat 重建（flat 重建在 8.9K 命中时几乎为 0，收益要到 10 万级才显现）。

**两个必须记住的坑**：

1. **`~/Library` = 264,800 条目**（可见目录总和的 32 倍）。
   排除规则必须**目录级剪枝**且**逐层生效（含顶层）** ——
   探针漏判顶层时条目数从 8,320 变 273,415、耗时从 0.49s 变 10.7s。
2. **不要造倒排索引**。线性匹配 10ns/条，1M 条目才 10ms。
   真正的提速点是**把 Unicode 折叠从匹配期挪到索引期**（已做）。

**还有一个必须记住的坑（踩过）**：
`&self` 方法里用 `*mut` 裸指针改字段是 **UB** —— 优化器会把没被改过的字段
缓存进寄存器 → release 构建 panic，debug 看不出来。
→ 内部可变性一律用 `RefCell`（引擎单线程持有，`RefCell` 零成本）。

**测量方法**（别靠推断）：

- 引擎：`cargo run --release -p search-core --example bench`
- 量 CPU **只有 `ps -o time` 的增量法可信**；`top -l -pid <pid>` 的过滤模式不可信，
  `ps -o %cpu` 是衰减均值会严重低估。解析 `ps -o time` 要**按冒号个数分支**。
- 判断「界面是不是白屏」不能只看进程存活，要看 `sample <WebContent 进程>` 的栈里有没有
  `WebCore::` 渲染符号。

---

## 9. 下一步要做什么

**M2 — 补齐查询语法：✅ 已完成（2026-10-24）**
表里的每一项现在都有实现，而且**表与实现是同一份数据**（`syntax.rs` 的
`FIELD_TABLE` / `MODIFIER_TABLE` 既是实现也是导出的表 → 结构上不可能漂移）。
护栏 `syntax_tables.rs` 会拿**真引擎**去问表里的每一项。
每加一个语法，速查卡与手册要同步。

**M3 — 增量更新**：`watcher.rs` 收到 FSEvents → 记脏目录 → **只重扫脏目录** →
diff 后写回索引档。目标 <5ms，取代现在的全量重扫。

**M4 — 收口**：
- 补 UX 缺口 8 项（⌘K 命令面板从 `prototype/index.html` 搬）
- 重写 `docs/USER-GUIDE.md` 里的引擎描述（现在还写着「引擎来自 FSearch」）
- 打包验证：`.app` ≤12M、`otool -L` 无外部 dylib
- 加 CI（GitHub Actions：cargo test + vitest + 打包自检）

**另有两件小事**：
- `CreditsPanel.vue` 里还写着上游 FSearch 与 GPL 的说明 → 要重写
- `app/README.md` 里还有旧的上游构建步骤 → 要重写

---

## 10. 不能改动和需要注意的约束

### 10.1 通用红线

1. **删文件必须 `mavis-trash <绝对路径>`** —— `rm -f`、glob 删除、`meson setup --wipe` 会被安全网关拦。
2. **未经明确同意不得 `brew install`**。
3. **改 `identifier` = 换设置目录，必须配迁移**：`settings::migrate_legacy_store()`
   （`app/src-tauri/src/settings.rs`），跑在 `setup()` 第一件事，幂等、只拷不删。
   迁移链：`com.fsearch.macos` → `com.lodestar.macos` → `com.sextant.macos`。
4. **打包只能用 `bash app/scripts/bundle-app.sh`** —— 它带 `tauri/custom-protocol` feature
   重编 + 自检。**看到 `✓ 前端已嵌进二进制` 才算成功。**
   ⚠️ `cargo build --release` 的产物**没有**该 feature → 前端不内嵌 → **白屏但引擎照跑**
   （看起来像「启动了」，实则完全不可用）。⚠️ 用 `cp` 覆盖 bundle 里的二进制 = 制造白屏。
5. **Rust 侧搜索线程必须串行**（`search-core/src/worker.rs`：单线程 + 单 mpsc）。
   状态栏 `elapsed_ms` 是**引擎内部**耗时、**不含排队** —— 一次全量重扫会把排队的搜索全堵住。
6. **Tauri 事件不是免费的**：`app.emit()` = 在 webview 里执行一段 JS（≈7ms CPU）。
   想用事件传状态，先问「**能不能用查询代替**」—— 让前端 ack，后端只在「实际 ≠ 认知」时推
   → 稳态零 IPC，且事件丢了能自愈。
7. **窗口隐藏 ≠ 前端定时器停了**（macOS 只限流到 ~1Hz）。隐藏时必须**主动停**前端定时器；
   可见性要问**窗口服务器的真相**（`win.is_visible()`），不是我们自己的标志。
   猜错的方向要一致：**宁可性能出问题，也不要功能出问题**。
8. **CSS 颜色只能用 `:root` 里已有的令牌**；`z-index` 走 `--z-*` 那一族
   （`tests/z_index.rs` 是护栏，违反会红）。
9. **README 同时发 GitHub + Gitee，被内链引用的标题一律不带标点**（两平台 slug 规则不同），
   不能用 `{#custom-id}`（GitHub 不开 attributes 扩展，会原样显示）。
10. **同一台机器可能有并发会话在跑自己的冒烟测试**：别用 `pkill -f`（会杀掉执行它的那条命令自己），
    用 `pkill -x`；验收实例要**改名 + 换 bundle id**。

### 10.2 本项目特有

- **上游 `src/` 已经不在仓库里了** —— 旧仓库 `/Users/dongjiahao/Downloads/fsearch-0.3.2`
  是**只读参考**，不要往里写东西。
- **GPL 义务已经消失** —— 但**不要**因此以为可以抄上游的代码：
  上游的 query parser 是 GPL 代码，**能力对齐可以，代码不能抄**。
- **别只抽查就下结论**：报一个数字之前先把它算全（例：统计索引里有哪些目录要遍历整棵树）。
- **Unicode 语义**：归一化用 **NFD**（不是 NFC），大小写折叠用**转小写**（不是 full case folding）。
  改 `fold.rs` 前先想清楚这两条 —— 它们决定「搜不搜得到」。
