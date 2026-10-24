//! 护栏 —— 应用菜单里必须真的有「完整手册」这一项，**而且不能把 Edit 弄丢**。
//!
//! 【为什么要这两条】
//! 1. 「Help → 完整手册」是这整个功能的**唯一入口**。
//!    菜单项装不上（子菜单 id 变了 / 追加失败 / 忘了 `set_menu`）时，
//!    **编译期不报错、运行期也不报错** —— 只是手册面板永远打不开。
//!    下面那条测试把这个「静默失效」钉住。
//!
//! 2. 默认菜单里的 **Edit（复制/粘贴/全选）** 是搜索框的刚需。
//!    哪天有人嫌 `Menu::default()` 太啰嗦、改成手写整套菜单，
//!    Edit 一丢，搜索框里就没法复制粘贴了 —— 而这个回归**没有任何测试会红**。
//!    下面那条测试把「Edit 还在、里面还有 copy / paste」钉住。
//!
//! 【判据】
//!   - 从 Help 子菜单里把「完整手册」拿掉 → 第一条检查必须失败。
//!   - 把手写整套菜单换掉默认菜单（Edit 消失）→ 第二条检查必须失败。
//!
//! 【为什么这个文件没有 `#[test]`】
//! muda（macOS 上的菜单实现）要求**菜单项必须在进程的主线程上创建**
//! （`MainThreadMarker::new()` —— 不是主线程就 panic：
//!  `` `muda::PlatformMenuItem` can only be created on the main thread ``）。
//! 而 cargo 的测试 harness 把每个 `#[test]` 放在【工作线程】上跑 ——
//! 在那里建菜单会直接 panic。
//!
//! 所以这个测试目标用 `harness = false`（见 Cargo.toml 的 `[[test]]`），
//! 自己写 `main()` —— 那就是进程的主线程，菜单建得出来。
//! 断言失败 → panic → 退出码非 0 → cargo 把这条测试报成失败。

use lodestar_app_lib::{
    build_menu, OPEN_SYNTAX_CHEATSHEET_EVENT, OPEN_SYNTAX_CHEATSHEET_ID, OPEN_USER_GUIDE_EVENT,
    OPEN_USER_GUIDE_ID,
};
use tauri::menu::{Menu, MenuItemKind, HELP_SUBMENU_ID};
use tauri::test::{mock_app, MockRuntime};

/// 检查零：菜单项 id 与事件名必须是同一个字符串，且与前端监听的一致。
///
/// 【为什么这条重要】Rust 发事件、前端收事件，**两边各写各的就会「事件发了但没人听」** ——
/// 而这**不会报错**，只是手册面板永远打不开。
/// 前端监听的名字是字面量 "open-user-guide"（见 app/src/App.vue），
/// 这里把它钉住 —— 改了 Rust 侧而忘了改前端，这条会红。
fn event_name_matches_the_frontend() {
    assert_eq!(
        OPEN_USER_GUIDE_ID, "open-user-guide",
        "菜单项 id 变了 —— 前端监听的是 \"open-user-guide\"（app/src/App.vue）"
    );
    assert_eq!(
        OPEN_USER_GUIDE_EVENT, OPEN_USER_GUIDE_ID,
        "事件名与菜单项 id 不一致 —— 菜单点了会发一个前端没在听的事件"
    );
    assert_eq!(
        OPEN_SYNTAX_CHEATSHEET_ID, "open-syntax-cheatsheet",
        "菜单项 id 变了 —— 前端监听的是 \"open-syntax-cheatsheet\"（app/src/App.vue）"
    );
    assert_eq!(
        OPEN_SYNTAX_CHEATSHEET_EVENT, OPEN_SYNTAX_CHEATSHEET_ID,
        "事件名与菜单项 id 不一致 —— 菜单点了会发一个前端没在听的事件"
    );
}

/// 菜单里所有子菜单的标题。
fn submenu_titles(menu: &Menu<MockRuntime>) -> Vec<String> {
    menu.items()
        .expect("读菜单项")
        .iter()
        .filter_map(|i| match i {
            MenuItemKind::Submenu(s) => s.text().ok(),
            _ => None,
        })
        .collect()
}

/// 检查一：Help 子菜单里有 id == "open-user-guide"、文案是「完整手册」的那一项。
fn help_submenu_contains_the_user_guide_item() {
    let app = mock_app();
    let handle = app.handle().clone();
    let menu = build_menu(&handle).expect("装菜单");

    // Help 子菜单必须还在
    let help = menu
        .get(HELP_SUBMENU_ID)
        .expect("默认菜单里应该有 Help 子菜单");
    let help = match help {
        MenuItemKind::Submenu(s) => s,
        _ => panic!("Help 子菜单的 kind 不是 Submenu"),
    };

    let items = help.items().expect("读 Help 子菜单");

    // 里面必须有 id == "open-user-guide" 的那一项
    let ids: Vec<String> = items.iter().map(|i| i.id().as_ref().to_string()).collect();
    assert!(
        ids.contains(&OPEN_USER_GUIDE_ID.to_string()),
        "Help 子菜单里没有 id = {OPEN_USER_GUIDE_ID:?} 的那一项。\n\
         实际有：{ids:?}\n\
         （菜单项装不上时【编译期不报错、运行期也不报错】—— \
         只是手册面板永远打不开。）"
    );

    // 文案要是「完整手册」（用户原话）
    let texts: Vec<String> = items
        .iter()
        .filter(|i| i.id().as_ref() == OPEN_USER_GUIDE_ID)
        .filter_map(|i| match i {
            MenuItemKind::MenuItem(m) => m.text().ok(),
            _ => None,
        })
        .collect();
    assert_eq!(
        texts,
        vec!["完整手册".to_string()],
        "「完整手册」那一项的文案不对"
    );
}

/// 检查二：Help 子菜单里还有「搜索语法速查」这一项。
///
/// 【为什么它是独立的一条】速查和手册是**两页**（同一个面板的不同页签）——
///  菜单上分两项，用户不用先想「我要看的是速查还是手册」。
///  少了这一项，速查就只能靠 ⌘/ 找 —— 而新用户根本不知道有 ⌘/。
fn help_submenu_contains_the_syntax_cheatsheet_item() {
    let app = mock_app();
    let handle = app.handle().clone();
    let menu = build_menu(&handle).expect("装菜单");

    let help = menu
        .get(HELP_SUBMENU_ID)
        .expect("默认菜单里应该有 Help 子菜单");
    let help = match help {
        MenuItemKind::Submenu(s) => s,
        _ => panic!("Help 子菜单的 kind 不是 Submenu"),
    };
    let items = help.items().expect("读 Help 子菜单");

    let texts: Vec<String> = items
        .iter()
        .filter(|i| i.id().as_ref() == OPEN_SYNTAX_CHEATSHEET_ID)
        .filter_map(|i| match i {
            MenuItemKind::MenuItem(m) => m.text().ok(),
            _ => None,
        })
        .collect();
    assert_eq!(
        texts,
        vec!["搜索语法速查".to_string()],
        "「搜索语法速查」那一项的文案不对（或没装上）"
    );
}

/// 检查三：默认菜单的 Edit 子菜单还在，里面还有 copy / paste / select_all。
///
/// 【为什么按【文案】找，而不是按 id】
///   - 预定义菜单项（undo / redo / cut / copy / paste / select_all）的 id
///     是 muda 分配的**数字**（"15" / "16" / …），不是 "copy" / "paste"；
///   - 它们的文案来自**系统**，中文系统上显示「拷贝 / 粘贴 / 全选」。
/// 所以这里对每项都接受一组常见写法。
fn default_menu_keeps_edit_with_copy_and_paste() {
    let app = mock_app();
    let handle = app.handle().clone();
    let menu = build_menu(&handle).expect("装菜单");

    let titles = submenu_titles(&menu);
    assert!(
        titles.iter().any(|t| t == "Edit"),
        "菜单里没有 Edit 子菜单：{titles:?}\n\
         （默认菜单的 Edit 带 copy / paste —— 搜索框要靠它复制粘贴。\
         手写整套菜单很容易把它弄丢。）"
    );

    let edit = menu
        .items()
        .expect("读菜单项")
        .into_iter()
        .find_map(|i| match i {
            MenuItemKind::Submenu(s) if s.text().unwrap_or_default() == "Edit" => Some(s),
            _ => None,
        })
        .expect("找 Edit 子菜单");

    let edit_texts: Vec<String> = edit
        .items()
        .expect("读 Edit 子菜单")
        .iter()
        .filter_map(|i| match i {
            MenuItemKind::Predefined(p) => p.text().ok(),
            MenuItemKind::MenuItem(m) => m.text().ok(),
            _ => None,
        })
        .filter(|t| !t.trim().is_empty())
        .collect();

    for (label, variants) in [
        ("复制（Copy）", &["Copy", "拷贝", "复制"][..]),
        ("粘贴（Paste）", &["Paste", "粘贴"][..]),
        ("全选（Select All）", &["Select All", "全选"][..]),
    ] {
        assert!(
            edit_texts.iter().any(|t| variants.contains(&t.as_str())),
            "Edit 子菜单里没有{label} —— 现有的文案有：{edit_texts:?}\n\
             （Edit 丢了复制/粘贴，搜索框里就没法复制粘贴了。）"
        );
    }
}

/// 检查四：其它顶层菜单（File / View / Window / Help）都还在。
fn default_menu_keeps_the_other_top_level_menus() {
    let app = mock_app();
    let handle = app.handle().clone();
    let menu = build_menu(&handle).expect("装菜单");

    let titles = submenu_titles(&menu);
    for want in ["File", "Edit", "View", "Window", "Help"] {
        assert!(
            titles.iter().any(|t| t == want),
            "菜单里没有 {want:?} 子菜单 —— 现有的有：{titles:?}"
        );
    }
}

fn main() {
    // 逐条跑 —— 一条挂了就把名字打出来，后面的照样跑（能看到全部问题）。
    let checks: [(&str, fn()); 5] = [
        (
            "事件名与菜单项 id 一致（Rust ↔ 前端的契约）",
            event_name_matches_the_frontend,
        ),
        (
            "Help 子菜单里有「完整手册」",
            help_submenu_contains_the_user_guide_item,
        ),
        (
            "Help 子菜单里有「搜索语法速查」",
            help_submenu_contains_the_syntax_cheatsheet_item,
        ),
        (
            "默认菜单的 Edit 子菜单还在（含 copy/paste/select_all）",
            default_menu_keeps_edit_with_copy_and_paste,
        ),
        (
            "默认菜单的其它顶层菜单都在（File/Edit/View/Window/Help）",
            default_menu_keeps_the_other_top_level_menus,
        ),
    ];

    let mut failed = 0usize;
    for (name, f) in checks {
        // 【为什么 catch_unwind】一个检查挂了不影响后面的 ——
        // 一次跑完能看到「到底坏了哪几条」，而不是修一条再跑一次。
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
        match r {
            Ok(()) => println!("ok   - {name}"),
            Err(e) => {
                failed += 1;
                let msg = e
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string()))
                    .unwrap_or_else(|| "(panic 详情丢失)".to_string());
                println!("FAIL - {name}\n{msg}");
            }
        }
    }

    if failed > 0 {
        eprintln!("\n{failed} 条菜单护栏检查失败");
        std::process::exit(1);
    }
    println!("\n{} 条菜单护栏检查全部通过", checks.len());
}
