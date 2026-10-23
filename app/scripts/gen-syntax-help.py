#!/usr/bin/env python3
"""从上游的 Mallard 帮助文档生成语法说明 JSON。

上游文档在 help/C/search_syntax_*.page（Mallard XML）。每个 <tr> 是一行，
若干 <td> 是列。**表头一律用 <th>**（实测 4 个页面都是），所以按 <td> 提取
天然跳过表头。

本脚本只做【通用提取】：把每行的每个单元格剥成纯文本。
**不理解语义** —— 各页面的列序并不一致：
  - functions / modifiers / wildcards：Keyword | Description | Example
  - operators：                 Operator | Keyword | Description
  - escaping：                  Sequence | Description | Example
    （上游这一页的表头拼成了 "Sequence / Descritpion / Example"，
      但列的**顺序**跟 functions 一样：写法在前、说明在中间、示例在最后。）
所以 JSON 里保留原始的单元格数组，列的含义由前端按类别渲染。

用法（从仓库根跑）：
    python3 app/scripts/gen-syntax-help.py

产物：
    app/src/lib/syntax-help.json
"""

import html
import json
import pathlib
import re

REPO = pathlib.Path(__file__).resolve().parents[2]
HELP_DIR = REPO / "help" / "C"
OUT = REPO / "app" / "src" / "lib" / "syntax-help.json"

PAGES = {
    "functions": "search_syntax_functions.page",
    "modifiers": "search_syntax_modifiers.page",
    "operators": "search_syntax_operators.page",
    "wildcards": "search_syntax_wildcards.page",
    # 转义（双引号 / 反斜杠）—— 之前漏了这一页，是简报的覆盖缺口。
    "escaping": "search_syntax_escaping.page",
}

# 行内标签：标签去掉、里面的文字保留
INLINE = r"</?(?:p|var|input|output|code|em|strong|span)\b[^>]*>"
ANY_TAG = re.compile(r"<[^>]+>")


def cell_text(fragment: str) -> str:
    """把一个单元格的 HTML 剥成纯文本。"""
    s = re.sub(INLINE, "", fragment)
    s = ANY_TAG.sub("", s)
    s = html.unescape(s)
    return re.sub(r"\s+", " ", s).strip()


def rows_of(path: pathlib.Path) -> list:
    """提取一个页面的所有数据行（表头是 <th>，不会被 <td> 正则匹配到）。"""
    source = path.read_text(encoding="utf-8")
    out = []
    for tr in re.findall(r"<tr>(.*?)</tr>", source, re.S):
        cells = [
            cell_text(td) for td in re.findall(r"<td[^>]*>(.*?)</td>", tr, re.S)
        ]
        if cells and any(cells):
            out.append(cells)
    return out


def main() -> None:
    data = {}
    for key, filename in PAGES.items():
        page = HELP_DIR / filename
        if not page.exists():
            raise SystemExit(f"找不到上游文档: {page}")
        data[key] = rows_of(page)

    OUT.parent.mkdir(parents=True, exist_ok=True)
    OUT.write_text(
        json.dumps(data, ensure_ascii=False, indent=2) + "\n", encoding="utf-8"
    )
    total = sum(len(v) for v in data.values())
    print(f"已生成 {OUT}")
    for key, rows in data.items():
        print(f"  {key:10s} {len(rows):3d} 行")
    print(f"  合计 {total} 行")


if __name__ == "__main__":
    main()
