# Lodestar —— 应用层开发说明

这个目录是 **Tauri v2（Rust）+ Vue 3 + TypeScript** 的应用层。
**搜索引擎内核不在这里**，在 `src-tauri/search-core/`（纯 Rust，不依赖 Tauri）。

> 想知道项目整体的方向、约束与已知问题 → 看仓库根目录的
> [`AGENTS.md`](../AGENTS.md) 与 [`docs/PROJECT-HANDOFF.md`](../docs/PROJECT-HANDOFF.md)。

---

## 目录结构

```
app/
├── src/                       Vue 前端
│   ├── App.vue                   总控（1101 行）—— 窗口、快捷键、事件、可见性
│   ├── components/               13 个组件（结果表、预览卡、设置面板、帮助…）
│   ├── lib/                      15 个工具模块（api / paging / tokenizer / highlight…）
│   └── styles.css                全部样式（颜色只能用 :root 里的令牌）
├── src-tauri/
│   ├── src/                    Rust 后端（Tauri 层）
│   │   ├── lib.rs                 组装 app、注册 17 个 command、建原生菜单
│   │   ├── commands.rs            全部 #[tauri::command]
│   │   ├── watcher.rs             FSEvents 监控 + 排除判断缓存
│   │   ├── lifecycle.rs           窗口可见性真相 / ack 驱动事件
│   │   ├── settings.rs            设置读写 + 迁移链
│   │   ├── index_state.rs         扫描代次令牌（防竞态）
│   │   └── shortcut.rs tray.rs event_names.rs logging.rs main.rs
│   ├── search-core/            ★ 自研搜索引擎内核（纯 Rust）
│   ├── tests/                  5 个护栏集成测试
│   ├── assets/                 icon-1024.png（母版）· tray.png（菜单栏图标）
│   ├── icons/                  tauri icon 生成的成品（32/128/128@2x/icns）
│   └── tauri.conf.json         窗口/托盘/图标/打包配置
├── shared/                     syntax-tables.json · cheatsheet.json（单一真相）
└── scripts/                    bundle-app.sh 等 12 个脚本
```

---

## 怎么跑

```bash
# 1. 前端依赖
cd app && pnpm install

# 2. 开发（前端热更新 + Rust 自动重编）
pnpm tauri dev

# 3. 只跑前端（浏览器里看界面，没有 Rust 后端）
pnpm dev        # → http://localhost:1420
```

> ⚠️ **浏览器里跑会看到「引擎不可用」** —— 前端的所有数据都来自 Rust 命令
> （`invoke`），浏览器里没有 Tauri 运行时 → 命令调用会失败。
> 这是正常的，开发界面行为时用 `pnpm tauri dev`。

---

## 怎么测

```bash
# 引擎单测（不依赖 Tauri，最快）
cd app/src-tauri && cargo test -p search-core

# 全部 Rust 测试（含 5 个护栏集成测试）
cd app/src-tauri && cargo test

# 前端单测
cd app && pnpm vitest run

# 只测一个文件
cd app && pnpm vitest run src/lib/paging.test.ts
```

### 测试规模

| 套件 | 数量 | 说明 |
|---|---|---|
| `search-core` | 43 | 引擎：扫描 / 存储 / 查询 / 排序分页 / **速查卡护栏** |
| Rust 护栏（`src-tauri/tests/`） | 20 项检查 | 菜单 / 语法表 / 手册 / watcher 契约 / z-index |
| Vitest | 264 | 前端组件与工具模块 |

**护栏是「静默失效」的护栏** —— 它们盯的不是「代码写了没有」，
而是「改了会不会悄悄坏掉」。改这几个地方时**必须**跑：

| 改了 | 跑 |
|---|---|
| 引擎的语法表 | `cargo test -p search-core`（速查卡护栏会抓） |
| 查询语法 | 同上 + `cargo test --test syntax_tables --test user_guide` |
| CSS 的 `z-index` | `cargo test --test z_index` |
| 菜单项 / 事件名 | `cargo test --test menu` |
| watcher 链路 | `cargo test --test watch_contract` |

---

## 怎么打包

```bash
bash app/scripts/bundle-app.sh
```

**这是唯一的打包入口**，不要自己拼 `cargo build --release` + `tauri bundle`：

- `tauri build` 会带上 `tauri/custom-protocol` feature → 前端嵌进二进制。
  少了它 → **白屏**（webview 去连 `localhost:1420`，连不上）。
  症状很有欺骗性：Rust 侧引擎照跑、日志照打，**只有看渲染才发现是白屏**。
- 脚本还会装许可文件、ad-hoc 签名、跑一整套自检。

打包产物在 `app/src-tauri/target/release/bundle/macos/`：

| 文件 | 说明 |
|---|---|
| `Lodestar.app` | 应用包本体（当前 **5.8M**） |
| `Lodestar.dmg` | 磁盘映像，给别人用（当前 **4.2M**） |

### 自检都查什么

| 检查 | 为什么 |
|---|---|
| 前端资源是否嵌进二进制 | 白屏的判据 |
| 二进制是否只依赖系统框架 | 有外部 dylib → 换机器起不来 + 体积回弹 |
| rpath 里有没有绝对路径 | 会把家目录路径烤进要分发的 .app |
| 签名是否真的有效 | 没签名的 .app 在 Finder 里双击开不了 |
| Frameworks 是不是空的 | 有东西就说明混进外部依赖了 |

---

## 一些必须知道的约定

### 颜色只用令牌

CSS 里的颜色**只能**用 `:root` 里定义过的令牌；`z-index` **只能**用 `var(--z-*)`
那一族。`src-tauri/tests/z_index.rs` 是护栏，违反会红。

### 事件不是免费的

`app.emit()` 在 Tauri v2 里 = **在 webview 里执行一段 JavaScript**（实测约 7ms CPU）。
窗口隐藏时 webview 是睡着的 → 每推一次事件就把它叫醒一次。

→ 想用事件传状态，先问「**能不能用查询代替**」：
让前端把它的认知告诉后端（ack），后端就只在「实际 ≠ 认知」时推 → 稳态零 IPC，
而且事件丢了还能自愈。

### 隐藏 ≠ 定时器停了

macOS 只把 webview 的定时器**限流到 ~1Hz**，不会停掉它。
→ 窗口隐藏时必须**主动停**前端定时器。
→ 「什么时候该停」**不能靠前端自己查**（它停了就没法再问「可见性变没变」）
→ 必须由 Rust **推事件**（`lifecycle::VISIBILITY_EVENT`），当前状态由前端**查**
（`index_status` 返回值里的 `visible`）。

猜错的方向要一致：**宁可性能出问题，也不要功能出问题**
（猜「不可见」→ 状态栏停止更新；猜「可见」→ 只是多跑一点轮询）。

### 搜索线程是串行的

`search-core/src/worker.rs`：单线程 + 单 mpsc 通道，
`Search` / `Page` / `Rescan` 全部排队。这是**正确性**要求（引擎视图有状态）。

→ 一次全量重扫会把排在它后面的搜索全堵住。
→ 状态栏的 `elapsed_ms` 是**引擎内部**耗时、**不含排队** ——
症状是「数字显示很快，但感觉很慢」，很容易误判成搜索算法慢。

### 设置文件在哪

`~/Library/Application Support/com.lodestar.macos/settings.json`
（目录名 = `tauri.conf.json` 的 `identifier`）。

改 `identifier` = 换目录 = 老用户的设置全读不到 → **必须**在
`settings::migrate_legacy_store()` 里加一条迁移（链式的，见该文件的注释）。

### 图标

- **母版** = `src-tauri/assets/icon-1024.png`（1024×1024，透明底）
- **成品** = `src-tauri/icons/`（`tauri icon` 生成，打包只取 32x32 / 128x128 / 128x128@2x / icon.icns）
- ⚠️ 改图标只改母版，别手改成品。
- ⚠️ `src-tauri/assets/tray.png` 是**菜单栏托盘图标**（单色模板图），
  与应用图标是**两个不同的文件**，别混。

---

## 第三方组件

见仓库根目录的 [`THIRD-PARTY-NOTICES.md`](../THIRD-PARTY-NOTICES.md)。

引擎**不需要** ICU / GLib / Pango / PCRE2 ——
Unicode 归一化用 `unicode-normalization`，正则用 Rust 的 `regex`
（线性时间，没有灾难性回溯）。
