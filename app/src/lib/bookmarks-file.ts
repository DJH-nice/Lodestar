/**
 * 书签的导入 / 导出 —— **文件格式**这一层的纯逻辑。
 *
 *  【为什么单独一个模块】格式规则（字段名、版本号、校验规则）
 *   跟「谁来读写文件」是两件事：
 *   - 「读写文件」在 App.vue（前端用 dialog 拿路径，Rust 侧用 std::fs 落盘）；
 *   - 「格式」在这里（纯函数，好测）。
 *   混在一起的话，格式规则只能靠点按钮测 —— 很难钉住每一条校验。
 *
 *  【格式】
 *   {
 *     "format": "fsearch-bookmarks",
 *     "version": 1,
 *     "bookmarks": [ { "kind": "query", "value": "java" } ]
 *   }
 *
 *  【导出什么】**只有书签** —— 索引目录 / 窗口位置是本机状态，不该被搬走。
 *
 *  【导入的语义】**追加**（合并），不是替换 —— 替换会静默丢掉用户已有的书签。
 */

import { bookmarkKey } from "./settings";
import type { Bookmark } from "./types";

/** 文件里的 `format` 字段 —— 必须是这个值。 */
export const BOOKMARKS_FORMAT = "fsearch-bookmarks";

/** 文件里的 `version` 字段 —— 现在只有 1。 */
export const BOOKMARKS_VERSION = 1;

/** 导出文件的结构。 */
export interface BookmarksFile {
  format: string;
  version: number;
  bookmarks: Bookmark[];
}

/** 生成导出文件的内容（JSON 文本）。
 *
 *  【排版】两空格缩进 + 结尾换行 —— 人能直接看懂，也能 diff。
 *  【label】这个项目从来不设 label（见 settings.ts 的说明），
 *   但万一文件里有，就**原样带出去**（导入时也留着）——
 *   导出不该悄悄丢字段。 */
export function buildBookmarksFile(bookmarks: Bookmark[]): string {
  const file: BookmarksFile = {
    format: BOOKMARKS_FORMAT,
    version: BOOKMARKS_VERSION,
    bookmarks: bookmarks.map((b) => {
      const out: Bookmark = { kind: b.kind, value: b.value };
      if (b.label) out.label = b.label;
      return out;
    }),
  };
  return JSON.stringify(file, null, 2) + "\n";
}

/** 解析结果 —— 校验通过，或者失败（带一句人话原因）。 */
export type ParsedBookmarks =
  | { ok: true; bookmarks: Bookmark[] }
  | { ok: false; error: string };

/** 解析并校验一个书签文件的内容。
 *
 *  【校验不过 → 一条都不导入】宁可什么都不做，
 *   也不能把用户已有的书签弄坏（导入是【追加】语义，
 *   但一个坏文件不该往设置里塞半截数据）。
 *
 *  【校验项】
 *   - `format` 必须是 `"fsearch-bookmarks"`
 *   - `version` 必须是 1
 *   - `bookmarks` 必须是数组
 *   - 每项的 `kind` 必须是 `"query"` / `"path"`，`value` 必须是非空字符串
 */
export function parseBookmarksFile(text: string): ParsedBookmarks {
  let data: unknown;
  try {
    data = JSON.parse(text);
  } catch (e) {
    return { ok: false, error: `不是合法的 JSON（${String(e)}）` };
  }

  if (typeof data !== "object" || data === null || Array.isArray(data)) {
    return { ok: false, error: "文件内容不是一个 JSON 对象" };
  }
  const obj = data as Record<string, unknown>;

  if (obj.format !== BOOKMARKS_FORMAT) {
    return {
      ok: false,
      error: `不是 Lodestar 书签文件（format 应为 "${BOOKMARKS_FORMAT}"）`,
    };
  }
  if (obj.version !== BOOKMARKS_VERSION) {
    return {
      ok: false,
      error: `书签文件的版本不认识（version 应为 ${BOOKMARKS_VERSION}）`,
    };
  }
  if (!Array.isArray(obj.bookmarks)) {
    return { ok: false, error: "bookmarks 字段不是一个数组" };
  }

  const bookmarks: Bookmark[] = [];
  for (let i = 0; i < obj.bookmarks.length; i++) {
    const item = obj.bookmarks[i];
    if (typeof item !== "object" || item === null || Array.isArray(item)) {
      return { ok: false, error: `第 ${i + 1} 条书签不是一个对象` };
    }
    const b = item as Record<string, unknown>;

    if (b.kind !== "query" && b.kind !== "path") {
      return {
        ok: false,
        error: `第 ${i + 1} 条书签的 kind 只能是 "query" 或 "path"`,
      };
    }
    if (typeof b.value !== "string" || b.value.trim() === "") {
      return { ok: false, error: `第 ${i + 1} 条书签的 value 必须是非空字符串` };
    }

    const bm: Bookmark = { kind: b.kind, value: b.value };
    // label 可选 —— 有就留着，没有就算了。
    if (typeof b.label === "string" && b.label !== "") bm.label = b.label;
    bookmarks.push(bm);
  }

  return { ok: true, bookmarks };
}

/** 合并导入的书签 —— **追加**（不是替换）。
 *
 *  【为什么追加】替换会静默丢掉用户已有的书签；追加更安全。
 *  【怎么去重】按 `kind:value`（`bookmarkKey`）——
 *   同一个文件导入两次不会产生重复条目。 */
export function mergeBookmarks(current: Bookmark[], incoming: Bookmark[]): Bookmark[] {
  const seen = new Set(current.map(bookmarkKey));
  const next = [...current];
  for (const b of incoming) {
    const key = bookmarkKey(b);
    if (seen.has(key)) continue;
    seen.add(key);
    next.push(b);
  }
  return next;
}
