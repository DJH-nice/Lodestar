/* 引擎真实的语法表 —— **一份 JSON**，不在这里手抄。
 *
 * 【为什么不能手抄】手抄的那份是副本：shim 以后加一个字段，副本不会跟着变，
 * 下面的测试仍然是绿的，但面板会用这份过期副本标灰 —— 把引擎明明支持的
 * 新字段标成「当前引擎不支持」（误灰）。
 *
 * 【护栏在哪】Rust 侧有测试盯着这份 JSON 跟 shim 的真实表对账：
 *   app/src-tauri/tests/syntax_tables.rs
 * 引擎改了表而这份 JSON 没更新 → 那边会红。
 *
 * 这里不去问后端：单元测试不该依赖 Tauri 运行时。 */
import tables from "../../shared/syntax-tables.json";

import { describe, expect, it } from "vitest";
import {
  buildLookup,
  clickToken,
  helpGroups,
  isRowSupported,
  keywordTokens,
  supportNote,
  syntaxHelp,
} from "./syntax-help";
import type { HelpGroup } from "./syntax-help";

const lookup = buildLookup(tables);

function groupOf(key: string): HelpGroup {
  const g = helpGroups.find((x) => x.key === key);
  if (!g) throw new Error(`没有这个分组：${key}`);
  return g;
}

describe("keywordTokens", () => {
  it("字段带占位符", () => {
    expect(keywordTokens("ext:<semicolon-seperated-list>")).toEqual([
      { token: "ext", colon: true },
    ]);
  });

  it("一个单元格里多个关键字", () => {
    expect(keywordTokens("datemodified:<date>, dm:<date>")).toEqual([
      { token: "datemodified", colon: true },
      { token: "dm", colon: true },
    ]);
  });

  it("没有值的修饰符", () => {
    expect(keywordTokens("file:, files:")).toEqual([
      { token: "file", colon: true },
      { token: "files", colon: true },
    ]);
  });

  it("运算符没有冒号", () => {
    expect(keywordTokens("space, AND, &&")).toEqual([
      { token: "space", colon: false },
      { token: "AND", colon: false },
      { token: "&&", colon: false },
    ]);
  });
});

describe("isRowSupported", () => {
  it("引擎认识的字段算支持", () => {
    const g = groupOf("functions");
    expect(isRowSupported(g, ["ext:<x>", "说明", "示例"], lookup)).toBe(true);
  });

  it("引擎不认识的字段算不支持", () => {
    const g = groupOf("functions");
    expect(isRowSupported(g, ["name:<str>", "说明", "示例"], lookup)).toBe(false);
  });

  it("修饰符要跟修饰符表比（不是跟字段表混着比）", () => {
    const g = groupOf("modifiers");
    expect(isRowSupported(g, ["path:", "说明", "示例"], lookup)).toBe(true);
    // path 是修饰符不是字段 —— 放错表就查不到
    expect(isRowSupported(g, ["datemodified:", "说明", "示例"], lookup)).toBe(false);
  });

  it("空格是隐式 AND —— 不在运算符表里也算支持", () => {
    const g = groupOf("operators");
    expect(isRowSupported(g, ["AND", "space, AND, &&", "说明"], lookup)).toBe(true);
  });

  it("括号分组也是运算符", () => {
    const g = groupOf("operators");
    expect(isRowSupported(g, ["Grouping", "(, )", "说明"], lookup)).toBe(true);
  });

  it("通配符不跟任何表比，一律算支持", () => {
    const g = groupOf("wildcards");
    expect(isRowSupported(g, ["*", "说明", "示例"], lookup)).toBe(true);
  });

  it("表还没拿到时一律算支持（别在加载途中把整张表灰掉）", () => {
    const g = groupOf("functions");
    expect(isRowSupported(g, ["name:<str>", "说明", "示例"], null)).toBe(true);
  });
});

describe("clickToken", () => {
  it("字段填关键字 + 冒号", () => {
    const g = groupOf("functions");
    expect(clickToken(g, ["ext:<x>", "说明", "示例"], lookup)).toBe("ext:");
  });

  it("修饰符填第一个关键字", () => {
    const g = groupOf("modifiers");
    expect(clickToken(g, ["file:, files:", "说明", "示例"], lookup)).toBe("file:");
  });

  it("运算符跳过不是 token 的 space", () => {
    const g = groupOf("operators");
    expect(clickToken(g, ["AND", "space, AND, &&", "说明"], lookup)).toBe("AND");
  });

  it("引擎表还没拿到时也不能给出伪 token space", () => {
    // 【回归】评审实测：lookup 为 null 时兜底取 kws[0]，
    // 而运算符组的 kws[0] 就是伪 token "space" ——
    // 塞进搜索框会变成搜「文件名里含 space 的文件」，静默错答。
    const g = groupOf("operators");
    expect(clickToken(g, ["AND", "space, AND, &&", "说明"], null)).toBe("AND");
    expect(clickToken(g, ["AND", "space, AND, &&", "说明"], null)).not.toBe("space");
  });

  it("整格都是伪 token 时不给出任何东西（宁可什么都不插）", () => {
    const g = groupOf("operators");
    expect(clickToken(g, ["AND", "space", "说明"], lookup)).toBe("");
    expect(clickToken(g, ["AND", "space", "说明"], null)).toBe("");
  });

  it("分组填左括号", () => {
    const g = groupOf("operators");
    expect(clickToken(g, ["Grouping", "(, )", "说明"], lookup)).toBe("(");
  });

  it("通配符原样填", () => {
    const g = groupOf("wildcards");
    expect(clickToken(g, ["*", "说明", "示例"], lookup)).toBe("*");
  });

  it("没有引擎表时退回第一个名字", () => {
    const g = groupOf("functions");
    expect(clickToken(g, ["dm:<date>", "说明", "示例"], null)).toBe("dm:");
  });
});

describe("上游文档与引擎的一致性", () => {
  it("文档里的每一项引擎都认（今天不该有任何一项是灰的）", () => {
    const gray: string[] = [];
    for (const g of helpGroups) {
      for (const row of syntaxHelp[g.key]) {
        if (!isRowSupported(g, row, lookup)) {
          gray.push(`${g.key}: ${row[g.keywordColumn]}`);
        }
      }
    }
    expect(gray).toEqual([]);
  });

  it("每一组都有内容", () => {
    for (const g of helpGroups) {
      expect(syntaxHelp[g.key].length).toBeGreaterThan(0);
    }
  });

  it("每组都有关键字列，且列数与表头一致", () => {
    for (const g of helpGroups) {
      for (const row of syntaxHelp[g.key]) {
        expect(row.length).toBe(g.columns.length);
      }
    }
  });
});

describe("supportNote", () => {
  /* fail-open 时一项都没灰 —— 还在说「灰掉的项表示当前引擎不支持」就是在骗人。 */
  it("拿到引擎表 → 说「灰掉的项表示当前引擎不支持」", () => {
    expect(supportNote(lookup)).toContain("灰掉的项表示当前引擎不支持");
  });

  it("没拿到引擎表 → 换成「还没加载，暂时无法标注」", () => {
    const note = supportNote(null);
    expect(note).not.toContain("灰掉的项表示");
    expect(note).toContain("还没加载");
  });
});

describe("转义组（上游 search_syntax_escaping.page）", () => {
  it("分组在、而且有内容", () => {
    const g = groupOf("escaping");
    expect(syntaxHelp.escaping.length).toBeGreaterThan(0);
    expect(g.columns).toEqual(["写法", "说明", "示例"]);
  });

  it("引号和反斜杠两行都在", () => {
    const texts = syntaxHelp.escaping.map((r) => r[0]);
    expect(texts.some((t) => t.includes("Double quotes"))).toBe(true);
    expect(texts.some((t) => t.includes("Backslash"))).toBe(true);
  });

  it("不跟任何引擎表比 —— 一律算支持（不灰）", () => {
    const g = groupOf("escaping");
    expect(g.table).toBeNull();
    expect(isRowSupported(g, syntaxHelp.escaping[0], lookup)).toBe(true);
  });

  it("关键字列【不可点】—— 那一列是说明词，插进搜索框是错的", () => {
    const g = groupOf("escaping");
    expect(g.clickable).toBe(false);
    // 就算有人绕过 UI 直接调 clickToken，也拿不到可插入的东西
    expect(clickToken(g, syntaxHelp.escaping[0], lookup)).toBe("");
  });
});
