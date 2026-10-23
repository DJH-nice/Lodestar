import type { SyntaxTables } from "./types";

export type TokenKind =
  | "plain"
  | "modifier"
  | "field"
  | "operator"
  | "string"
  | "path"
  | "not";

export interface Token {
  text: string;
  kind: TokenKind;
}

/** 把查询串切成带语义的 token —— 只用于【着色】，不参与匹配。 */
export function tokenize(query: string, tables: SyntaxTables): Token[] {
  const modifiers = new Set(tables.modifiers);
  const fields = new Set(tables.fields);
  const operators = new Set(tables.operators);
  const tokens: Token[] = [];
  let i = 0;

  const push = (text: string, kind: TokenKind) => {
    if (!text) return;
    const last = tokens[tokens.length - 1];
    if (last && last.kind === kind) last.text += text;
    else tokens.push({ text, kind });
  };

  while (i < query.length) {
    const c = query[i];

    // 引号短语
    if (c === '"') {
      const end = query.indexOf('"', i + 1);
      const stop = end === -1 ? query.length : end + 1;
      push(query.slice(i, stop), "string");
      i = stop;
      continue;
    }

    // 标识符 / 关键字 / 路径
    //
    // 【上游没有 "/.../" 正则语法】FSearch 的正则靠 `regex:` 修饰符开启
    // （src/fsearch_query_parser.c:106），lexer 的字符分派里也没有 '/' 这一条
    // （src/fsearch_query_lexer.c:79-112 只认 = : < > ! ( ) 与空白）。
    // 所以 "/usr/local" 里的斜杠【不是】正则定界符 ——
    // 早期版本把 "/" 到下一个 "/" 当正则，会把普通路径染成正则色。
    // 路径整词标 "path" 即可。
    if (/[A-Za-z_/]/.test(c)) {
      let j = i;
      while (j < query.length && /[A-Za-z0-9_\-./]/.test(query[j])) j++;
      const word = query.slice(i, j);
      let k = j;
      while (k < query.length && /\s/.test(query[k])) k++;
      const next = query[k];

      if (next === ":") {
        const lower = word.toLowerCase();
        if (modifiers.has(lower)) push(word, "modifier");
        else if (fields.has(lower)) push(word, "field");
        else push(word, "plain"); // 未知字段 —— 交给 shim 的 warning 提示
        push(":", "plain");
        i = k + 1;
        continue;
      }

      // 含斜杠的整词 = 路径
      if (word.includes("/")) {
        push(word, "path");
        i = j;
        continue;
      }

      const upper = word.toUpperCase();
      if (operators.has(upper) || operators.has(word)) {
        push(word, upper === "NOT" ? "not" : "operator");
      } else {
        push(word, "plain");
      }
      i = j;
      continue;
    }

    // 取反符 '!'
    //
    // 上游 lexer 把 '!' 一律当 FSEARCH_QUERY_TOKEN_NOT
    // （src/fsearch_query_lexer.c:106），不管后面跟的是不是字母 ——
    // 所以 "!foo" 的 '!' 就是取反，不能因为后面是字母就降级成普通字符。
    if (c === "!") {
      push("!", "not");
      i++;
      continue;
    }

    // 单字符运算符：= < > ( ) 等
    // （':' 走字段分支消费掉了，走不到这里）
    if (operators.has(c)) {
      push(c, "operator");
      i++;
      continue;
    }

    push(c, "plain");
    i++;
  }

  return tokens;
}
