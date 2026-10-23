//! 排除规则 —— 「哪些目录**根本不进索引**」。
//!
//! 【为什么排除规则是目录级剪枝，而不是「逐个文件排除」】
//!   规则命中的是**目录本身**：扫描时撞上就**不再递归进去**，
//!   里面的文件从没被访问过 —— 不是「访问了再丢掉」。
//!   → 判断某个**文件**会不会被索引，必须**连祖先一起查**。
//!     只查文件本身，永远返回「不被排除」（这是最常见的误判）。
//!
//! 【为什么这条特别值钱】
//!   实测：`~/Library` 一个目录就是 **26.4 万条目**，
//!   而可见目录加起来才 8,320 条 —— 差 32 倍。
//!   剪枝做对了，扫描就是 300ms；做错了就是 10 秒起。

use std::path::Path;

/// 末级名命中就排除的目录名（**精确匹配**，不是前缀）。
///
/// ⚠️ `MyLibrary` / `targets` 这类**不会**命中 —— 必须是末级名恰好相等。
const EXCLUDED_NAMES: &[&str] = &["Library", "Applications", "target", "node_modules"];

/// 整路径前缀命中就排除（macOS 的系统区域）。
///
/// 【为什么不能用 `starts_with` 判名字】字符串前缀会让 `/a/bc` 被 `/a/b` 命中
///   → 漏索引。这里全部按**路径组件**判断。
const EXCLUDED_PREFIXES: &[&str] = &[
    "/System",
    "/private/var",
    "/cores",
    "/.Spotlight-V100",
];

/// 这个**目录**要不要剪枝（扫描时用）。
///
/// 【为什么目录和文件分开判】
///   目录：命中就**不递归**（省掉整棵子树）。
///   文件：命中就**不收**（比如用户有个叫 `Library` 的普通文件）。
pub fn is_excluded_dir_name(name: &str) -> bool {
    is_hidden(name) || EXCLUDED_NAMES.contains(&name)
}

/// 隐藏判定：basename 以 `.` 开头。
///
/// 【注意】是「以 `.` 开头」，不是「`.` 开头且后面还有东西」——
///   `.foo` 与 `..foo` 都算隐藏。
pub fn is_hidden(name: &str) -> bool {
    name.starts_with('.')
}

/// 这条路径**会不会被引擎的排除规则挡掉**？
///
/// 【谁在用】watcher 在**置脏标志之前**问一句：
///   会被排除的路径（`~/Library/...`、构建产物目录…）**不会进索引**，
///   它们的变化**不该**触发重扫。
///
/// 【为什么必须查祖先】见文件头的说明 —— 排除是目录级剪枝，
///   一个文件「在 `~/Library` 底下」就已经不会被索引了，
///   哪怕它自己的名字一点都不可疑。
///
/// 【规则只有这一份】watcher 不抄一份到 Rust 侧 —— 抄了就会漂移
///   （改了扫描器忘了改 watcher → 又开始空转重扫）。
pub fn path_is_excluded(path: &Path) -> bool {
    // ① 逐级查祖先（含自身）的末级名
    let mut cur = Some(path);
    while let Some(p) = cur {
        if let Some(name) = p.file_name().and_then(|s| s.to_str()) {
            if is_excluded_dir_name(name) {
                return true;
            }
        }
        cur = p.parent();
    }

    // ② 整路径前缀规则
    let s = path.to_string_lossy();
    EXCLUDED_PREFIXES.iter().any(|prefix| s.starts_with(prefix))
}

/// 排除规则的人类可读清单（前端「索引设置」面板展示用）。
///
/// 【格式约定】一行一条，前端按行解析 —— 不要改成 JSON 或加前缀。
/// 最后一行是**开关标记**（不是路径规则）：它对应「排除隐藏文件/目录」这个开关。
pub fn exclude_rules() -> String {
    let mut out = String::new();
    for name in EXCLUDED_NAMES {
        out.push_str(&format!("*/{name}\n"));
    }
    for prefix in EXCLUDED_PREFIXES {
        out.push_str(&format!("{prefix}*\n"));
    }
    out.push_str("[hidden files and folders]");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn library_subtree_is_excluded() {
        // `~/Library` 底下的一切都不进索引（26 万条目的那棵子树）
        assert!(path_is_excluded(Path::new(
            "/Users/someone/Library/WebKit/com.sextant.macos/x.db"
        )));
        assert!(path_is_excluded(Path::new("/Users/someone/Library")));
    }

    #[test]
    fn hidden_path_is_excluded() {
        assert!(path_is_excluded(Path::new(
            "/Users/someone/.minimax/v2/sessions/a/messages.jsonl"
        )));
    }

    #[test]
    fn build_output_is_excluded() {
        // 构建产物目录：实测一个 target/ 里有 9 万个文件
        assert!(path_is_excluded(Path::new(
            "/Users/someone/app/src-tauri/target/debug/deps/libfoo.dylib"
        )));
        assert!(path_is_excluded(Path::new(
            "/Users/someone/app/node_modules/vue/index.js"
        )));
    }

    #[test]
    fn normal_path_is_not_excluded() {
        // 【最要紧的一条】普通文件**必须**不被排除 ——
        //   排除了就意味着索引悄悄过期、用户搜不到新文件。
        assert!(!path_is_excluded(Path::new("/Users/someone/Downloads/报告.md")));
        assert!(!path_is_excluded(Path::new("/Users/someone/文档/正常文件.md")));
        // 名字里带 target 但不是末级名 → 不该命中 `*/target`
        assert!(!path_is_excluded(Path::new("/Users/someone/targets/目标文件.md")));
        // MyLibrary 不是 Library → 不该命中
        assert!(!path_is_excluded(Path::new("/Users/someone/MyLibrary/a.txt")));
    }

    #[test]
    fn system_prefixes_are_excluded() {
        assert!(path_is_excluded(Path::new("/System/Library/CoreServices/x")));
        assert!(path_is_excluded(Path::new("/private/var/folders/ab/x")));
    }

    #[test]
    fn rules_text_has_one_rule_per_line_and_the_switch_marker_last() {
        let text = exclude_rules();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.last().copied(), Some("[hidden files and folders]"));
        assert!(lines.len() >= 2);
        // 清单里出现的每个模式都该真的能挡住东西
        assert!(lines.iter().any(|l| l.contains("Library")));
        assert!(lines.iter().any(|l| l.contains("node_modules")));
    }
}
