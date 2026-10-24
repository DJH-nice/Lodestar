mod commands;
// 【护栏：Rust 发的事件名，前端必须真的在听】
//   —— 只在测试构建里存在（它只是一组断言，没有运行期价值）。
//   为什么需要它：见 `event_names.rs` 的文件头 ——
//   `open-settings` 曾经「发了没人听」，两边各审各的一半，谁也没看见。
#[cfg(test)]
mod event_names;
mod index_state;
// 【为什么 `lifecycle` 与 `tray` 都是私有 `mod`】
//   私有模块里的 `pub` 项**照样会被 `dead_code` 判死** ——
//   这正是我们要的：这两个模块里每个 `pub` 项都**必须有 crate 内的调用点**，
//   没有的话编译器会报出来（以后再写出**真的**死代码，编译器**会**报出来）。
//
//   【它们为什么曾经是 `pub mod`】
//   本 crate 是 lib（`crate-type = ["cdylib","rlib"]`）——
//   lib crate 里 `pub` 的项**永远不会被 `dead_code` 判死**。
//   Task 1 时 `Lifecycle::phase()` / `take_catchup()` / `restore_catchup()`
//   还没有调用点，Task 3 之前 `tray::set_warning` 也没有调用点 ——
//   当时用 `pub mod` 顶住告警。
//   现在这些方法都接上了真实调用点
//   （lifecycle 那三个被 watcher 的对账循环调，见 watcher.rs 的 `decide_rescan`；
//     `tray::set_warning` 被快捷键注册失败的分支调，见下面 setup 里那段）——
//   所以都改回私有 `mod`。
//
//   （不用 blanket `#[allow(dead_code)]`：那会把以后新写的真死代码一并盖住，
//     而「以后记得删」没有任何强制力。）
mod lifecycle;
mod logging;
mod settings;
mod shortcut;
// （`tray` 同上 —— 见上面那段「为什么 `lifecycle` 与 `tray` 都是私有 `mod`」。）
mod tray;
mod watcher;

use std::sync::{Arc, Mutex};

use search_core::Searcher;
use index_state::IndexState;
use lifecycle::Lifecycle;
use tauri::menu::{Menu, MenuItemBuilder, MenuItemKind, HELP_SUBMENU_ID};
use tauri::{AppHandle, Emitter, Manager, Runtime};

/// 应用的 IPC 命令表 —— **生产与测试共用同一份**。
///
/// 【为什么要共用这一份】
///   命令「定义了」不等于「注册了」——
///   忘了写进 `invoke_handler` 的话**编译期不报错**，
///   只有前端真调的时候才报 `command xxx not found`。
///   以前每个测试各自抄一份命令表 → 抄漏了测试也发现不了
///   （验的是「抄的那份」，不是「生产真正在用的那份」）。
///   现在只有这一份 → 测试里注册的就是生产在用的，
///   「忘了注册」这类漏洞在测试期就暴露。
///
/// 【为什么泛型化】测试用 `MockRuntime`、生产用 `Wry` —— 两边要能用同一份表。
pub fn app_commands_handler<R: tauri::Runtime>(
) -> impl Fn(tauri::ipc::Invoke<R>) -> bool + Send + Sync + 'static {
    tauri::generate_handler![
        commands::search,
        commands::rescan,
        commands::syntax_tables,
        commands::exclude_rules,
        commands::index_status,
        commands::ack_visibility,
        commands::open_path,
        commands::get_settings,
        commands::set_settings,
        commands::patch_settings,
        commands::reindex,
        commands::write_bookmarks_file,
        commands::read_bookmarks_file,
        commands::set_global_shortcut,
        commands::get_global_shortcut,
        commands::set_launch_at_login,
        commands::get_launch_at_login,
    ]
}

/* ============================================================
   应用菜单
   ============================================================ */

/// 菜单项 id —— Rust 侧与前端约定的是同一个字符串。
///
/// 【为什么要有 id】菜单事件只带一个 id 回来；
/// 前端靠它知道「用户点的是哪一项」。
pub const OPEN_USER_GUIDE_ID: &str = "open-user-guide";

/// 事件名 —— Rust 发、前端收。
pub const OPEN_USER_GUIDE_EVENT: &str = "open-user-guide";

/// 菜单项 id —— 「搜索语法速查」（帮助中心里的速查页）。
///
/// 【为什么单独一项，而不是让用户自己在手册里翻】
/// 速查是**给非科班用户的入口**（按「我想找什么」组织的写法），
/// 手册是**给愿意读文档的人的完整参考**。两者内容不同、用途不同，
/// 但都在「帮助中心」这一个面板里 —— 菜单上给两个入口，用户不用先想
/// 「我要看的是速查还是手册」。
pub const OPEN_SYNTAX_CHEATSHEET_ID: &str = "open-syntax-cheatsheet";

/// 事件名 —— Rust 发、前端收。
pub const OPEN_SYNTAX_CHEATSHEET_EVENT: &str = "open-syntax-cheatsheet";

/// 菜单项 id —— 「致谢与许可」。
///
/// 【为什么有这一项】本软件分发了 FSearch 0.3.2 的引擎代码（GPL-3.0），
///   GPL-3.0 §5(d) 要求「醒目标注你修改过它」并说明它按该许可发布。
///   → 必须在**应用里**有一处能看见这句话的地方，
///     不能只写在仓库 README 里（装到 /Applications 之后就没人看得到）。
///
/// 【为什么不叫「关于」】macOS 的应用菜单里已经有系统的「关于 Lodestar」
///   （由 `Menu::default()` 提供，显示版本号与版权）。
///   再来一个同名的项会让人以为是同一个东西。
///   → 叫「致谢与许可」，指向的是**法务信息**（引擎出处 + 许可证），
///     而不是「版本号 + 版权声明」。
pub const OPEN_CREDITS_ID: &str = "open-credits";

/// 事件名 —— Rust 发、前端收。
pub const OPEN_CREDITS_EVENT: &str = "open-credits";

/// **开机自启的启动标记** —— 登录项拉起我们时带着它。
///
/// 【为什么要有它】开机自启的承诺是「登录后在**后台**待命」——
///   弹一个窗口出来既吵又不对（用户还没开始工作呢）。
///   登录项（LaunchAgent）的 ProgramArguments 由 `auto-launch` 写成
///   `[app_path] + args`（`auto-launch-0.6.0/src/macos.rs` 的
///   `build_launch_agent_plist`），所以我们在
///   `tauri_plugin_autostart::init(..., Some(vec![HIDDEN_FLAG]))` 里
///   传这个参数 → 登录时进程带着它启动 → 应用据此**不显示窗口**。
pub const HIDDEN_FLAG: &str = "--hidden";

/// 这次是不是「开机自启」拉起来的（带着 [`HIDDEN_FLAG`]）。
///
/// 【为什么是纯函数】它只读 `env::args` —— 纯到可以在任何地方调，
///   不用把状态传来传去。
pub fn started_hidden() -> bool {
    std::env::args().any(|a| a == HIDDEN_FLAG)
}

/// 把主窗口显示出来并聚焦 —— **手动启动**时用。
///
/// 【为什么不在 setup 里就 `show()`】setup 跑在事件循环**之前**，
///   那时 `show()` 能让窗口出现，但**抢不到焦点**（应用还没进前台）。
///   `RunEvent::Ready` 是事件循环马上要开跑的那一刻 —— 那儿聚焦才靠谱。
fn show_main_window<R: Runtime>(app: &AppHandle<R>) {
    if let Some(win) = app.get_webview_window("main") {
        let _ = win.show();
        let _ = win.set_focus();
    }
}

/// 装应用菜单：**默认菜单 + Help 子菜单里的两项**。
///
/// 【Help 子菜单里有什么】
///   - 「搜索语法速查」→ 帮助中心的速查页（给非科班用户的写法清单）
///   - 「完整手册」    → 帮助中心的手册页（完整参考）
///
/// 【为什么是两个入口】速查是**照着抄**的写法清单（按「我想找什么」组织），
/// 手册是**愿意读**的完整文档。两者内容不同、用途不同，
/// 但都在「帮助中心」这一个面板里 —— 菜单上分两项，用户不用先想
/// 「我要看的是速查还是手册」。

/// 【为什么用 `Menu::default()`，不手写整套】
/// 默认菜单在 macOS 上是：
///   Lodestar（关于 / 服务 / 隐藏 / 退出）+ File + Edit + View + Window + Help
/// 其中 **Edit 子菜单带 undo / redo / cut / copy / paste / select_all**。
/// 手写一套的话几乎一定会漏掉 Edit ——
/// 而 Edit 的复制/粘贴是搜索框的刚需
/// （引擎的词法器把 `"` 当引号，用户要粘一段带空格的路径进去）。
///
/// 另外默认菜单里还有一堆平台相关的细节
/// （macOS 上 Quit 在应用菜单里而不在 File 里、File 只有 close window、
/// View 只有 fullscreen……），手写等于把这些细节重新踩一遍。
///
/// 【为什么抽成 pub fn】为了能写测试 ——
/// `app/src-tauri/tests/menu.rs` 直接调它，断言
/// 「Help 子菜单里确实有 id == OPEN_USER_GUIDE_ID 的那一项」
/// 以及「默认菜单的 Edit 子菜单还在、里面还有 copy / paste」。
pub fn build_menu<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<Menu<R>> {
    let menu = Menu::default(app)?;
    let guide_item = MenuItemBuilder::with_id(OPEN_USER_GUIDE_ID, "完整手册").build(app)?;
    let cheat_item =
        MenuItemBuilder::with_id(OPEN_SYNTAX_CHEATSHEET_ID, "搜索语法速查").build(app)?;
    let credits_item = MenuItemBuilder::with_id(OPEN_CREDITS_ID, "致谢与许可").build(app)?;
    match menu.get(HELP_SUBMENU_ID) {
        Some(MenuItemKind::Submenu(help)) => {
            // 速查排在前面 —— 它是更常用、也更「救命」的那个入口。
            help.append(&cheat_item)?;
            help.append(&guide_item)?;
            // 致谢与许可排最后 —— 它是「偶尔要看一眼」的那类信息。
            help.append(&credits_item)?;
        }
        // 拿不到 Help 子菜单 —— 入口装不上，但应用其余部分照常用。
        _ => eprintln!("[lodestar] 默认菜单里没有 Help 子菜单，「完整手册」入口没装上"),
    }
    Ok(menu)
}

/// 装菜单 + 收菜单事件。
///
/// 【菜单事件回调里只发事件】不碰界面、不做重活 ——
/// 让前端去开手册面板。菜单回调跑在主线程上，
/// 在里面做重活会把整个应用卡住。
fn install_menu<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<()> {
    let menu = build_menu(app)?;
    app.set_menu(menu)?;

    app.on_menu_event(|app, event| {
        if event.id() == OPEN_USER_GUIDE_ID {
            if let Err(e) = app.emit(OPEN_USER_GUIDE_EVENT, ()) {
                eprintln!("[lodestar] 通知前端打开手册失败: {e}");
            }
        } else if event.id() == OPEN_SYNTAX_CHEATSHEET_ID {
            if let Err(e) = app.emit(OPEN_SYNTAX_CHEATSHEET_EVENT, ()) {
                eprintln!("[lodestar] 通知前端打开语法速查失败: {e}");
            }
        } else if event.id() == OPEN_CREDITS_ID {
            if let Err(e) = app.emit(OPEN_CREDITS_EVENT, ()) {
                eprintln!("[lodestar] 通知前端打开致谢与许可失败: {e}");
            }
        }
    });

    Ok(())
}

/// **装菜单栏图标 + 注册全局快捷键** —— 在「事件循环刚开跑」的那一刻做。
///
/// 【为什么不在 `setup` 里做】
///   `setup` 钩子跑在 **`applicationDidFinishLaunching` 之前**
///   （`tauri/src/app.rs:2697-2698`：`setup` 在 `Builder::build()` 里跑，
///    那时事件循环还没开跑）。
///   而菜单栏状态项（`NSStatusItem`）正是在那一刻被加进菜单栏的 ——
///   在它之前建，会不会导致「状态项建了、却没被画出来」？
///
///   **这是终审 I-2 留下的疑点**（托盘图标建了却不渲染）：
///   复审指出，之前那个「纯 AppKit 探针也一样不显示」的对照**不成立** ——
///   那个探针的状态项**也**建在 `app.run()` 之前，
///   跟 FSearch 犯的是同一个错 → 构不成有效对照。
///   → 要判定，只能让 FSearch 跟探针**在时序上对齐**：
///     托盘挪到 `RunEvent::Ready`（= `applicationDidFinishLaunching` 之后），
///     探针也建在 `applicationDidFinishLaunching` 之后 —— 两边才可比。
///     （实测见 `.superpowers/sdd/2026-10-22-background-resident-mode/final-fix-report.md`。）
///
/// 【为什么两样东西放在一起，而且【先托盘、后快捷键】】
///   快捷键注册失败要在**托盘菜单**顶部插一条警告项
///   （托盘是常驻的；设置面板可能正关着）。
///   而 `tray::set_warning` 在托盘还不存在时会**静默 return**
///   （`tray.rs`：`tray_by_id` 返回 `None`）→ 警告就丢了。
///   → 先建托盘、再注册快捷键，顺序天然对。
///
/// 【失败不许弄死应用】
///   托盘装不上 = 少一个入口（窗口、快捷键照常用）；
///   快捷键注册不上 = 少一个入口（多半是被别的软件占着）。
///   两者都只记日志 + 降级，**绝不 panic**。
fn install_tray_and_shortcut<R: Runtime>(app: &AppHandle<R>) {
    // 【装菜单栏图标】—— 装不上不许 panic。
    if let Err(e) = tray::build_tray(app) {
        eprintln!("[lodestar] 装菜单栏图标失败: {e}");
    }
    // 【注册全局快捷键】——
    //   读设置里存的那个（没有就用默认值）。
    //   注册失败**不许 panic**：快捷键被别的软件占着是很常见的事。
    let s = settings::load(app);
    let spec = s.global_shortcut.clone();
    match shortcut::apply(app, None, spec.as_deref()) {
        // 【成功时什么都不做】托盘菜单刚由 `build_tray` 用
        //   `warning = None` 建好 —— 再 `set_warning(app, None)`
        //   是把**同样的菜单**重建一遍换上去：
        //   语义 no-op，却多一次 muda 菜单创建 + 一次托盘菜单替换，
        //   还多碰一次 muda 的主线程操作面。
        Ok(()) => {}
        Err(e) => {
            eprintln!("[lodestar] 注册全局快捷键失败: {e}");
            // 【D5】注册失败必须**看得见** ——
            //   在托盘菜单顶部插一条禁用的警告项。
            //   （托盘是常驻的；设置面板可能正关着。）
            //
            // 【为什么把 `e` 记进日志而不是菜单项】
            //   打包成 .app 后 **stderr 对用户不可见**，
            //   只留一句「可能已被占用」的话 → 用户只知道「没生效」，
            //   不知道是**哪个键**、**为什么**。
            //   而且「可能已被占用」对「格式非法」（比如 settings.json 被手改）
            //   是**误导**的 —— 真正的原因在 `e` 里。
            //
            // 【为什么菜单项只放一句短的】
            //   macOS 的菜单项过长会被**截断**显示。
            //   托盘上留个「没生效」的提示就够，
            //   详情在设置面板里（那里有完整的句子）。
            //
            // 【托盘这会儿已经建好了】—— 见上面的顺序说明。
            crate::tray::set_warning(app, Some("⚠️ 快捷键未生效"));
        }
    }
}

/// 关窗时把窗口位置与大小存回设置。
///
/// 【为什么抽成函数】原来的逻辑内联在 `on_window_event` 的闭包里，
///   现在那个闭包要同时处理三件事（存位置 / 拦关闭 / 记焦点），
///   塞不下 —— 抽出来更好读。
fn save_window_geometry<R: Runtime>(win: &tauri::WebviewWindow<R>, handle: &AppHandle<R>) {
    let (pos, size) = match (win.outer_position(), win.outer_size()) {
        (Ok(p), Ok(sz)) => (p, sz),
        _ => return,
    };
    // 【写盘走 `settings::update`】（终审 M-10）
    //   「读-改-写」要**原子** —— 关窗这一刻前端可能正在写书签，
    //   两个写者交错时后写的那份会把对方没读到的字段写回旧值
    //   （最常见的后果：刚加的书签被抹掉 / 窗口位置没存住）。
    let result = settings::update(handle, |s| {
        s.window_x = Some(pos.x);
        s.window_y = Some(pos.y);
        s.window_width = Some(size.width);
        s.window_height = Some(size.height);
        Ok(())
    });
    if let Err(e) = result {
        eprintln!("[lodestar] 保存窗口位置失败: {e}");
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_store::Builder::default().build())
        .plugin(tauri_plugin_dialog::init())
        // 【全局快捷键插件】——
        //   注册逻辑全在 Rust 侧（见 `shortcut::apply`），
        //   不需要前端 capability。
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        // 【开机自启插件】——
        //   用 **LaunchAgent**（`~/Library/LaunchAgents` 下的 plist）——
        //   macOS 13+ 上它比「登录项」更干净（不往「系统设置 → 通用 → 登录项」
        //   里塞一条用户看得见、也删得掉的条目）。
        //   插件的 Rust 侧 API（`app.autolaunch()`）不需要前端 capability ——
        //   开关逻辑全在 Rust 侧（见 `commands::set_launch_at_login`）。
        //
        // 【为什么带 `--hidden`】登录项拉起我们时**不该弹窗口** ——
        //   它的承诺是「登录后在后台待命」，弹个窗口出来既吵又不对。
        //   `auto-launch` 会把这里的参数原样写进 plist 的 ProgramArguments
        //   （`auto-launch-0.6.0/src/macos.rs` 的 `build_launch_agent_plist`），
        //   于是登录项启动时进程带着 `--hidden` → 应用侧据此**不显示窗口**。
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec![HIDDEN_FLAG]),
        ))
        .manage(Mutex::new(Option::<Arc<Searcher>>::None))
        .manage(IndexState::new())
        .manage(Lifecycle::new())
        .setup(|app| {
            // 【设置迁移 —— 必须是 setup 的第一件事】
            //   产品名从 FSearch 改成 Lodestar → `identifier` 跟着变 →
            //   tauri-plugin-store 落在**以 identifier 命名**的目录里 →
            //   老用户的书签 / 最近搜索 / 索引目录 / 窗口位置全在新目录之外。
            //   → 这里先把旧目录里的 settings.json 拷过来。
            //
            //   【为什么必须排在最前面】第一次读设置发生在下面的启动线程里
            //   （`settings::load(&thread_handle)`）—— 迁移要赶在它前面。
            //   而且它【只做一次】：新目录里已经有 settings.json 就直接跳过。
            settings::migrate_legacy_store(app.handle());
            // 【开机自启的启动】—— 登录项带着 `--hidden` 拉起我们时，
            //   窗口要**藏起来**（菜单栏图标待命），不能弹出来。
            //   （`tauri.conf.json` 里窗口默认 `visible: false` → 窗口本来就是藏着的；
            //     这里要把**状态**也记对：窗口不可见 → 判空闲 → 不空转重扫。）
            //   反过来，**手动启动**时窗口要出来 —— 见下面 `show_main_window`。
            let start_hidden = started_hidden();
            if start_hidden {
                let life = app.state::<Lifecycle>();
                life.set_visible(false);
                life.mark_idle();
            }
            // 【装应用菜单】默认菜单 + Help 子菜单里的「完整手册」。
            // 装不上也不该让应用起不来 —— 记一句日志，继续跑。
            if let Err(e) = install_menu(app.handle()) {
                eprintln!("[lodestar] 装菜单失败: {e}");
            }
            // 【托盘图标与全局快捷键 —— 不在这里建，见 `RunEvent::Ready`】
            //   这两样都要在「事件循环刚开跑」的那一刻建（理由见下面那段注释）。
            //   （菜单栏图标建在 `applicationDidFinishLaunching` **之前**这件事，
            //     是终审 I-2 留下的疑点 —— 见 `RunEvent::Ready` 分支里的说明。）
            //
            // 【文件变化自动刷新】—— FSEvents 监听 + 去抖 + 后台重扫。
            //
            // 【为什么在这里建、而且是【常驻】的】
            //   watcher 要在 setup 里建（那时才拿得到 AppHandle），
            //   之后**一直活到进程结束** —— 不要在每次 reindex 时重建它：
            //   「监听哪些目录」由 watcher 自己每 1 秒**对账**一次决定
            //   （读 IndexState::paths()，见 watcher.rs），换目录它自己会跟上。
            watcher::spawn_watcher(app.handle());
            // 【空闲看门狗】—— 每秒重新判定「你是不是不用我了」。
            // 必须在 Lifecycle 被 manage 之后启动（它要读那个状态）。
            lifecycle::spawn_idle_watchdog(app.handle().clone());
            // 【只把 handle 搬进线程】`app` 是 setup 收到的 `&mut App`，
            // 不是 'static —— 把 `app.state::<IndexState>()` 借来的 State
            // 放进 spawn 的闭包里会编译失败（borrowed value does not live long enough）。
            // AppHandle 是 Clone + Send + 'static，在线程里重新 state() 取即可。
            let handle = app.handle().clone();
            // 【线程要一份自己的 handle】`handle` 后面还要用来恢复窗口位置，
            // 直接 move 进 spawn 的闭包会报
            // error[E0382]: borrow of moved value: `handle`
            let thread_handle = handle.clone();
            std::thread::spawn(move || {
                // 启动时读设置 —— 必须在建引擎【之前】，
                // 因为引擎要知道索引哪些目录。
                // store 的 Rust API 是同步的，这里直接读即可。
                let settings = settings::load(&thread_handle);
                let paths = settings::effective_index_paths(&settings);

                let state = thread_handle.state::<IndexState>();
                let path_strs: Vec<String> = paths
                    .iter()
                    .map(|p| p.to_string_lossy().into_owned())
                    .collect();
                // 【建引擎走 rebuild_engine_if_absent —— 不是 rebuild_engine】
                //   以前这里是【无条件替换】全局句柄：
                //   于是「启动线程建引擎」和「用户换索引目录（reindex）」撞上时，
                //   谁后到谁赢 —— 启动线程后到就把用户刚换的目录**覆盖回去**，
                //   设置里写的是新目录、引擎却索引着旧目录（设置 ≠ 引擎）。
                //
                //   实测踩过（探针日志）：
                //     reindex(临时目录) → 索引就绪：2 项
                //     search "marker"  → 1 条 ✓
                //     [lodestar] 索引就绪：57817 项   ← 启动线程把引擎换回 $HOME
                //     search "marker"  → 0 条 ✗      ← 同一句查询，结果凭空没了
                //
                //   现在：**只在全局槽位还空着时装** —— 若 reindex 抢先装上了，
                //   启动线程就跳过（引擎与 IndexState 都不动）。
                //   两种先后顺序，最后活着的引擎都是用户后来选的那个。
                //
                //   【顺序不变】仍然是「先装引擎、再 finish() 状态」
                //   （Task 2 定的不变式：indexed 与 paths 永远描述活着的那个引擎）。
                //
                //   【state.begin() 不在这里调】—— 扫描的生命周期由
                //   rebuild_engine_if_absent 自己管
                //   （begin → 建 → 提交 → end_scan(令牌)；
                //     放弃时也用它自己的令牌放回 scanning）。
                //   上一版 begin() 在这儿、放弃时没人管 → scanning 永远卡在 true
                //   （状态栏永远「正在扫描…」+ 索引就绪后不自动重搜）。
                match settings::rebuild_engine_if_absent(&thread_handle, &path_strs, &state) {
                    Ok((indexed, true)) => {
                        println!("[lodestar] 索引就绪：{indexed} 项");
                    }
                    Ok((_indexed, false)) => {
                        // 已经有引擎了（reindex 抢先）—— 本次什么都不改：
                        // 引擎是用户的那个，状态也是 reindex 提交的那份。
                        // （① 预检放弃路径**什么都不动** —— 这一趟没 begin() 过，
                        //   没有自己的扫描标志可放；别人的 scanning 不归我们管。
                        //   详见 index_state.rs 的 ScanToken。）
                        println!("[lodestar] 启动索引已被换掉，跳过（以当前引擎为准）");
                    }
                    Err(e) => {
                        // fail() 已在 rebuild_engine_if_absent 里做过
                        // （它 begin() 过，失败时要自己把 scanning 放回去）
                        // —— 现在那一步由 run_scan_blocking 统一管。
                        eprintln!("[lodestar] 建索引失败: {e}");
                    }
                }
            });
            // 【恢复上次的窗口位置/大小】——
            // 没记录就用 tauri.conf.json 里的默认（x/y 硬编码在那儿）。
            // 【物理像素】—— 和保存时读的 outer_size() 单位一致，
            // 读物理写逻辑会在 Retina 屏上偏移。
            if let Some(win) = handle.get_webview_window("main") {
                let settings = settings::load(&handle);
                if let (Some(w), Some(h)) = (settings.window_width, settings.window_height) {
                    let _ = win.set_size(tauri::PhysicalSize::new(w, h));
                }
                if let (Some(x), Some(y)) = (settings.window_x, settings.window_y) {
                    let _ = win.set_position(tauri::PhysicalPosition::new(x, y));
                }
                // 【手动启动 → 窗口要出来】
                //   （`tauri.conf.json` 里窗口默认 `visible: false` ——
                //     那是给「开机自启」用的：登录时**不该**弹窗口。
                //     手动启动是另一回事 —— 用户就是来开窗口的。）
                //   聚焦放在 `RunEvent::Ready`（见文件末尾）—— setup 阶段
                //   应用还没进前台，那儿 `set_focus()` 不一定管用。
                if !start_hidden {
                    let _ = win.show();
                }
                // 窗口事件：**关窗不退出，改为隐藏到后台**。
                //
                // 【`on_window_event` 消费 self】—— 签名是 `fn on_window_event(self, f)`，
                // 所以 `win` 会被 move 进去。而闭包里又要读窗口的位置/大小，
                // 于是闭包又借用了 `win` → 报
                // error[E0505]: cannot move out of `win` because it is borrowed。
                // 解法：先 clone 一份给闭包用（WebviewWindow 是 Clone）。
                let win_for_event = win.clone();
                let save_handle = handle.clone();
                let event_handle = handle.clone();
                win.on_window_event(move |event| match event {
                    tauri::WindowEvent::CloseRequested { api, .. } => {
                        // ① 先把位置与大小存回去
                        save_window_geometry(&win_for_event, &save_handle);
                        // ② 【关键】拦下来 ——
                        //    不拦的话 webview 会被**销毁**，
                        //    后面就再也唤不回来了（唤起会是个白屏）。
                        api.prevent_close();
                        // ③ 隐藏到后台，进程继续活着
                        lifecycle::hide(&event_handle, &win_for_event);
                    }
                    tauri::WindowEvent::Focused(focused) => {
                        // 焦点变了 → 更新「在前台」的记录（看门狗据此判空闲）
                        // 【为什么要 `*focused`】`on_window_event` 的闭包收到的是
                        // `&WindowEvent`，匹配 `Focused(focused)` 时按 Rust 的
                        // 默认绑定模式，`focused` 拿到的是 `&bool` 而不是 `bool` ——
                        // 直接传给 `set_focused(bool)` 会报
                        // error[E0308]: expected `bool`, found `&bool`。
                        let life = event_handle.state::<Lifecycle>();
                        life.set_focused(*focused);
                        // 【窗口重新拿到焦点 → 它一定可见了】
                        //   这条补的是「⌘H 把窗口收走」那条路：
                        //   系统收走窗口时**我们的 `hide()` 不会被调用** →
                        //   前端收不到「窗口没了」的通知 → 它把状态轮询停了。
                        //   用户用 ⌘Tab 切回来时窗口重新获得焦点 → 这里通知前端「可见了」
                        //   → 它把轮询开回来。
                        //
                        //   【为什么「有焦点」能推出「可见」】不可见的窗口拿不到焦点。
                        //   【为什么不用 `focused == false` 来停】失焦 ≠ 隐藏 ——
                        //   窗口还开着、用户在看别的 app 时也是失焦，
                        //   那时状态栏是看得见的，轮询还得留着。
                        if *focused {
                            lifecycle::emit_visibility(&event_handle, true);
                        }
                    }
                    _ => {}
                });
            }
            Ok(())
        })
        .invoke_handler(app_commands_handler::<tauri::Wry>())
        .build(tauri::generate_context!())
        .expect("构建 Tauri 应用失败")
        .run(|app, event| match event {
            // 【手动启动 → 窗口要出来，并且要拿到焦点】
            //   setup 里已经 `show()` 过了（窗口出现），
            //   但**聚焦**要等到事件循环马上要开跑的那一刻才靠谱 ——
            //   setup 阶段应用还没进前台，那儿 `set_focus()` 常常不生效。
            //   开机自启（`--hidden`）那次**不**显示窗口。
            tauri::RunEvent::Ready => {
                // 【托盘图标 + 全局快捷键 —— 为什么在这儿建】
                //   `setup` 早于 `applicationDidFinishLaunching`；
                //   状态项（NSStatusItem）在那个时刻之前建，
                //   有没有可能「建了却不渲染」——
                //   这是终审 I-2 的疑点，挪到这里是为了跟探针对齐时序。
                //   详见 [`install_tray_and_shortcut`] 的说明。
                install_tray_and_shortcut(app);
                if !started_hidden() {
                    show_main_window(app);
                }
            }
            // 【点 Dock 图标 → 窗口要出来】
            //   窗口默认是隐藏的（`visible: false`）+ 关窗会隐藏到后台
            //   → 没有这一条，点 Dock 图标会**什么也没发生**。
            //   `wake` = 显示 + 聚焦 + 通知前端聚焦搜索框（见 lifecycle.rs）。
            //
            // 【为什么只在 `!has_visible_windows` 时才动手】
            //   窗口**本来就可见**时点 Dock 也会触发这个事件 ——
            //   那时 macOS 自己会把应用激活，我们再 `wake` 一次就是
            //   **多一次补扫**（实测：索引 8119 项时约 300 毫秒）
            //   + **搜索框被全选**
            //   （用户正看着结果表呢，文本被选中很难受）。
            //
            // 【为什么整段按平台 cfg】`Reopen` 是 **macOS 专属**的事件 ——
            //   非 macOS 平台上这个变体**根本不存在** → 不 cfg 的话编译不过。
            //   本项目是 macOS-only，但把这条**平台假设显式写出来**，
            //   将来若要支持别的平台，这里就是提醒。
            #[cfg(target_os = "macos")]
            tauri::RunEvent::Reopen {
                has_visible_windows,
                ..
            } => {
                if !has_visible_windows {
                    lifecycle::wake(app);
                }
            }
            _ => {}
        });
}
