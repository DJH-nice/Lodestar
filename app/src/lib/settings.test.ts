import { describe, expect, it } from "vitest";
import {
  appendKeyword,
  bookmarkKey,
  hasUnclosedQuote,
  isQueryBookmarked,
  isPathBookmarked,
  pushRecent,
  recentChanged,
  removeBookmark,
  splitBookmarks,
  toggleBookmark,
} from "./settings";
import type { Bookmark } from "./types";

describe("pushRecent", () => {
  it("新的查询排在最前", () => {
    expect(pushRecent(["b", "a"], "c")).toEqual(["c", "b", "a"]);
  });

  it("重复的查询移到最前而不是重复出现", () => {
    expect(pushRecent(["b", "a"], "a")).toEqual(["a", "b"]);
  });

  it("空查询不进来", () => {
    expect(pushRecent(["a"], "   ")).toEqual(["a"]);
  });

  it("超过上限时截断", () => {
    const list = Array.from({ length: 20 }, (_, i) => `q${i}`);
    expect(pushRecent(list, "new")).toHaveLength(20);
    expect(pushRecent(list, "new")[0]).toBe("new");
  });
});

describe("recentChanged", () => {
  /* 没变就别写盘 —— 再搜一遍同样的词，最近列表原封不动。 */
  it("完全一样 → 没变", () => {
    expect(recentChanged(["a", "b"], ["a", "b"])).toBe(false);
  });

  it("新的查询排在最前 → 变了", () => {
    expect(recentChanged(["a", "b"], ["c", "a", "b"])).toBe(true);
  });

  it("长度一样但顺序变了 → 变了", () => {
    expect(recentChanged(["a", "b"], ["b", "a"])).toBe(true);
  });

  it("空 → 有 → 变了", () => {
    expect(recentChanged([], ["a"])).toBe(true);
  });

  /* pushRecent 之后再比 —— 重复搜同一个词不该触发一次写入 */
  it("重复搜同一个词 → pushRecent 之后没变", () => {
    const list = ["a", "b"];
    expect(recentChanged(list, pushRecent(list, "a"))).toBe(false);
  });
});

describe("toggleBookmark", () => {
  const q: Bookmark = { kind: "query", value: "ext:rs" };
  const p: Bookmark = { kind: "path", value: "/tmp" };

  it("没有就加上", () => {
    expect(toggleBookmark([], q)).toEqual([q]);
  });

  it("有就删掉", () => {
    expect(toggleBookmark([q], q)).toEqual([]);
  });

  it("kind 不同算两条", () => {
    const list = toggleBookmark([q], p);
    expect(list).toHaveLength(2);
  });

  it("key 带上 kind", () => {
    expect(bookmarkKey(q)).not.toBe(bookmarkKey(p));
  });
});

describe("removeBookmark", () => {
  /* 删除必须是【只删不加】——
     用 toggle 的话，这条已经被别处删掉时会被「加回来」
     （用户点的是删除，不是收藏）。 */
  const q: Bookmark = { kind: "query", value: "ext:rs" };
  const p: Bookmark = { kind: "path", value: "/tmp" };

  it("删掉指定的那一条", () => {
    expect(removeBookmark([q, p], q)).toEqual([p]);
  });

  it("不在列表里 → 什么都不做（不会把它加进来）", () => {
    expect(removeBookmark([p], q)).toEqual([p]);
    expect(removeBookmark([], q)).toEqual([]);
  });

  it("kind 不同算两条 —— 只删同 kind 的那条", () => {
    const same = removeBookmark([q, p], q);
    expect(same.some((b) => b.kind === "path")).toBe(true);
  });

  /* 护栏：key 里【必须带 kind】——
     两条书签 value 相同、只有 kind 不同（一条查询书签、一条路径书签）
     是真实存在的情况（用户收藏了 "/tmp" 这个查询，又收藏了 "/tmp" 这个目录）。
     key 少了 kind 前缀的话，删一条会把另一条也删掉。 */
  it("同 value 不同 kind → 各删各的（不会互删）", () => {
    const queryOne: Bookmark = { kind: "query", value: "/tmp" };
    const pathOne: Bookmark = { kind: "path", value: "/tmp" };
    expect(removeBookmark([queryOne, pathOne], queryOne)).toEqual([pathOne]);
    expect(removeBookmark([queryOne, pathOne], pathOne)).toEqual([queryOne]);
  });

  /* 反证：key 少了 kind 前缀的话，上面那条会红（两条会被一起删掉）——
     所以这里把「key 确实带 kind」也钉住。 */
  it("反证：同 value 不同 kind → 两条的 key 不同", () => {
    expect(bookmarkKey({ kind: "query", value: "/tmp" })).not.toBe(
      bookmarkKey({ kind: "path", value: "/tmp" }),
    );
  });
});

describe("splitBookmarks", () => {
  it("分成查询书签与路径书签两栏", () => {
    const q: Bookmark = { kind: "query", value: "ext:rs" };
    const p: Bookmark = { kind: "path", value: "/tmp" };
    const g = splitBookmarks([q, p]);
    expect(g.query).toEqual([q]);
    expect(g.path).toEqual([p]);
  });

  it("空列表 → 两栏都空", () => {
    const g = splitBookmarks([]);
    expect(g.query).toEqual([]);
    expect(g.path).toEqual([]);
  });
});

describe("isQueryBookmarked", () => {
  /* 标题栏「收藏」按钮的状态判定 ——
     它必须跟【设置文件里真实的书签列表】一致，
     否则会出现「面板里已经删了、按钮还以为收藏着」。 */
  const list: Bookmark[] = [{ kind: "query", value: "ext:rs" }];

  it("在列表里 → 已收藏", () => {
    expect(isQueryBookmarked(list, "ext:rs")).toBe(true);
  });

  it("不在列表里 → 没收藏", () => {
    expect(isQueryBookmarked(list, "ext:md")).toBe(false);
  });

  it("前后空白不算（查询是 trim 过的）", () => {
    expect(isQueryBookmarked(list, "  ext:rs  ")).toBe(true);
  });

  it("空查询 → 没收藏", () => {
    expect(isQueryBookmarked(list, "   ")).toBe(false);
  });

  it("kind 不同不算（路径书签跟查询书签是两回事）", () => {
    expect(isQueryBookmarked([{ kind: "path", value: "ext:rs" }], "ext:rs")).toBe(false);
  });

  it("删掉之后就变成「没收藏」—— 状态跟着列表走", () => {
    const q: Bookmark = { kind: "query", value: "ext:rs" };
    const after = removeBookmark(list, q);
    expect(isQueryBookmarked(after, "ext:rs")).toBe(false);
  });
});

describe("isPathBookmarked", () => {
  /* 右键菜单 / 预览卡的「收藏 / 取消收藏」状态判定 ——
     必须跟【设置文件里真实的书签列表】一致。 */
  const list: Bookmark[] = [{ kind: "path", value: "/a/b" }];

  it("空列表 → 没收藏", () => {
    expect(isPathBookmarked([], "/a/b")).toBe(false);
  });

  it("在列表里 → 已收藏", () => {
    expect(isPathBookmarked(list, "/a/b")).toBe(true);
  });

  it("不在列表里 → 没收藏", () => {
    expect(isPathBookmarked(list, "/a/c")).toBe(false);
  });

  it("kind 不同不算（查询书签跟路径书签是两回事）", () => {
    expect(isPathBookmarked([{ kind: "query", value: "/a/b" }], "/a/b")).toBe(false);
  });

  it("空路径 → 没收藏", () => {
    expect(isPathBookmarked(list, "")).toBe(false);
  });

  it("删掉之后就变成「没收藏」—— 状态跟着列表走", () => {
    const p: Bookmark = { kind: "path", value: "/a/b" };
    const after = removeBookmark(list, p);
    expect(isPathBookmarked(after, "/a/b")).toBe(false);
  });
});

describe("appendKeyword", () => {
  it("空查询 → 直接就是关键字", () => {
    expect(appendKeyword("", "ext:")).toBe("ext:");
  });

  it("有查询 → 追加，用空格分隔（空格 = 引擎的隐式 AND）", () => {
    expect(appendKeyword("report", "ext:")).toBe("report ext:");
  });

  it("末尾已经有空格就不再补一个", () => {
    expect(appendKeyword("report ", "ext:")).toBe("report ext:");
  });

  it("用户原有的内容一个字都不动", () => {
    expect(appendKeyword("  Report  ", "ext:")).toBe("  Report  ext:");
  });

  it("关键字是空的 → 查询不变", () => {
    expect(appendKeyword("report", "   ")).toBe("report");
  });

  it("全空白的查询当成空查询", () => {
    expect(appendKeyword("   ", "ext:")).toBe("ext:");
  });

  /* ---- 未闭合引号 → 拒绝追加（静默错答比不追加坏得多）----
     引擎的词法器（src/fsearch_query_lexer.c 的 parse_quoted_string）
     读到输入结束也没等到收尾的引号时，会把余下全部吞成一个字面词。 */
  it("查询里有未闭合的引号 → 拒绝追加（返回 null）", () => {
    expect(appendKeyword('"foo', "ext:")).toBeNull();
  });

  it("未闭合引号 + 追加 = 字段被吞进引号（这就是要避免的错答）", () => {
    // 反证：直接拼的话会得到 "foo ext:" —— 引擎把它读成一个字面词
    expect(`"foo ext:`).not.toBe(`"foo" ext:`);
    expect(appendKeyword('"foo', "ext:")).not.toBe('"foo ext:');
  });

  it("引号闭合了照常追加", () => {
    expect(appendKeyword('"foo"', "ext:")).toBe('"foo" ext:');
  });

  it("引号在词的中间闭合也算闭合（引擎读成一个词，不是未闭合）", () => {
    expect(appendKeyword('"foo "bar', "ext:")).toBe('"foo "bar ext:');
  });
});

describe("hasUnclosedQuote", () => {
  it("没有引号 → 没未闭合", () => {
    expect(hasUnclosedQuote("report")).toBe(false);
  });

  it("成对的引号 → 没未闭合", () => {
    expect(hasUnclosedQuote('"foo"')).toBe(false);
  });

  it("只有开引号 → 未闭合", () => {
    expect(hasUnclosedQuote('"foo')).toBe(true);
  });

  it("引号外的反斜杠转义了引号 → 那个引号不算数", () => {
    expect(hasUnclosedQuote('foo\\"bar')).toBe(false);
  });

  it("引号内的反斜杠【不】转义（引擎的引号内没有转义规则）", () => {
    // 引擎：parse_quoted_string 里的 \ 是普通字符
    expect(hasUnclosedQuote('"foo\\"')).toBe(false);
    // 但如果反斜杠后面【没有】引号，引号就没闭合
    expect(hasUnclosedQuote('"foo\\')).toBe(true);
  });

  it("引号在词的中间闭合 → 闭合了", () => {
    expect(hasUnclosedQuote('"foo "bar')).toBe(false);
  });

  it("空查询 → 没未闭合", () => {
    expect(hasUnclosedQuote("")).toBe(false);
  });
});