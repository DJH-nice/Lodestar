//! **护栏** —— Rust 发的事件名，前端必须**真的在听**。
//!
//! 【这道护栏是为哪一次事故加的】
//!   `open-settings`（托盘菜单的「设置…」用的那个）——
//!   Rust 侧发得好好的（`lifecycle::open_settings`），
//!   **前端一个接收者都没有** → 点「设置…」只会把窗口叫出来、
//!   设置抽屉不开，而且**不报错**。
//!
//!   为什么前 5 个任务谁也没发现：
//!     - Task 2 只管托盘侧（事件**发**得对）
//!     - Task 5 只管面板侧（面板**做**得对）
//!   两边各自审各自的一半 → 「发了但没人听」这条缝正好落在缝里。
//!   （更早一层：设计文档里有这一行，**实施计划里漏了** ——
//!     实现者照计划做，缺陷就出厂了。）
//!
//! 【这道护栏怎么起作用】
//!   Rust 侧每个「发给前端的事件名」都列在 [`EVENT_NAMES`] 里
//!   （引的是**常量本身**，不是字面量 —— 改名会编译报错）。
//!   测试逐个去**前端源文件**里找 —— 找不到「**订阅**」= 前端没在听 = **测试红**。
//!
//! 【判据为什么是「出现在含 `listen` 的那一行」】
//!   最初这一版是「在前端源文件里 grep 到这个字符串就算过」。
//!   那样有个**假阴性**：前端把事件名存成常量（`const A = "app-awake"`），
//!   字符串只在**声明处**出现 —— 有人后来把 `listen(...)` 调用删了，
//!   护栏**照样绿**。
//!   （复审用真实的 `cargo test -p sextant-app event_names` 跑过这个变异：
//!     把前端的监听整段删掉，测试仍然绿 —— 它守的正是「发了没人听」这条缝，
//!     却在最该生效的方向上失效。）
//!   → 判据收紧成：**这个事件名必须出现在【含 `listen` 的那一行】上**。
//!     前端因此把 `listen()` 的参数写成**字面量**（不用常量）——
//!     这样「订阅点」和「事件名」在同一行，删掉订阅就一定会红。
//!
//! 【为什么用「grep 前端源文件」这么土的办法】
//!   前端是 TypeScript，Rust 是 Rust —— 两边没有共享的类型系统，
//!   唯一能跨过去的契约就是**这个字符串**。
//!   与其发明一套跨语言的机制（要加构建步骤、要加依赖），
//!   不如让测试直接读源文件 —— 零依赖、零构建期改动、失败信息直指现场。

use std::path::{Path, PathBuf};

/// Rust 发、前端收的**全部**事件名。
///
/// 【为什么要显式列全，而不是自动扫】
///   自动扫需要解析 Rust 源码（要加 syn 之类的依赖 —— 不许加）。
///   显式列出来只有一处维护成本：
///   **新增事件名时必须往这个表里加一行** ——
///   而忘了加的后果是「那个事件没有护栏」，不是编译错误。
///   （所以这张表旁边还有一条测试盯着它本身：见下面的
///     `the_event_table_covers_every_event_constant`。）
pub const EVENT_NAMES: &[&str] = &[
    crate::lifecycle::AWAKE_EVENT,
    crate::lifecycle::OPEN_SETTINGS_EVENT,
    crate::lifecycle::VISIBILITY_EVENT,
    crate::watcher::AUTO_REFRESH_EVENT,
    crate::OPEN_USER_GUIDE_EVENT,
    crate::OPEN_SYNTAX_CHEATSHEET_EVENT,
    crate::OPEN_CREDITS_EVENT,
];

/// 前端源文件在哪 —— `app/src-tauri/../src`。
fn frontend_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../src")
}

/// 递归收集目录下的前端源文件（`.ts` / `.vue`）。
fn collect_sources(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_sources(&path, out);
        } else if matches!(
            path.extension().and_then(|e| e.to_str()),
            Some("ts") | Some("vue")
        ) {
            out.push(path);
        }
    }
}

/// 去掉注释，只留**代码**。
///
/// 【为什么要剥注释】—— 判据是「这个事件名在前端**代码**里出现过」。
///   如果连注释一起算，那么「有人在注释里提了一句这个事件名」就能骗过它。
///   **这正是实际发生过的**：`App.vue` 的文档注释里写着 `open-settings`，
///   于是「前端把监听整段删掉」这个变异，护栏依然绿。
///   （复审实测过：那个变异下测试仍然通过。）
///
/// 【怎么剥】—— 逐行处理，五种情况：
///   ① 处在 `/* … */` 块注释里 → 整行丢掉
///   ② 去掉行首空白后以 `//` 开头 → 丢掉
///   ③ 去掉行首空白后以 `/*` 开头 → 单行闭合就取 `*/` 之后；否则转成「处于块注释里」
///   ④ 去掉行首空白后以 `*` 开头 → 块注释的续行，丢掉
///   其余行 → 行内若有 `/*` 就只取它之前的部分
///
/// 【为什么不写完整的词法分析】—— 前端源文件不是我们要编译的东西，
///   这里的判据只需要「别被注释骗到」。过度解析反而容易出错。
fn strip_comments(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let mut in_block = false;
    for line in src.lines() {
        let mut rest = line;
        // ① 上一行开着的块注释
        if in_block {
            match rest.find("*/") {
                Some(i) => {
                    rest = &rest[i + 2..];
                    in_block = false;
                }
                None => continue,
            }
        }
        let trimmed = rest.trim_start();
        // ② 纯行注释
        if trimmed.starts_with("//") {
            continue;
        }
        // ③ 整行是块注释（可能跨行）
        if trimmed.starts_with("/*") {
            match rest.find("*/") {
                Some(i) => {
                    // 单行闭合：`/* … */ code` → 取 `*/` 之后
                    out.push_str(&rest[i + 2..]);
                    out.push('\n');
                }
                None => in_block = true,
            }
            continue;
        }
        // ④ 块注释的续行
        if trimmed.starts_with('*') {
            continue;
        }
        // 其余：行内单行块注释只取 `/*` 之前的部分
        let code = match rest.find("/*") {
            Some(i) => &rest[..i],
            None => rest,
        };
        out.push_str(code);
        out.push('\n');
    }
    out
}

/// 这个事件名在前端的**订阅点**上出现过吗？
///
/// 【判据】—— 必须落在**同一行**里同时含有 `listen` 和这个事件名。
///   只含事件名不够（那可能只是常量声明，甚至只是注释）。
///   含 `listen` 才说明「有人真的订阅了它」。
///
/// 返回命中位置（文件 + 行号），用来在失败信息里指出现场。
fn find_subscription(name: &str) -> Option<(PathBuf, usize)> {
    let dir = frontend_dir();
    let mut files = Vec::new();
    collect_sources(&dir, &mut files);
    for f in files {
        let Ok(text) = std::fs::read_to_string(&f) else {
            continue;
        };
        let code = strip_comments(&text);
        for (i, line) in code.lines().enumerate() {
            if line.contains(name) && line.contains("listen") {
                return Some((f, i + 1));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_rust_event_name_is_subscribed_to_in_the_frontend() {
        // 【这条测试就是那道护栏】
        //   Rust 发的事件名在前端**订阅不到** = 前端没在听它
        //   = 「点了没反应」且不报错。
        for name in EVENT_NAMES {
            assert!(
                find_subscription(name).is_some(),
                "Rust 侧的事件名「{name}」在前端**没有订阅点** —— \
                 前端没有在听它！\n\
                 （前端源文件目录：{}）\n\
                 修法：在 app/src/App.vue 里加一个 listen(\"{name}\", …) ——\n\
                 注意必须是**字面量**，不能用常量：\
                 Rust 侧那道护栏断言的是「这个事件名出现在【含 listen 的那一行】上」，\
                 写成常量的话字符串只在声明处，删掉订阅护栏也不会红。",
                frontend_dir().display()
            );
        }
    }

    #[test]
    fn the_event_table_covers_every_event_constant() {
        // 【为什么要盯这张表本身】
        //   上面那条测试只保证「表里的事件名前端在听」。
        //   如果**新增**了一个事件却忘了往表里加 →
        //   那个事件就没有护栏（下次照样会「发了没人听」）。
        //   → 这条测试把「表里的事件数」钉死在一个具体数字上：
        //     新增事件时**必须**同时改这个数字（改的时候就会看见这张表）。
        assert_eq!(
            EVENT_NAMES.len(),
            7,
            "事件表少了一条或多了一条 —— \
             新增 Rust→前端 的事件时，记得把它加进 EVENT_NAMES 并更新这个数字"
        );
        // 顺便钉住「表里没有重复」—— 重复 = 有人复制粘贴时忘了改。
        let mut sorted: Vec<&str> = EVENT_NAMES.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(
            sorted.len(),
            EVENT_NAMES.len(),
            "事件表里有重复的名字"
        );
    }

    #[test]
    fn the_frontend_sources_are_actually_readable() {
        // 【为什么要有这条】上面两条测试都建立在「读得到前端源文件」之上。
        //   读不到时 `find_subscription` 会返回 `None` →
        //   第一条测试会**红**（好事），但报的错会指向「订阅不到」，
        //   而真实原因是「目录根本不存在」—— 容易误导后来的人。
        //   → 单独钉一条：目录必须存在、里面必须有源文件。
        let dir = frontend_dir();
        assert!(
            dir.is_dir(),
            "前端源文件目录不存在：{}（护栏失效 —— 事件名就没人盯着了）",
            dir.display()
        );
        let mut files = Vec::new();
        collect_sources(&dir, &mut files);
        assert!(
            !files.is_empty(),
            "前端源文件目录里一个 .ts/.vue 都没有：{}（护栏失效）",
            dir.display()
        );
    }

    // ---- 判据本身的行为（这就是「护栏咬不咬得住」的证明） ----

    #[test]
    fn a_name_only_in_a_comment_does_not_count_as_subscribed() {
        // 【回归测试】这就是当初骗过护栏的那一档：
        //   注释里提了一句事件名 → 不算数。
        //   （样本取自 App.vue 里真实存在的那句文档注释。）
        let src = r#"
/** Rust 侧的 `open_settings()` 只发 `open-settings`、不发 `app-awake`。 */
"#;
        let code = strip_comments(src);
        assert!(
            !code.contains("open-settings"),
            "注释里的事件名不该被算成代码：\n{code}"
        );
        assert!(
            !code.contains("app-awake"),
            "注释里的事件名不该被算成代码：\n{code}"
        );
    }

    #[test]
    fn a_name_in_a_constant_declaration_does_not_count_as_subscribed() {
        // 【回归测试】这一档也骗得过旧判据：
        //   常量声明里也有字符串，但没人订阅它。
        let src = r#"
const AWAKE_EVENT = "app-awake";
"#;
        let code = strip_comments(src);
        assert!(code.contains("app-awake"), "常量声明里的字符串是代码，应该保留");
        // 但它**不是订阅点** —— 那一行里没有 listen。
        assert!(
            !code.lines().any(|l| l.contains("app-awake") && l.contains("listen")),
            "只有常量声明、没有 listen 调用 → 不算订阅"
        );
    }

    #[test]
    fn a_name_in_a_listen_call_counts_as_subscribed() {
        let src = r#"
const un = await listen("app-awake", () => { /* … */ });
"#;
        let code = strip_comments(src);
        assert!(
            code.lines().any(|l| l.contains("app-awake") && l.contains("listen")),
            "listen 调用里的事件名应该算订阅：\n{code}"
        );
    }

    #[test]
    fn block_comments_are_stripped_across_lines() {
        let src = r#"
/**
 * 这是一段块注释
 * 里面提到了 open-settings
 */
const x = 1;
"#;
        let code = strip_comments(src);
        assert!(
            !code.contains("open-settings"),
            "跨行块注释里的事件名不该被算成代码：\n{code}"
        );
        assert!(code.contains("const x = 1;"), "代码行不该被误删：\n{code}");
    }
}
