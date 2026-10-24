//! 持久化设置 —— 走 tauri-plugin-store 的 store 文件。
//!
//! 【为什么用 store 插件，而不是自己读写 JSON】
//!   启动时（`setup` 阶段）webview 还没跑，前端拿不到任何东西 ——
//!   但**索引目录必须在那一刻就知道**，否则引擎建不出来。
//!   所以这个文件的读取方只能是 Rust。
//!
//! 【为什么【只】用 Rust 侧 API】
//!   store 插件的 Rust API（`app.store(path)`）是**同步**的、且**不走权限系统**
//!   （crate 2.4.4 `src/lib.rs:310` → `StoreBuilder::build()`，全程无 ACL 检查）。
//!   前端侧那套 `load()/set()` 走 IPC、要 `store:default` 权限。
//!   本项目**不需要**前端直接碰这个文件 —— 前端要改设置就走我们自己的
//!   `get_settings` / `set_settings` 命令。
//!   → **单一写入方**，不会出现两边同时写互相覆盖。

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::{AppHandle, Runtime};
// `app.state::<T>()` 是 `Manager` trait 的方法 —— trait 不在作用域里就报
// error[E0599]: no method named `state` found for reference `&AppHandle<R>`
use tauri::Manager;
use tauri_plugin_store::StoreExt;

/// store 文件名（相对 app config 目录）与顶层 key。
const STORE_FILE: &str = "settings.json";
const SETTINGS_KEY: &str = "settings";

/// **旧版**的配置目录名，**按时间倒序**（最近的在前）。
///
/// 【为什么需要它】
///   `tauri.conf.json` 的 `identifier` 决定 tauri-plugin-store 把
///   `app.store("settings.json")` 落在哪个目录 —— 那个目录名**就是 identifier**。
///   → 改 identifier = 换目录 = 老用户的书签 / 最近搜索 / 窗口位置 /
///     索引目录**全都读不到了**（用户看到的是「设置被清空」）。
///
/// 【为什么是「一条链」而不是一个】
///   identifier 改过三次：
///     `com.fsearch.macos` →（2026-10-23 改名 Lodestar）→ `com.lodestar.macos`
///     →（2026-10-23 改名 Sextant）→ `com.sextant.macos`
///     →（2026-10-24 改回 Lodestar）→ `com.lodestar.macos`
///   每一代的用户都可能在用，所以**每一代都要能被迁过来**。
///   顺序：**最近的优先**（`com.sextant.macos` 是当前用户实际在用的那一代 ——
///
///   ⚠️ **当前 identifier 本身就是 `com.lodestar.macos`**，
///   所以它**绝不能出现在这个列表里** ——
///   出现了的话，迁移会拿「自己」当源拷贝到自己身上（自我覆盖）。
///   （2026-10-24 那次改名回退就踩过这个坑：列表里留着 `com.lodestar.macos`，
///   而新目录恰好也叫它 → 迁移在自环。）
///
/// 【为什么只迁最靠前的那一个】
///   同时存在两个旧目录的情况几乎不存在（迁移时旧目录会被改名成 `.bak`），
///   而「挑最靠前的那个」符合直觉：那是用户最后一次真正在用的那份设置。
const LEGACY_CONFIG_DIR_NAMES: &[&str] = &["com.sextant.macos", "com.fsearch.macos"];

/// 把**旧目录**里的设置迁移到**新目录**（一次性、幂等）。
///
/// 【什么时候跑】`setup` 的最开头 —— 必须早于任何一次 `settings::load()`。
///   （首次读设置发生在启动线程里，见 `lib.rs` 的 setup。）
///
/// 【什么时候不跑】
///   - 新目录里已经有 `settings.json` → **不动**。
///     （不能覆盖：用户可能已经在新版本里改过设置了。）
///   - 旧目录里没有 `settings.json` → 首次安装，没有旧数据，不动。
///
/// 【旧目录怎么处理】改名成 `<旧名>.bak`，**不删**。
///   - 不删：万一迁移出问题（拷贝到一半被杀），用户的东西还在。
///   - 改名而不是留着：留着的话，旧版 app 还能继续读写那个目录，
///     两边的设置会各走各的，下次启动又「变回去」了。
///
/// 【失败怎么办】**绝不让应用因为迁移失败就起不来**。
///   迁移只是「锦上添花」，失败了就当没有旧设置，用默认设置跑。
pub fn migrate_legacy_store<R: Runtime>(app: &AppHandle<R>) {
    let Ok(new_dir) = app.path().app_config_dir() else {
        eprintln!("[lodestar] 迁移设置：取不到 app config 目录，跳过");
        return;
    };
    let new_file = new_dir.join(STORE_FILE);
    if new_file.exists() {
        // 新目录已经有设置了 —— 一个字都不动。
        return;
    }
    let Some(parent) = new_dir.parent() else {
        return;
    };

    // 按「最近的优先」找第一个还留着的旧目录。
    let Some(legacy_name) = LEGACY_CONFIG_DIR_NAMES
        .iter()
        .find(|name| parent.join(name).join(STORE_FILE).is_file())
    else {
        // 首次安装 —— 没有任何旧数据。
        return;
    };
    let legacy_dir = parent.join(legacy_name);
    let legacy_file = legacy_dir.join(STORE_FILE);

    if let Err(e) = std::fs::create_dir_all(&new_dir) {
        eprintln!("[lodestar] 迁移设置：建新目录失败: {e}");
        return;
    }
    if let Err(e) = std::fs::copy(&legacy_file, &new_file) {
        eprintln!("[lodestar] 迁移设置：拷贝失败: {e}");
        return;
    }

    // 拷贝成功了才把旧目录改名 —— 顺序反了会把还没拷的数据弄丢。
    let backup = parent.join(format!("{legacy_name}.bak"));
    if let Err(e) = std::fs::rename(&legacy_dir, &backup) {
        // 改名失败不影响已经拷过去的数据，只是旧目录还留在那儿。
        eprintln!("[lodestar] 迁移设置：旧目录改名失败（数据已迁移，不影响使用）: {e}");
    }
    eprintln!("[lodestar] 已把旧设置从 {legacy_file:?} 迁移到 {new_file:?}");
}

/// 一条书签。
///
/// `kind` 决定点一下之后发生什么：
///   - `"query"` —— 存的是**搜索条件**（如 `ext:rs size:>1M`），
///     点一下重新跑这个搜索。**结果依赖索引**，索引变了结果就变。
///   - `"path"`  —— 存的是**一个文件/目录的路径**，
///     点一下在访达里定位。**不依赖索引**。
///
/// 两种并存（用户已确认），UI 分两栏显示。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Bookmark {
    pub kind: String,
    pub value: String,
    /// 展示名。`None` 时前端用 `value`。
    #[serde(default)]
    pub label: Option<String>,
}

/// 全部可持久化的设置。
///
/// `index_paths` 为空 = 用默认目录（`$HOME`）。
/// 每个字段都带 `#[serde(default)]` —— 读老版本的文件或被手改过的文件
/// 都能补齐缺字段，不会整个读不出来。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    #[serde(default)]
    pub index_paths: Vec<String>,
    #[serde(default)]
    pub sort_pref: Option<String>,
    #[serde(default)]
    pub kind_pref: Option<String>,
    #[serde(default)]
    pub bookmarks: Vec<Bookmark>,
    /// 最近搜索，最多留 20 条（去重，最新的在前）。
    #[serde(default)]
    pub recent_queries: Vec<String>,
    /// 窗口的物理像素宽度 / 高度。`None` = 用 `tauri.conf.json` 里的默认值。
    #[serde(default)]
    pub window_width: Option<u32>,
    #[serde(default)]
    pub window_height: Option<u32>,
    /// 窗口左上角的物理像素坐标。`None` = 用默认。
    #[serde(default)]
    pub window_x: Option<i32>,
    #[serde(default)]
    pub window_y: Option<i32>,
    /// 【文件变化时自动刷新索引】—— 默认 **true**。
    ///
    /// 【为什么默认开】文件一落地索引就跟上 —— 这是这个工具该有的直觉。
    ///
    /// 【为什么要有这个开关】
    ///   索引 `$HOME`（**实测 8119 项**）时全量扫描 ≈**300 毫秒**（重扫复用索引库 ≈50 毫秒）。
    ///   （排除规则修好**之前**是 7.95 万项 / 1.7 秒 —— **那是历史数据，别混**：
    ///     见 `AGENTS.md` 里「1.7 秒 / 7.95 万项」那条。）
    ///   没有开关的话，**在任何一个被索引的目录里存一次文件**
    ///   就触发一次 2 秒全量重扫 —— CPU 消耗不可接受。
    ///   关掉之后 watcher 仍然监听（监听很便宜），只是不触发重扫。
    ///
    /// 【`serde(default = ...)` 很重要】老的 `settings.json` 里
    ///   没有这个字段 —— 要能正常读出来（默认 true），不能整个读不出来。
    #[serde(default = "default_true")]
    pub auto_refresh: bool,
    /// **全局快捷键** —— 在任何 app 里按下它都能唤起 Lodestar。
    ///
    /// `None` = 不注册（用户主动关掉了）。
    ///
    /// 【格式】`global-hotkey` 的语法：修饰键在前、只能有一个主键。
    ///   "Option+Cmd+Space" / "CmdOrCtrl+Shift+F12"
    ///
    /// 【为什么是 `Option<String>` 而不是 `String`】
    ///   「没设过」和「设成空」要能区分开 —— 前者用默认值，后者表示用户关掉了。
    #[serde(default = "default_global_shortcut")]
    pub global_shortcut: Option<String>,
    /// **开机自启** —— 登录后自动在后台驻留。
    ///
    /// 【为什么设置里还要存一份】插件本身能查当前状态（`is_enabled()`），
    ///   但登录项可能被用户手动删掉 —— 设置里存一份是为了让 UI 有个默认值。
    #[serde(default)]
    pub launch_at_login: bool,
}

/// `#[serde(default = ...)]` 用的默认值 —— **默认开**。
///
/// 【为什么不能直接用 `bool::default()`】——
///   `bool::default()` 是 **false** —— 那就变成「默认关」了。
///   这里显式写一个返回 `true` 的函数。
fn default_true() -> bool {
    true
}

/// 默认的全局快捷键。
///
/// 【为什么是 ⌥⌘Space】macOS 上 ⌘Space 是 Spotlight 的默认键、
///   ⌥Space 是输入法切换 —— 两者都有人用。⌥⌘Space 冲突最少。
///
/// 【为什么不直接写字符串】默认快捷键只有**一个**真相：
///   `crate::shortcut::DEFAULT_SHORTCUT`。
///   两边各写一份字面量，改了一处忘了另一处 → 默认值和实际注册的不是同一个键。
fn default_global_shortcut() -> Option<String> {
    Some(crate::shortcut::DEFAULT_SHORTCUT.to_string())
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            index_paths: Vec::new(),
            sort_pref: None,
            kind_pref: None,
            bookmarks: Vec::new(),
            recent_queries: Vec::new(),
            window_width: None,
            window_height: None,
            window_x: None,
            window_y: None,
            auto_refresh: true,
            global_shortcut: default_global_shortcut(),
            launch_at_login: false,
        }
    }
}

/// 写设置的**进程内互斥锁**。
///
/// 【为什么需要它】（终审 M-10）
///   设置有**两个写者**，各自做「读 → 改一个字段 → 整体写回」：
///     - 前端 `patchSettings`（`app/src/lib/settings-io.ts`）
///     - Rust 侧（`set_global_shortcut` / `set_launch_at_login` / 存窗口位置）
///   两边**交错**时，后写的那份会把它没读到的字段**写回旧值**
///   （典型症状：刚改完快捷键，自动刷新开关的改动被抹掉）。
///
///   窗口很窄（都是毫秒级），但两个入口都在设置面板里、用户可以连着点。
///
/// 【为什么这把锁在 Rust 侧】
///   前端那条路是**两次 IPC**（读一次、写一次）——
///   锁在 Rust 侧也盖不住「读」与「写」之间那一段。
///   → 所以前端的写入改成**一条命令**（`patch_settings`）：
///     「读盘上的真值 → 合并差量 → 写回」全在**锁内**完成。
///   这样两个写者就**真正串行**了。
static WRITE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// 取写锁 —— **中毒了也继续跑**。
///
/// 【为什么要这样】
///   中毒只说明「某个持锁者 panic 过」，
///   而这里持锁期间做的是「读一个文件、写一个文件」——
///   数据本身不会因为中毒而变坏。
///   `.unwrap()` → 中毒时 panic → **设置再也写不进去了**
///   （而设置写不进去 = 用户的配置全丢）。
fn write_lock() -> std::sync::MutexGuard<'static, ()> {
    match WRITE_LOCK.lock() {
        Ok(g) => g,
        Err(e) => e.into_inner(),
    }
}

/// 读设置。读不到 / 解析失败 → 返回默认值。
///
/// **绝不让应用因为配置坏了就起不来** —— 宁可退回默认设置。
pub fn load<R: Runtime>(app: &AppHandle<R>) -> Settings {
    let store = match app.store(STORE_FILE) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("[lodestar] 打开配置失败，用默认设置: {e}");
            return Settings::default();
        }
    };
    let raw: Option<Value> = store.get(SETTINGS_KEY);
    match raw {
        Some(v) => match serde_json::from_value(v) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("[lodestar] 配置解析失败，用默认设置: {e}");
                Settings::default()
            }
        },
        None => Settings::default(),
    }
}

/// 写设置。
pub fn save<R: Runtime>(app: &AppHandle<R>, settings: &Settings) -> Result<(), String> {
    let store = app.store(STORE_FILE).map_err(|e| e.to_string())?;
    let value = serde_json::to_value(settings).map_err(|e| e.to_string())?;
    store.set(SETTINGS_KEY, value);
    store.save().map_err(|e| e.to_string())?;
    Ok(())
}

/// **读 → 改 → 写** —— **原子**（进程内那把写锁）。
///
/// 【为什么要有这个】
///   原来每个调用点各写各的「load → 改 → save」——
///   两个调用点**交错**时，后写的那份会把它没读到的字段**写回旧值**
///   （终审 M-10）。→ 全部改走这里，锁内一气呵成。
///
/// 【为什么不把锁放进 `load` / `save`】
///   那两个是**分开的**调用 —— 锁只能保证「各自不被打断」，
///   保证不了「读到的和写的是同一份」。
///   → 锁必须罩住**整个**「读-改-写」序列。
///
/// 【闭包返回 `Err` 的语义】= **什么都没改**（不写盘）。
///   「注册失败 → 设置不该改」这类语义就靠它表达。
pub fn update<R: Runtime, F>(app: &AppHandle<R>, f: F) -> Result<Settings, String>
where
    F: FnOnce(&mut Settings) -> Result<(), String>,
{
    let _guard = write_lock();
    let mut s = load(app);
    f(&mut s)?;
    save(app, &s)?;
    Ok(s)
}

/// 把一个**差量**合并到盘上的真值上 —— **原子**（同一把写锁）。
///
/// 【为什么前端要走这条路，而不是「整体替换」】
///   前端原来自己「读一份 → 改一个字段 → 整体写回」——
///   读与写之间是**两次 IPC**，中间 Rust 侧写一次就被覆盖回去
///   （典型症状：刚改完快捷键，自动刷新开关的改动被抹掉）。
///   → 差量交给 Rust：**在锁内**读真值、合并、写回 → 两个写者真正串行。
///
/// 【合并的语义】
///   差量里的每个**顶层字段**覆盖盘上的同名字段；
///   **差量里没提到的字段一律保留**（这正是「整体替换」最危险的地方）。
///
/// 【不认识的字段怎么办】
///   合并完反序列化回 [`Settings`] 时，未知的键会被 serde **忽略**
///   （结构体上没有 `deny_unknown_fields`）→ 它们不会进盘。
pub fn patch<R: Runtime>(app: &AppHandle<R>, delta: Value) -> Result<Settings, String> {
    let _guard = write_lock();
    let current = load(app);
    let Value::Object(delta_map) = delta else {
        return Err("设置差量必须是一个对象".to_string());
    };
    let mut map = serde_json::to_value(&current).map_err(|e| e.to_string())?;
    if let Value::Object(m) = &mut map {
        for (k, v) in delta_map {
            m.insert(k, v);
        }
    }
    let merged: Settings = serde_json::from_value(map).map_err(|e| e.to_string())?;
    save(app, &merged)?;
    Ok(merged)
}

/// 索引目录的**唯一回退点**：空列表 → `$HOME`。
///
/// 【为什么必须有这一个函数】
///   三条路径都要「空 = 索引家目录」：
///     - 启动路径（`lib.rs`）：`Settings.index_paths` 为空
///     - 换目录路径（`reindex`）：前端传的 `paths` 为空
///     - 底层（`rebuild_engine`）：直接传空数组
///   以前**只有启动路径**有回退（`effective_index_paths`），
///   `reindex` 那条路没有 → 用户把最后一个目录「移除」后再点应用，
///   `Engine::new(&[])` 拿到空数组 → shim 返回 NULL（契约见 `fsearch_shim.h`：
///   `path_count == 0` → NULL）→ 用户看到 "invalid argument"，
///   而且 UI **永远回不到家目录模式**（唯一出路是重启应用）。
///
///   收敛到一处，三条路径不可能再走偏。
///
/// 【空 + 没有 HOME】→ 返回空列表 → 引擎建不起来。
///   这时由调用方报错（`rebuild_engine` 会把 `InvalidArg` 传上去），
///   **不是**静默地索引零个目录。
pub fn effective_paths(paths: &[String]) -> Vec<String> {
    if !paths.is_empty() {
        return paths.to_vec();
    }
    match std::env::var("HOME") {
        Ok(home) if !home.is_empty() => vec![home],
        _ => Vec::new(),
    }
}

/// 实际要索引的目录：用户没配就用 `$HOME`。
///
/// **空列表 = 默认**，不是「索引零个目录」——
/// 引擎要求至少一个路径（`fs_engine_new` 传空数组会返回 NULL）。
///
/// 回退逻辑在 [`effective_paths`] 里（唯一真相），这里只做类型转换。
pub fn effective_index_paths(settings: &Settings) -> Vec<PathBuf> {
    effective_paths(&settings.index_paths)
        .iter()
        .map(PathBuf::from)
        .collect()
}

/// 索引档（store）目录 —— **唯一真相**。
///
/// 【为什么要抽出来】
///   两个地方要用**同一个**目录：
///   - 建引擎时**写**档（[`build_engine`]）
///   - 卷附加时**读**档（`volumes::scan_volume` —— 插回来的盘要秒开）
///   两处各写一遍 `app_config_dir().join("index")` → 改了一处忘了另一处 →
///   卷永远命不中旧档 → 每次插上都要全量重扫（几十万文件 → 几十秒）。
pub fn index_store_dir<R: Runtime>(app: &AppHandle<R>) -> Option<PathBuf> {
    app.path()
        .app_config_dir()
        .ok()
        .map(|dir| dir.join("index"))
}

/// 建引擎（**不碰全局状态**）—— 返回 (Searcher, indexed, 引擎实际的根)。
///
/// 【参数是【已解析】的路径】空 → `$HOME` 的回退由调用方做
/// （[`effective_paths`]）—— 这样「引擎索引哪些目录」与
/// 「状态里报哪些目录」一定是**同一份**（见 [`rebuild_engine_if_absent`]）。
///
/// 【为什么要单独抽出来】装引擎有两种语义：
///   - 无条件替换（换索引目录，见 [`rebuild_engine`]）
///   - 仅当没有引擎时装（启动路径，见 [`rebuild_engine_if_absent`]）
/// 两种都要先建引擎 —— 建引擎是慢的那一步（要扫一遍），
/// 必须留在锁外，不然会把状态锁占住一秒多。
///
/// 【索引档（store）】自研引擎支持把索引**落盘**——
///   启动时直接读档，不用重新扫盘。
///   路径放在 app config 目录里（与 `settings.json` 同级），
///   换 identifier 时跟着设置目录一起迁移。
fn build_engine<R: Runtime>(
    app: &AppHandle<R>,
    resolved: &[String],
) -> Result<(search_core::Searcher, u32, Vec<String>), String> {
    let refs: Vec<&Path> = resolved.iter().map(|p| Path::new(p)).collect();
    // 索引档现在是「**每根一个文件**」的目录（见 search-core/src/store.rs 的 v3 格式）。
    // ⚠️ 不能叫 index.bin —— 磁盘上那个 3.4MB 的旧 v2 档还占着这个名字，
    // 建目录会失败 → 档永远写不进去 → 每次冷启动都静默地全量重扫。
    let store_dir = index_store_dir(app);
    let (engine, indexed) =
        search_core::Engine::new_with_store(&refs, store_dir).map_err(|e| e.to_string())?;
    // 【为什么报「引擎实际的根」，而不是请求的那份】
    //   引擎会**静默丢弃嵌套根**（$HOME 与 $HOME/Documents → 只留前者）——
    //   请求的路径表与引擎实际根**不一致** →
    //     - 多一路**冗余**的 FSEvents 监听（`watcher.rs` 的 `desired_watch_dirs`
    //       读的就是状态里的这一份）；
    //     - 状态栏显示的目录数也不对。
    //   → 状态里报**引擎实际**的那份（唯一真相）。
    let roots: Vec<String> = engine
        .roots()
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    Ok((search_core::Searcher::new(engine), indexed, roots))
}

/// 重建引擎并替换掉全局句柄。**阻塞** —— 必须在后台线程跑。
///
/// 【为什么必须重建而不是增量改】
///   `fs_engine_rescan` 只认引擎记住的路径（见 `shim/fsearch_shim.h`），
///   所以换索引目录 = 销毁旧引擎 + 新建 + 重新扫描。
///
/// 【换的时候会发生什么】
///   `*guard = Some(new)` 会 drop 旧的 `Arc<Searcher>` → 它的 `Drop` 里
///   join 搜索线程 + `fs_engine_free`。如果旧线程正在跑搜索，这一步要等它跑完
///   （宽查询实测 2.4s）—— 所以整个函数必须在后台线程，不能占住 async 运行时。
///
/// 【失败时旧引擎原封不动】
///   `Engine::new` 在换之前就失败了 → 直接返回 Err，旧的 `Arc` 压根没被碰过。
///
/// 【空列表 = 索引家目录】
///   与启动路径同一套回退（[`effective_paths`] 是唯一回退点）。
///   不套的话 `Engine::new(&[])` → shim 返回 NULL（`path_count == 0`）
///   → `InvalidArg` → 用户看到 "invalid argument"。
///   这里再套一次是**幂等**的：调用方已经解析过的话不受影响，
///   直接调本函数的调用方也不会踩空数组的坑。
///
/// 【装引擎 + 提交状态 = 同一把锁内的一个原子步骤】（M1）
///   锁内做三件事：**装引擎 → 递增引擎代次 → 提交状态**。
///   顺序不能反：代次必须在**装引擎之后**才递增 ——
///   反过来会开一个「新代次 + 旧引擎」的窗口，
///   那期间开始的 rescan 提交时校验会通过 →
///   把旧引擎的结果写进代表新引擎的状态（反向错位，比原 bug 更糟）。
///   （同一把锁内 → 中间没有「装好了但状态还没跟上」的缝。）
pub fn rebuild_engine<R: Runtime>(
    app: &AppHandle<R>,
    paths: &[String],
) -> Result<u32, String> {
    let mut resolved = effective_paths(paths);
    // 【已挂载的卷要一起带上】换索引目录不该把 U 盘踢出索引 ——
    //   卷是**运行期**挂上来的（不在用户的设置里）→ 重建引擎时忘了带上
    //   → 新引擎里根本没有它 → 它的条目全没了（用户：换完目录 U 盘就搜不到了）。
    // 【枚举失败就不要乱改根列表】拿不到清单时保持原样 ——
    //   宁可少索引一个卷，也不能把用户正在用的根踢掉。
    if let Ok(list) = crate::volumes::mounted_local_volumes() {
        for v in list {
            let s = v.to_string_lossy().into_owned();
            if !resolved.contains(&s) {
                resolved.push(s);
            }
        }
    }
    let (searcher, indexed, actual_roots) = build_engine(app, &resolved)?;
    let new_searcher = std::sync::Arc::new(searcher);
    let state = app.state::<std::sync::Mutex<Option<std::sync::Arc<search_core::Searcher>>>>();
    // 【索引状态也要从 app 拿】—— 它跟引擎句柄一样是全局的一份
    let index_state = app.state::<crate::index_state::IndexState>();
    let mut guard = state.lock().map_err(|e| e.to_string())?;
    // 旧 Arc 在这里被 drop（join 旧 worker 线程 + fs_engine_free）
    *guard = Some(new_searcher);
    // 【同一把锁内提交状态】—— 装引擎 → 递增代次 → 提交，一步到位。
    // paths 用的是**引擎实际**的那一份（引擎会丢弃嵌套根 → 请求的那份可能更多）。
    // 【不动 scanning】—— scanning 归**持令牌的那一方**管：
    //   reindex 命令自己 begin() 拿到令牌，收尾时自己 end_scan(token)。
    index_state.commit_new_engine(actual_roots, indexed);
    Ok(indexed)
}

/// 重建引擎并装进全局状态 —— 但**只在当前还没有引擎时**。
///
/// 【为什么需要它】启动路径（`lib.rs` 的 setup 线程）与「换索引目录」（reindex）
///   都会建引擎，而启动线程是【后台】跑的 —— 读设置 + 建索引要一秒多。
///   这段时间里用户可能已经换了索引目录（`reindex` 已经把新引擎装进状态了）。
///   启动线程这时再把自己的引擎装上去 → **覆盖掉用户的选择**：
///   设置里写的是新目录，引擎却索引着旧目录 —— **设置 ≠ 引擎**。
///
///   实测踩过（探针日志）：
///     reindex(临时目录) → 索引就绪：2 项
///     search "marker"  → 1 条 ✓
///     [lodestar] 索引就绪：57817 项     ← 启动线程把引擎换回 $HOME
///     search "marker"  → 0 条 ✗        ← 同一句查询，结果凭空没了
///
/// 【竞态为什么这样就没有了】两个装入点：
///   - 启动线程：**仅当状态为空时装** → 若 reindex 先装，它就跳过；
///   - reindex：无条件替换 → 若启动线程先装，它替换掉。
///   两种先后顺序，最后活着的引擎都是**用户后来选的那个**。
///
/// 【流程：run_scan_blocking（快速预检 → 建引擎（锁外）→ 再判空 → 装 + 提交）】
///
///   ① **快速预检**（只读锁）：槽位已经被占了 → 立刻放弃。
///      【为什么要在建引擎【之前】预检】建引擎要扫一遍（$HOME 量级 1.1 秒）——
///      白扫一遍再丢掉是纯浪费。
///      【这一趟我们【没有 begin() 过】→ **什么都不动**——
///        没有自己的扫描标志可放；去清别人的是错的（A1）。】
///   ② 建引擎（**锁外**）：慢的那一步不能占着状态锁。
///      失败 → 错误由 `run_scan_blocking` 记下（`fail(token, …)`）+ 放回 scanning。
///   ③ **再判空**（同一把锁）：这 1.1 秒里 reindex 可能抢先装上了 →
///      那就放弃，**不覆盖它的引擎**。
///   ④ 装上 + `commit_new_engine()`：状态与引擎用**同一份**已解析路径，
///      并**递增引擎代次**（换引擎了 → 代次 +1，M1）。
///
/// 【scanning 的生命周期归 `run_scan_blocking` 管】
///   这一趟是**同步**的（跑在 `lib.rs` 的后台线程里，不是 async）→ 用同步版。
///   以前 `begin()` / `end_scan()` 散在本函数里，**任何一条提前 return 都会绕过收尾**
///   → scanning 卡在 true：状态栏永远「正在扫描索引…」+ 索引就绪后不自动重搜。
///   （上一版就是踩了这个：begin() 在调用方、放弃时没人管 → 卡死。）
///   现在收尾在 `run_scan_blocking` 里，**除了「闭包 panic」那种情况，没法再绕过**
///   （panic 会 unwind 过去；release 的 profile 是 `panic = "abort"` → 整个进程死掉，
///     不存在「scanning 卡住」的用户可见状态）。
///   `end_scan(token)` **只**放**自己**的 scanning，不动 paths/indexed ——
///   引擎没换，那 paths/indexed 就还该是当前活着的那个引擎的那份。
///   【令牌过期就不放】已经有更新的扫描接管了 → 这一趟什么都不做。
///
/// 【返回值的含义】`(indexed, installed)`：
///   `installed == false` = 状态里已经有引擎了，本次**什么都没改**（引擎与状态）。
pub fn rebuild_engine_if_absent<R: Runtime>(
    app: &AppHandle<R>,
    paths: &[String],
    index_state: &crate::index_state::IndexState,
) -> Result<(u32, bool), String> {
    // 空 → $HOME 的回退只在这里做一次 ——
    // 引擎索引的目录与状态里报的目录必须是同一份。
    let resolved = effective_paths(paths);

    // ① 快速预检：只读锁，看一眼就够
    //
    //   【必须在 run_scan_blocking 【之外】】——
    //   这一趟我们**还没开始扫描**（还没 begin 过），
    //   所以**没有自己的扫描标志可放**。
    //   若把它包进 run_scan_blocking，那边会先 begin()（**抢走当前扫描的令牌**）、
    //   结束时再 end_scan() → **把别人的扫描标志清掉** → A1 原地复活。
    //   （`startup_precheck_give_up_does_not_clear_another_scans_flag` 就在钉这条。）
    {
        let state = app.state::<std::sync::Mutex<Option<std::sync::Arc<search_core::Searcher>>>>();
        let guard = state.lock().map_err(|e| e.to_string())?;
        if guard.is_some() {
            // 已经有引擎了 → 放弃。
            // 【什么都不动】—— 别人的 scanning 不归我们管（那正是 A1）。
            return Ok((0, false));
        }
    }

    // ② 建引擎 —— 【从这一刻起】才开始扫描
    //
    // 【走 run_scan_blocking】—— scanning 的 begin/收尾由它管，绕不过去。
    crate::index_state::run_scan_blocking(index_state, || {
        // ② 建引擎（锁外，慢）
        //
        // 【仅测试构建】`BUILD_HOOK` 让「建引擎期间」这个窗口可控 ——
        //   T5 要命中的 ③ 路径（建引擎期间槽位被别人抢占）靠 sleep 撞时序
        //   会偶发红，钩子把它变成确定的。release 二进制里这段代码不存在。
        #[cfg(test)]
        if let Some(hook) = take_build_hook() {
            hook();
        }
        let (searcher, indexed, actual_roots) = build_engine(app, &resolved)?;

        // ③ 同一把锁：再判空 → 装上 + 提交状态
    let state = app.state::<std::sync::Mutex<Option<std::sync::Arc<search_core::Searcher>>>>();
    let mut guard = state.lock().map_err(|e| e.to_string())?;
    if guard.is_some() {
        // 这 1.1 秒里被 reindex 抢先了 → 放弃，不覆盖它的引擎。
        // 本函数建出来的那个引擎在函数结束时被 drop（此时锁已先释放）——
        // 它从没被共享过，不可能有搜索在它上面跑，free 是安全的。
        // 【放回自己的 scanning】—— run_scan_blocking 会做。
        return Ok((indexed, false));
    }
    *guard = Some(std::sync::Arc::new(searcher));
    // 【同一把锁内提交状态】—— 判空 → 装引擎 → 提交状态，一步到位。
    // paths 用的是**引擎实际**的那一份（引擎会丢弃嵌套根 → 请求的那份可能更多）。
    // 【递增引擎代次】—— 换引擎了 → 代次 +1（M1，见 `index_state.rs`）。
    // 【不动 scanning】—— 收尾归 run_scan_blocking。
    index_state.commit_new_engine(actual_roots, indexed);
    // ④ 续：自己的扫描结束（scanning 由 run_scan_blocking 管的令牌持有者负责）
    // 【放在槽位锁释放之后】—— end_scan 拿的是 IndexState 自己的锁，
    //   与槽位锁不是同一把 → 不会死锁（锁序：槽位锁 → IndexState 锁）。
    drop(guard);
        Ok((indexed, true))
    })
}

/// 【仅测试用】建引擎阶段的钩子。
///
/// 【为什么需要】④ 路径（建引擎期间被别人抢占槽位）只有在「建引擎够慢」时
///   才能命中 —— 靠 sleep 撞时序会偶发红。钩子让「建引擎期间」这个窗口
///   变成**确定的**：测试在钩子里把引擎槽位抢走，④ 就必然命中。
///
/// 【只在测试构建里存在】`#[cfg(test)]` → release 二进制里这段代码不存在。
#[cfg(test)]
static BUILD_HOOK: std::sync::Mutex<Option<Box<dyn Fn() + Send + Sync>>> =
    std::sync::Mutex::new(None);

/// 取走（并清掉）建引擎钩子 —— **一次性**的，取走就没了，
/// 免得漏掉钩子影响别的用例。
#[cfg(test)]
fn take_build_hook() -> Option<Box<dyn Fn() + Send + Sync>> {
    BUILD_HOOK.lock().unwrap().take()
}
#[cfg(test)]
mod tests {
    //! 设置的【运行时】验证 —— 走真实的 IPC 层，按命令名调用。
    //!
    //! 为什么要有这个测试：命令「定义了」不等于「注册了」。
    //! 忘了写进 `invoke_handler` 的话，**编译期不报错**，
    //! 只有前端真调的时候才报 `command get_settings not found`。
    //! 这里用 `tauri::test::get_ipc_response` 走真实 IPC 路径把它验掉。
    //!
    //! 【HOME 要指到临时目录】mock runtime 的 identifier 是空串，
    //! store 会写进用户真实的 `~/Library/Application Support/`。
    //! `dirs::data_dir()` 每次调用都重读 `$HOME`（已实测），所以这样能完全隔离。
    //!
    //! ⚠️ 改 `HOME` 是**进程级**的 —— cargo test 默认并行跑同进程内的测试，
    //! 两个用例各改各的 HOME 会打架（实测：一个用例的文件落到了另一个用例的目录里）。
    //! 所以用一把全局互斥锁把依赖 HOME 的用例**串行化**。

    use super::*;
    use tauri::ipc::{CallbackFn, InvokeBody};
    use tauri::webview::InvokeRequest;
    use tauri::test::{mock_context, noop_assets, INVOKE_KEY};

    /// 串行化所有改 `HOME` 的用例。
    static HOME_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn isolate_home(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join(format!("fsearch-settings-test-{tag}-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        std::env::set_var("HOME", &dir);
        dir
    }

    fn invoke(cmd: &str, body: serde_json::Value) -> InvokeRequest {
        InvokeRequest {
            cmd: cmd.into(),
            callback: CallbackFn(0),
            error: CallbackFn(1),
            url: "tauri://localhost".parse().unwrap(),
            body: InvokeBody::Json(body),
            headers: Default::default(),
            invoke_key: INVOKE_KEY.to_string(),
        }
    }

    fn build_app() -> tauri::App<tauri::test::MockRuntime> {
        tauri::test::mock_builder()
            .plugin(tauri_plugin_store::Builder::default().build())
            // 【引擎句柄 + 索引状态必须 manage】
            //   `reindex` 命令要从 state 里取旧引擎换新的，
            //   mock app 没 manage 的话 `app.state::<...>()` 直接 panic
            //   （state not managed for field ...）—— 不是返回 Err，是崩。
            .manage(std::sync::Mutex::new(
                Option::<std::sync::Arc<search_core::Searcher>>::None,
            ))
            .manage(crate::index_state::IndexState::new())
            // 【命令表用生产的那一份】—— 不是抄一份。
            //   抄的话，「生产忘了注册」这件事测试是发现不了的。
            .invoke_handler(crate::app_commands_handler::<tauri::test::MockRuntime>())
            .build(mock_context(noop_assets()))
            .expect("构建 mock app")
    }

    #[test]
    fn settings_commands_are_registered_and_round_trip() {
        let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = isolate_home("roundtrip");
        let app = build_app();
        let webview = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
            .build()
            .expect("创建 webview");

        // 1) get_settings 必须能被 IPC 按名字调到 —— 这就是「注册没漏」的证明
        let body = tauri::test::get_ipc_response(&webview, invoke("get_settings", serde_json::json!({})))
            .expect("get_settings 应返回 Ok（命令没注册的话这里会 Err）");
        let val: serde_json::Value = body.deserialize().unwrap();
        assert_eq!(val["indexPaths"], serde_json::json!([]));
        assert_eq!(val["bookmarks"], serde_json::json!([]));
        assert_eq!(val["recentQueries"], serde_json::json!([]));

        // 2) set_settings → get_settings 要能读回来
        let payload = serde_json::json!({
            "indexPaths": ["/tmp/whatever"],
            "sortPref": "size",
            "kindPref": "files",
            "bookmarks": [{"kind": "query", "value": "ext:rs"}],
            "recentQueries": ["ext:rs"]
        });
        // 【body 要按参数名包一层】命令的参数名叫 `settings`，
        // Tauri 找的是 body["settings"] —— 直接把设置对象当 body 传会报
        // `command set_settings missing required key settings`
        tauri::test::assert_ipc_response(
            &webview,
            invoke("set_settings", serde_json::json!({ "settings": payload })),
            Ok(()),
        );

        let body = tauri::test::get_ipc_response(&webview, invoke("get_settings", serde_json::json!({})))
            .expect("get_settings 应返回 Ok");
        let val: serde_json::Value = body.deserialize().unwrap();
        assert_eq!(val["indexPaths"], serde_json::json!(["/tmp/whatever"]));
        assert_eq!(val["sortPref"], "size");
        assert_eq!(val["bookmarks"][0]["kind"], "query");
        assert_eq!(val["bookmarks"][0]["value"], "ext:rs");
        assert_eq!(val["recentQueries"][0], "ext:rs");

        // 3) 配置文件真的落盘了（在临时 HOME 里）
        let cfg = home.join("Library/Application Support/settings.json");
        assert!(cfg.exists(), "配置文件应落盘: {}", cfg.display());
    }

    /// 【终审 M-10】差量写入必须**只改差量里的字段**，其余字段**原样保留**。
    ///
    /// 【为什么这条重要】
    ///   前端原来自己「读一份 → 改一个字段 → 整体写回」——
    ///   读与写之间是**两次 IPC**，中间 Rust 侧写了一次设置
    ///   （改快捷键 / 存窗口位置）→ 前端那份**已经旧了**的整体对象
    ///   一写回去，Rust 那边的改动就被抹掉。
    ///   → 差量交给 Rust，在**锁内**读盘上的真值再合并。
    #[test]
    fn patching_one_field_keeps_the_others() {
        let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = isolate_home("patch");
        let app = build_app();
        let webview = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
            .build()
            .expect("创建 webview");

        // 先放一份「别的字段都有值」的设置
        tauri::test::assert_ipc_response(
            &webview,
            invoke(
                "set_settings",
                serde_json::json!({
                    "settings": {
                        "indexPaths": ["/tmp/keep-me"],
                        "bookmarks": [{ "kind": "query", "value": "ext:rs" }],
                        "recentQueries": ["ext:rs"]
                    }
                }),
            ),
            Ok(()),
        );

        // 只改一个字段（模拟前端「拨动自动刷新开关」）
        let body = tauri::test::get_ipc_response(
            &webview,
            invoke(
                "patch_settings",
                serde_json::json!({ "delta": { "autoRefresh": false } }),
            ),
        )
        .expect("patch_settings 应返回 Ok（命令没注册的话这里会 Err）");
        let val: serde_json::Value = body.deserialize().unwrap();
        assert_eq!(val["autoRefresh"], false, "差量里的字段要改掉");

        // 读回来 —— 其余字段必须**原样保留**
        let body = tauri::test::get_ipc_response(&webview, invoke("get_settings", serde_json::json!({})))
            .expect("get_settings 应返回 Ok");
        let val: serde_json::Value = body.deserialize().unwrap();
        assert_eq!(val["autoRefresh"], false);
        assert_eq!(
            val["indexPaths"],
            serde_json::json!(["/tmp/keep-me"]),
            "差量里没提到的字段必须保留 —— 整体写回会把它们抹掉"
        );
        assert_eq!(val["bookmarks"][0]["value"], "ext:rs");
        assert_eq!(val["recentQueries"][0], "ext:rs");

        let _ = home;
    }

    /// 差量里的**不认识的字段**不能进盘（否则配置文件里会多出野字段）。
    #[test]
    fn patching_with_an_unknown_field_ignores_it() {
        let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = isolate_home("patch-unknown");
        let app = build_app();
        let webview = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
            .build()
            .expect("创建 webview");

        // 【返回的是合并后的完整设置】——
        //   前端要拿它刷新本地状态（书签 / 最近搜索那几个 ref）。
        let body = tauri::test::get_ipc_response(
            &webview,
            invoke(
                "patch_settings",
                serde_json::json!({ "delta": { "notARealField": 42 } }),
            ),
        )
        .expect("patch_settings 应返回 Ok");
        let val: serde_json::Value = body.deserialize().unwrap();
        assert!(
            val.get("notARealField").is_none(),
            "不认识的字段不该进盘（也不该出现在返回值里），实际：{val}"
        );

        let _ = home;
    }

    #[test]
    fn missing_fields_fall_back_to_defaults() {
        // 老版本 / 被手改过的配置文件要能补齐缺字段，而不是整个读不出来
        let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = isolate_home("defaults");
        let app = build_app();
        let webview = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
            .build()
            .expect("创建 webview");

        // 只给 indexPaths，其余字段全缺
        tauri::test::assert_ipc_response(
            &webview,
            invoke(
                "set_settings",
                serde_json::json!({
                    "settings": { "indexPaths": ["/tmp/only-this"] }
                }),
            ),
            Ok(()),
        );

        let body = tauri::test::get_ipc_response(&webview, invoke("get_settings", serde_json::json!({})))
            .expect("get_settings 应返回 Ok");
        let val: serde_json::Value = body.deserialize().unwrap();
        assert_eq!(val["indexPaths"], serde_json::json!(["/tmp/only-this"]));
        assert_eq!(val["bookmarks"], serde_json::json!([]));
        assert_eq!(val["recentQueries"], serde_json::json!([]));

        let _ = home;
    }

    // ── 「文件变化时自动刷新索引」开关 ──

    /// 【默认开】全新的设置里这个开关必须是**开**的。
    ///
    /// 【为什么这条重要】默认关的话，用户装上就发现「文件变了索引不跟上」，
    ///   而且没有任何提示说「你得去设置里打开它」。
    #[test]
    fn auto_refresh_is_on_by_default() {
        let s = Settings::default();
        assert!(
            s.auto_refresh,
            "「文件变化时自动刷新索引」默认必须开"
        );
    }

    /// 老配置里**没有** `autoRefresh` 这个字段 → 要按**默认开**读出来。
    ///
    /// 【为什么这条重要】`#[serde(default = "default_true")]` 写漏了
    ///   （或者误写成 `bool::default()` —— 那是 **false**）的话，
    ///   老用户的 `settings.json` 会把这个字段读成 false
    ///   → 自动刷新**静默关掉**，用户根本不知道为什么索引不更新了。
    #[test]
    fn auto_refresh_defaults_to_on_when_the_field_is_missing() {
        let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = isolate_home("auto-refresh-default");
        let app = build_app();
        let webview = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
            .build()
            .expect("创建 webview");

        // 只给 indexPaths —— **故意不给 autoRefresh**（模拟老版本的配置文件）
        tauri::test::assert_ipc_response(
            &webview,
            invoke(
                "set_settings",
                serde_json::json!({
                    "settings": { "indexPaths": ["/tmp/only-this"] }
                }),
            ),
            Ok(()),
        );

        let body = tauri::test::get_ipc_response(&webview, invoke("get_settings", serde_json::json!({})))
            .expect("get_settings 应返回 Ok");
        let val: serde_json::Value = body.deserialize().unwrap();
        assert_eq!(
            val["autoRefresh"],
            serde_json::json!(true),
            "老配置里缺这个字段时必须默认开（关掉的话用户会莫名其妙）"
        );

        let _ = home;
    }

    /// 开关**能关掉** —— 写 false、读回来还是 false。
    #[test]
    fn auto_refresh_can_be_turned_off() {
        let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = isolate_home("auto-refresh-off");
        let app = build_app();
        let webview = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
            .build()
            .expect("创建 webview");

        tauri::test::assert_ipc_response(
            &webview,
            invoke(
                "set_settings",
                serde_json::json!({
                    "settings": { "indexPaths": ["/tmp/only-this"], "autoRefresh": false }
                }),
            ),
            Ok(()),
        );

        let body = tauri::test::get_ipc_response(&webview, invoke("get_settings", serde_json::json!({})))
            .expect("get_settings 应返回 Ok");
        let val: serde_json::Value = body.deserialize().unwrap();
        assert_eq!(
            val["autoRefresh"],
            serde_json::json!(false),
            "关掉之后必须读回来还是 false"
        );

        let _ = home;
    }

// ── Task 2：换索引目录要真的重建引擎 ──

    /// 造一个临时目录，里面放一个可辨认的文件。
    fn make_dir_with_file(tag: &str, filename: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "fsearch-reindex-test-{tag}-{}-{}",
            std::process::id(),
            filename
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(filename), b"hello").unwrap();
        dir
    }

    fn request(query: &str) -> search_core::types::SearchRequest {
        use search_core::types::{ListKind, SortField, SortOrder};
        search_core::types::SearchRequest {
            query: query.to_string(),
            sort: SortField::Name,
            order: SortOrder::Asc,
            kind: ListKind::All,
            page: 0,
            page_size: 50,
        }
    }

    /// 取「全局引擎句柄」的 state。
    ///
    /// 【为什么不能链式 `.state().lock()`】
    ///   `app.state::<T>()` 返回的是**临时值** `State<'_, T>`，
    ///   锁借用它 → 语句一结束临时值就 drop，
    ///   报 error[E0716]: temporary value dropped while borrowed。
    ///   必须先绑到变量上，锁才有可借的宿主（`lib.rs` 的 setup 里也是这么写的）。
    fn engine_state(
        app: &tauri::App<tauri::test::MockRuntime>,
    ) -> tauri::State<'_, std::sync::Mutex<Option<std::sync::Arc<search_core::Searcher>>>> {
        app.state::<std::sync::Mutex<Option<std::sync::Arc<search_core::Searcher>>>>()
    }

    #[test]
    fn rebuild_engine_switches_to_new_paths() {
        let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        isolate_home("reindex");

        let old_dir = make_dir_with_file("reindex", "old_marker.txt");
        let new_dir = make_dir_with_file("reindex", "new_marker.txt");

        let app = build_app();

        // 1) 先给全局状态装一个「只索引 old_dir」的引擎
        let old_refs = [old_dir.as_path()];
        let (engine, _) = search_core::Engine::new(&old_refs).expect("建引擎");
        {
            let st = engine_state(&app);
            let mut g = st.lock().unwrap();
            *g = Some(std::sync::Arc::new(search_core::Searcher::new(engine)));
        }

        let names_now = |q: &str| -> Vec<String> {
            let st = engine_state(&app);
            let g = st.lock().unwrap();
            let s = g.as_ref().unwrap();
            // 【search 收 SearchRequest【按值】】不是 &SearchRequest
            s.search(request(q)).unwrap().entries.iter().map(|e| e.name.clone()).collect()
        };

        let before = names_now("marker");
        assert!(
            before.iter().any(|n| n == "old_marker.txt"),
            "换之前应能搜到 old_marker.txt，实际: {before:?}"
        );

        // 2) 换到新目录
        let paths = vec![new_dir.to_string_lossy().into_owned()];
        // 【rebuild_engine 要的是 &AppHandle】mock app 是 `App<MockRuntime>`，
        // 用 `app.handle()` 取它的 handle —— 直接传 &app 会报
        // expected `&AppHandle<_>`, found `&App<MockRuntime>`
        let indexed = rebuild_engine(app.handle(), &paths).expect("rebuild_engine 应成功");
        assert!(indexed >= 1, "新目录应索引到至少 1 项，实际 {indexed}");

        let after = names_now("marker");
        assert!(
            after.iter().any(|n| n == "new_marker.txt"),
            "换之后应能搜到 new_marker.txt，实际: {after:?}"
        );
        assert!(
            !after.iter().any(|n| n == "old_marker.txt"),
            "换之后不该再搜到 old_marker.txt，实际: {after:?}"
        );

        // 3) 换成不存在的目录 → 报错，且旧引擎原封不动
        let engine_before = {
            let st = engine_state(&app);
            let g = st.lock().unwrap();
            g.as_ref().unwrap().clone()
        };
        let err = rebuild_engine(app.handle(), &["/definitely/not/exist/xyz".to_string()]);
        assert!(err.is_err(), "不存在的目录应该报错");
        let engine_after = {
            let st = engine_state(&app);
            let g = st.lock().unwrap();
            g.as_ref().unwrap().clone()
        };
        assert!(
            std::sync::Arc::ptr_eq(&engine_before, &engine_after),
            "失败的 rebuild 不该动旧引擎"
        );
    }

    /// 启动线程与 reindex 撞车时，**启动线程不许覆盖**。
    ///
    /// 【这条不变式为什么重要】设置 ≠ 引擎：
    ///   设置里写的是用户刚换的目录，引擎却索引着启动线程建的那份 →
    ///   同一句查询的结果会凭空变掉（实测：1 条 → 0 条）。
    ///
    /// 【验什么】
    ///   ① reindex 先装上引擎 → 启动线程到达时**不覆盖引擎**；
    ///   ② 启动线程**也不覆盖 IndexState**（否则状态栏会报启动那组路径/条目数）；
    ///   ③ 反过来（启动线程先装）→ reindex 照样替换它（reindex 是用户的显式选择）。
    #[test]
    fn startup_engine_does_not_overwrite_a_reindexed_one() {
        let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        isolate_home("startup-race");

        let reindexed_dir = make_dir_with_file("startup-race", "reindexed_marker.txt");
        let startup_dir = make_dir_with_file("startup-race", "startup_marker.txt");
        let reindexed_str = reindexed_dir.to_string_lossy().into_owned();
        let startup_str = startup_dir.to_string_lossy().into_owned();

        let app = build_app();

        // ① 用户先换了索引目录（reindex 抢先装上引擎）
        let indexed = rebuild_engine(app.handle(), &[reindexed_str.clone()]).expect("reindex 应成功");
        assert!(indexed >= 1, "reindex 应索引到至少 1 项，实际 {indexed}");
        // rebuild_engine 本身就会提交 IndexState
        // （装引擎 + 递增引擎代次 + 提交状态 = 同一把锁内）
        let index_state = app.state::<crate::index_state::IndexState>();

        // ② 启动线程这时才到达 → 不许覆盖
        let (_idx, installed) =
            rebuild_engine_if_absent(app.handle(), &[startup_str.clone()], &index_state)
                .expect("启动路径不该报错");
        assert!(!installed, "引擎已被 reindex 装上 → 启动线程不该覆盖它");

        // ③ 引擎还是 reindex 那个（能搜到 reindexed_marker，搜不到 startup_marker）
        let names_now = |q: &str| -> Vec<String> {
            let st = engine_state(&app);
            let g = st.lock().unwrap();
            let s = g.as_ref().unwrap();
            s.search(request(q)).unwrap().entries.iter().map(|e| e.name.clone()).collect()
        };
        let names = names_now("marker");
        assert!(
            names.iter().any(|n| n == "reindexed_marker.txt"),
            "引擎应仍是 reindex 那个，实际: {names:?}"
        );
        assert!(
            !names.iter().any(|n| n == "startup_marker.txt"),
            "启动线程的引擎不该顶上来，实际: {names:?}"
        );

        // ④ IndexState 也不该被启动线程覆盖（状态栏要报 reindex 那组）
        let snap = index_state.snapshot();
        assert_eq!(snap.paths, vec![reindexed_str.clone()], "状态不该被启动线程覆盖");
        assert_eq!(snap.indexed, indexed, "条目数不该被启动线程覆盖");
        assert!(!snap.scanning, "不该还停在 scanning");
    }

    /// 反过来的顺序：启动线程先装 → reindex 照样替换它
    /// （用户的显式选择必须赢）。
    #[test]
    fn reindex_still_replaces_the_startup_engine() {
        let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        isolate_home("startup-then-reindex");

        let startup_dir = make_dir_with_file("startup-then-reindex", "startup_marker.txt");
        let reindexed_dir = make_dir_with_file("startup-then-reindex", "reindexed_marker.txt");
        let startup_str = startup_dir.to_string_lossy().into_owned();
        let reindexed_str = reindexed_dir.to_string_lossy().into_owned();

        let app = build_app();
        let index_state = app.state::<crate::index_state::IndexState>();

        // 启动线程先装
        let (_idx, installed) =
            rebuild_engine_if_absent(app.handle(), &[startup_str.clone()], &index_state)
                .expect("启动路径不该报错");
        assert!(installed, "槽位是空的 → 启动线程应该装上");

        // reindex 随后替换它
        rebuild_engine(app.handle(), &[reindexed_str.clone()]).expect("reindex 应成功");

        let names: Vec<String> = {
            let st = engine_state(&app);
            let g = st.lock().unwrap();
            let s = g.as_ref().unwrap();
            s.search(request("marker")).unwrap().entries.iter().map(|e| e.name.clone()).collect()
        };
        assert!(
            names.iter().any(|n| n == "reindexed_marker.txt"),
            "reindex 应替换启动的引擎，实际: {names:?}"
        );
        assert!(
            !names.iter().any(|n| n == "startup_marker.txt"),
            "启动的引擎应已被替换掉，实际: {names:?}"
        );
    }

    /// 【T3 / A1 的反证】启动线程的 ① 预检放弃 → **别人的 scanning 不受影响**。
    ///
    /// 【A1 是什么】
    ///   ① 预检路径以前会 `cancel_scan()` —— **无条件**清掉 scanning。
    ///   而这一趟**根本没 begin() 过** → 清掉的是**别人**（reindex）的扫描标志。
    ///
    ///   生产序列：用户换索引目录（reindex 在建引擎，scanning=true）→
    ///   启动线程到达 → ① 预检放弃 → reindex 的「正在扫描」提示被清掉 →
    ///   前端 `App.vue` 的 800ms 轮询撞见 `wasScanning=true && scanning=false`
    ///   → `refreshKey++` → **在新引擎装好之前就重搜一次** → 那次搜索跑在旧引擎上。
    ///
    /// 【修法】① 路径**什么都不动**；
    ///   scanning 归**持令牌的那一方**管（`ScanToken`，见 `index_state.rs`）。
    ///
    /// 【这条在修之前会红】—— 它就是 A1 的反证。
    #[test]
    fn startup_precheck_give_up_does_not_clear_another_scans_flag() {
        let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        isolate_home("scan-stuck");

        let reindexed_dir = make_dir_with_file("scan-stuck", "reindexed_marker.txt");
        let startup_dir = make_dir_with_file("scan-stuck", "startup_marker.txt");
        let reindexed_str = reindexed_dir.to_string_lossy().into_owned();
        let startup_str = startup_dir.to_string_lossy().into_owned();

        let app = build_app();
        let index_state = app.state::<crate::index_state::IndexState>();

        // ① reindex 抢先装上了引擎
        //    【照抄 reindex 命令的完整流程】commands.rs::reindex 走的是
        //    「rebuild_engine（装引擎 + 提交状态 + 递增引擎代次）」——
        //    状态是它一起提交的，这里不用再补一次。
        rebuild_engine(app.handle(), &[reindexed_str.clone()]).expect("reindex 应成功");

        // ② 【别人的扫描正在进行】（模拟 reindex 正在建引擎 → scanning=true）
        let other_token = index_state.begin();
        assert!(
            index_state.snapshot().scanning,
            "别的扫描正在进行 → scanning 应该是 true"
        );

        // ③ 启动线程到达 → ① 预检发现槽位已被占 → 放弃
        let (_idx, installed) =
            rebuild_engine_if_absent(app.handle(), &[startup_str.clone()], &index_state)
                .expect("启动路径不该报错");

        // ④ 它必须放弃（不能覆盖 reindex 的引擎）
        assert!(!installed, "引擎已被 reindex 装上 → 启动线程不该覆盖它");

        // ⑤ 【关键】别人的 scanning **不该**被清掉（A1）
        let snap = index_state.snapshot();
        assert!(
            snap.scanning,
            "启动线程的预检放弃不该清掉别人的扫描标志（A1）—— \
             清了它，前端就会在引擎装好之前提前重搜一次（跑在旧引擎上）"
        );
        // 状态里的 paths/indexed 也不该被启动线程改动
        assert_eq!(snap.paths, vec![reindexed_str.clone()], "状态不该被启动线程覆盖");

        // ⑥ 别人的扫描自己收尾 → 才清（这才是「有主权」的含义）
        assert!(index_state.end_scan(other_token), "当前令牌应该能结束这一趟");
        assert!(!index_state.snapshot().scanning);
    }

    /// 【T5】建引擎期间被别人抢占 → ④ 路径：
    ///   放回**自己的** scanning，状态里的 paths/indexed 仍是**别人**那份。
    ///
    /// 【怎么做到确定】`BUILD_HOOK`（仅测试构建）会在「建引擎」之前触发 ——
    ///   钩子把「别人的引擎」装进槽位 + 提交别人的状态 → ④ **必然**命中。
    ///   不用 sleep 撞时序 → **不会偶发红**。
    ///   （施工图给的「睡 150ms 再抢」那种做法依赖「建引擎要 1.1s」，
    ///     而测试里索引的是临时目录 → 建引擎只要几毫秒 → 必然偶发红。）
    #[test]
    fn startup_path_releases_its_own_scan_flag_when_the_slot_is_taken() {
        let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        isolate_home("preempt");

        let startup_dir = make_dir_with_file("preempt", "startup_marker.txt");
        let other_dir = make_dir_with_file("preempt", "other_marker.txt");
        let startup_str = startup_dir.to_string_lossy().into_owned();
        let other_str = other_dir.to_string_lossy().into_owned();

        let app = build_app();
        let index_state = app.state::<crate::index_state::IndexState>();

        // 【别人】的引擎先建好（钩子里只是把它装进槽位，不用再扫一遍）
        let other_refs = [other_dir.as_path()];
        let (other_engine, other_indexed) =
            search_core::Engine::new(&other_refs).expect("建别人的引擎");
        let other_searcher = std::sync::Arc::new(search_core::Searcher::new(other_engine));

        // 钩子：主线程进入「建引擎」阶段时 → 别人抢占槽位 + 提交自己的状态
        let handle = app.handle().clone();
        let other_str_for_hook = other_str.clone();
        *BUILD_HOOK.lock().unwrap() = Some(Box::new(move || {
            {
                let slot = handle
                    .state::<std::sync::Mutex<Option<std::sync::Arc<search_core::Searcher>>>>();
                let mut g = slot.lock().unwrap();
                *g = Some(other_searcher.clone());
            }
            // 别人提交自己的状态（【不动 scanning】—— scanning 归令牌持有者管）
            let is = handle.state::<crate::index_state::IndexState>();
            is.commit_new_engine(vec![other_str_for_hook.clone()], other_indexed);
        }));

        // 启动线程：① 预检（槽位还空着）→ ② begin → ③ 建引擎（钩子在这里被抢）→ ④ 放弃
        let (_idx, installed) =
            rebuild_engine_if_absent(app.handle(), &[startup_str.clone()], &index_state)
                .expect("启动路径不该报错");

        assert!(!installed, "槽位被抢 → 启动线程不该装上自己的引擎");

        let snap = index_state.snapshot();
        // 【自己的 scanning 放回去了】—— 自己没被别人接管 → 令牌还有效
        assert!(
            !snap.scanning,
            "放弃时必须放回**自己**的 scanning（否则卡在 true：状态栏永远「正在扫描…」）"
        );
        // 状态里的 paths/indexed 仍是**别人**那份
        assert_eq!(snap.paths, vec![other_str.clone()], "状态不该被启动线程覆盖");
        assert_eq!(snap.indexed, other_indexed, "条目数不该被启动线程覆盖");
    }

    /// 成功路径：启动线程装上了引擎 → scanning 归 false，状态与引擎一致。
    #[test]
    fn startup_scan_flag_is_cleared_when_the_engine_is_installed() {
        let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        isolate_home("scan-ok");

        let dir = make_dir_with_file("scan-ok", "startup_marker.txt");
        let dir_str = dir.to_string_lossy().into_owned();

        let app = build_app();
        let index_state = app.state::<crate::index_state::IndexState>();

        // 【生产真实顺序】调用方**不** begin（见 lib.rs）——
        //   扫描的生命周期由 rebuild_engine_if_absent 自己管。
        let (indexed, installed) =
            rebuild_engine_if_absent(app.handle(), &[dir_str.clone()], &index_state)
                .expect("启动路径不该报错");

        assert!(installed, "槽位是空的 → 启动线程应该装上");
        let snap = index_state.snapshot();
        assert!(!snap.scanning, "装完了 scanning 该归 false");
        assert_eq!(snap.paths, vec![dir_str.clone()], "状态里的路径应与引擎一致");
        assert_eq!(snap.indexed, indexed, "状态里的条目数应与引擎一致");
    }

    /// 空路径 → 状态里报的目录必须与引擎实际索引的目录**一致**。
    ///
    /// 之前 `finish(paths.to_vec(), …)` 用的是**未解析**的 paths：
    /// 传空数组时引擎会回退到 `$HOME`，而状态却报「空路径」——
    /// 状态与引擎对不上（`IndexState` 的不变式：indexed 与 paths
    /// 永远描述当前活着的那个引擎）。
    #[test]
    fn empty_paths_report_the_same_directory_the_engine_uses() {
        let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        isolate_home("empty-paths");

        let app = build_app();
        let index_state = app.state::<crate::index_state::IndexState>();

        let (_idx, installed) =
            rebuild_engine_if_absent(app.handle(), &[], &index_state).expect("不该报错");
        assert!(installed, "槽位是空的 → 应该装上");

        let snap = index_state.snapshot();
        // 引擎索引的是 $HOME（空列表的回退）→ 状态必须报同一个
        let home = std::env::var("HOME").expect("HOME");
        assert_eq!(
            snap.paths,
            vec![home],
            "状态报的路径必须与引擎实际索引的目录一致（空 → $HOME）"
        );
    }

    #[test]
    fn reindex_command_is_registered_and_round_trip() {        // 【为什么要有这个测试】命令「定义了」不等于「注册了」。
        // 忘了写进 `invoke_handler` 的话编译期不报错，
        // 只有前端真调的时候才报 `command reindex not found`。
        // 走真实 IPC（`get_ipc_response`）按命令名调，把它验掉。
        let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        isolate_home("reindex-ipc");

        let dir = make_dir_with_file("reindex-ipc", "ipc_marker.txt");
        let dir_str = dir.to_string_lossy().into_owned();

        let app = build_app();
        let webview = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
            .build()
            .expect("创建 webview");

        // 【body 要按参数名包一层】命令的参数名叫 `paths`，
        // Tauri 找的是 body["paths"] —— 直接传数组会报
        // `command reindex missing required key paths`
        let body = tauri::test::get_ipc_response(
            &webview,
            invoke("reindex", serde_json::json!({ "paths": [dir_str] })),
        )
        .expect("reindex 应返回 Ok（命令没注册的话这里会 Err）");
        let val: serde_json::Value = body.deserialize().unwrap();
        assert_eq!(val["scanning"], false, "重建完不该还在 scanning");
        assert!(
            val["indexed"].as_u64().unwrap_or(0) >= 1,
            "新目录应索引到至少 1 项，实际: {val}"
        );
        assert_eq!(val["paths"][0], dir_str, "IndexStatus.paths 应反映新目录");
        assert_eq!(val["error"], serde_json::Value::Null);

        // 引擎真的换掉了 → 新目录里的文件能搜到
        let names = {
            let st = engine_state(&app);
            let g = st.lock().unwrap();
            let s = g.as_ref().expect("引擎应在 state 里");
            s.search(request("ipc_marker"))
                .unwrap()
                .entries
                .iter()
                .map(|e| e.name.clone())
                .collect::<Vec<_>>()
        };
        assert!(
            names.iter().any(|n| n == "ipc_marker.txt"),
            "换完之后应能搜到 ipc_marker.txt，实际: {names:?}"
        );

        // 非法路径 → IPC 返回 Err（旧引擎不动，这里不再重复断言引擎）
        let err = tauri::test::get_ipc_response(
            &webview,
            invoke(
                "reindex",
                serde_json::json!({ "paths": ["/definitely/not/exist/xyz"] }),
            ),
        );
        assert!(err.is_err(), "不存在的目录应该报错，实际: {err:?}");
    }

    #[test]
    fn empty_paths_fall_back_to_home_instead_of_failing() {
        // 【覆盖 C1】`reindex([])` / `rebuild_engine(&[])` 不能报错。
        //
        //   以前只有启动路径有「空 = 家目录」的回退，reindex 那条路没有 →
        //   Engine::new(&[]) → shim 返回 NULL（path_count == 0）
        //   → 用户看到 "invalid argument"，而且「移除最后一个目录」后
        //   UI 永远回不到家目录模式。
        let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = isolate_home("empty-fallback");
        // 在家目录里放个标记文件 → 引擎建好后应该能搜到它
        std::fs::write(home.join("home_marker.txt"), b"hi").unwrap();

        let app = build_app();

        // 1) 底层：rebuild_engine 传空数组 → 回落到 $HOME，不报错
        let indexed = rebuild_engine(app.handle(), &[]).expect("空路径应回落到 $HOME，不该报错");
        assert!(
            indexed >= 1,
            "回落到 $HOME 后应索引到至少 1 项（家目录本身），实际 {indexed}"
        );

        // 2) 引擎真的建在 $HOME 上 → 能搜到家目录里的标记文件
        let names = {
            let st = engine_state(&app);
            let g = st.lock().unwrap();
            let s = g.as_ref().expect("引擎应在 state 里");
            s.search(request("home_marker"))
                .unwrap()
                .entries
                .iter()
                .map(|e| e.name.clone())
                .collect::<Vec<_>>()
        };
        assert!(
            names.iter().any(|n| n == "home_marker.txt"),
            "回落后应能搜到家目录里的 home_marker.txt，实际: {names:?}"
        );

        // 3) effective_paths 本身就是那条唯一的回退
        assert_eq!(
            effective_paths(&[]),
            vec![home.to_string_lossy().into_owned()],
            "空列表应回落到 $HOME"
        );
        assert_eq!(
            effective_paths(&["/tmp/keep-me".to_string()]),
            vec!["/tmp/keep-me".to_string()],
            "非空列表应原样保留"
        );

        // 4) 命令层同样成立：reindex([]) 走真实 IPC 也不能报错
        let webview = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
            .build()
            .expect("创建 webview");
        let body = tauri::test::get_ipc_response(
            &webview,
            invoke("reindex", serde_json::json!({ "paths": [] })),
        )
        .expect("reindex([]) 应回落到 $HOME，不该报错");
        let val: serde_json::Value = body.deserialize().unwrap();
        assert!(
            val["indexed"].as_u64().unwrap_or(0) >= 1,
            "reindex([]) 回落后应索引到至少 1 项，实际: {val}"
        );
        assert_eq!(
            val["paths"][0],
            home.to_string_lossy().into_owned(),
            "IndexStatus.paths 应报回落后的 $HOME，实际: {val}"
        );
    }

    #[test]
    fn failed_reindex_keeps_previous_paths_and_count() {
        // 【覆盖 Important 1】reindex 失败后，IndexState 仍要报【旧引擎】的
        //   路径与条目数 —— 不能报「失败的那组路径 + 0 项」。
        //
        //   否则状态栏说「已索引 0 项」，而结果表里还是旧结果 ——
        //   那是「状态与引擎错位」的反向版本。
        let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        isolate_home("fail-state");

        let dir = make_dir_with_file("fail-state", "ok_marker.txt");
        let dir_str = dir.to_string_lossy().into_owned();

        let app = build_app();
        let webview = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
            .build()
            .expect("创建 webview");

        // 1) 先成功建一次 → paths = [dir]，indexed = N
        let body = tauri::test::get_ipc_response(
            &webview,
            invoke("reindex", serde_json::json!({ "paths": [dir_str] })),
        )
        .expect("第一次 reindex 应成功");
        let before: serde_json::Value = body.deserialize().unwrap();
        let indexed_before = before["indexed"].as_u64().unwrap();
        assert!(indexed_before >= 1, "应索引到至少 1 项，实际 {indexed_before}");
        assert_eq!(before["paths"][0], dir_str);

        // 2) 用非法路径重建 → 必须失败
        let err = tauri::test::get_ipc_response(
            &webview,
            invoke(
                "reindex",
                serde_json::json!({ "paths": ["/definitely/not/exist/xyz"] }),
            ),
        );
        assert!(err.is_err(), "不存在的目录应该报错，实际: {err:?}");

        // 3) 关键断言：状态仍报【旧的那组路径】与【旧的条目数】
        let body = tauri::test::get_ipc_response(
            &webview,
            invoke("index_status", serde_json::json!({})),
        )
        .expect("index_status 应返回 Ok");
        let after: serde_json::Value = body.deserialize().unwrap();
        assert_eq!(
            after["paths"][0], dir_str,
            "失败后 paths 应仍是旧引擎的那组，实际: {after}"
        );
        assert_eq!(
            after["indexed"].as_u64().unwrap(),
            indexed_before,
            "失败后 indexed 应仍是旧引擎的条目数（不是 0），实际: {after}"
        );
        assert_eq!(after["scanning"], false, "失败后不该还在 scanning");
        assert!(
            !after["error"].is_null(),
            "失败原因应记进 IndexStatus.error，实际: {after}"
        );
    }

    #[test]
    fn effective_index_paths_defaults_to_home() {
        // 没配置索引目录时 → 用 $HOME（不是「索引零个目录」——
        // fs_engine_new 传空数组会返回 NULL）
        let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        isolate_home("defaults-home");
        let s = Settings::default();
        let paths = effective_index_paths(&s);
        assert_eq!(paths.len(), 1, "默认应有一个目录");
        assert!(paths[0].is_absolute(), "默认目录应是绝对路径");
    }
}
