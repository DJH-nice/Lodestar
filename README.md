<div align="center">

# Sextant for macOS

**一个 macOS 原生的文件搜索工具**

菜单栏常驻 · 全局快捷键唤起 · 输入即搜 · 自研搜索引擎内核

[安装](#安装) · [使用](#使用) · [查询语法](#查询语法) · [开发](#开发)

</div>

---

## 是什么

Sextant 是一款 macOS / Apple Silicon 原生的文件搜索工具。

- **随时能叫醒** —— 菜单栏常驻图标，`⌥⌘Space`（可改）一键唤起
- **输入即搜** —— 打字的同时就在搜，命中词直接高亮
- **搜得到一切** —— 文件名、路径、扩展名、大小、修改时间
- **跟得上磁盘** —— 文件一变，索引跟着更新

界面是 Vue 3 写的，引擎是 Rust 写的 —— 整个应用**没有一个第三方搜索引擎的代码**。

## 安装

从 [Releases](https://github.com/YOUR_ACCOUNT/sextant/releases) 下载 `Sextant.dmg`，
双击挂载后把 `Sextant.app` 拖进「应用程序」。

> 首次打开如果 macOS 说「无法验证开发者」——
> 对着 `Sextant.app` **右键 → 打开** → 再点「打开」。
> （这个包没有付费的 Apple 开发者证书，只能 ad-hoc 签名。）

装好后建议先做两件事：

1. **打开菜单栏图标 → 设置 → 选要索引的目录**（默认是整个家目录 `$HOME`）
2. **设置 → 索引 → 点「应用并重建索引」** —— 第一次扫描会花几秒

## 使用

| 操作 | 怎么做 |
|---|---|
| 唤起窗口 | `⌥⌘Space`（可在设置里改） |
| 搜索 | 直接打字 |
| 预览文件信息 | 右键结果行 → 预览 |
| 在访达中定位 | 右键结果行 → 在访达中显示 |
| 用别的应用打开 | 右键结果行 → 打开方式 |
| 收藏搜索条件 | 右键结果行 → 收藏 |
| 语法速查表 | `⌘/` |
| 完整手册 | 菜单栏 → 帮助 → 完整手册 |

搜索框支持 `AND` / `OR` / `NOT` 与括号，可以组合多个条件。

## 查询语法

速查表（`⌘/`）里有全部写法，这里是最常用的几条：

| 想做什么 | 这么写 |
|---|---|
| 按名字找 | `报告` |
| 按后缀找 | `ext:pdf`（多个用分号：`ext:pdf;doc;xlsx`） |
| 找大文件 | `size:>10M`（十进制，1M = 1 000 000） |
| 在某个目录里找 | `path:Documents` |
| 今天改过的文件 | `dm:today` |
| 最近三天改过的 | `dm:past3days` |
| 精确匹配文件名 | `exact:alpha.txt` |
| 区分大小写 | `case:ALPHA` |
| 正则 | `regex:"^gam"` |
| 精确短语 | `"my file.txt"` |
| 排除某种文件 | `NOT ext:png` |
| 组合 | `(alpha OR beta) AND txt` |

**大小写**：默认不区分（搜「报告」能搜到「Report.pdf」）。
但查询里**带大写字母就自动区分**（搜「Report」不会命中「report.pdf」）——
这条叫 smart case。

**通配符**：`*` 匹配任意多个字符，`?` 匹配一个。`*` 单独用就是「匹配全部」。

## 索引与性能

索引的是你选中的目录（默认 `$HOME`）。有几件事值得知道：

- **排除规则默认生效**：`~/Library`、`/Applications`、`target/`、`node_modules/`
  与隐藏目录**整棵不进索引**。这很重要 —— `~/Library` 一个目录就有 26 万个条目。
- **索引会落盘**：第二次启动直接读档，**不用重新扫盘**。
- **实时更新**：文件变了会自动更新索引（可以关掉，尤其索引整个家目录时）。

## 隐私

Sextant **不联网**。没有遥测、没有崩溃上报、没有更新检查。
索引只存在你自己的机器上（`~/Library/Application Support/com.sextant.macos/`）。

## 开发

```bash
# 前端
cd app && pnpm install && pnpm dev

# 引擎单测（不依赖 Tauri，可单独跑）
cd app/src-tauri && cargo test -p search-core

# 全部 Rust 测试（含 5 个护栏集成测试）
cd app/src-tauri && cargo test

# 前端单测
cd app && pnpm vitest run

# 打包（唯一入口，带自检）
bash app/scripts/bundle-app.sh
```

### 架构

```
app/src/                Vue 3 前端（虚拟滚动、语法高亮、预览卡…）
app/src-tauri/src/      Tauri 后端（窗口、托盘、设置、FSEvents 监控）
app/src-tauri/search-core/
                        ★ 自研搜索引擎内核
                        scan   并行遍历 + 排除剪枝
                        store  索引落盘（冷启动加速）
                        query  查询语法（词法 / 语法 / 匹配）
                        fold   Unicode 折叠（索引期做一次）
                        engine 排序 / 分页 / 高亮
```

`search-core` **不依赖 Tauri** —— 这是刻意的：引擎必须能独立测试。

## 许可

Sextant 采用 [MIT 许可](LICENSE)。第三方组件见
[THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md)。

应用内也可以看：菜单栏 → 帮助 → 致谢与许可。
