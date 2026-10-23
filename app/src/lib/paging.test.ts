import { describe, expect, it } from "vitest";
import { neededPages, separatorRow } from "./paging";
import type { SearchResult } from "./types";

describe("neededPages", () => {
  it("视口在已加载区内时不请求", () => {
    const loaded = new Set([0]);
    expect(neededPages(30, 400, 1000, 25, loaded)).toEqual([]);
  });

  it("用户把滚动条拖到中间时请求对应页", () => {
    const loaded = new Set([0]);
    // scrollTop=5000 → 第 ~177 行 → 第 7 页（每页 25 行）
    const pages = neededPages(5000, 400, 1000, 25, loaded);
    expect(pages).toContain(7);
  });

  it("滚过头时仍请求最后一页（而不是一页都不请求）", () => {
    // totalEntries=10、pageSize=25 → 只有第 0 页存在
    const pages = neededPages(999999, 400, 10, 25, new Set());
    expect(pages).toEqual([0]);
  });

  it("总数为 0 时不请求任何页", () => {
    expect(neededPages(0, 400, 0, 25, new Set())).toEqual([]);
  });
});

describe("separatorRow", () => {
  const base: SearchResult = {
    entries: [],
    total_entries: 10,
    total_files: 7,
    total_folders: 3,
    complete: true,
    elapsed_ms: 1,
    warning: null,
    group_boundary: null,
  };

  it("升序时分隔行在目录段之后（第 3 行）", () => {
    expect(separatorRow(base, "all", "asc")).toBe(3);
  });

  it("降序时分隔行在文件段之后（第 7 行）", () => {
    expect(separatorRow(base, "all", "desc")).toBe(7);
  });

  it("不是「全部」列表时没有分隔行", () => {
    expect(separatorRow(base, "files", "asc")).toBeNull();
  });

  it("分界在两端时不插分隔行", () => {
    const onlyFiles: SearchResult = { ...base, total_folders: 0 };
    expect(separatorRow(onlyFiles, "all", "asc")).toBeNull();
  });
});
