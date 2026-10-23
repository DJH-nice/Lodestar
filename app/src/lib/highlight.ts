import type { Highlight } from "./types";

export interface Segment {
  text: string;
  hit: boolean;
}

/** 把命中区间切成「普通段 / 高亮段」序列。
 *
 * 【关键】length 可能是 `G_MAXUINT - start`（≈4294967293），
 * 表示「一直高亮到字符串结尾」—— shim 头文件里明确写了这个行为。
 * 所以区间右端必须按【实际渲染的字符串长度】截断，不能直接用 length。
 */
export function segmentize(text: string, highlights: Highlight[]): Segment[] {
  if (!text) return [];
  if (!highlights.length) return [{ text, hit: false }];

  const ranges = highlights
    .map((h) => {
      const start = Math.min(h.start, text.length);
      // h.length 可能是 4e9 —— JS 里 number 不溢出，min 一下就夹住了
      const end = Math.min(start + h.length, text.length);
      return { start, end };
    })
    .filter((r) => r.end > r.start)
    .sort((a, b) => a.start - b.start);

  // 合并重叠区间
  const merged: Array<{ start: number; end: number }> = [];
  for (const r of ranges) {
    const last = merged[merged.length - 1];
    if (last && r.start <= last.end) last.end = Math.max(last.end, r.end);
    else merged.push({ start: r.start, end: r.end });
  }

  const out: Segment[] = [];
  let cur = 0;
  for (const r of merged) {
    if (r.start > cur) out.push({ text: text.slice(cur, r.start), hit: false });
    out.push({ text: text.slice(r.start, r.end), hit: true });
    cur = r.end;
  }
  if (cur < text.length) out.push({ text: text.slice(cur), hit: false });
  return out.length ? out : [{ text, hit: false }];
}
