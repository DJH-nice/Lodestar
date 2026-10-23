# FSearch macOS —— 后台常驻模式设计

> 日期：2026-10-22
> 状态：设计已确认，待写实施计划
> 范围：让 FSearch 在「不用的时候」以最小占用常驻后台，需要时一键唤起

---

## 1. 背景与目标

FSearch 目前是一个「打开就用、用完就关」的普通桌面应用：窗口一关，进程还在（macOS 的
默认行为），但 webview 已被销毁、也没有任何入口能把它叫回来。

本设计要解决的是：

> **让它在后台待命时几乎不占系统，需要用的时候瞬间能唤起。**

### 1.1 目标

| # | 目标 | 验收标准 |
|---|---|---|
| G1 | 后台常驻，不退出 | 点关闭按钮 → 窗口消失、进程仍在；快捷键能唤回 |
| G2 | 唤回够快 | 唤回到窗口可见 **< 150 ms** |
| G3 | 空闲几乎不耗 CPU | 空闲时 **< 0.5%** |
| G4 | 菜单栏图标 + 全局快捷键 | 默认 `⌥⌘Space`，**可自定义**；图标菜单含「打开 / 设置 / 退出」 |
| G5 | 开机自启 | 登录即驻留，设置里有开关 |
| G6 | 唤醒后索引是新的 | 唤起后 ≤ 1s 内结果反映空闲期间的变化 |

### 1.2 非目标

- ❌ 不做方案 B（丢界面、保引擎）—— 见 §3.2 的取舍分析
- ❌ 不做 Spotlight 式命令行面板、不做模糊匹配
- ❌ 不做剪贴板历史、不做计算器
- ❌ 不做多显示器 / 多空间的位置记忆（本轮只记「上次位置」）
- ❌ 不做「省电模式」手动开关（方案 B 那档已排除）
- ❌ 不改上游 `src/`（硬约束）
- ❌ 不动 `fsearch-core` 的 FFI 契约（`shim/fsearch_shim.h` 一个字不改）

---

## 2. 实测基线

**所有数字都是实跑测出来的，不是估算。** 测量对象是 2026-10-21 21:53 构建的 release
bundle（`app/src-tauri/target/release/bundle/macos/FSearch.app`），索引目录 `$HOME`
（25,608 ~ 27,042 项）。

### 2.1 内存

用 `footprint -p <pid>` 量（**不是** `ps` 的 RSS —— RSS 里绝大部分是共享 dylib 的
映射页，不是这个应用真正占的内存）。

一个运行中的 FSearch 实际是 **4 个进程**：

| 进程 | footprint | 是什么 |
|---|---|---|
| `fsearch-app`（主进程） | 21–26 MB | Rust 程序 + Tauri/wry + GLib + 搜索引擎 + 索引 |
| `com.apple.WebKit.WebContent` | 37–39 MB | Vue 界面（HTML/CSS/JS） |
| `com.apple.WebKit.GPU` | 16–18 MB | 界面渲染的 GPU 帮手 |
| `com.apple.WebKit.Networking` | 5–6 MB | 界面的网络帮手（实际用不到） |
| **合计** | **79–87 MB** | |

把主进程再拆开（用 `fsearch-core` 的独立探针量，见 §2.5）：

| 组成 | footprint | 说明 |
|---|---|---|
| **纯引擎 + 25,917 项索引** | **9.8 MB** | 文件索引本体 —— **很便宜** |
| Tauri/wry 运行时 + Rust + GLib | ≈11–16 MB | 倒推得出（21~26 − 9.8） |

> **关键结论：索引只占 9.8 MB，界面占 63 MB。**
> 要省内存该丢的是**界面**，不是索引。
>
> ⚠️ **勘误（2026-10-22 终审）**：上面这组数字是**早期实测**（索引 25,917 项时
> 索引本体 9.8 MB）。**当前实测**（索引 **79,594 项**）索引本体约 **19 MB** ——
> 差的不是「内存模型变了」，是**索引条数**变了（2.6 万 → 8 万）。
> 用户手册里写的是**实测值**（19 MB），以手册为准。

### 2.2 唤醒耗时

| 场景 | 耗时 | 怎么量的 |
|---|---|---|
| 隐藏 → 唤回（进程/界面/索引全活着） | **68–116 ms**（稳态 ~70–90 ms） | `NSRunningApplication.activate` → `CGWindowList` 轮询到窗口 on-screen |
| 冷启动 → 窗口出现在屏幕上 | **185–257 ms** | `Process.run()` → `CGWindowList` 轮询 |
| 引擎索引 `$HOME`（25,917 项） | 965 ms | 探针实测，**跑在后台线程，不阻塞窗口显示** |

### 2.3 空闲 CPU

用 `ps -o time=` 的差分法（`ΔCPU时间 / Δ墙钟`），采样窗口 10–15 秒：

| 场景 | CPU | 说明 |
|---|---|---|
| `autoRefresh` 开（`$HOME`，10 秒内触发 2 次自动重扫） | **5.8%** | 现在的常态 |
| `autoRefresh` 关（`$HOME`，收到 6 次文件事件、0 次重扫） | **0.27%** | **关掉重扫后** |
| 安静目录（222 项） | **0.07%** | 兜底循环的成本 |
| **单次自动重扫耗时** | **422–720 ms** | 25,609 项全量重扫 |

> **关键结论：CPU 大头是「重扫」不是「监听」。**
> FSEvents 监听本身很便宜（0.27% 那档），全量重扫才是 0.4–0.7 秒的大头。

### 2.4 「隐藏」不省内存

隐藏窗口后主进程 footprint = 26 MB（可见时 24 MB）—— **基本没变**。
webview 还活着，63 MB 照占。隐藏只是「看不见」，不是「不存在」。

### 2.5 怎么复现这些数字

三件工具，都是现成的：

1. **引擎探针**（`fsearch-core` 的 example）—— 只建引擎、扫索引、不建窗口，
   用来把「引擎 + 索引」从整个应用里单独量出来：

   ```rust
   // app/src-tauri/fsearch-core/examples/mem_probe.rs
   let (engine, indexed) = Engine::new(&[path.as_path()]).expect("建引擎失败");
   loop { std::thread::sleep(Duration::from_secs(1)); std::hint::black_box(&engine); }
   ```

   > ⚠️ 这个探针在 2026-10-21 的 `80eecb8 refactor: 只留核心代码` 里被一并删了
   > （那次清理把「开发期脚手架」都清了）。实施阶段要重新加回来。

2. **`footprint -p <pid>`** —— 量真实占用。
3. **CGWindowList 轮询的 Swift 小工具** —— 量唤回耗时。逻辑是：
   `CGWindowListCopyWindowInfo([.optionOnScreenOnly, .excludeDesktopElements], ...)`
   里找 owner PID 匹配、且 `kCGWindowIsOnscreen == true`、且宽高 > 100 的窗口。

   > 这套工具在 2026-10-21 已在 `/tmp/fs-measure/` 跑通（`winwatch2` / `wake`），
   > 但 `/tmp` 会被清空 —— 实施时要重新写，或者收进 `app/scripts/`。

---

## 3. 架构总览

### 3.1 选型：方案 A（全部保留）

评估过三个方案：

| 方案 | 空闲内存 | 唤醒 | 空闲 CPU | 实现风险 |
|---|---|---|---|---|
| **A 全部保留** | 87 MB | 70–90 ms | 0.27% | 低 |
| B 丢界面、保引擎 | 24 MB | ~200 ms（估算） | ~0.1% | **高** |
| C 连索引也丢 | 14 MB | 185–257 ms + 索引 1s | 0 | 中 |

**选 A。** 理由：

1. **唤醒速度不缺。** A 已经只要 70–90 ms。B 省下的 63 MB 换来的是唤醒慢一倍多
   外加实现风险（Tauri v2 里销毁 + 重建 `WebviewWindow` 容易出「事件监听丢失」
   「卡在白屏」这类坑）。
2. **87 MB 对常驻工具是正常水位。** 16 GB 机器上约占 0.5%；
   Alfred 约 50 MB、Raycast 100 MB+。
3. **真正的问题不是内存，是 CPU。** 5.8% → 0.27% 这一刀，
   在 A 里就能砍掉，而且改动小。

**C 已排除**：只比 B 多省 10 MB，却让每次唤醒多等 1 秒重建索引，明显亏。

**B 的唤醒 200 ms 是唯一的估算值**（其余全是实测）。它没有进入决策的关键路径 ——
但如果将来要重开这个话题，这个数字需要先用一个 spike 验掉。

### 3.2 两个状态

| 状态 | 窗口 | 进程 | webview | 索引 | watcher |
|---|---|---|---|---|---|
| **待用**（Ready） | 可见 | 活 | 活 | 活 | 监听 + 重扫 |
| **空闲**（Idle） | 隐藏，**或可见但不在前台** | 活 | 活 | 活 | **只监听，不重扫** |
| （退出） | — | 结束 | 结束 | 结束 | 结束 |

> **空闲态的不变式不是「窗口看不见」，而是「webview 活、索引活、watcher 不重扫」。**
>
> 空闲有两种成因，窗口的可见性不一样：
> - 窗口被收起（点关闭按钮 / 托盘收起）→ 窗口不可见
> - 窗口还开着但你切去了别的 app，超过了 20 秒 → 窗口**仍然可见**
>
> 两种成因在实现上是同一个状态，区别只在「什么时候进入空闲」的判定条件。

> 只有两个状态，因为「收起」和「空闲」在方案 A 里不是两套机制 ——
> 进入空闲后，webview 和索引都还在，唯一变的是 watcher 的重扫闸门。
> 方案 B 才需要拆出第三个状态（丢 webview）。

### 3.3 模块结构

```
app/src-tauri/src/
├── lifecycle.rs   ← 新增：状态机 + 空闲判定 + 唤起序列
├── tray.rs        ← 新增：菜单栏图标
├── shortcut.rs    ← 新增：全局快捷键注册 / 改键 / 冲突处理
├── watcher.rs     ← 改：decide_auto_rescan 加两道闸
├── settings.rs    ← 改：Settings 加两个字段
└── lib.rs         ← 改：装托盘、装快捷键、窗口事件

app/src/
├── App.vue                       ← 改：listen("app-awake") → 聚焦搜索框
├── components/SettingsPanel.vue  ← 改：快捷键录制 + 开机自启开关
└── lib/shortcut.ts               ← 新增：KeyboardEvent → 快捷键字符串
```

---

## 4. 关键设计决定

**D1 · 空闲时暂停的是「重扫」，不是「监听」。**
FSEvents 流在内核里，监听本身只花 0.27%；重扫才是 422–720 ms 的大头。
保留监听 = 唤醒时不用重新注册 watch = 唤醒更快、代码更简单。

**D2 · 唤醒时的补扫必须主动触发，不能等「下一个文件事件」。**
如果空闲期间压根没文件变化，就没有事件 → 永远不重扫 → 索引一直是旧的 → G6 不成立。
所以唤醒路径要**显式**打脏标志 + 强制重扫。

**D3 · 补扫跑在后台线程，窗口不等它。**
唤起 → 70–90 ms 窗口就出来了，用户可以立刻打字；
结果在 ~0.5 s 后通过现有的 `index-auto-refreshed` 事件自动刷新（前端已会重搜）。

**D4 · 空闲状态的切换要幂等。**
反复隐藏/唤起不能漏事件、不能重复扫。
`decide_auto_rescan` 已有「不清脏标志」的模式（闸一），空闲闸照抄这个语义。

**D5 · 快捷键注册失败必须可见。**
`⌥⌘Space` 可能被别的软件占着。注册失败要在**托盘菜单**里明确显示，
并在设置里让用户改 —— 静默失败会让人以为功能坏了。

**D6 · 空闲判定必须放 Rust 侧。**
webview 的渲染进程在应用不前台时会被 macOS 限流（定时器与 Tauri IPC 回调一起停摆，
看起来像「应用卡死」）。前端定时器不可信，判定只能在 Rust 侧做。

**D7 · 唤起时要检查窗口位置。**
设置里存着 `window_x/y`（物理像素）。用户换显示器或改分辨率后，那个坐标可能落在
屏幕之外 —— 表现是「按了快捷键，窗口弹不出来」。要做**预防**，不是等用户报。

---

## 5. 组件设计

### 5.1 `lifecycle.rs` —— 状态机

```rust
pub enum Phase { Ready, Idle }
```

职责：

| 职责 | 做什么 |
|---|---|
| 状态 | 持有 `Phase`，切换时通知 watcher 暂停 / 恢复重扫 |
| 窗口 | 隐藏（`win.hide()`）/ 唤起（`show()` → `set_focus()` → emit `app-awake`） |
| 空闲判定 | 「窗口隐藏」立即空闲；「不在前台持续 20 秒」也空闲 |
| 兜底 | 唤起时检查窗口位置是否还在当前屏幕内，不在就拉回主屏幕 |

**空闲判定的纯函数**（好测）：

```rust
/// 失焦多久算空闲。20 秒。
pub const IDLE_GRACE: Duration = Duration::from_secs(20);

pub fn should_be_idle(
    visible: bool,             // 窗口可见吗
    frontmost: bool,           // 应用在前台吗
    since_foreground: Duration, // 距上次在前台多久
) -> bool {
    if !visible { return true; }              // 隐藏 = 立刻空闲
    if frontmost { return false; }           // 在用 = 不空闲
    since_foreground >= IDLE_GRACE            // 失焦够久 = 空闲
}
```

由一个专门的 watchdog 线程每 1 秒 tick 一次调用它（成本与现有 1 秒对账循环同级别，
0.07% 那档）。**为什么是 20 秒**：比 30 秒更省，又不至于让「切窗口复制个路径回来」
这种 3 秒的往返误判。误判的代价只是多一次 0.5 秒的补扫，不影响功能。

**唤起序列**（用户唯一能感知的路径）：

```
快捷键 / 托盘左键
  → Phase = Ready
  → win.show()                    ← 先把窗口弄出来（用户等的就是这个）
  → win.set_focus()
  → emit("app-awake")
       → 前端 searchBar.focus()   ← 选中已有查询，直接打字可替换
  → watcher.request_catchup_scan() ← 再要补扫（见下）
  → （后台线程里补扫完 → index-auto-refreshed 事件 → 结果自动刷新）
```

> **为什么「先出窗、后补扫」**：补扫是 0.4–0.7 秒的 CPU 活，如果和 webview 的
> 首次绘制抢 CPU，窗口出���会被拖慢。**用户等的是窗口，不是索引。**
> 窗口 70–90 ms 就出来了，结果在 ~0.5 s 后自己刷新。

**托盘左键的两种行为**（Raycast 手感）：

> - FSearch 是前台且窗口可见 → **收起**到后台（等同点关闭按钮）
> - 其他情况（窗口藏着 / 被别的窗口盖住 / 你在别的 app）→ **唤起到前台**

### 5.2 `tray.rs` —— 菜单栏图标

```rust
TrayIconBuilder::with_id("main")
    .icon(Image::from_bytes(include_bytes!("../icons/tray.png"))?)  // 需 image-png feature
    // ↑ 勘误（2026-10-22 终审）：实际路径是 `../assets/tray.png`
    //   （`icons/` 是 Tauri bundle 的 app 图标目录，托盘资源另放 `assets/`；
    //    施工图见 `app/src-tauri/src/tray.rs`）。
    .icon_as_template(true)          // macOS 自动适配深浅色菜单栏
    .show_menu_on_left_click(false)  // ← 默认是 true，必须显式关掉
    .menu(&menu)
    .on_menu_event(...)              // 「打开 / 设置… / 退出」
    .on_tray_icon_event(|tray, event| {
        // 匹配 button_state == Up，否则按下和抬起会触发两次
        if let TrayIconEvent::Click {
            button: MouseButton::Left,
            button_state: MouseButtonState::Up,
            ..
        } = event {
            lifecycle::toggle(tray.app_handle());
        }
    })
    .build(app)?;
```

**图标资源**：复用现有 `#i-search` 那个放大镜字形，渲染成 **22×22 的黑色 + alpha PNG**
（`icon_as_template(true)` 后 macOS 会自动反色适配深色菜单栏）。
用 `qlmanage -t` 从 SVG 渲染，不引新工具链。

**菜单项**：

| 菜单项 | 行为 |
|---|---|
| 打开 FSearch | 唤起窗口（若已在前台则收起） |
| 设置… | 唤起窗口 + emit `open-settings` → 前端打开设置抽屉 |
| 退出 | `app.exit(0)` —— **真的退出**，不是隐藏 |

### 5.3 `shortcut.rs` —— 全局快捷键

用 `tauri-plugin-global-shortcut`（2.4.1，tuna 镜像可拉，已验证）：

```rust
app.global_shortcut().register(shortcut)?;   // 冲突时返回 Err ← 要用
app.global_shortcut().on_shortcut(shortcut, |app, _| lifecycle::wake(app));
app.global_shortcut().unregister(old);        // 改键时先注销旧的
```

**改键流程**：注销旧的 → 注册新的 → **成功才写盘**。
失败就保持旧键不变，并把错误报给前端（不能让设置和实际状态对不上）。

**快捷键字符串语法**（`global-hotkey` 的 `parse_hotkey`）：

- 修饰键必须在前，只能有一个主键：`"Option+Cmd+Space"` ✅ / `"Ctrl+Shift+C+A"` ❌
- 支持的修饰键名：`OPTION`/`ALT`、`CONTROL`/`CTRL`、`COMMAND`/`CMD`/`SUPER`、`SHIFT`、
  `CMDORCTRL`（macOS 上解析为 Command）
- 默认值：`Option+Cmd+Space`

### 5.4 前端改动

| 文件 | 改什么 |
|---|---|
| `App.vue` | `listen("app-awake")` → `searchBarRef.value?.focus()`（`SearchBar` 已 `defineExpose({ focus, focusEnd })`） |
| `App.vue` | `listen("open-settings")` → 打开设置抽屉 |
| `SettingsPanel.vue` | 加「全局快捷键」行（显示当前键 + 「录制」按钮） |
| `SettingsPanel.vue` | 加「开机自启」开关 |
| `lib/shortcut.ts`（新） | `KeyboardEvent` → Tauri 快捷键字符串的转换与校验 |
| `lib/types.ts` | `Settings` 类型加两个字段 |

**录制交互**：

```
设置面板 → 快捷键那行 → 点「录制」
                             ↓
                   按钮变成「请按下组合键…」
                   （此刻按下什么键都会被记下来）
                             ↓
      按下 ⌥⇧K  → 立刻显示成「Option+Shift+K」→ 自动保存
```

**校验规则**（必须有修饰键）：

- ✅ `Option+Cmd+Space`、`CmdOrCtrl+Shift+K`
- ❌ `Space`（纯字母/功能键会和正常打字打架）
- ❌ 只按了修饰键（还没按主键）

### 5.5 设置项新增

```rust
/// 全局快捷键。`None` = 不注册（用户主动关掉）。
/// 格式是 global-hotkey 的语法：修饰键在前、只能有一个主键。
///   "Option+Cmd+Space" / "CmdOrCtrl+Shift+F12"
#[serde(default = "default_global_shortcut")]
pub global_shortcut: Option<String>,

/// 开机自启（写登录项）。
#[serde(default)]
pub launch_at_login: bool,
```

`serde(default)` 保证老的 `settings.json` 能正常读出来（这个项目踩过「老设置读不出来」的坑）。

**用户把快捷键整个关掉（`global_shortcut: None`）时**：
不注册任何全局快捷键，托盘图标照常工作，唤起只靠点图标（或从 Dock 点开）。
这跟「注册失败」是两回事 —— 注册失败是**冲突**（别的软件占着），
用户关掉是**主动选择**。两者在 UI 上要能区分开。

### 5.6 开机自启

用 `tauri-plugin-autostart`（2.7.0，已验证可拉）：

```rust
app.autolaunch().enable()?;    // 写登录项
app.autolaunch().disable()?;
```

`.app` 用 `MacosLauncher::LaunchAgent`。设置里的开关直接映射过去，**不自己写 plist**。

---

## 6. 空闲与 CPU 策略

### 6.1 watcher 加两道闸

`watcher.rs` 的 `decide_auto_rescan` 现在是 4 道闸的纯函数（去抖 800ms、最小间隔 5s、
覆盖判断、开关），加第 6 道：

```rust
pub struct RescanInputs {
    // ... 现有字段
    /// 【新增】应用是否处于空闲态（窗口隐藏 / 失焦够久）
    pub idle: bool,
    /// 【新增】外部要求立刻补扫（唤醒时置位，用完即清）
    pub force: bool,
}

pub enum RescanDecision {
    // ... 现有变体
    /// 【新增】应用空闲 → 不重扫，但**保留脏标志**
    Paused,
}
```

闸的顺序（很重要）：

```
1. dirty == false              → Wait
2. scan_in_flight              → Wait        （不清脏标志）
3. force                       → Rescan      （跳过静默期与最小间隔）
4. idle                        → Paused      （不清脏标志 ← 唤醒后自然重扫）
5. 覆盖判断 / 静默期 / 最小间隔 / 开关        （现有四道闸）
```

- 第 2 道在「扫描在飞」**之后** —— 不能打断正在跑的那趟扫描
- **第 3 道（`force`）必须在第 4 道（`idle`）之前** —— 这是竞态安全：
  唤醒路径会先设 `Phase = Ready`、再置 `force`。如果这两步之间 watcher 恰好读到
  旧的 `idle = true`，`force` 就会被 `idle` 挡掉，补扫要等到下一个事件才发生
  （G6 就不成立了）。**`force` 优先 = 「用户刚叫醒我，立刻补扫」的意思永远压过
  「我以为你在空闲」。**
- 第 4 道在静默期 / 最小间隔**之前** —— 否则空闲两小时后刚唤醒，还得再等 5 秒最小间隔

`force` 是一次性标志：触发重扫后立刻清掉（否则会反复补扫，反而更耗 CPU）。

### 6.2 唤醒时的主动补扫

```rust
// lifecycle.rs
watcher::request_catchup_scan();   // 置 force = true + dirty = true
```

下一轮决策直接 `Rescan`（前提是没有扫描在飞）。

### 6.3 与 `autoRefresh` 开关的关系

| `autoRefresh` | 空闲暂停 | 实际行为 |
|---|---|---|
| **开**（默认） | 生效 | 你在用 → 正常重扫；你不用 → 暂停，唤醒时补扫一次 |
| **关** | 无意义 | 本来就不自动重扫，没有可暂停的东西 |

**这两个不是叠加的开关，是「自动重扫」在两种状态下的不同表现。**
`autoRefresh` 关着时，空闲暂停机制不产生任何额外影响。

> 现有 `auto_refresh` 字段的注释写着「CPU 消耗不可接受」——
> 说明这个坑**之前就被发现了，但解法是让用户手动关**。
> 本设计等于把这个判断自动化了：用户不用管，应用自己知道什么时候该干活。

---

## 7. 错误处理与降级

### 7.1 降级原则

> **任何后台增强功能失败，都不能影响主功能（搜索）。**

| 断了 | 还剩什么 |
|---|---|
| 托盘图标建不起来 | 全局快捷键 + Dock 图标 |
| 全局快捷键被占用 | 托盘图标 + Dock 图标 |
| 快捷键录制 / 改键失败 | **保留旧键继续用**（设置与实际状态不许对不上） |
| 空闲暂停失效 | 退化成现在的行为（照常重扫）—— 只是多烧 CPU，不出错 |

**实现约束**：托盘和快捷键的创建**不能 panic**，也不能 `expect()` ——
失败就记日志 + 提示，应用照常跑。

### 7.2 逐个失败点

| 失败点 | 现象 | 处理 | 用户能看到吗 |
|---|---|---|---|
| 托盘创建失败 | 菜单栏没图标 | 记日志，应用照常跑 | 启动时弹一次提示（可关） |
| 快捷键被占用 | 按快捷键没反应 | 托盘菜单项显示「⚠️ 快捷键未生效（已被占用）」，设置里红字 | **必须能看到**（D5） |
| 快捷键字符串非法 | — | **写盘前**校验，保留旧键 | 设置里红字 |
| 开机自启写登录项失败 | 开机没自动起 | 设置里显示错误，开关回弹成关 | 能 |
| **窗口位置跑到屏幕外** | 按快捷键「弹不出来」 | 唤起时检查位置，不在当前屏幕内就拉回主屏幕 | 不用管，自动修 |
| 唤醒时补扫失败 | 索引是旧的 | **不阻断唤起**；下一轮 watcher 会重试；状态栏显示错误 | 状态栏 |
| 补扫时用户正在输入 | 结果列表跳动 | 走现有的 `index-auto-refreshed` → 前端重搜 | 正常 |

---

## 8. 测试策略

### 8.1 现有基线（不能破坏）

```bash
cd app/src-tauri && cargo test -p fsearch-core    # 必须全绿
cd app && pnpm vitest run                          # 必须全绿
cd app && pnpm build                               # vue-tsc + vite 必须过
cd .. && bash shim/check-upstream-pure.sh          # 上游零改动
```

### 8.2 自动测试

| 层 | 测什么 | 形式 |
|---|---|---|
| Rust 纯函数 | `should_be_idle` 真值表（隐藏 / 前台 / 失焦 19s / 失焦 21s） | 单元测试 |
| Rust 纯函数 | `decide_auto_rescan` 新增的 `idle` / `force` 两道闸 | 扩展现有表驱动测试 |
| Rust 纯函数 | 快捷键字符串解析 + 「必须有修饰键」校验 | 单元测试 |
| 前端 | `KeyboardEvent` → `"Option+Cmd+Space"` 的转换 | vitest（happy-dom） |
| 前端 | 录制时拒绝纯字母键 | vitest |
| 前端 | 收到 `app-awake` 后聚焦搜索框 | 组件测试（`App.test.ts` 已有 `createApp` + `h` 模式） |
| 前端 | 老 `settings.json`（无新字段）能正常读出来 | vitest |

### 8.3 真机验收（自动测不了，必须跑脚本）

| 验收项 | 怎么验 |
|---|---|
| 菜单栏图标出现，深/浅两种菜单栏下都看得清 | 真机截图（`icon_as_template` 应自动适配） |
| 快捷键在**别的 app 前台**时也能唤起 | 真机脚本 + 焦点切换 |
| **隐藏后 webview 还活着** | 隐藏后看进程树：4 个 WebKit 进程还在 + footprint 不掉到 0 |
| 唤起后窗口正常显示、不是白屏 | 真机截图 |
| 隐藏 → 唤回耗时 < 150 ms | `CGWindowList` 轮询计时（§2.5 那套） |
| 空闲 CPU < 0.5% | `ps -o time=` 差分法（§2.5 那套） |

> 这套脚本 2026-10-21 已在 `/tmp/fs-measure/` 跑通（`winwatch2` / `wake` / `footprint` 计时），
> 但 `/tmp` 会被清空 —— 实施时要重新写，或者收进 `app/scripts/`。

---

## 9. 风险

| 风险 | 概率 | 影响 | 缓解 |
|---|---|---|---|
| **`prevent_close` 漏了** → webview 被销毁，唤起后白屏 | 中 | **致命** | 单测钉住 + 真机验「隐藏后进程树和 footprint 不变」 |
| 空闲判定漏了某个窗口事件 → 一直不空闲 → CPU 白烧 | 中 | 中 | 兜底：watchdog 线程**同时**看「窗口可见性」和「前台状态」，任一命中就判空闲 |
| 补扫标志没清 → 反复补扫 → 反而更耗 CPU | 低 | 中 | `force` 是一次性标志，用完即清 |
| 20 秒失焦判定偏敏感 → 切窗口时多扫 | 中 | 低 | 补扫幂等，实测 0.5 s CPU，可接受 |
| 托盘图标在深色菜单栏上看不清 | 低 | 低 | `icon_as_template(true)` + 真机两种外观看一眼 |
| 快捷键和其他软件撞了 | **高** | 中 | 冲突提示 + 录制重设 |
| 新增 3 个 crate 让打包体积变大 | 低 | 低 | 都是小 crate；打包后对比 `.app` 体积 |

---

## 10. 已敲定的决策清单

| # | 决策 |
|---|---|
| 1 | 架构 A：全部保留（87 MB），唤醒 70–90 ms |
| 2 | 关闭按钮 = 隐藏到后台，不退出 |
| 3 | 快捷键 `⌥⌘Space`，**录制式**自定义 |
| 4 | 菜单栏图标：左键唤起（已在前台则收起）/ 右键菜单（打开·设置·退出） |
| 5 | 空闲判定：隐藏 = 立即；**失焦 20 秒** = 空闲 |
| 6 | 空闲只暂停**重扫**，不暂停**监听**；唤醒时**主动补扫**一次（不等文件事件） |
| 7 | 补扫在后台线程，**不阻塞窗口**（70 ms 出窗，0.5 s 后结果自动刷新） |
| 8 | 开机自启，设置里带开关 |
| 9 | 空闲判定放 **Rust 侧**（前端定时器会被 macOS 限流） |
| 10 | 窗口位置兜底：唤起时越界就拉回主屏幕 |

---

## 11. 实施切分建议

这是一个完整的单一功能（后台常驻模式），可以走一份实施计划。大致切成 6 个 Task，
**顺序有依赖，不能乱**：

| Task | 内容 | 依赖 |
|---|---|---|
| 1 | **托盘图标**（`tray.rs` + 图标资源） | 无 —— 先让「有图标」这件事成立 |
| 2 | **全局快捷键**（`shortcut.rs` + 依赖接入） | 无 —— 与 Task 1 独立，可并行 |
| 3 | **lifecycle 状态机 + 关闭按钮隐藏**（`lifecycle.rs`，`prevent_close` + `hide()`） | 依赖 1、2（要有唤起入口） |
| 4 | **空闲判定 + watcher 两道闸 + 唤醒补扫** | 依赖 3 |
| 5 | **设置项 + 录制 UI + 开机自启** | 依赖 2（要能改键） |
| 6 | **真机验收** | 全部 |

> Task 3 是整个功能的**地基**：没有 `prevent_close` + `hide()`，
> 后面所有的「唤起」都无从谈起（webview 已经被销毁了）。
> 建议 Task 1~3 做完先做一次真机验收（隐藏后进程树不变），再往下走。

---

## 附录 A · Tauri v2.12.4 API 核对结论

写代码前已逐条核对过本地 crate 源码（`~/.cargo/registry/src/*/tauri-2.12.4`）：

| 需要的 API | 位置 | 结论 |
|---|---|---|
| `WindowEvent::CloseRequested { api }` → `api.prevent_close()` | `src/app.rs:118` / `src/app.rs:103` | ✅ 有 |
| `WindowEvent::Focused(bool)` | `src/app.rs:127` | ✅ 有 |
| `TrayIconBuilder::icon_as_template(bool)` | `src/tray/mod.rs:295` | ✅ 有 |
| `TrayIconBuilder::show_menu_on_left_click(bool)` | `src/tray/mod.rs:319` | ✅ 有，**但默认是 `true`**，左键唤起必须显式关掉 |
| `TrayIconEvent::Click { button, button_state, .. }` | `src/tray/mod.rs` | ✅ 有；`MouseButton::Left` / `MouseButtonState::Up` |
| `Image::from_bytes` 读 PNG | `src/lib.rs` | ⚠️ 需要 `tauri` 的 `image-png` feature |
| 快捷键字符串语法 | `global-hotkey-0.8.0/src/hotkey.rs` | ✅ `Option`/`Alt`、`Control`/`Ctrl`、`Command`/`Cmd`/`Super`、`Shift`、`CmdOrCtrl` |

依赖可获得性（tuna 镜像 `cargo fetch` 实测）：

| crate | 版本 | 状态 |
|---|---|---|
| `tauri-plugin-global-shortcut` | 2.4.1 | ✅ 可拉（依赖 `global-hotkey 0.8.0`） |
| `tauri-plugin-autostart` | 2.7.0 | ✅ 可拉（依赖 `auto-launch 0.6.0`） |
| `tauri` 的 `tray-icon` feature | 2.12.4 | ✅ `muda` / `tray-icon` 已在本地缓存 |

> ⚠️ 加插件会把 `tauri` 从 2.12.4 顶到 2.12.5（`cargo fetch` 拉到的是 2.12.5）。
> 实施时要在 `Cargo.lock` 里确认版本，并跑一遍现有测试确保没被破坏。
