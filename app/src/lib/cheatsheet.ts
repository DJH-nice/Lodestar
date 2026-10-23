import raw from "../../shared/cheatsheet.json";

/**
 * 搜索语法**速查卡**的数据与纯逻辑。
 *
 *  【数据在哪儿】`app/shared/cheatsheet.json` —— 前端 import 它，
 *  Rust 侧的测试（fsearch-core/tests/cheatsheet_queries.rs）用 include_str! 读同一份。
 *  → **只有一份真相**：改速查内容不用改两处，语法写错了 Rust 那边会直接红。
 *
 *  【为什么单独一个模块】速查卡有两件事：
 *   - 「有哪些条目」→ 数据（这里）
 *   - 「按关键词筛出哪些条目」→ 纯函数（这里）
 *  混进组件里就只能靠点界面测，筛不准很难发现。
 */

/** 速查卡里的一条 —— 一个「我想找什么 → 这么写」。 */
export interface CheatItem {
  /** 人话：我想找什么。 */
  intent: string;
  /** 这么写。**点一下就填进搜索框** —— 每一条都必须是引擎真认的写法。 */
  query: string;
  /** 补充说明（可选）。 */
  note?: string;
  /**
   * 常见的**错误**写法（可选）。
   *
   *  【为什么单独一个字段】错误写法**不能**点 —— 点了就把错的填进搜索框了。
   *  它只是给人看一眼「我以前就是这么写的，难怪搜不到」。
   */
  wrong?: string;
}

/** 一组速查条目。 */
export interface CheatGroup {
  /** 组的标识（稳定，测试与样式都用它）。 */
  key: string;
  /** 组标题。 */
  title: string;
  /** 组上的一句说明（可选）。 */
  hint?: string;
  /** 这一组的条目。 */
  items: CheatItem[];
}

/** JSON 文件的结构（顶层还带 version / note，这里用不到）。 */
interface CheatsheetFile {
  version: number;
  note?: string;
  groups: CheatGroup[];
}

const file = raw as unknown as CheatsheetFile;

/** 速查卡的全部条目（按组）。 */
export const cheatGroups: CheatGroup[] = file.groups;

/** 一条速查里**能被搜到**的文本 —— 意图、写法、说明、错误写法都算。 */
export function cheatSearchText(item: CheatItem): string {
  return [item.intent, item.query, item.note ?? "", item.wrong ?? ""]
    .join(" ")
    .toLowerCase();
}

/**
 * 按关键词过滤速查卡。
 *
 *  【规则】
 *   - 关键词为空 → 原样返回（一个都不筛掉）
 *   - 关键词不区分大小写
 *   - 组里**一条都不剩**的组整个丢掉 —— 不留空标题
 *
 *  【为什么搜「说明」也搜】用户想找「怎么排除某类文件」时，
 *  心里想的词是「排除」，可能只出现在说明里 —— 只搜写法会漏。
 */
export function filterCheatsheet(groups: CheatGroup[], keyword: string): CheatGroup[] {
  const kw = keyword.trim().toLowerCase();
  if (!kw) return groups;
  const out: CheatGroup[] = [];
  for (const g of groups) {
    const items = g.items.filter((it) => cheatSearchText(it).includes(kw));
    if (items.length) out.push({ ...g, items });
  }
  return out;
}
