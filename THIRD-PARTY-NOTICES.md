# 第三方组件与许可

Lodestar 自身的代码采用 [MIT 许可](../LICENSE)（见仓库根目录的 `LICENSE`）。

**引擎完全自研** —— 仓库里没有任何第三方搜索引擎的代码。
下面是构建产物里用到的第三方组件。

## Rust 侧

| 组件 | 许可 | 用途 |
|---|---|---|
| [tauri](https://github.com/tauri-apps/tauri) | MIT / Apache-2.0 | 应用外壳（窗口、托盘、菜单、IPC） |
| tauri-plugin-opener | MIT | 「在访达中显示」「用别的应用打开」 |
| tauri-plugin-store | MIT | 设置持久化（`settings.json`） |
| tauri-plugin-dialog | MIT | 系统对话框 |
| tauri-plugin-global-shortcut | MIT | 全局快捷键唤起 |
| tauri-plugin-autostart | MIT | 开机自启（LaunchAgent） |
| [notify](https://github.com/notify-rs/notify) | CC0-1.0 / MIT | 文件系统事件（macOS 底层是 FSEvents） |
| [regex](https://github.com/rust-lang/regex) | MIT / Apache-2.0 | 查询里的正则（线性时间，无灾难性回溯） |
| [unicode-normalization](https://github.com/unicode-rs/unicode-normalization) | (MIT OR Apache-2.0) AND Unicode-3.0 | Unicode NFD 归一化（大小写不敏感匹配的基础） |
| [serde](https://github.com/serde-rs/serde) | MIT OR Apache-2.0 | 设置与 IPC 的序列化 |
| [libc](https://github.com/rust-lang/libc) | MIT OR Apache-2.0 | 本地时区偏移（`tm_gmtoff`） |
| [rayon](https://github.com/rayon-rs/rayon) 等传递依赖 | 见各自仓库 | 由上面的依赖间接引入 |

> **为什么引擎不需要 ICU**
> 上一版（基于上游 C 引擎）依赖 ICU 做 Unicode 大小写折叠，随包带去了 34MB 的
> `libicudata`。现在用纯 Rust 的 `unicode-normalization` + 标准库 `to_lowercase`
> 承担同样的职责 —— **`.app` 里不再有任何 Unicode 数据文件**。

## 前端

| 组件 | 许可 | 用途 |
|---|---|---|
| [vue](https://github.com/vuejs/core) | MIT | 前端框架 |
| [vite](https://github.com/vitejs/vite) | MIT | 构建工具 |
| [typescript](https://github.com/microsoft/TypeScript) | Apache-2.0 | 类型系统 |
| [vitest](https://github.com/vitest-dev/vitest) | MIT | 单元测试 |
| [@tauri-apps/api](https://github.com/tauri-apps/tauri) | MIT / Apache-2.0 | 前端侧的 Tauri API |
| [marked](https://github.com/markedjs/marked) | MIT | 把用户手册（Markdown）渲染成 HTML |

## 系统框架

应用链接的系统框架（AppKit、WebKit、Foundation、CoreFoundation 等）
由 macOS 本身提供，**不随应用分发**。
