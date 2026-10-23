import type { Bookmark } from "./types";

export const RECENT_LIMIT = 20;

/** 把一条新查询并进最近列表：去重（最新的在前）、上限 `limit` 条。 */
export function pushRecent(
  list: string[],
  query: string,
  limit: number = RECENT_LIMIT,
): string[] {
  const q = query.trim();
  if (!q) return list;
  const next = [q, ...list.filter((x) => x !== q)];
  return next.slice(0, limit);
}

/** 最近列表有没有真的变。
 *
 *  用来判断「这次回车要不要写盘」——
 *  再搜一遍同样的词，最近列表原封不动，没必要为它写一次文件。
 *  （逐项比，不只看长度 —— 长度一样但顺序变了也是变了。） */
export function recentChanged(current: string[], next: string[]): boolean {
  if (current.length !== next.length) return true;
  return current.some((v, i) => v !== next[i]);
}

/** 书签的唯一标识 —— 同一 kind + 同一 value 算同一条。 */
export function bookmarkKey(b: Bookmark): string {
  return `${b.kind}:${b.value}`;
}

/** 加/删一条书签（已存在则删）。 */
export function toggleBookmark(list: Bookmark[], b: Bookmark): Bookmark[] {
  const key = bookmarkKey(b);
  return list.some((x) => bookmarkKey(x) === key)
    ? list.filter((x) => bookmarkKey(x) !== key)
    : [...list, b];
}

/** 删掉一条书签（按 kind + value 精确匹配）。
 *
 *  【为什么不用 toggleBookmark 做删除】toggle 是「有就删、没有就加」——
 *  删除的动作必须是【只删不加】：万一这条已经被别处删了，
 *  toggle 会把它**加回来**（用户点的是删除，不是收藏）。 */
export function removeBookmark(list: Bookmark[], b: Bookmark): Bookmark[] {
  const key = bookmarkKey(b);
  return list.filter((x) => bookmarkKey(x) !== key);
}

/** 把书签分成【查询书签】与【路径书签】两栏（面板要分开显示）。 */
export function splitBookmarks(list: Bookmark[]): {
  query: Bookmark[];
  path: Bookmark[];
} {
  return {
    query: list.filter((b) => b.kind === "query"),
    path: list.filter((b) => b.kind === "path"),
  };
}

/** 某个查询是不是已经收藏了。
 *
 *  【谁在用】标题栏那个「收藏」按钮的状态 ——
 *  它得跟【设置文件里真实的书签列表】一致，
 *  否则会出现「面板里已经删了、按钮还以为收藏着」这种不同步。 */
export function isQueryBookmarked(list: Bookmark[], query: string): boolean {
  const q = query.trim();
  if (!q) return false;
  const key = bookmarkKey({ kind: "query", value: q });
  return list.some((b) => bookmarkKey(b) === key);
}

/** 某个路径是不是已经收藏了。
 *
 *  【谁在用】右键菜单与预览卡的「收藏 / 取消收藏」文案与状态 ——
 *  必须跟设置文件里真实的书签列表一致，否则会出现
 *  「面板里已经删了、菜单还以为收藏着，再点一下又加回来」这种不同步。 */
export function isPathBookmarked(list: Bookmark[], path: string): boolean {
  if (!path) return false;
  const key = bookmarkKey({ kind: "path", value: path });
  return list.some((b) => bookmarkKey(b) === key);
}

/** 查询里有没有【未闭合的引号】。
 *
 *  引擎的词法器（上游 src/fsearch_query_lexer.c:55-65 的 parse_quoted_string）
 *  读到输入结束也没等到收尾的引号时，会把余下【全部】吞成一个字面词。
 *  所以 `"foo` 单独用是能搜到 "foo" 的（上游语义本身没错），
 *  但 `"foo` 后面再追加 `ext:` → `"foo ext:` → 整串是一个字面词，
 *  追加的字段过滤被吞掉，而且【没有任何提示】—— 静默错答。
 *
 *  这里照着词法器的规则走一遍引号状态机：
 *    - 引号【外】遇到 `\` → 下一个字符是转义的，不算引号
 *    - 引号【外】遇到 `"` → 进入引号内
 *    - 引号【内】遇到 `"` → 回到引号外（引号内的 `\` 不是转义）
 *  走完还在引号内 = 未闭合。
 *
 *  【为什么不数引号个数】`"foo "bar` 有两个引号，但引号在词的中间闭合，
 *  词仍然是合法的（引擎把它读成一个词 "foo bar"）——
 *  只有「读到输入结束还没闭合」才会吞掉后面的东西。 */
export function hasUnclosedQuote(query: string): boolean {
  let inQuote = false;
  for (let i = 0; i < query.length; i++) {
    const c = query[i];
    if (c === "\\" && !inQuote) {
      i++; // 引号外的反斜杠：下一个字符被转义，跳过
      continue;
    }
    if (c === '"') inQuote = !inQuote;
  }
  return inQuote;
}

/** 把一个关键字【追加】到现有查询后面。
 *
 *  【为什么是追加不是替换】用户点语法面板里的关键字，
 *  是在自己已经敲好的条件上【再加一条】，不是要丢掉原来那部分。
 *
 *  【分隔符用一个空格】引擎把空白切开的两个词当隐式 AND
 *  （上游 src/fsearch_query_parser.c 的 get_implicit_and_if_necessary），
 *  所以空格就是「两个条件同时生效」的写法 —— 不用显式再写个 AND。
 *  查询末尾已经有空格就不再补一个。
 *
 *  【未闭合引号 → 拒绝追加，返回 null】引擎的词法器会把未闭合引号
 *  之后的全部吞成一个字面词（见 hasUnclosedQuote），
 *  追加的字段会被吞进引号里、过滤完全失效，而且没有任何提示。
 *  宁可什么都不做 —— 追加绝不能产出一个静默错答的查询。
 *
 *  用户原有的内容一个字都不动（不 trim、不改大小写）。 */
export function appendKeyword(query: string, keyword: string): string | null {
  const kw = keyword.trim();
  if (!kw) return query;
  if (hasUnclosedQuote(query)) return null;
  if (!query.trim()) return kw;
  const sep = /\s$/.test(query) ? "" : " ";
  return `${query}${sep}${kw}`;
}