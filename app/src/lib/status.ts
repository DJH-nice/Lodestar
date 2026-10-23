/**
 * 索引状态的**比较** —— 「这两份状态是不是同一份」。
 *
 *  【为什么需要它】
 *   `status.value = s` 每 800ms 换上一个**新对象** —— 即便内容一模一样，
 *   Vue 的响应式也会让所有依赖 `status` 的组件**重渲染** → 状态栏重绘
 *   → WebKit 重新合成。
 *   采样实测（窗口可见时）：光这一项就占掉可见态 CPU 的绝大部分 ——
 *   栈里能直接看到 `RemoteLayerTreeDrawingArea::updateRendering` 被反复调用。
 *
 *   → 只有内容**真的变了**才该替换 `status.value`。
 *
 *  【比较的字段】就是 `IndexStatus` 的全部字段。
 *   `visible` 用 `?? true` 兜底 —— 字段缺失时按「可见」算；
 *   两边用**同一套**兜底规则，缺一个字段不会让两边算出「不相等」。
 *
 *  【会不会漏掉该更新的时刻】不会：
 *   「扫描结束」那个翻转检测用的是**新来的那份**的 `scanning`，
 *   而 `scanning` 一变，这个比较必然判成「不相等」→ `status.value` 照样更新。
 */

import type { IndexStatus } from "./types";

/** 两份索引状态是不是同一份（内容完全一致）。 */
export function sameIndexStatus(a: IndexStatus | null, b: IndexStatus): boolean {
  if (!a) return false;
  return (
    a.scanning === b.scanning &&
    a.indexed === b.indexed &&
    a.error === b.error &&
    (a.visible ?? true) === (b.visible ?? true) &&
    a.paths.length === b.paths.length &&
    a.paths.every((p, i) => p === b.paths[i])
  );
}
