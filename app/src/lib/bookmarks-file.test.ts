// @vitest-environment happy-dom
/**
 * 书签文件格式（导出 / 导入 / 合并）的纯逻辑测试。
 *
 *  【为什么单独测】格式规则跟「谁来读写文件」是两件事 ——
 *   读写在 App.vue + Rust，格式在这里。
 *   混在一起的话，每一条校验规则都只能靠点按钮验 —— 很难钉住。
 */

import { describe, expect, it } from "vitest";

import {
  BOOKMARKS_FORMAT,
  BOOKMARKS_VERSION,
  buildBookmarksFile,
  mergeBookmarks,
  parseBookmarksFile,
} from "./bookmarks-file";
import type { Bookmark } from "./types";

/** 造一个合法的导出文件内容。 */
function fileJson(bookmarks: unknown[]): string {
  return JSON.stringify({
    format: BOOKMARKS_FORMAT,
    version: BOOKMARKS_VERSION,
    bookmarks,
  });
}

describe("书签文件的导出", () => {
  it("结构正确：format / version / bookmarks", () => {
    const text = buildBookmarksFile([
      { kind: "query", value: "java" },
      { kind: "path", value: "/Users/x/y" },
    ]);
    const parsed = JSON.parse(text);
    expect(parsed.format).toBe("fsearch-bookmarks");
    expect(parsed.version).toBe(1);
    expect(parsed.bookmarks).toEqual([
      { kind: "query", value: "java" },
      { kind: "path", value: "/Users/x/y" },
    ]);
  });

  it("空列表也能导出一个合法的文件", () => {
    const parsed = parseBookmarksFile(buildBookmarksFile([]));
    expect(parsed.ok).toBe(true);
    if (parsed.ok) expect(parsed.bookmarks).toEqual([]);
  });

  it("导出的内容能被自己解析回来（往返）", () => {
    const list: Bookmark[] = [{ kind: "query", value: "ext:rs" }];
    const parsed = parseBookmarksFile(buildBookmarksFile(list));
    expect(parsed.ok).toBe(true);
    if (parsed.ok) expect(parsed.bookmarks).toEqual(list);
  });
});

describe("书签文件的导入校验", () => {
  it("不是 JSON → 报错", () => {
    const r = parseBookmarksFile("{ 这不是 JSON");
    expect(r.ok).toBe(false);
    if (!r.ok) expect(r.error).toContain("JSON");
  });

  it("format 不对 → 报错", () => {
    const r = parseBookmarksFile(
      JSON.stringify({ format: "别的", version: 1, bookmarks: [] }),
    );
    expect(r.ok).toBe(false);
    if (!r.ok) expect(r.error).toContain("format");
  });

  it("version 不对 → 报错", () => {
    const r = parseBookmarksFile(
      JSON.stringify({ format: BOOKMARKS_FORMAT, version: 2, bookmarks: [] }),
    );
    expect(r.ok).toBe(false);
    if (!r.ok) expect(r.error).toContain("version");
  });

  it("bookmarks 不是数组 → 报错", () => {
    const r = parseBookmarksFile(
      JSON.stringify({ format: BOOKMARKS_FORMAT, version: 1, bookmarks: {} }),
    );
    expect(r.ok).toBe(false);
    if (!r.ok) expect(r.error).toContain("数组");
  });

  it("kind 不认识 → 报错", () => {
    const r = parseBookmarksFile(fileJson([{ kind: "wat", value: "x" }]));
    expect(r.ok).toBe(false);
    if (!r.ok) expect(r.error).toContain("kind");
  });

  it("value 是空串 → 报错", () => {
    const r = parseBookmarksFile(fileJson([{ kind: "query", value: "  " }]));
    expect(r.ok).toBe(false);
    if (!r.ok) expect(r.error).toContain("value");
  });

  it("value 不是字符串 → 报错", () => {
    const r = parseBookmarksFile(fileJson([{ kind: "query", value: 42 }]));
    expect(r.ok).toBe(false);
  });

  it("顶层不是对象 → 报错", () => {
    expect(parseBookmarksFile("[]").ok).toBe(false);
    expect(parseBookmarksFile("null").ok).toBe(false);
    expect(parseBookmarksFile("123").ok).toBe(false);
  });
});

describe("书签的合并（导入是追加，不是替换）", () => {
  it("已有的书签一条都不丢", () => {
    const current: Bookmark[] = [{ kind: "query", value: "java" }];
    const next = mergeBookmarks(current, [{ kind: "path", value: "/tmp/x" }]);
    expect(next).toEqual([
      { kind: "query", value: "java" },
      { kind: "path", value: "/tmp/x" },
    ]);
  });

  it("同一个文件导入两次 → 不产生重复", () => {
    const incoming: Bookmark[] = [
      { kind: "query", value: "java" },
      { kind: "path", value: "/tmp/x" },
    ];
    const once = mergeBookmarks([], incoming);
    const twice = mergeBookmarks(once, incoming);
    expect(twice).toHaveLength(2);
  });

  it("kind 相同、value 不同 → 是两条（去重按 kind:value）", () => {
    const next = mergeBookmarks(
      [{ kind: "query", value: "java" }],
      [{ kind: "query", value: "rust" }],
    );
    expect(next).toHaveLength(2);
  });

  it("kind 不同、value 相同 → 是两条", () => {
    const next = mergeBookmarks(
      [{ kind: "query", value: "x" }],
      [{ kind: "path", value: "x" }],
    );
    expect(next).toHaveLength(2);
  });
});
