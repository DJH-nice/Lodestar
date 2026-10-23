# FSearch macOS 界面原型

FSearch 0.3.2 是 Linux 优先的 GTK3/C 应用（见 [上游分析](../ANALYSIS-macos-mchip.md)）。
这是面向 **macOS + Apple Silicon** 的新界面原型，采用 **Tauri v2 + Vue 3** 技术路线。

> **原型即生产代码。** 这一版不是「废弃的设计稿」——它的 DOM 结构、状态管理、
> 样式令牌都按 Vue 3 SFC 的写法组织，迁到 Tauri 工程时是机械搬运，不需要重画。

---

## 打开方式

单文件、**完全离线**（Vue 3.5.13 已内联），双击即可：

```bash
open prototype/index.html
```

或起个本地服务：

```bash
python3 -m http.server 8765 --directory prototype
# → http://127.0.0.1:8765/index.html
```

> 💡 **直接双击打开（`file://`）比本地服务更省事**：`python3 -m http.server` 的
> keep-alive 连接会占住单线程服务，多标签页时会把浏览器拖到超时。
> 原型是自包含单文件，`file://` 完全够用。

**深链**：URL 加 `#q=<urlencoded 查询>` 可直接进入某个搜索态并分享，例如

```
index.html#q=ext%3Ats%20size%3A%3E1M%20path%3AProjects
```

**回归测试**（每次改 `index.html` 后都跑一遍）：

```bash
node prototype/test/search-engine.test.js
```

测试会**从 `index.html` 现抽核心逻辑**再跑，测的是当前文件而不是副本。

---

## 已实现的交互（全部可点，不是静态图）

| 区域 | 能力 | 状态 |
|---|---|---|
| 搜索框 | 即时搜索（70ms 防抖）、**语法实时高亮**、命中片段标色 | ✅ |
| 搜索语法 | `name:` `path:` `ext:` `size:` `date:` `type:` `content:` / `AND` `OR` `NOT` / `"短语"` / `*` `?` 通配 / `/正则/` / **绝对路径** | ✅ |
| 结果表 | **虚拟滚动**（5 万条数据只渲染可视区）、5 列、列头排序（升/降序） | ✅ |
| 过滤 | 全部 / 文件 / 目录，实时计数，**1 / 2 / 3 快捷键** | ✅ |
| 命令面板 | `⌘K`，12 条命令，模糊过滤，↑↓ + ⏎ | ✅ |
| 语法帮助 | `⌘/`，双栏速查表 | ✅ |
| 书签 | `⌘B`，预置 3 条，`⌘D` 保存当前搜索，`⌫` 删除 | ✅ |
| 最近搜索 | 工具栏「最近」或命令面板，搜索后 `⏎` 打开即记录 | ✅ |
| 索引设置 | `⌘,`，三个页签：索引目录 / 排除规则 / 常规 | ✅ |
| 预览卡 | 右键结果行 **或 `⌘I`**（对齐 Finder「显示信息」），点外部关闭 | ✅ |
| 状态栏 | 已索引数、匹配数、耗时、扫描状态、快捷键提示 | ✅ |
| 键盘 | `↑↓` 选择 · `⏎` 打开 · `Esc` 清空 · `⌘F` 聚焦搜索 · `⌘I` 预览 | ✅ |

### 空状态会归因，而不是甩锅给关键词

这是原型的核心设计决策 —— 搜不到东西时，**必须说清楚为什么**：

| 场景 | 提示 | 动作 |
|---|---|---|
| 没输入 | 输入关键字开始搜索 | 提示 `⌘K` / `⌘/` |
| **被当前过滤器筛掉** | 「全部」下有 N 项，是被「目录」筛掉的 | 一键「切换到全部」 |
| **路径不在索引范围内** | FSearch 只搜索**已索引**的目录 | 一键「加入索引目录」 |
| 确实没匹配 | 试试更短的关键词，或用 `ext:` / `size:>1M` | — |

### 性能实测（浏览器内，5 万条内存数据集）

| 查询 | 命中 | 耗时 |
|---|---|---|
| `index` | 1,053 | 11–15 ms |
| `ext:ts size:>1M path:Projects` | 109 | 7–11 ms |

> 真实产品里这个耗时由 C 引擎承担，**前端只负责渲染**，所以实际会更快。
> 但虚拟滚动是必须的：10 万–100 万条时 DOM 节点数必须与数据量解耦。

---

## 设计系统令牌

全部集中在 `<style>` 顶部的 `:root`，直接搬进 Vue 工程即可。

```css
--bg-app:#0B0D12      --bg-chrome:#10131A   --bg-surface:#161A23
--bg-row-hover:#1C2130 --bg-row-sel:#173358  --bg-elevated:#1B1F2A
--fg:#E8ECF4          --fg-2:#9AA3B8        --fg-3:#7C8699
--accent:#0A84FF      --accent-2:#409CFF    --match:#FFD466
--success:#32D74B     --warn:#FFD60A        --danger:#FF453A
```

**为什么是这套值**：`--accent` / `--success` / `--warn` / `--danger` 直接取自
**macOS 系统深色模式色板**（systemBlue / systemGreen / systemYellow / systemRed），
这样在 Tauri 里配 `windowEffects` 毛玻璃时，UI 底色和系统材质是同一套色系，不会打架。
`--match`（琥珀）专门用于搜索命中片段，在深底上对比度最高。

字体走系统栈：`-apple-system` → SF Pro；中文落到 PingFang SC；
路径等宽用 `SF Mono`。**不引入任何 Web Font**（离线可用 + 原生观感）。

图标是内联 SVG sprite（Phosphor 风格，16px 网格），无 emoji、无外部依赖。

---

## 迁到 Tauri + Vue 3 的路径

### 1. 拆 SFC

```
src/
├── App.vue                    ← 现在的根模板
├── components/
│   ├── TitleBar.vue            ← .titlebar
│   ├── SearchField.vue         ← .field + 语法高亮 overlay
│   ├── FilterChips.vue        ← .chips
│   ├── ResultTable.vue         ← .rt-head + 虚拟滚动 .vp
│   ├── StatusBar.vue
│   ├── CommandPalette.vue
│   ├── SyntaxHelp.vue
│   ├── BookmarksPanel.vue
│   ├── SettingsPanel.vue
│   └── PreviewCard.vue
├── composables/
│   ├── useVirtualList.ts       ← startIdx/endIdx/visibleRows 这套
│   ├── useSearchQuery.ts       ← tokenizeQuery / parseQuery / matchToken
│   └── useKeyboard.ts          ← 全局快捷键
└── styles/tokens.css           ← :root 里的令牌
```

### 2. 搜索引擎从「假数据」换成「真 C 引擎」

原型里 `runSearch()` 跑的是内存假数据。真实实现：

```
C 引擎 (dylib)
  └── fsearch_shim.c   ← 约 300 行，把 GLib 类型关进黑盒
        └── 纯 C 接口：const char* / uint32_t / int / double
              ├── Tauri Rust 层（bindgen 或手写 extern）
              │     └── invoke('search', {query}) → JSON
              └── Vue 前端
```

**`fsearch_shim.c` 要暴露的接口**（依据 `src/fsearch_database.h` 的 8 个导出函数）：

| C 侧原函数 | shim 后的纯 C 签名 |
|---|---|
| `fsearch_database_new(GFile*, ...)` | `int fs_open(const char *db_path, const char *cfg_path)` — 内部自己造 GFile，GLib 不外泄 |
| `fsearch_query_new(const char *term, ...)` | `void* fs_query_new(const char *term)` |
| `fsearch_query_match(q, match_data)` | `int fs_query_match(void *q, void *entry)` |
| `fsearch_query_highlight(q, match_data)` | `int fs_query_highlight(void *q, void *entry, int *out_spans, int max)` |
| `fsearch_database_search_view_sort(view, ...)` | `int fs_sort(void *view, int key, int desc)` |
| `fsearch_database_search_view_get_entry_for_idx(view, idx)` | `const char *fs_entry_path(void *view, uint32_t idx)` |
| `fsearch_database_search_view_get_info(view)` | `int fs_result_count(void *view)` |
| `fsearch_database_try_get_item_info(...)` | `int fs_entry_meta(void *view, uint32_t idx, struct fs_meta *out)` |

**只有 3 个 GLib 类型需要藏在 shim 内部**：`GFile*`（构造时）、
`FsearchDatabaseIncludeManager*` / `ExcludeManager*`（GObject，构造时）。
查询、匹配、高亮、排序、选择模型全是普通 C struct + 整数，直接跨 FFI 没问题。

> 关键前提：`fsearch_database.h` 对外只导出 **8 个函数**，且核心类型
> （`FsearchDatabaseEntryInfo` 等）都是普通 struct 而非 GObject。
> 这点已在源码中核对，是 B/C 两条路线体量可控的根本原因。

### 3. Tauri v2 的 macOS 原生能力

| 需求 | Tauri v2 API | 对应原型的哪个元素 |
|---|---|---|
| 真毛玻璃 | `WindowEffectsBuilder::new().effects(WindowEffect::Sidebar)` | 整窗背景 |
| 红黄绿浮在自绘栏上 | `titleBarStyle: "Overlay"` + `hiddenTitle: true` | `.titlebar` 的交通灯 |
| **真·macOS 顶部系统菜单栏** | `tauri::menu`（macOS 下映射为 NSMenu） | 原型暂缺，落地时补 |
| 用默认应用打开 | `tauri-plugin-opener` / `open` | 「打开 / 在访达中显示」 |
| 原生文件选择器 | `tauri-plugin-dialog` | 索引目录「添加」 |
| 深浅色跟随系统 | `theme: None` + CSS `prefers-color-scheme` | 主题令牌 |

### 4. 需要在 Tauri 侧补的原生能力

- **FSEvents 实时监控**：macOS 没有 inotify/fanotify，FSearch 会退化成周期重扫。
  要真实时，需新增一个 FSEvents 监听后端（GIO 的 `GFileMonitor` 在 macOS 上走 FSEvents，
  但 FSearch 自己没用它，是自研的 inotify/fanotify 层）。
- **TCC 权限**：访问 桌面/文稿/下载 需在 Info.plist 声明
  `NSDesktopFolderUsageDescription` / `NSDocumentsFolderUsageDescription` /
  `NSDownloadsFolderUsageDescription`，否则索引会静默缺目录。
- **QuickLook 预览**：FSearch 原版的预览走 GNOME Nautilus D-Bus，macOS 无此服务，
  需换成 QuickLook（`QLPreviewPanel`）。

---

## 原型里哪些是「真的」，哪些是「假的」

| | 说明 |
|---|---|
| ✅ **真的** | 全部布局、配色、间距、字阶、交互反馈、键盘流、语法解析器、高亮算法、虚拟滚动、排序/过滤逻辑、URL 深链 |
| ⚠️ **模拟的** | 5 万条假数据（种子固定，刷新结果一致）；`已索引 1,408,753` 是写死的展示值 |
| ❌ **占位的** | 「打开 / 在访达中显示」只弹 toast；「最近搜索」面板只弹提示；书签存在内存不落盘 |

---

## 真索引模式（连真实硬盘）

原型默认跑在内置演示数据上。启动本地索引服务后，它会**扫你真实的目录**：

```bash
node prototype/dev-server.mjs                                    # 默认扫 ~/java ~/Downloads
node prototype/dev-server.mjs --path ~/java --path ~/Documents   # 指定目录
node prototype/dev-server.mjs --path ~                           # 扫整个家目录（~92 万条）
```

然后打开 <http://127.0.0.1:8791/>。状态栏会显示 **「已索引 7,359 · 真索引」**。

### 它怎么做到「界面和服务端用同一份语法」

`dev-server.mjs` **不复制粘贴**搜索实现，而是运行时从 `index.html` 里抽出核心逻辑：

```js
const html = fs.readFileSync('index.html', 'utf8');
let core = html.slice(coreStart, coreEnd);
core = core.replace('const DATA = buildDataset(50000);', 'const DATA = __REAL_INDEX__;');
core = core.replace("let HOME_PREFIX = '/Users/turan';",   'let HOME_PREFIX = __HOME__;');
const factory = new Function('__REAL_INDEX__', '__HOME__', core + '…');
```

好处：**语法分词器只有一份实现**，界面高亮和后端查询不可能漂移。
回归测试 `test/search-engine.test.js` 用的是同一套抽取手法。

### 接口

| 接口 | 用途 | 界面对应 |
|---|---|---|
| `GET /api/status` | 索引状态、条目数、目录列表、家目录 | 顶部连接探测 |
| `GET /api/search?q=&filter=&sort=&desc=` | 查询（服务端已过滤 + 排序） | 主搜索 |
| `POST /api/index {path}` | 加入索引目录并重扫 | 空态「加入索引目录」/ 设置页「添加」 |
| `POST /api/rescan` | 重建索引 | 标题栏「重建索引」 |

### 自动降级

服务不可用时，界面**静默回退**到内置演示数据，状态栏显示「演示数据 · 未连接索引服务」。
真索引模式下排序与过滤交给服务端（会重新查询），筛选/排序变化即时反映到真实结果集。

### macOS 专属：默认排除规则必须包含 `Library` 和隐藏目录

实测你这台机器的 `~`：

| 目录 | 文件数 |
|---|---|
| `find ~ -type f` 总计 | **923,867** |
| 其中 `~/Library` | **520,520** |
| 其中 `~/Desktop`（其中 91% 在隐藏目录里） | 148,289 → 可见仅 **4,200** |
| **排除后真正有价值的文件** | **≈ 15,000** |

`~/Library` 是 App 缓存 / 邮件 / 消息数据；隐藏目录里是 `$RECYCLE.BIN`、各类 `.xxx` 工具目录。
**不排除的话索引会膨胀 60 倍且全是垃圾。** 这是 FSearch 移植到 macOS 时
**排除规则比 Linux 更关键**的原因。

---

## 已知取舍

1. **路径列用 RTL 裁剪**（`direction:rtl` + 内层 `unicode-bidi:bidi-override`），
   让溢出裁在开头、露出靠近文件名的尾部。比「截尾」更有用，但复制路径时要注意取全文。
2. **单栏结构**：按你选的「中等深度」保持 FSearch 原有的单栏信息架构。
   真要改双栏（侧边栏 + 内容区），得动布局，`fsearch_preferences.ui` 那 1,647 行是主要成本。
3. **深色主题为主**：设计系统只做了深色。浅色主题需要再出一套色值
   （`prefers-color-scheme` 已在 CSS 中预留位置）。
4. **`data/` 里的图标**用统一描边的通用符号，不区分文件类型。真实产品应接
   macOS `NSWorkspace.shared.icon(forFile:)`，走 QuickLook 的图标缓存。
5. **排序在 JS 侧做**（`filtered.slice().sort()`），100 万条时会卡。真实产品必须把排序
   下沉到 C 引擎的 `fsearch_database_sort.c`，前端只渲染可视窗口。

---

## 修复记录（2026-10-19 代码审查）

原型第一版有 12 个缺陷，均已修复并有回归测试覆盖。根因集中在**搜索引擎**：

| # | 缺陷 | 根因 | 修法 |
|---|---|---|---|
| A1-A5 | 绝对路径被误判成正则（`/Users/x` → 正则 `Users` + 词 `x`） | 正则 token 的匹配没有锚定到整个 token | 重写为**统一分词器**：先按 token 切分，再判定 `re` / `path` / `field` / `bool` / `word`；正则必须整段是 `/.../` 且含元字符 |
| A6 | 绝对路径搜不到任何东西 | 同上 | 新增 `path` token，按目录前缀匹配 |
| B1 | 帮助面板写的 `path:~/Dev` **恒为 0 命中** | 从不展开 `~` | 新增 `expandHome()`，`HOME_PREFIX` 与假数据集一致 |
| B2 | 帮助面板写 `name:foo` 是「文件名包含」，实际是**完全相等** | `name:` 走了 glob | 无通配符时改为包含匹配，与裸词、与文档三者对齐 |
| C1 | 重叠关键词产生**嵌套 `<mark>`**，HTML 结构损坏 | 逐个 needle 依次全局替换，后者会匹配到前者刚插入的标签内 | 改为**单次替换**（正则 alternation + 长度降序） |
| E1 | 高亮与匹配是**两套独立正则**，切分结果可能不一致 | 重复实现 | 合并为单一分词器，两个视图共用 |
| D1 | 深链 `?v=2#q=java` 把 `java?v=2` 整段当查询 | `[^&]*` 未排除 `?` 和 `#` | 字符类改为 `[^&#]*`，且按 `search + hash` 顺序拼接 |
| D2 | 深链打开后**先闪「没有匹配的结果」** | 防抖窗口内 `searching` 还是 false | 变更时立刻置 `searching`，窗口内显示「搜索中…」 |
| D3 | 深链初始查询走防抖 → 后台标签页里 `setTimeout` 被节流到分钟级，**永远卡在「搜索中…」** | 一次性查询不该防抖 | 初始值立即执行，只有后续输入才防抖；并监听 focus/visibilitychange 补跑 |
| — | 预览卡只能右键打开 | — | 新增 `⌘I`（对齐 Finder「显示信息」） |
| — | 预览卡点外面不关闭 | 只在 Esc / 点「打开」时清空 | 加 document 级 click 监听（卡片内 `@click.stop`） |
| — | 书签面板宣称「⌫ 删除」但没实现 | 文档说了功能没做 | 实现 `deleteBookmark()` + 每行删除按钮 |
| — | 命令面板宣称「1/2/3」但没实现 | 同上 | 实现 1/2/3 切换过滤器 |
| — | 「最近搜索」是占位（只弹 toast） | 同上 | 实现完整面板 + 搜索后 `⏎` 自动记录 |
| — | 状态栏快捷键提示竖排换行 | 加了 3 个提示后宽度不够 | `white-space:nowrap` + 窄窗口按优先级隐藏次要项 |
