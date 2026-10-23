import raw from "./syntax-help.json";
import type { SyntaxTables } from "./types";

/** 一行 = 一个单元格的纯文本数组。
 *  列序按类别不同（见生成脚本的注释），所以这里不拆成具名字段。 */
export type HelpRow = string[];

export interface SyntaxHelp {
  functions: HelpRow[];
  modifiers: HelpRow[];
  operators: HelpRow[];
  wildcards: HelpRow[];
  /** 上游 help/C/search_syntax_escaping.page —— 引号 / 反斜杠的转义。 */
  escaping: HelpRow[];
}

export const syntaxHelp: SyntaxHelp = raw as SyntaxHelp;

/** 引擎的哪一张语法表。 */
export type SyntaxTableName = keyof SyntaxTables;

export interface HelpGroup {
  key: keyof SyntaxHelp;
  title: string;
  columns: string[];
  /** 关键字在第几列（0 起）。
   *  运算符那一组的第 0 列是【名字】（"AND" / "Grouping"），
   *  真正的 token 在第 1 列（"space, AND, &&"）—— 名字不在引擎的运算符表里。 */
  keywordColumn: number;
  /** 跟引擎的哪张表比；null = 不比。
   *  通配符不是 token —— 引擎是在【搜索词】里把 * / ? 转成正则
   *  （上游 src/fsearch_query_node.c 的 fsearch_query_node_new_wildcard），
   *  所以它不归任何一张表管，一律算支持。 */
  table: SyntaxTableName | null;
  /** 伪 token —— 【写法的说明词】，不是可插入的字面文本。
   *
   *  运算符组的 "space, AND, &&" 里的 "space" 指「空格 = 隐式 AND」，
   *  插进搜索框会变成搜「文件名里含 space 的文件」—— 静默错答。
   *
   *  它同时被两条路径用到，规则是同一套：
   *    - isRowSupported：伪 token 算【支持】（它描述的写法引擎确实支持）
   *    - clickToken：伪 token【不插进搜索框】（跳过它） */
  pseudoTokens?: string[];
  /** 关键字那一列能不能点（点了把关键字填进搜索框）。
   *
   *  默认 true。转义组（escaping）是 false —— 那一列写的是
   *  "Double quotes \"\"" 这种【说明词】，不是能插进搜索框的字面文本：
   *  插进去会变成搜「文件名里含 Double quotes 的文件」，静默错答。
   *  通配符组是 true —— "*" / "?" 本来就是能直接插的字面文本。 */
  clickable?: boolean;
}

/** 面板的分组与列标题 —— 与上游各页面的表头一致。 */
export const helpGroups: HelpGroup[] = [
  {
    key: "functions",
    title: "函数字段",
    columns: ["关键字", "说明", "示例"],
    keywordColumn: 0,
    table: "fields",
  },
  {
    key: "modifiers",
    title: "修饰符",
    columns: ["关键字", "说明", "示例"],
    keywordColumn: 0,
    table: "modifiers",
  },
  {
    key: "operators",
    title: "运算符",
    columns: ["运算符", "写法", "说明"],
    keywordColumn: 1,
    table: "operators",
    // "space" 是「空格 = 隐式 AND」这句说明的缩写 ——
    // 上游词法器按空白切词（src/fsearch_query_parser.c 的
    // get_implicit_and_if_necessary），所以这个写法引擎确实支持；
    // 但它是说明词，不能当字面文本插进搜索框。
    pseudoTokens: ["space"],
  },
  {
    key: "wildcards",
    title: "通配符",
    columns: ["模式", "说明", "示例"],
    keywordColumn: 0,
    table: null,
  },
  {
    key: "escaping",
    title: "转义",
    // 上游 search_syntax_escaping.page 的列序是
    // Sequence | Description | Example —— 第 0 列是【写法】。
    columns: ["写法", "说明", "示例"],
    keywordColumn: 0,
    // 引号 / 反斜杠是词法器处理的（src/fsearch_query_lexer.c 的
    // parse_quoted_string 与 fsearch_query_lexer_get_next_token
    // 里的 '\' 分支），不在任何一张「名字表」里 —— 不跟任何表比。
    table: null,
    // 第 0 列是说明词（`Double quotes ""` / `Backslash \`），
    // 不是能插进搜索框的字面文本 —— 不可点。
    clickable: false,
  },
];

/** 从一个关键字单元格里抽出引擎认的名字。 */
export interface HelpKeyword {
  /** 引擎认的名字（不带冒号） */
  token: string;
  /** 后面要不要跟冒号 —— 字段/修饰符要，运算符/通配符不要。 */
  colon: boolean;
}

/** 把一个关键字单元格拆成名字列表。
 *
 *  文档里的写法有三种，按逗号切开后每段都是「名字[:值占位符]」：
 *    "ext:<semicolon-seperated-list>" → [{ ext, colon:true }]
 *    "datemodified:<date>, dm:<date>"  → [{ datemodified }, { dm }]
 *    "file:, files:"                   → [{ file }, { files }]
 *    "space, AND, &&"                  → [{ space }, { AND }, { && }]
 *  运算符那一组没有冒号，所以 colon 全是 false。 */
export function keywordTokens(cell: string): HelpKeyword[] {
  const out: HelpKeyword[] = [];
  for (const raw of cell.split(",")) {
    const part = raw.trim();
    if (!part) continue;
    const i = part.indexOf(":");
    if (i === -1) {
      out.push({ token: part, colon: false });
      continue;
    }
    const token = part.slice(0, i).trim();
    if (token) out.push({ token, colon: true });
  }
  return out;
}

/** 引擎三张表的查找集合（名字一律小写，方便不分大小写地比）。 */
export type SyntaxLookup = Record<SyntaxTableName, Set<string>>;

export function buildLookup(tables: SyntaxTables): SyntaxLookup {
  const lower = (xs: string[]) => new Set(xs.map((x) => x.toLowerCase()));
  return {
    modifiers: lower(tables.modifiers),
    fields: lower(tables.fields),
    operators: lower(tables.operators),
  };
}

/** 面板正文顶部那句提示。
 *
 *  【为什么不能在 fail-open 时还说「灰掉的项表示当前引擎不支持」】
 *  拿不到引擎语法表时（还没加载 / 加载失败）面板是 fail-open 的 ——
 *  **一项都不会灰**。那句话此刻在骗人：用户看不到任何灰项，
 *  却在读一句「灰掉的项表示……」。所以换成「还没加载，暂时无法标注」。 */
export function supportNote(lookup: SyntaxLookup | null): string {
  return lookup
    ? "灰掉的项表示当前引擎不支持。"
    : "引擎语法表还没加载，暂时无法标注哪些项当前引擎不支持。";
}

/** 这一行的关键字，引擎认不认。
 *
 *  【为什么要按组分表】上游文档的各组关键字落在引擎的不同表里：
 *  函数 → 字段表，修饰符 → 修饰符表，运算符 → 运算符表，通配符 → 哪张都不是。
 *  合成一张表来比会串味（"space" 在修饰符表里查不到、"AND" 在字段表里也查不到）。 */
export function isRowSupported(
  group: HelpGroup,
  row: HelpRow,
  lookup: SyntaxLookup | null,
): boolean {
  // 表还没拿到（IPC 失败 / 还没回来）时【一律算支持】——
  // 宁可先不灰，也别在加载途中把整张表灰掉。
  if (!lookup) return true;
  const kws = keywordTokens(row[group.keywordColumn] ?? "");
  if (!kws.length) return true;
  if (!group.table) return true;
  const set = lookup[group.table];
  // 伪 token（"space"）不在表里，但它描述的写法引擎确实支持 —— 算支持。
  const pseudo = group.pseudoTokens ?? [];
  return kws.every(
    (k) => set.has(k.token.toLowerCase()) || pseudo.includes(k.token.toLowerCase()),
  );
}

/** 点一下往搜索框里填什么。
 *
 *  【为什么不是直接把单元格原文塞进去】文档里写的是
 *  "ext:<semicolon-seperated-list>" —— 塞进搜索框会变成一次搜
 *  「字面量 <semicolon-seperated-list>」的搜索，一次都搜不到。
 *  真正能用的是关键字本身（ext: / dm: / AND …）。
 *
 *  【伪 token 一律跳过】"space, AND, &&" 里的 "space" 是「空格 = 隐式 AND」
 *  这句说明的缩写 —— 插进搜索框会变成搜「文件名里含 space 的文件」，
 *  静默错答。所以它在【引擎表还没拿到】时也不能当兜底值。 */
export function clickToken(
  group: HelpGroup,
  row: HelpRow,
  lookup: SyntaxLookup | null,
): string {
  /* 整组不可点（转义组）—— 那一列写的是说明词
   * （`Double quotes ""`），不是能插进搜索框的字面文本。
   * 插进去会变成搜「文件名里含 Double quotes 的文件」，静默错答。 */
  if (group.clickable === false) return "";
  const kws = keywordTokens(row[group.keywordColumn] ?? "");
  if (!kws.length) return "";
  const pseudo = group.pseudoTokens ?? [];
  const usable = kws.filter((k) => !pseudo.includes(k.token.toLowerCase()));
  if (!usable.length) return "";
  let chosen = usable[0];
  if (lookup && group.table) {
    const set = lookup[group.table];
    // 优先选引擎认识的那个 —— "space, AND, &&" 里的 "AND" / "&&" 才是 token
    const hit = usable.find((k) => set.has(k.token.toLowerCase()));
    if (hit) chosen = hit;
  }
  return chosen.token + (chosen.colon ? ":" : "");
}
