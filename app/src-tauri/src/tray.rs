//! 菜单栏图标 —— 让用户不看 Dock 也能唤起 Lodestar。
//!
//! 【左键 vs 右键】
//!   `show_menu_on_left_click` 的**默认值是 `true`** ——
//!   那样左键会直接弹菜单，唤起就没法做了。必须**显式关掉**：
//!   左键 → 唤起/收起（`toggle`），右键 → 弹菜单。
//!
//! 【左键是 `toggle`，菜单里的「打开 Lodestar」是 `wake`】
//!   菜单项的文案是「**打开**」—— 窗口已经可见且聚焦时，
//!   点「打开」应该把它**叫到前面来**，而不是「点了反而没了」。
//!   → 菜单项走 `wake`（只唤起），`toggle`（会收起）只留给**左键图标**。
//!
//! 【为什么要能加「警告项」】
//!   快捷键注册失败（被别的软件占着）时，用户得**看得见** ——
//!   设置面板可能正关着，而托盘菜单是**常驻**的。
//!   → 菜单顶部插一条**禁用的**警告项（见 [`set_warning`]）。
//!
//! 【⚠️ 建菜单必须在【进程主线程】调用】
//!   本模块的菜单最终由 muda 建出来，而 muda 在 macOS 上
//!   **只能在进程主线程**建菜单
//!   （muda 0.20 `platform_impl/macos/mod.rs:121` 会直接 panic：
//!    「`muda::Menu` can only be created on the main thread」）。
//!   **崩的是整个进程**，不是一次调用失败。
//!   → 调用方（`build_tray` / `set_warning`）必须在主线程上跑。
//!     Tauri 的 `setup` 钩子跑在主线程 ✅；
//!     但**别从 async 命令里调** —— Tauri 会把 async 命令 spawn 到
//!     tokio 线程池上 → muda panic → 进程崩。

use tauri::menu::{IsMenuItem, Menu, MenuItem, MenuItemBuilder};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
// 【为什么不要 Manager】`app.tray_by_id(..)` 是 `AppHandle` 的**固有方法**
//   （tauri 2.12 `app.rs` 的 `shared_app_impl!` 宏里），不是 trait 方法 ——
//   把 `Manager` 引进来的话编译器会报 unused import。
use tauri::{AppHandle, Runtime};

/// 菜单项 id —— Rust 侧与前端约定的是同一个字符串。
pub const TRAY_OPEN_ID: &str = "tray-open";
pub const TRAY_SETTINGS_ID: &str = "tray-settings";
pub const TRAY_QUIT_ID: &str = "tray-quit";
/// 警告项的 id（禁用状态，只是给人看的）。
pub const TRAY_WARNING_ID: &str = "tray-warning";
/// 托盘图标的 id —— 后面要靠它找回这个图标来改菜单。
pub const TRAY_ID: &str = "main";

/// 托盘图标资源（22×22@2x 的模板图）。
///
/// 【为什么用 include_bytes! 而不是运行时读文件】
///   打包成 .app 后资源在 bundle 里，运行时读路径不好算；
///   编进二进制最省事。
///
/// 【为什么资源在 `assets/` 而不是 `icons/`】
///   `icons/` 是 **Tauri bundle 的 app 图标目录**（`tauri.conf.json` 按固定文件名取
///   `32x32.png` / `icon.icns` …），混放托盘资源以后加 app 图标时容易混淆。
const TRAY_ICON: &[u8] = include_bytes!("../assets/tray.png");

/// 一条菜单项的**描述** —— 纯数据（好测）。
///
/// 【为什么要有它】
///   真正把菜单**建出来**（muda）在 macOS 上**只能在进程主线程**做：
///   muda 0.20 `platform_impl/macos/mod.rs:121` 会直接 panic ——
///   「`muda::Menu` can only be created on the main thread」。
///   而 Rust 的测试框架把每个 `#[test]` 放在**新建的线程**上跑
///   → 单测里**根本建不出菜单**（实测：mock runtime 也一样炸）。
///   → 于是把「菜单该长什么样」抽成纯函数（[`menu_entries`]），
///     muda 只当**渲染器**。结构可测，渲染那一层薄到一眼能看对。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuEntry<'a> {
    /// 菜单项 id。
    pub id: &'static str,
    /// 菜单项文字。
    pub text: &'a str,
    /// 能不能点。
    pub enabled: bool,
}

/// 菜单的**结构** —— 纯函数（好测）。
///
/// 【为什么警告项在**顶部**】
///   快捷键注册失败时，用户能看见的地方就是这条警告项
///   （设置面板可能正关着，托盘菜单才是常驻的）。
///   插在顶部 → 一打开菜单第一眼就看到，不用往下找。
///
/// 【为什么警告项是**禁用的**】
///   它是给人看的**状态**，不是一个能点的按钮 ——
///   能点却什么都不发生，用户会以为程序卡了。
pub fn menu_entries(warning: Option<&str>) -> Vec<MenuEntry<'_>> {
    let mut entries: Vec<MenuEntry<'_>> = Vec::new();
    if let Some(msg) = warning {
        entries.push(MenuEntry {
            id: TRAY_WARNING_ID,
            text: msg,
            enabled: false,
        });
    }
    entries.push(MenuEntry {
        id: TRAY_OPEN_ID,
        text: "打开 Lodestar",
        enabled: true,
    });
    entries.push(MenuEntry {
        id: TRAY_SETTINGS_ID,
        text: "设置…",
        enabled: true,
    });
    entries.push(MenuEntry {
        id: TRAY_QUIT_ID,
        text: "退出",
        enabled: true,
    });
    entries
}

/// 建托盘菜单。
///
/// `warning = Some(msg)` 时在**顶部**插一条禁用的警告项。
pub fn build_menu<R: Runtime>(
    app: &AppHandle<R>,
    warning: Option<&str>,
) -> tauri::Result<Menu<R>> {
    // 【结构只有一份真相】`menu_entries` 是纯函数、可测；
    //   这里只负责把结构**渲染**成真正的菜单项。
    let mut items: Vec<MenuItem<R>> = Vec::new();
    for entry in menu_entries(warning) {
        let item = MenuItemBuilder::with_id(entry.id, entry.text).build(app)?;
        // 【为什么无条件设一次】渲染层要**无分支** ——
        //   「能不能点」直接来自被测过的 `MenuEntry.enabled`，
        //   不再有「忘了写 if」把警告项留成可点的可能。
        //   （代价：每个菜单项多一次 muda 的 `set_enabled` 调用，可忽略。）
        item.set_enabled(entry.enabled)?;
        items.push(item);
    }
    let refs: Vec<&dyn IsMenuItem<R>> =
        items.iter().map(|i| i as &dyn IsMenuItem<R>).collect();
    Menu::with_items(app, &refs)
}

/// 装菜单栏图标。
///
/// 【装不上不许 panic】—— 装不上只是少一个入口，
///   应用其余部分（窗口、快捷键）照常用。
pub fn build_tray<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<()> {
    let menu = build_menu(app, None)?;
    TrayIconBuilder::with_id(TRAY_ID)
        .icon(tauri::image::Image::from_bytes(TRAY_ICON)?)
        // 【模板图】macOS 按菜单栏深/浅自动反色
        .icon_as_template(true)
        // 【默认是 true】必须显式关掉，否则左键只会弹菜单
        .show_menu_on_left_click(false)
        .menu(&menu)
        .on_menu_event(|app, event| {
            match event.id().as_ref() {
                // 【菜单项要「打开」，不是「切换」】文案写的是「打开 Lodestar」——
                //   窗口已经可见且聚焦时，用户点它反而把窗口**收起**了（点了反而没了）。
                //   → 菜单项走 `wake`（只唤起，不收起）；`toggle` 只留给**左键图标**。
                TRAY_OPEN_ID => crate::lifecycle::wake(app),
                TRAY_SETTINGS_ID => crate::lifecycle::open_settings(app),
                // 【真的退出】不是隐藏 —— 用户点「退出」就是要关掉它
                TRAY_QUIT_ID => app.exit(0),
                _ => {}
            }
        })
        .on_tray_icon_event(|tray, event| {
            // 【要匹配 button_state == Up】—— 不然按下和抬起会触发两次
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                crate::lifecycle::toggle(tray.app_handle());
            }
        })
        .build(app)?;
    Ok(())
}

/// 改托盘菜单顶部的警告项 —— `None` = 撤掉警告。
///
/// 【托盘图标可能还没建好】`build_tray` 失败时（或者调用顺序反了），
///   `tray_by_id` 会返回 `None` —— 这时**什么也不做**，
///   不能让「加个警告」把应用弄崩。
///
/// 【⚠️ 必须在【进程主线程】调用】
///   本函数会重建菜单（`build_menu` → muda 建菜单），
///   而 muda 在 macOS 上**只能在进程主线程**建菜单
///   （muda 0.20 `platform_impl/macos/mod.rs:121` 直接 panic：
///    「`muda::Menu` can only be created on the main thread」）。
///   **崩的是整个进程**，不是一次调用失败。
///   → 调用方必须在主线程上跑：Tauri 的 `setup` 钩子 ✅；
///     **别从 async 命令里调**（Tauri 会把 async 命令 spawn 到 tokio 线程）
///     —— 那会让整个应用崩掉。
pub fn set_warning<R: Runtime>(app: &AppHandle<R>, message: Option<&str>) {
    let Some(tray) = app.tray_by_id(TRAY_ID) else {
        return;
    };
    let menu = match build_menu(app, message) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("[lodestar] 重建托盘菜单失败: {e}");
            return;
        }
    };
    if let Err(e) = tray.set_menu(Some(menu)) {
        eprintln!("[lodestar] 更新托盘菜单失败: {e}");
    }
}

#[cfg(test)]
mod tests {
    //! 托盘菜单**结构**的测试。
    //!
    //! 【为什么盯住「警告项在顶部且是禁用的」】
    //!   快捷键注册失败时，用户唯一能看见的地方就是这条警告项
    //!   （见 `crate::shortcut` 与 `lib.rs` 的 setup）。
    //!   它要是插错了位置、或者没禁用，用户会以为那是个能点的按钮 ——
    //!   点下去什么也不发生，「快捷键坏了」这件事就永远传不到用户耳朵里。
    //!
    //! 【为什么测 `menu_entries` 而不是 `build_menu`】
    //!   `build_menu` 会真的去建 muda 菜单，而 muda 在 macOS 上
    //!   **只能在进程主线程**建（muda 0.20 `platform_impl/macos/mod.rs:121`：
    //!   「`muda::Menu` can only be created on the main thread」——
    //!   直接 panic）。Rust 的测试框架把每个 `#[test]` 放在**新建的线程**上跑
    //!   → 单测里建不出菜单（实测：连「菜单项数量」这种断言都建不出来）。
    //!   所以结构由纯函数 [`menu_entries`] 决定，测试也测它；
    //!   `build_menu` 只是把结构渲染成菜单项（薄到一眼能看对）。
    //!
    //! 【为什么不在这里测「点图标 → 窗口回来」】
    //!   那是真机验收的事（托盘事件要真的 macOS 运行环境）。

    use super::*;

    /// 菜单项的 id 列表（按顺序）。
    fn ids(entries: &[MenuEntry<'_>]) -> Vec<&'static str> {
        entries.iter().map(|e| e.id).collect()
    }

    #[test]
    fn a_warning_becomes_the_first_item_and_is_disabled() {
        let entries = menu_entries(Some("⚠️ 快捷键未生效"));
        assert_eq!(entries[0].id, TRAY_WARNING_ID, "警告项必须在**顶部**");
        assert!(
            !entries[0].enabled,
            "警告项是给人看的，不能点 —— 必须禁用"
        );
        assert_eq!(entries[0].text, "⚠️ 快捷键未生效", "警告文字要原样传下去");
    }

    #[test]
    fn without_a_warning_the_open_item_is_first() {
        let entries = menu_entries(None);
        assert_eq!(entries[0].id, TRAY_OPEN_ID, "没有警告时第一项应该是「打开 Lodestar」");
    }

    #[test]
    fn the_menu_has_the_expected_number_of_items() {
        let with_warning = menu_entries(Some("警告"));
        let without = menu_entries(None);
        assert_eq!(
            with_warning.len(),
            without.len() + 1,
            "有警告时应该恰好多一项"
        );
        assert_eq!(without.len(), 3, "没有警告时是「打开 / 设置 / 退出」三项");
    }

    #[test]
    fn the_menu_order_is_stable() {
        // 【回归测试】菜单项的**顺序**是用户肌肉记忆的一部分 ——
        //   「设置…」在「退出」上面，顺序一乱用户就点错。
        assert_eq!(
            ids(&menu_entries(None)),
            vec![TRAY_OPEN_ID, TRAY_SETTINGS_ID, TRAY_QUIT_ID]
        );
        assert_eq!(
            ids(&menu_entries(Some("警告"))),
            vec![TRAY_WARNING_ID, TRAY_OPEN_ID, TRAY_SETTINGS_ID, TRAY_QUIT_ID]
        );
    }

    #[test]
    fn only_the_warning_item_is_disabled() {
        for entry in menu_entries(Some("警告")) {
            let should_be_enabled = entry.id != TRAY_WARNING_ID;
            assert_eq!(
                entry.enabled, should_be_enabled,
                "只有警告项该是禁用的（出问题的项：{}）",
                entry.id
            );
        }
    }
}
