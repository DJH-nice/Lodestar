//! 运行日志 —— **业务代码自己的日志出口**。
//!
//! 【为什么单独一个模块】
//!   以前这些日志挂在「诊断探针」模块（`dev_probe`）上 —— 但探针是**开发脚手架**，
//!   而业务代码（`commands` / `watcher`）**真的要靠它说话**：
//!   「自动重扫 #N 完成」「这次变化已被覆盖」这些行是**运行时的事实**，
//!   出问题时第一眼要看的就是它们。
//!   → 探针可以删，日志出口必须留 —— 所以把它拆成独立模块。
//!
//! 【它做什么】
//!   一行到 stdout（`[sextant] …`）。debug 构建下**额外**落一份到
//!   `/tmp/sextant-app.log` —— 因为 `pnpm tauri dev` 的 stdout 走管道，
//!   管道被读满/中断时 `println!` 会阻塞，落盘不受影响。

/// 日志前缀 —— 便于在混着别的输出的终端里一眼认出来。
const PREFIX: &str = "[sextant]";

/// 落盘路径（**只在 debug 构建里用**）。
///
/// 【为什么落盘而不只用 stdout】
///   `pnpm tauri dev` 的 stdout 走管道，管道被读满/中断时 `println!` 会阻塞 ——
///   那样界面看起来像「卡死」，其实是日志通道的问题。落盘不受影响。
#[cfg(debug_assertions)]
const LOG_PATH: &str = "/tmp/sextant-app.log";

/// 记一行运行日志。
pub fn log_line(line: &str) {
    println!("{PREFIX} {line}");
    // 落盘只在 debug 构建里做 —— release 下每次搜索都写文件太脏。
    #[cfg(debug_assertions)]
    {
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(LOG_PATH)
        {
            use std::io::Write;
            let _ = writeln!(f, "{line}");
        }
    }
}
