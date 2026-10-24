export type SortField = "name" | "path" | "size" | "mtime";
export type SortOrder = "asc" | "desc";
export type ListKind = "all" | "files" | "folders";

export interface Highlight {
  start: number;
  length: number;
}

export interface Entry {
  name: string;
  path: string;
  dir: string;
  extension: string;
  size: number;
  mtime: number;
  is_folder: boolean;
  highlights: Highlight[];
  path_highlights: Highlight[];
}

export interface SearchResult {
  entries: Entry[];
  total_entries: number;
  total_files: number;
  total_folders: number;
  complete: boolean;
  elapsed_ms: number;
  warning: string | null;
  group_boundary: number | null;
}

export interface SyntaxTables {
  modifiers: string[];
  fields: string[];
  operators: string[];
}

export interface IndexStatus {
  scanning: boolean;
  indexed: number;
  paths: string[];
  error: string | null;
  /** **窗口现在可见吗**（Rust 侧问的是窗口服务器的真相）。
   *
   *  前端用它决定「要不要继续跑那个 800ms 的状态轮询」——
   *  窗口隐藏时用户看不见状态栏，那个轮询是纯浪费。
   *
   *  【为什么是可选的】**缺省一律当成「可见」**（`s.visible !== false` 才算可见）。
   *  - 猜「不可见」→ 状态栏会停止更新（用户看得见的故障）
   *  - 猜「可见」→ 只是多跑一点轮询（性能问题）
   *  → 宁可性能出问题，也不要功能出问题。这跟 Rust 侧
   *    `IndexState::snapshot()` 里那个占位 `true` 是同一个取舍。
   */
  visible?: boolean;
}

/** 书签的两种类型：
 *  - `query` = 存的是**搜索条件**，点一下重新跑这个搜索（依赖索引）
 *  - `path`  = 存的是**一个文件/目录的路径**，点一下在访达里定位（不依赖索引）
 */
export type BookmarkKind = "query" | "path";

export interface Bookmark {
  kind: BookmarkKind;
  value: string;
  label?: string;
}

/** 与 Rust 侧 `settings::Settings` 一一对应（serde camelCase）。 */
export interface Settings {
  indexPaths: string[];
  sortPref?: string;
  kindPref?: string;
  bookmarks: Bookmark[];
  recentQueries: string[];
  /** 窗口的物理像素尺寸与位置。缺省 = 用 tauri.conf.json 里的默认。 */
  windowWidth?: number;
  windowHeight?: number;
  windowX?: number;
  windowY?: number;
  /** 「文件变化时自动刷新索引」—— 缺省 = **开**。
   *
   *  【为什么是可选的】老的 `settings.json` 里没有这个字段；
   *  Rust 侧的 `#[serde(default = "default_true")]` 会把它补成 true。
   *  TS 侧标成可选，是为了兼容「还没被 Rust 补过」的那些快照。 */
  autoRefresh?: boolean;
  /** 全局快捷键（后端规格，如 "Option+Cmd+Space"）。缺省 = 用默认值。 */
  globalShortcut?: string | null;
  /** 开机自启。 */
  launchAtLogin?: boolean;
}

/** 改全局快捷键的结果 —— **后端总是返回这个**（不是抛错）。
 *
 *  【为什么不是简单的成功/失败】
 *   注册失败（被别的软件占着）→ **什么都没变**，旧键还在生效；
 *   注销失败 → **状态已经变了**（生效的键不是「用户请求的那个」）。
 *   → 用 `Err` 表达会丢掉「实际生效的是哪个键」这个信息，
 *     面板只能显示一句笼统的「操作失败」，或者骗人说「已切换」。
 */
export interface ShortcutChange {
  /** **实际生效**的快捷键（`null` = 确实没有键生效）。 */
  effective: string | null;
  /** 警告文案（`null` = 一切正常）。 */
  warning: string | null;
}

/** 快捷键的**运行时真相** —— 后端 `get_global_shortcut` 的返回。
 *
 *  【为什么要有它】
 *   设置里存的是**用户的偏好**（"我想用 ⌥⌘Space"），
 *   而「此刻按下去会不会唤起 Lodestar」是**另一回事** ——
 *   启动时那个键被别的软件占着 → 注册失败 → 设置里一个字都没改 →
 *   面板若照着设置显示，就是在骗人说「它已生效」。
 */
export interface ShortcutRuntimeState {
  /** 本进程**实际注册着**的快捷键（`null` = 没有键生效）。 */
  effective: string | null;
  /** 出问题的原因（`null` = 一切正常）。 */
  error: string | null;
}
