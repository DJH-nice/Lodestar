// @vitest-environment happy-dom
/**
 * 速查卡（app/shared/cheatsheet.json）的回归测试。
 *
 *  【验什么】
 *   1. 数据本身完整（组有标题、条目有写法、组 key 不重）；
 *   2. 过滤逻辑对（空关键词全给、命中才算、组空了整组丢）；
 *   3. **每条写法里的字段/修饰符都是引擎真认的** —— 拿引擎自己的语法表比。
 *
 *  【第 3 条为什么重要】速查卡是给普通用户照着抄的，
 *  写错一个字段名（比如把 parent: 写成 paren:）就是**静默错答**：
 *  引擎不报错，只给一个错的答案。这里拿
 *  `app/shared/syntax-tables.json`（引擎的语法表）当场比对，
 *  写错就红。
 *
 *  【为什么不在这里做「语义」验证】语义（这条查询真的能搜到该搜的东西）
 *  要真引擎 + 真实文件系统 → 那在 Rust 侧做
 *  （app/src-tauri/fsearch-core/tests/cheatsheet_queries.rs）。
 */

import { describe, expect, it } from "vitest";

import tables from "../../shared/syntax-tables.json";
import { cheatGroups, cheatSearchText, filterCheatsheet } from "./cheatsheet";

/** 引擎认识的字段与修饰符（小写）。 */
const fields = new Set(tables.fields.map((f) => f.toLowerCase()));
const modifiers = new Set(tables.modifiers.map((m) => m.toLowerCase()));

/**
 * 从一条查询里抽出所有「字段:」/「修饰符:」形式的 token。
 *
 *  【怎么抽】只看冒号**前面**的那段词 —— `ext:pdf;doc` 抽到 `ext`，
 *  `path:/Users/me/Docs` 抽到 `path`，`size:>10M` 抽到 `size`。
 *  值里的冒号（URL 之类）这里不出现，不用处理。
 */
function tokensOf(query: string): string[] {
  const out: string[] = [];
  const re = /(?:^|[\s(])"?([a-zA-Z]+):/g;
  let m: RegExpExecArray | null;
  while ((m = re.exec(query)) !== null) out.push(m[1].toLowerCase());
  return out;
}

describe("速查卡：数据完整性", () => {
  it("S1 有组，组有标题和条目", () => {
    expect(cheatGroups.length).toBeGreaterThan(0);
    for (const g of cheatGroups) {
      expect(g.key, "组要有 key").toBeTruthy();
      expect(g.title, "组要有标题").toBeTruthy();
      expect(g.items.length, `组 ${g.key} 不能是空的`).toBeGreaterThan(0);
    }
  });

  it("S2 组 key 不重复（样式与测试都靠它定位）", () => {
    const keys = cheatGroups.map((g) => g.key);
    expect(new Set(keys).size).toBe(keys.length);
  });

  it("S3 每条都有说法与写法，写法非空", () => {
    for (const g of cheatGroups) {
      for (const it of g.items) {
        expect(it.intent, `组 ${g.key} 里的条目缺 intent`).toBeTruthy();
        expect(it.query.trim(), `组 ${g.key} 里的「${it.intent}」缺写法`).not.toBe("");
      }
    }
  });

  it("S4 错误写法不能与正确写法相同（否则这坑就白写了）", () => {
    for (const g of cheatGroups) {
      for (const it of g.items) {
        if (!it.wrong) continue;
        expect(it.wrong.trim(), `「${it.intent}」的错误写法不能跟正确写法一样`).not.toBe(
          it.query.trim(),
        );
      }
    }
  });
});

describe("速查卡：写法必须是引擎真认的", () => {
  it("S5 每条写法里的字段/修饰符都在引擎语法表里", () => {
    const unknown: string[] = [];
    for (const g of cheatGroups) {
      for (const it of g.items) {
        for (const tok of tokensOf(it.query)) {
          if (!fields.has(tok) && !modifiers.has(tok)) {
            unknown.push(`${g.key} / ${it.intent} → ${tok}:`);
          }
        }
      }
    }
    expect(unknown, `这些字段/修饰符引擎不认：\n${unknown.join("\n")}`).toEqual([]);
  });

  it("S6 AND / OR / NOT 必须大写（小写会被当成普通搜索词）", () => {
    for (const g of cheatGroups) {
      for (const it of g.items) {
        expect(it.query, `「${it.intent}」里出现了小写的逻辑词`).not.toMatch(
          /(^|[\s(])(and|or|not)([\s)]|$)/,
        );
      }
    }
  });
});

describe("速查卡：过滤", () => {
  const groups = [
    {
      key: "a",
      title: "A",
      items: [
        { intent: "找大文件", query: "size:>10M", note: "单位是十进制" },
        { intent: "找图片", query: "ext:png" },
      ],
    },
    {
      key: "b",
      title: "B",
      items: [{ intent: "排除图片", query: "NOT ext:png", note: "排除某一类" }],
    },
  ];

  it("S7 关键词为空 → 一个都不筛掉", () => {
    expect(filterCheatsheet(groups, "")).toHaveLength(2);
    expect(filterCheatsheet(groups, "   ")).toHaveLength(2);
  });

  it("S8 按意图/写法/说明都能搜到", () => {
    expect(filterCheatsheet(groups, "大文件")).toHaveLength(1);
    expect(filterCheatsheet(groups, "ext:png")).toHaveLength(2);
    expect(filterCheatsheet(groups, "排除")).toHaveLength(1);
  });

  it("S9 不区分大小写", () => {
    expect(filterCheatsheet(groups, "EXT:PNG")).toHaveLength(2);
  });

  it("S10 组里一条不剩 → 整组丢掉（不留空标题）", () => {
    const out = filterCheatsheet(groups, "找图片");
    expect(out).toHaveLength(1);
    expect(out[0].key).toBe("a");
    expect(out[0].items).toHaveLength(1);
  });

  it("S11 一个都搜不到 → 空数组", () => {
    expect(filterCheatsheet(groups, "不存在的词")).toEqual([]);
  });

  it("S12 搜索文本包含意图、写法、说明", () => {
    const text = cheatSearchText(groups[0].items[0]);
    expect(text).toContain("找大文件");
    expect(text).toContain("size:>10m");
    expect(text).toContain("十进制");
  });
});
