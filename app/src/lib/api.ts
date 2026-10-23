import { invoke } from "@tauri-apps/api/core";
import type {
  IndexStatus,
  ListKind,
  SearchResult,
  SortField,
  SortOrder,
  SyntaxTables,
} from "./types";

export function search(
  query: string,
  sort: SortField,
  order: SortOrder,
  kind: ListKind,
  page: number,
  pageSize: number,
): Promise<SearchResult> {
  return invoke<SearchResult>("search", {
    query,
    sort,
    order,
    kind,
    page,
    pageSize,
  });
}

/** 重新扫描索引（标题栏的「刷新」）。
 *
 *  返回**新的索引状态** —— 条目数变了（新增/删除了文件），
 *  前端要拿它更新状态栏，并**重搜一次**（旧结果要清掉）。
 *  （以前这个命令返回 void、也不更新状态 → 结果表留着 rescan 前的行。） */
export function rescan(): Promise<IndexStatus> {
  return invoke<IndexStatus>("rescan");
}

export function syntaxTables(): Promise<SyntaxTables> {
  return invoke<SyntaxTables>("syntax_tables");
}

export function excludeRules(): Promise<string[]> {
  return invoke<string[]>("exclude_rules");
}

export function indexStatus(): Promise<IndexStatus> {
  return invoke<IndexStatus>("index_status");
}

/**
 * 告诉后端「**我现在的认知**：窗口可见吗」。
 *
 *  后端只在「实际状态 ≠ 你的认知」时才给你推可见性事件 ——
 *  这样稳态下**零 IPC**（而 Tauri 的事件 = 在 webview 里跑一段 JS，很贵），
 *  事件万一丢了它也知道「你还没收到」→ 继续推 → 自愈。
 *
 *  【什么时候调】**只有认知真的变了的时候**（见 `App.vue` 的 `applyVisibility`）。
 *  每次都调的话，前端每 800ms 一次的轮询会带上一次 ack → 白花 IPC。
 */
export function ackVisibility(visible: boolean): Promise<void> {
  return invoke<void>("ack_visibility", { visible });
}

/** 换索引目录：重建引擎。失败时旧引擎原封不动。 */
export function reindex(paths: string[]): Promise<IndexStatus> {
  return invoke<IndexStatus>("reindex", { paths });
}

/** 把书签写到用户选的文件里（Rust 侧用 std::fs 落盘）。
 *
 *  【为什么不让前端直接写文件】前端没有文件系统的能力
 *  （`@tauri-apps/plugin-fs` 不装，也不打算装）——
 *  路径由前端用**已装的** `@tauri-apps/plugin-dialog` 拿，落盘交给 Rust。 */
export function writeBookmarksFile(path: string, contents: string): Promise<void> {
  return invoke<void>("write_bookmarks_file", { path, contents });
}

/** 读一个书签文件的内容 —— **解析与校验在前端做**（格式规则只留一份）。 */
export function readBookmarksFile(path: string): Promise<string> {
  return invoke<string>("read_bookmarks_file", { path });
}
