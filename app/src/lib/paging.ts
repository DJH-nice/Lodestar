import type { ListKind, SearchResult, SortOrder } from "./types";

export const ROW_H = 28;
export const OVERSCAN = 8;
/** 表头高度。只用于【样式对齐】（表头在滚动容器之外，不占行坐标）。 */
export const HEADER_H = 27;

/** 根据滚动位置算出「需要加载哪些页」。
 *
 * 关键：不能只看「已加载了多少条」—— 用户可以把滚动条一把拖到 50% 处，
 * 需要的页远在已加载区之外。所以按【视口第一行】反推页号。
 *
 * 行号要【夹到有效范围】再算页号：否则滚过头时 firstPage 会算出
 * 一个远大于 totalPages 的页号，循环条件 `p < totalPages` 直接不成立，
 * 结果一页都不请求（滚到底反而空白）。
 *
 * 【坐标约定】表头在滚动容器【之外】（`.result-wrap` 的独立一行），
 * 所以滚动容器的 scrollTop=0 就是第 0 行的顶部 —— 行号与 scrollTop
 * 之间【没有】表头高度的偏移。早期版本把表头算进去，列表顶部会多出
 * 27px 空隙（行整体下移一格）。
 */
export function neededPages(
  scrollTop: number,
  viewportH: number,
  totalEntries: number,
  pageSize: number,
  loadedPages: Set<number>,
): number[] {
  if (totalEntries <= 0 || pageSize <= 0) return [];
  const totalPages = Math.ceil(totalEntries / pageSize);
  const lastRow = totalEntries - 1;

  const firstRow = Math.min(lastRow, Math.max(0, Math.floor(scrollTop / ROW_H)));
  const endRow = Math.min(
    lastRow,
    Math.max(0, Math.ceil((scrollTop + viewportH) / ROW_H)),
  );

  const firstPage = Math.floor(firstRow / pageSize);
  const lastPage = Math.min(totalPages - 1, Math.floor(endRow / pageSize));

  const out: number[] = [];
  for (let p = firstPage; p <= lastPage; p++) {
    if (!loadedPages.has(p)) out.push(p);
  }
  return out;
}

/** 分组分隔行的【全局】行号（插在这一行之前）。
 *
 * 引擎的扁平索引空间是「先目录后文件」：
 *   升序 → 目录段在前 → 分隔行在第 total_folders 行
 *   降序 → 整段倒过来 → 文件段在前 → 分隔行在第 total_files 行
 * 与 shim 头文件里的页内公式一致（clamp(total_folders - p·size, ...)）。
 *
 * 用【总数】直接算全局位置，而不是等那一页加载完才知道 ——
 * 否则滚动条总高度会随加载跳动。
 *
 * 返回 null = 不需要分隔行（不是「全部」列表，或分界正好在两端）。
 */
export function separatorRow(
  result: SearchResult | null,
  kind: ListKind,
  order: SortOrder,
): number | null {
  if (!result || kind !== "all") return null;
  const total = result.total_entries;
  const at = order === "asc" ? result.total_folders : result.total_files;
  if (at <= 0 || at >= total) return null;
  return at;
}
