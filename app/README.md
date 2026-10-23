# Lodestar for macOS

把开源工具 **FSearch 0.3.2**（Linux GTK 文件搜索器）做成 macOS / Apple Silicon 原生 GUI。

- **外壳**：Tauri v2（`macos-private-api`）
- **前端**：Vue 3 + TypeScript + Vite + pnpm
- **引擎**：上游 FSearch 的搜索引擎子集，经 `shim/` 的 C FFI 层调用

> 上游 FSearch 源码**一个字都没改**。`src/` 目录与原始 tarball 逐字节一致，
> 平台差异全部由**编译参数 + 垫片头 + 独立构建脚本**吸收。

---

## 快速上手

> **下面每条命令都从「仓库根」执行**（仓库根 = 含有 `src/`、`shim/`、`app/` 的那一层）。
> 命令里用 `--dir=app` / `--manifest-path` 自己指定目录，**不用拼 `cd`** ——
> 照着一条条往下敲不会跑飞（以前 `cd app && …` 连着敲会变成 `cd app/app`）。

```bash
# 1. 依赖（需要 Xcode 完整版 + Node ≥ 22.12 + Rust ≥ 1.77）
brew install pkg-config icu4c          # 已有就别装

# 2. 生成上游的 config.h（meson 产物）+ 构建 shim 生产 dylib
#    【为什么先要 meson 那步】上游的 src/fsearch_database.c 等文件
#    #include <config.h>，那个头文件由上游的 meson 生成（不是源码改动）。
#    —— 这一步【每个新 clone 都要跑一次】，产物落在 build/（已被上游 .gitignore 覆盖）。
make -C shim upstream-config
make -C shim                   # 产出 build-shim/libfsearch_shim.dylib

# 3. 装前端依赖
pnpm --dir=app install

# 4. 跑起来
pnpm --dir=app tauri dev
```

> ⚠️ `pnpm` 的目录参数**必须带等号**：`--dir=app` ✅ / `--dir app` ❌
> （不带等号时 pnpm 会把 `app` 当成要执行的命令，报 `Command "app" not found`）。

> ⚠️ **shim 的 dylib 必须自己编**（第 2 步）——
> Rust 侧的 `build.rs` **不会**替你调 `make`，它只负责把已经编好的
> `build-shim/libfsearch_shim.dylib` 链进去。忘了编 → 链接期报「找不到库」。

首次 `pnpm tauri dev` 会编译 Tauri 依赖（数分钟），慢是正常的。
可以先 `cargo fetch --manifest-path app/src-tauri/Cargo.toml` 预热。

### 打包

**用仓库自带的脚本**（不要直接 `pnpm tauri build`）：

```bash
# 在仓库根执行
bash app/scripts/bundle-app.sh
```

产物有两个，都在 `app/src-tauri/target/release/bundle/macos/`：

| 产物 | 是什么 | 给谁 |
|---|---|---|
| **`Lodestar.dmg`**（20M） | **磁盘映像** —— macOS 上把软件交给别人的标准容器 | **给别人**。双击挂载 → 把 `Lodestar.app` 拖到「应用程序」→ 装好。映像里还有一份「请先读我.txt」，说明首次打开被 Gatekeeper 拦时怎么办（右键 → 打开） |
| `Lodestar.app`（45M） | **应用程序包本体**（一个目录） | 自己用 / 想直接双击就跑 |

两个都是**自包含**的：18 个 Homebrew 依赖都嵌在 `Contents/Frameworks/` 里，
`@loader_path` 引用 —— **拷到任何 Mac 上都能直接跑**（实测：拷到 `/tmp` 跑，
加载 1389 个库、来自 `/opt/homebrew` 的 0 条）。

> **⚠️ 只支持 Apple Silicon**（arm64）。18 个依赖库全是 arm64 thin，
> Intel Mac 跑不了。跨架构支持是独立的一摊工作，另开 Task。

脚本干的事（每一步都有自检，末尾会打印「✅ 全部自检通过」）：

1. `make` —— 构建 shim 生产 dylib；
2. `pnpm tauri build --bundles app` —— 编译 Rust（**必须带
   `tauri/custom-protocol` feature**，否则前端没嵌进二进制 → `.app` 白页）
   + 打包；
3. 删掉 rpath 里的**仓库绝对路径**（分发时不能把家目录路径烤进去）；
4. **嵌入全部依赖**（`app/scripts/embed-deps.py`）—— 把 18 个 Homebrew
   依赖（glib / pango / harfbuzz / icu…）拷进 `Contents/Frameworks/`，
   并把对它们的引用从 `/opt/homebrew/opt/...` 绝对路径改成
   `@loader_path/...`，`.app` 因此能拷到**任何** macOS 机器上跑；
5. `codesign --force --deep --sign -` —— **ad-hoc 签名**（没有 Developer ID，
   别人首次打开仍需「右键 → 打开」）。

> **为什么必须走脚本而不是 `pnpm tauri build`**：`tauri bundle` 只打包，
> 既不编译 Rust 也不跑 `build.beforeBuildCommand`；而
> `feature tauri/custom-protocol` 只在 `tauri build` 的路径上才会被加上。
> 少了它 → 前端不嵌进二进制 → webview 去连 `http://localhost:1420`
> → 连不上 → **白页，但 Rust 侧引擎照跑**，看起来像「启动了」。
>
> 回归自检（去掉 feature 后必须变红）：
> ```bash
> bash app/scripts/bundle-app.sh --self-check <二进制路径>
> ```

**依赖嵌入**（第 4 步）的细节都在
[`scripts/embed-deps.py`](scripts/embed-deps.py)，
施工图见
`app/scripts/embed-deps.py` 负责把依赖拷进 Frameworks 并改写引用。
有三个**容易踩的坑**（都在脚本注释里写了）：

- **`@loader_path` / `@rpath` 形式的依赖也是硬依赖** ——
  ICU 的数据库 `libicudata.78.dylib`（31.8 MB）就是这种形式，
  只按「绝对路径」收集依赖会漏掉它 → libicuuc 加载失败 → **整个 app 起不来**；
- **磁盘真名 ≠ 引用名** —— `libicuuc.78.3.dylib` ↔ `libicuuc.78.dylib`，
  落盘必须用 `LC_ID_DYLIB` 的 basename；
- **同一个库会被不同字面路径引用** —— Homebrew 的 `opt/` 和 `Cellar/`
  两种写法混用，改写时得逐条 `-change`。

自检可以单独跑（**嵌入前必须红、嵌入后必须绿**）：

```bash
python3 app/scripts/embed-deps.py --check-only \
  app/src-tauri/target/release/bundle/macos/Lodestar.app
```

**分发给别人的机器**：`.app` 现在是**可迁移的**（实测
`DYLD_PRINT_LIBRARIES=1` 跑起来，`/opt/homebrew` 加载数 = 0，
19 个库全部来自 `.app` 内部）。但**没有 Developer ID 签名和公证**，
所以：

```bash
# 终端下载不触发 Gatekeeper 的隔离标记（浏览器下载会）
curl -LO https://<你的仓库>/releases/download/v0.1.0/Lodestar.app.zip
unzip Lodestar.app.zip && xattr -cr Lodestar.app   # 双保险
open Lodestar.app
```

用**浏览器**下载的话，Gatekeeper 会拦（ad-hoc 签名没公证），
用户需要「系统设置 → 隐私与安全性 → 仍要打开」，
或者 `xattr -cr /Applications/Lodestar.app`。

---

## 设置与持久化

设置存在 **Tauri store 插件**的文件里（不是自己手写的 JSON 读写）：

```
~/Library/Application Support/com.fsearch.macos/settings.json
```

```jsonc
{
  "settings": {
    "indexPaths": [],          // 索引目录；空 = 索引整个家目录
    "sortPref": null,         // 排序偏好
    "kindPref": null,         // 只看文件 / 只看目录
    "bookmarks": [],          // 书签（kind = "query" | "path"）
    "recentQueries": [],      // 最近搜索（最多 20 条，去重，最新在前）
    "windowWidth": null,      // 窗口大小/位置（物理像素）
    "windowHeight": null,
    "windowX": null,
    "windowY": null,
    "autoRefresh": true       // 文件变化时自动刷新索引（默认开）
  }
}
```

- **改设置要用应用里的界面**（标题栏「设置」→ 改目录 → 「应用并重建索引」）。
  手改这个文件也能被读到，但**别在应用运行时改** —— 应用会整体覆盖它。
- **读写分工**：`get_settings` / `set_settings` 两个 Tauri 命令是唯一入口
  （Rust 侧 `src-tauri/src/settings.rs`）。
  前端的写入一律走 `src/lib/settings-io.ts` 的 `patchSettings()`：
  它先**重新读盘上的真值**、再改一个字段、最后**整体写回**，
  并把并发写入串成一条队列 —— 避免「收藏书签把索引目录写没」这类事故。
- **`set_settings` 是整体替换，不是增量** —— 直接 `invoke("set_settings")`
  少传字段就会把没传的字段清空（最严重的是把索引目录写没）。
- 窗口位置/大小在**关窗时**由 Rust 侧写一次（`src-tauri/src/lib.rs`）。

---

## Phase 3 新增功能的用法

| 功能 | 入口 | 说明 |
|---|---|---|
| **索引目录设置** | 标题栏「设置」→「添加目录…」→ 选目录 → 「应用并重建索引」 | 留空 = 索引整个家目录。换目录会**重建引擎并重扫一遍**。路径非法时会报错且**不动旧引擎** |
| **语法帮助面板** | `⌘/` | 五个分组（函数字段 / 修饰符 / 运算符 / 通配符 / 转义）。**关键字可点** → 追加到搜索框。引擎不支持的项会**变灰**（提示语会说明当前引擎支不支持） |
| **书签与最近搜索面板** | `⌘B` | 三个分组：**最近搜索**（回车提交过的条件）、**查询书签**（保存的搜索条件）、**路径书签**（文件/目录 → 在访达里定位）。点查询类条目 = 填进搜索框并重搜 |
| **收藏当前查询** | 标题栏「收藏」 | 把当前查询串存成一条**查询书签**；再点一次移除。**只有回车提交过的查询**才会进「最近搜索」 |
| **窗口位置/大小** | 自动 | 记住上次的位置与大小，下次启动恢复 |
| **文件变化时自动刷新索引** | 标题栏「设置」→「索引目录」里的开关 | **默认开**。被索引的目录里有文件新建/修改/删除时自动重扫并重搜（0.8 秒防抖、两次之间至少隔 5 秒）。**索引整个家目录时建议关掉** —— 别的程序一直写文件会反复触发全量重扫 |

其它约定：

- `⌘/` 与 `⌘B` 是**唯一入口**（标题栏里没有对应按钮）——
  面板打不开时先确认焦点在应用窗口里。
- 裸 `/` **不会**弹面板 —— 搜索框里 `/` 是路径语法（`path:/Users/...`）。
- 帮助面板的说明文字改编自 FSearch 官方文档（CC BY-SA 4.0），
  数据在 `src/lib/syntax-help.json`，由 `scripts/gen-syntax-help.py` 生成。

---

## 目录结构

```
app/
├── package.json / vite.config.ts / tsconfig*.json / index.html
├── scripts/
│   ├── bundle-app.sh          打包成 Lodestar.app（含自检）
│   ├── embed-deps.py          把 Homebrew 依赖嵌进 Frameworks（便携版）
│   ├── gen-syntax-help.py     从上游文档生成 syntax-help.json
│   ├── make-icon.swift        AI 生成图 → macOS squircle 图标（四角透明）
│   └── white-to-alpha.swift   AI 生成图 → 透明底 + 保留投影（现图标那一套）
├── shared/
│   └── syntax-tables.json     引擎真实的语法表（shim 导出，用于交叉校验）
├── src/
│   ├── main.ts              Vue 入口
│   ├── App.vue              布局 + 输入状态 + 全局快捷键
│   ├── styles.css           深色主题（移植自 prototype/index.html）
│   ├── components/
│   │   ├── AppIcons.vue     SVG 图标集（symbol）
│   │   ├── SearchBar.vue    搜索框 + 查询串着色 + IME 处理
│   │   ├── ResultTable.vue  等高行虚拟滚动 + 游标分页 + 分组分隔行
│   │   ├── StatusBar.vue    命中数 / 耗时 / 索引状态
│   │   ├── PreviewCard.vue  预览卡（右键菜单 →「详情」进入）
│   │   ├── ContextMenu.vue  右键菜单（打开 / 在访达中显示 / 收藏 / 详情）
│   │   ├── SettingsPanel.vue 设置抽屉（索引目录 / 排除规则 / 应用重建）
│   │   ├── HelpCenterPanel.vue 帮助中心（速查 / 语法表 / 手册 三页，⌘/）
│   │   ├── SyntaxCheatsheet.vue 搜索语法速查卡（按「我想找什么」组织，可点）
│   │   ├── HelpPanel.vue    完整语法表（引擎的字段 / 修饰符 / 运算符）
│   │   ├── UserGuidePanel.vue 完整手册正文（构建期内联的 docs/USER-GUIDE.md）
│   │   └── BookmarkPanel.vue 书签与最近搜索面板（⌘B）
│   └── lib/
│       ├── types.ts         与 Rust 侧对齐的 TS 类型
│       ├── api.ts           invoke 封装
│       ├── tokenizer.ts     查询串着色（语法表来自引擎）
│       ├── paging.ts        分页 / 分隔行的纯函数
│       ├── highlight.ts     命中区间切段（G_MAXUINT 截断）
│       ├── settings.ts      设置的纯逻辑（书签 / 最近搜索的增删）
│       ├── settings-io.ts   设置的读-改-写（串行化，防「写没别的字段」）
│       ├── settings-panel.ts 设置面板的纯逻辑（三态 / 按钮文案 / 守卫）
│       ├── cheatsheet.ts    速查卡的数据与筛选纯函数
│       ├── help-center.ts   帮助中心的三个页签
│       └── syntax-help.ts   语法表的分组、关键字、引擎支持判定
└── src-tauri/
    ├── Cargo.toml           workspace 根（. + fsearch-core）
    ├── build.rs             tauri_build + App 二进制的 rpath
    ├── tauri.conf.json
    ├── capabilities/default.json
    ├── assets/
    │   ├── icon-1024.png    应用图标母版（改图标时从这里重新生成全套）
    │   └── tray.png         菜单栏托盘图标（单色模板图，编译进二进制）
    ├── icons/               tauri icon 生成的成品（打包时只取这里）
    ├── src/
    │   ├── settings.rs      设置的读写（store 插件）+ 引擎重建
    │   ├── commands.rs      Tauri 命令层（含共用的 do_rescan）
    │   ├── index_state.rs   扫描状态（begin/finish/fail/cancel_scan）
    │   ├── watcher.rs       FSEvents 文件监控（去抖 + 对账 + 后台重扫）
    │   └── logging.rs       运行日志出口（业务代码用）
    └── fsearch-core/        ← 与 src/ 同级！不是 src/ 的子目录
        ├── Cargo.toml
        ├── build.rs         链 dylib + fsearch-core 自己的 rpath
        └── src/{lib,ffi,types,engine,worker}.rs
```

---

## 应用图标

母版是 [`src-tauri/assets/icon-1024.png`](src-tauri/assets/icon-1024.png)（1024×1024，透明底）。
成品在 [`src-tauri/icons/`](src-tauri/icons/)，打包时 `tauri.conf.json` 只取
`32x32.png` / `128x128.png` / `128x128@2x.png` / `icon.icns`。

**改图标的三步**：

```bash
# 1. 把新母版（必须是 1024×1024 透明底 PNG）放到 assets/
cp /path/to/new-icon-1024.png app/src-tauri/assets/icon-1024.png

# 2. 生成全套成品
pnpm --dir=app tauri icon app/src-tauri/assets/icon-1024.png

# 3. 删掉它顺手生成的移动端图标（这是纯 macOS 项目，iOS/Android 那两目录用不上）
rm -rf app/src-tauri/icons/ios app/src-tauri/icons/android   # 用 mavis-trash
```

⚠️ **两个坑**：

- `tauri icon` 会额外生成 `ios/` 和 `android/` 目录，**不要提交**（纯 macOS 项目用不上，白占仓库体积）。
- `icons/icon.png` 现在是**完整应用图标**（不是裸标记）。
  菜单栏托盘图标是**另一个文件**（`assets/tray.png`，单色模板图），
  两者不要混。

**如果新图是 AI 生成的**（带白底 + 圆角方块 + 投影），别直接塞进 `tauri icon` ——
先用 `scripts/white-to-alpha.swift` 把白底转成透明（保留投影），
或用 `scripts/make-icon.swift` 裁成 macOS 标准的 squircle（去掉投影）：

```bash
# 白底 → 透明底（保留方块自带的圆角和投影，和现图标同一套格式）
swift app/scripts/white-to-alpha.swift 输入.png 输出.png 1024

# 白色画布 + 圆角方块 → 铺满画布的 macOS squircle（四角透明）
swift app/scripts/make-icon.swift 输入.png 输出.png
```

---

## FFI 层在哪

**唯一真相是 [`../shim/fsearch_shim.h`](../shim/fsearch_shim.h)。**

- `shim/` —— C FFI 适配层（Phase 1 交付），13 个公开 API，
  产物 `../build-shim/libfsearch_shim.dylib`（install name `@rpath/libfsearch_shim.dylib`）。
- `app/src-tauri/fsearch-core/` —— 手写 `extern "C"` 绑定 + 类型映射 +
  RAII 引擎封装 + 专用搜索线程。**不依赖 Tauri，可单独测试。**

```bash
# 在仓库根执行
cargo test --manifest-path app/src-tauri/Cargo.toml -p fsearch-core   # 12 个测试
```

### rpath 要发两次（不是冗余）

| 在哪发 | 形式 | 管谁 |
|---|---|---|
| `fsearch-core/build.rs` | `cargo:rustc-link-arg` | fsearch-core 自己的 lib 与 **test 二进制** |
| `app/src-tauri/build.rs` | `cargo:rustc-link-arg-bins` | **App 二进制** |

`cargo:rustc-link-arg` 只作用于**发出它的那一个 package 的目标**，
不会顺着依赖关系传播。少任何一边都会得到
`Library not loaded: @rpath/libfsearch_shim.dylib`。

---

## 内存与线程约定

- Rust 侧**只拷贝** C 字符串，释放**只走** `fs_result_free()`，绝不调 libc `free`。
- 搜索线程**必须串行** —— 引擎的 view 是有状态的
  （`fs_engine_page` 读的是上一次 `fs_engine_search` 留下的 view）。
- 「新搜索覆盖旧搜索」由**前端序列号**负责（`ResultTable.vue` 里的 `seq`）。

---

## 已知延后项（本阶段明确不做）

- 命令面板 ⌘K
- Developer ID 代码签名与公证（现在只 ad-hoc 签名）
- **Intel Mac / universal binary** —— 现在 18 个依赖库全是 arm64 thin，
  `.app` 只支持 Apple Silicon
- 多窗口/多标签页
- **GitHub Release 发布** —— 仓库目前无远端；Homebrew Cask 分发
  （注意 Homebrew 7 会给 cask 打隔离标记，cask 里需要 `postflight_steps` 剥隔离）
- 自动更新（updater）—— 需要签名密钥，依赖 Developer ID

> ✅ 已完成（2026-10-21）：`.app` 打包时把 dylib 的 **18 个 Homebrew 依赖**
> 一起嵌进 Frameworks —— 见上面「打包」一节。
> ✅ 已完成（2026-10-23）：产品名改为 **Lodestar**（`identifier` 同步改成
> `com.lodestar.macos`），并写了**一次性设置迁移** ——
> 老用户的书签 / 最近搜索 / 索引目录不会丢，见 `settings::migrate_legacy_store()`。
> ✅ 已完成（2026-10-23）：打包时把 `LICENSE`（GPL-2.0 全文）、
> `THIRD-PARTY-NOTICES.md` 和**完整对应源码压缩包**装进
> `Lodestar.app/Contents/Resources/licenses/`（GPL-2.0 §1/§3 要求）。


### 行为上的坑（照着计划里的实测记录）

- `path:` 的值是**子串匹配**，引擎**不做 `~` 展开**：
  `path:~` 恒 0 条，要用绝对路径 `path:/Users/xxx`。
- 上游**没有** `/.../` 正则语法，正则靠 `regex:` 修饰符开启。
- 空查询被 shim 短路成 0 条（不进引擎）。想「匹配全部」用 `*`。
- 高亮的 `length` 可能是 `G_MAXUINT - start`（≈42 亿，「高亮到结尾」），
  渲染时**必须按字符串长度截断**（见 `lib/highlight.ts`）。
- `highlights[]` 相对**文件名**、`path_highlights[]` 相对**完整路径** —— 基准不同。
- 未知字段（如 `name:foo`）会返回 `warning`，前端在状态栏显示。

---

## 跑测试

```bash
# 每条都【在仓库根执行】
cargo test --manifest-path app/src-tauri/Cargo.toml --workspace   # Rust：203 项（单元测试 + 5 个集成护栏）
make -C shim test                                            # C 垫片的契约测试（GLib TAP，41 项）
cd app && pnpm test                                          # 前端（vitest，20 个文件）
bash shim/check-upstream-pure.sh                             # 上游零改动
```

> ⚠️ **`cargo test` 要带 `--workspace`** —— `fsearch-core` 是本 workspace 的成员，
> 但根目录默认**只测根包**（`fsearch-app`，191 项），`fsearch-core` 的 12 项不会被跑到。
> 两条分开跑也对（`cargo test ...` + `cargo test ... -p fsearch-core`），但一条 `--workspace` 更省事。

三套合起来 **508 项**（Rust 203 · C 41 · 前端 264）。

| 套件 | 跑在哪 | 护的是什么 |
|---|---|---|
| `app/src-tauri/src/**` 的 `#[cfg(test)]` | Rust 单元测试 | 索引状态机、托盘、快捷键、watcher 契约 |
| `app/src-tauri/tests/*.rs` | Rust 集成护栏 | 菜单契约、z-index 层级、语法表、用户手册 |
| `shim/tests/test_shim.c` | C 契约测试 | 13 个导出 API 的行为（分页游标、高亮区间、排除规则、并发串行化…） |
| `app/src/**/*.test.ts` | 前端单测 | 快捷键解析、分词、设置读写、书签、语法高亮 |

单元测试就写在各源文件的 `#[cfg(test)] mod tests` 里 ——
它们**不进 release 构建**，只跟着 `cargo test` 跑。

### 【C 套件为什么单独一个 dylib】

测试钩子（`-DFS_SHIM_TEST`）必须编进 `fs_engine_search` 里，
才能让搜索线程在持有 `engine->lock` 的状态下暂停 → 带钩子的实现只能活在
**另一个** dylib 里（`build-shim-test/libfsearch_shim_test.dylib`）。

- 生产 dylib（`build-shim/libfsearch_shim.dylib`）**只导出公开 API** —— 钩子一个都进不去
- 测试 dylib = 公开 API **全部** + 3 个测试钩子（`shim/fsearch_shim_test.symbols`）
- ⚠️ **生产白名单加了新 API 时，测试白名单要同步加** ——
  少一个符号 = 那个函数在测试 dylib 里**不可见** → 测试一旦调用它就**链接失败**

### 【Makefile 的重试逻辑：断言失败【不是】偶发崩溃】

`make -C shim test` 会对偶发崩溃自动重试 3 次（上游 `src/` 的并发缺陷，改不了）。

但 GLib 的 TAP 模式在**断言失败**时也是「not ok → Bail out! → abort」，
退出码 **134** —— 与「被信号打死的崩溃」**一模一样**。
只看退出码会把一个**确定性的失败**重试 3 次、最后报成「偶发崩溃」，真失败被掩盖。

→ 所以重试前会先扫一遍 TAP 输出：出现 `not ok` / `Bail out!` = 断言失败 = **不重试，直接红**。

