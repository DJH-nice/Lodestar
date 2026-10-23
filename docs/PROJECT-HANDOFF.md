# Sextant —— 项目交接文档（新会话必读）

> 生成时间：2026-10-23 · 分支 `master` · 工作区 `/Users/dongjiahao/Downloads/fsearch-0.3.2`
> 配套设计：[`docs/superpowers/specs/2026-10-23-self-owned-search-engine-design.md`](docs/superpowers/specs/2026-10-23-self-owned-search-engine-design.md)
> 目标读者：**新会话的 AI agent / 接手的人**。读完这一份就能接着干。

---

## 1. 项目目标

**Sextant** = 一个 macOS / Apple Silicon 原生的文件搜索工具。

**它长什么样**：菜单栏常驻（托盘图标）+ 全局快捷键唤起（默认 `⌥⌘Space`）→
输入即搜 → 虚拟滚动结果表 → 预览卡 / 右键菜单 → 访达中定位 / 打开。

**三条硬指标**（用户原话）：

| 要求 | 含义 | 验收数字 |
|---|---|---|
| **轻量** | 包小、内存小、不拖机器 | `.app` ≤ 12M；索引内存 ≤ 1.5MB |
| **有可视化页面** | 不是命令行玩具，是正经 GUI | 保留现有 Vue 前端 |
| **性能只能强不能弱** | 至少不输上游 FSearch，最好赢 | 见 §8 基线表 |

**当前方向（2026-10-23 决定）**：**放弃复用上游 FSearch 的 C 引擎，改自研 Rust 搜索引擎**。
原因：上游是别人的 GPL 开源代码。带来的直接收益：许可证义务消失、`.app` 减 48M、冷启动快 20 倍。

---

## 2. 当前目录结构

```
fsearch-0.3.2/
├── app/                        ← 主应用（Tauri v2 + Vue 3），真正的项目资产
│   ├── src/                    Vue 前端：13 组件 + 15 个 lib 工具模块 ≈ 4,439 行
│   ├── src-tauri/
│   │   ├── src/                Rust 后端 11 个模块，9,432 行
│   │   │   ├── commands.rs         全部 #[tauri::command]（17 个）
│   │   │   ├── watcher.rs          FSEvents 监控 + 排除判断缓存（2,483 行）
│   │   │   ├── lifecycle.rs        窗口可见性真相 / ack 驱动事件
│   │   │   ├── settings.rs         settings.json 读写 + 改名迁移
│   │   │   ├── index_state.rs      扫描代次令牌（防竞态）
│   │   │   ├── shortcut.rs  tray.rs  event_names.rs  logging.rs  lib.rs  main.rs
│   │   ├── fsearch-core/       纯 Rust FFI crate（909 行）★ 即将被 search-core/ 取代
│   │   ├── tests/              5 个护栏集成测试
│   │   └── tauri.conf.json     productName=Sextant / identifier=com.sextant.macos
│   ├── shared/                 syntax-tables.json / cheatsheet.json（语法单一真相）
│   └── scripts/                bundle-app.sh（打包唯一入口）等 12 个脚本
├── src/                        上游 FSearch 0.3.2 源码（只读）★ M4 删除
├── shim/                       C 垫片（13 个导出符号）★ M4 删除
├── prototype/                  HTML 原型（1,679 行）—— ⌘K 命令面板等交互的现成货源
├── docs/
│   ├── USER-GUIDE.md           用户手册（应用内「帮助 → 完整手册」渲染它）
│   ├── PROJECT-HANDOFF.md      ← 本文件
│   └── superpowers/{specs,plans}/  设计与实施计划
├── build-shim/  build-shim-test/  build/     构建产物（shim dylib 在 build-shim/）
├── AGENTS.md                   面向 agent 的项目规约
├── README.md  README-macos.md  THIRD-PARTY-NOTICES.md  LICENSE(GPL-2.0)  TODO.md
```

**新的引擎要住哪**：`app/src-tauri/search-core/`（纯 Rust，不依赖 Tauri，可 `cargo test -p search-core` 单独测）。

---

## 3. 已完成的功能（17 项，全部有测试护栏）

| 功能 | 实现位置 | 状态 |
|---|---|---|
| 即时搜索 + 查询语法（15 修饰符 / 18 字段 / 15 运算符） | `commands.rs` + `tokenizer.ts` | ✅ |
| 命中高亮（name / path 两套坐标系） | `highlight.ts` + shim 契约 | ✅ |
| 过滤（全部/文件/目录）+ 排序（名称/位置/大小/时间 × 升降） | `App.vue` + `SearchBar.vue` | ✅ |
| 分页 + 虚拟滚动 + 分组分隔行 | `paging.ts` + `ResultTable.vue` | ✅ |
| 预览卡 / 右键菜单（空白/输入框/结果行三种行为） | `PreviewCard.vue` / `ContextMenu.vue` | ✅ |
| 书签（query/path 两类）+ 导入导出 + 最近搜索 | `settings.rs` + `settings.ts` | ✅ |
| 设置面板（索引目录 / 自动刷新 / 快捷键 / 开机自启） | `SettingsPanel.vue` | ✅ |
| 帮助中心 + 语法速查表 + 完整手册内联 | `HelpPanel.vue` / `UserGuidePanel.vue` | ✅ |
| 致谢与许可面板（GPL 义务） | `CreditsPanel.vue` | ✅ |
| 菜单栏托盘图标 / 全局快捷键唤起 / 开机自启 | `tray.rs` / `shortcut.rs` | ✅ |
| 后台常驻 + 唤醒 + 隐藏时停轮询（后台 CPU 0.04%） | `lifecycle.rs` | ✅ |
| 实时更新（FSEvents + 去抖 800ms + 最小间隔 5s） | `watcher.rs` | ✅ 但**是全量重扫** |
| 手动刷新 / 重建索引 / 在访达中定位 | `commands.rs` | ✅ |

**测试规模**：266 个 vitest + 207 个 Rust `#[test]` + 41 个 C 用例。

---

## 4. 正在开发的功能

**自研搜索引擎**（设计已完成，未动工）。设计文档 §3–§6 是实现依据，§2 是性能基线。

四个已拍板的决策：

1. **查询语法**：保留通用惯例（`AND/OR/NOT` 关键字、`ext:`/`size:` 这类字段写法、
   smart case、含 `/` 搜路径），**其余自定**。⚠️ 上游 parser 是 GPL 代码，**不能抄**。
2. **`src/` + `shim/`：从仓库删掉**（不是只从构建摘掉）→ GPL 义务彻底消失。
3. **顺带补 UX 缺口**（见 §6 已知问题里的 8 项）。
4. **旧设置保留**：索引目录 / 书签 / 快捷键都留着（换引擎不换用户数据）。

**新引擎与前端的接缝**：`commands.rs` 的 `search` / `rescan` / `reindex` 三个实现换掉，
**command 契约保持不变** → Vue 前端零改动，266 个用例继续有效。

---

## 5. 关键技术栈

| 层 | 选型 | 备注 |
|---|---|---|
| 外壳 | **Tauri v2**（Rust 后端 + 系统 WKWebView） | 不是 Electron，不带 Chromium |
| 前端 | **Vue 3 + TypeScript + Vite** | 13 组件 + 15 lib 模块 |
| 引擎（现在） | 上游 C 引擎 + `shim/` C 垫片 | 13 个导出符号，dylib 278KB，**无 GTK 依赖** |
| 引擎（即将） | **纯 Rust** | `regex` / `unicode-normalization` / `unicase` / `memmap2` / `notify 8.2.0` |
| 监控 | `notify 8.2.0`（底层 FSEvents） | 已有 |
| 存储（即将） | mmap 列存（定长表 + 字符串 arena） | 单文件 `index.v1` |
| 序列化 | `serde` / `serde_json` | |
| 打包 | `bash app/scripts/bundle-app.sh` | **唯一入口** |
| 环境 | Rust 1.98.1 aarch64 / Node 22.14.0 / pnpm 9.15.9 / Xcode 完整版 / Homebrew 7.0.9 | |

**新增依赖的获取**：crates.io 可直连（实测 HTTP 200），本地 cargo 缓存是空的，首次构建会下载。

---

## 6. 重要文件说明

| 文件 | 作用 | 备注 |
|---|---|---|
| `app/src-tauri/fsearch-core/src/ffi.rs` | FFI 声明 | **即将被 `search-core/` 取代** |
| `app/src-tauri/fsearch-core/src/worker.rs` | 单线程串行 worker | 新引擎沿用这个模型 |
| `app/src-tauri/src/commands.rs` | 17 个 command | 换引擎只改这里 3 个函数 |
| `app/src-tauri/src/watcher.rs` | FSEvents + `ExcludedDirCache` | 新引擎的增量更新从这里接 |
| `app/src-tauri/src/settings.rs` | 设置读写 + `migrate_legacy_store()` | 换引擎不涉及 |
| `app/src-tauri/tauri.conf.json` | 窗口/托盘/图标/打包配置 | 改 `identifier` 必须配迁移 |
| `app/src/lib/api.ts` | 前端 ↔ Rust 调用封装 | 契约的单一入口 |
| `app/src/lib/paging.ts` | 分页 + 分组分隔行计算 | 依赖 shim 的页内边界公式，**换引擎要同步改** |
| `app/src/lib/highlight.ts` | 高亮区间切分 | 同上 |
| `app/src/lib/tokenizer.ts` | 查询串分词（仅着色） | 新引擎的 lexer 语法要与之对齐 |
| `app/shared/syntax-tables.json` | 语法单一真相 | 由 `app/scripts/gen-syntax-help.py` 从引擎导出生成 |
| `app/scripts/bundle-app.sh` | 打包 + 自检 | 唯一打包入口 |
| `shim/fsearch_shim.h` | 旧 FFI 契约 | **M4 删除**，但里面的注释是重要知识（内存所有权、高亮坐标系、析构语义） |
| `docs/USER-GUIDE.md` | 用户手册 | 改界面要同步改 |
| `prototype/index.html` | HTML 原型 | ⌘K 命令面板 / 三页签索引设置的现成货源 |

---

## 7. 已知问题

### 7.1 引擎层（M0–M3 要解决的）

| 问题 | 证据 | 影响 |
|---|---|---|
| **索引不落盘** | `shim/fsearch_shim.c:266` 传 NULL | 每次启动全量扫描 300–450ms |
| **实时更新是全量重扫** | `watcher.rs` 只有 `rescan` 一条路 | **弱于 Linux 上游**（上游走 inotify 增量） |
| 查询期做 Unicode 折叠 | 上游 `fsearch_utf.c` | 真实数据比合成数据慢 **18 倍** |
| 无倒排/无 trie | 上游 `index_store.c:243` 全表线性 | 实测 10ns/条，**1M 条目才 10ms，不要为此造索引** |
| `.app` 60M | ICU 34M + GPL 源码包 13M | M4 目标 ≤12M |

### 7.2 UX 缺口（顺带补）

命令面板 `⌘K` · 浅色主题 · QuickLook 预览面板 · 可配置排除规则 · `path:` 的 `~` 展开 ·
`size:` 认 MiB/GiB（现只认 k/M/G/T）· 排序偏好与列表种类偏好「存了但界面不用」· 多窗口/多标签页。

### 7.3 环境与工具链

- `.app` 依赖 Homebrew 那套 dylib → 换台没装库的机器跑不起来（M4 随自研引擎解决）。
- 无 CSP（`tauri.conf.json` `security.csp: null`）。
- `app/src-tauri/target/` 1.9G（构建缓存，可清；`app/node_modules` 135M 同理）。

---

## 8. 性能基线（自研引擎的对标线，全部实测）

| 指标 | 上游基线 | 自研目标 |
|---|---|---|
| 冷缓存全量扫描（8,352 项） | 300–450ms | 100–150ms（受 IO 限制） |
| 热缓存全量扫描 | **74.6ms** | **≤ 30ms**（4 线程并行，裸地板 16.6ms） |
| 冷启动 → 可搜 | 300–450ms | **≤ 15ms**（mmap 读档） |
| 查询（$HOME 8,352 项，命中 55%） | 2.6ms | ≤ 1ms |
| 查询（合成 100,002 项） | 0.99ms | ≤ 1ms |
| 切排序维度首次搜索 | 0.95–1.06ms | 同级（无冷启动惩罚） |
| 实时更新 | 全量重扫 50ms | **< 5ms**（增量） |
| 索引内存 | 4.3MB / 8,119 项 | ≤ 1.5MB |
| `.app` 体积 | 60M | ≤ 12M |

**两个必须记住的坑**：

1. **`~/Library` = 264,800 条目**（可见目录总和的 32 倍）。排除规则必须**目录级剪枝**，
   且**顶层也要判** —— 探针漏判顶层时条目数从 8,320 变 273,415、耗时从 0.49s 变 10.7s。
2. **不要造倒排索引**。上游线性匹配 10ns/条，1M 条目才 10ms。真正的提速点是
   **把 Unicode 折叠从匹配期挪到索引期**。

**测量方法**（别靠推断）：

- C 探针链 `build-shim/libfsearch_shim.dylib` 实跑（源码在 `/tmp/fsprobe/`，可重建）。
- 量 CPU **只有 `ps -o time` 的增量法可信**；`top -l -pid <pid>` 的过滤模式不可信，
  `ps -o %cpu` 是衰减均值会严重低估。解析 `ps -o time` 要**按冒号个数分支**（`MM:SS.ss` vs `HH:MM:SS`）。
- 判断「界面是不是白屏」不能只看进程存活，要看 `sample <WebContent 进程>` 的栈里有没有
  `WebCore::` 渲染符号。
- WebKit 子进程是 XPC 服务（ppid=1），认进程要用**启动时刻**，不能靠 `pgrep -P`。

---

## 9. 下一步要做什么

**M0（M1 之前）—— 先把地基和基线钉住：**

1. 建 `app/src-tauri/search-core/` crate（纯 Rust，无 Tauri 依赖）。
2. 实现磁盘格式（`layout.rs` + `store.rs`）：header + 定长 entry 表 + name/folded/path 三个 arena，
   mmap 只读打开，写走 temp+rename 原子替换。
3. 实现并行遍历器（`scan.rs`）：4 线程，**排除规则逐层生效（含顶层）**。
4. 用探针钉住两条线：**热扫描 ≤ 30ms**、**格式读写往返一致**。
5. 写 `plans/2026-10-23-self-owned-search-engine.md` 实施计划（superpowers 计划格式，
   可拆给子代理逐 Task 执行）。

**然后**：M1 可搜（冷启动 ≤15ms + 子串查询）→ M2 完整语法（字段/通配/正则/逻辑/排序/分页/高亮，
前端零改动通过）→ M3 增量更新（FSEvents → 脏目录 → diff）→ M4 收口（删 `src/`+`shim/`、
去 GPL 材料、重写 README/致谢/手册/AGENTS.md、`.app` ≤12M、`otool -L` 无 ICU/GLib）。

**M4 的删除动作走 `mavis-trash <绝对路径>`**（`rm -rf` / glob 删除会被安全网关拦）。

---

## 10. 不能改动和需要注意的约束

### 10.1 通用红线

1. **删文件必须 `mavis-trash <绝对路径>`** —— `rm -f`、glob 删除、`meson setup --wipe` 会被安全网关拦。
2. **未经明确同意不得 `brew install`**。
3. **改 `identifier` = 换设置目录，必须配迁移**：`settings::migrate_legacy_store()`
   （`app/src-tauri/src/settings.rs`），跑在 `setup()` 第一件事，幂等、只拷不删。
4. **打包只能用 `bash app/scripts/bundle-app.sh`** —— 它带 `tauri/custom-protocol` feature 重编 + 自检。
   看到 `✓ 前端已嵌进二进制` 才算成功。
   ⚠️ `cargo build --release` 的产物**没有**该 feature → 前端不内嵌 → **白屏但引擎照跑**，
   看起来像「启动了」实则完全不可用。⚠️ 用 `cp` 覆盖 bundle 里的二进制 = 制造白屏。
5. **Rust 侧搜索线程必须串行**（引擎 view 有状态）：`fsearch-core/src/worker.rs` 单线程 + 单 mpsc。
   状态栏 `elapsed_ms` 是**引擎内部**耗时、**不含排队** —— 一次全量重扫会把排队的搜索全堵住。
6. **Tauri 事件不是免费的**：`app.emit()` = 在 webview 里执行一段 JS（≈7ms CPU）。
   想用事件传状态，先问「能不能用查询代替」—— 让前端 ack，后端只在「实际 ≠ 认知」时推 → 稳态零 IPC。
7. **窗口隐藏 ≠ 前端定时器停了**（macOS 只限流到 ~1Hz）。隐藏时必须**主动停**前端定时器；
   可见性要问**窗口服务器的真相**（`win.is_visible()`），不是我们自己的标志。
   猜错的方向要一致：**宁可性能出问题，也不要功能出问题**。
8. **CSS 颜色只能用 `:root` 里已有的令牌**；`z-index` 走 `--z-*` 那一族。
9. **README 同时发 GitHub + Gitee，被内链引用的标题一律不带标点**（两平台 slug 规则不同），
   不能用 `{#custom-id}`（GitHub 不开 attributes 扩展，会原样显示）。
10. **同一台机器可能有并发会话在跑自己的冒烟测试**：别用 `pkill -f`（会杀掉执行它的那条命令自己），
    用 `pkill -x`；验收实例要**改名 + 换 bundle id**。

### 10.2 本项目特有

- **上游 `src/` 在 M4 之前仍是只读依赖**（与原始 tarball 逐字节一致，
  `bash shim/check-upstream-pure.sh` 可验）。**不要再往 `shim/` 上加新功能** —— 它即将被取代。
- **GPL 义务在 M4 之前仍然有效**：应用内「帮助 → 致谢与许可」面板、`LICENSE`、
  `THIRD-PARTY-NOTICES.md`、以及打包时装进 `Resources/licenses/` 的**完整对应源码压缩包**
  （必须含 `src/` + `shim/` + `app/` 三个目录，缺一个就不成立）。
  上游许可是 **GPL-2.0-or-later**（不是 GPL-3.0），写文档别写错版本。
- **M4 删掉上游后**：必须全仓检索上游残留（`fsearch_`、`g_assert`、`pcre2`、`ucasemap` 等标记），
  并重写 README / 致谢面板 / 手册 / `THIRD-PARTY-NOTICES.md` / `LICENSE`（自有版权，不再是 GPL）。
  删上游**不豁免**「查询语法不能抄上游 parser」—— GPL 代码的复制仍是侵权。
- **别只抽查就下结论**：报一个数字之前先把它算全（例：统计索引里有哪些目录要遍历整棵树）。
