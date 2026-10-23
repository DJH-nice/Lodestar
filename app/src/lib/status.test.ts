/**
 * `sameIndexStatus` 的单测 —— 「这两份索引状态是不是同一份」。
 *
 *  【为什么这个函数存在】`status.value = s` 每 800ms 换上一个**新对象**，
 *   即便内容一模一样，Vue 的响应式也会让依赖 `status` 的组件**重渲染**
 *   → 状态栏重绘 → WebKit 重新合成。
 *   采样实测（窗口可见时）：光这一项就占掉可见态 CPU 的绝大部分
 *   （栈里能看到 `RemoteLayerTreeDrawingArea::updateRendering` 被反复调用）。
 *
 *  【这些用例在钉什么】
 *   - 内容一致 → 必须判「相同」（否则优化白做）
 *   - 任何一个字段变了 → 必须判「不同」（否则状态栏会停在旧值上）
 *   - `visible` 缺失时的兜底 → 两边必须用**同一套**规则，
 *     否则「一个缺字段、一个有字段」会误判成不同 → 白白重渲染。
 */

import { describe, expect, it } from "vitest";

import { sameIndexStatus } from "./status";
import type { IndexStatus } from "./types";

function st(over: Partial<IndexStatus> = {}): IndexStatus {
  return {
    scanning: false,
    indexed: 100,
    paths: ["/tmp"],
    error: null,
    visible: true,
    ...over,
  };
}

describe("sameIndexStatus", () => {
  it("内容完全一样 → 判「相同」", () => {
    expect(sameIndexStatus(st(), st())).toBe(true);
  });

  it("第一份还没有（null）→ 判「不同」（必须更新一次）", () => {
    expect(sameIndexStatus(null, st())).toBe(false);
  });

  it("scanning 变了 → 判「不同」", () => {
    // 【这条最关键】扫描结束的那一刻必须能触发更新 ——
    // 否则状态栏会永远停在「正在扫描」。
    expect(sameIndexStatus(st({ scanning: true }), st({ scanning: false }))).toBe(
      false,
    );
  });

  it("indexed 变了 → 判「不同」", () => {
    expect(sameIndexStatus(st({ indexed: 100 }), st({ indexed: 101 }))).toBe(false);
  });

  it("error 变了 → 判「不同」", () => {
    expect(sameIndexStatus(st({ error: null }), st({ error: "boom" }))).toBe(false);
  });

  it("visible 变了 → 判「不同」", () => {
    expect(sameIndexStatus(st({ visible: true }), st({ visible: false }))).toBe(false);
  });

  it("paths 变了（内容或顺序）→ 判「不同」", () => {
    expect(sameIndexStatus(st({ paths: ["/a"] }), st({ paths: ["/b"] }))).toBe(false);
    expect(sameIndexStatus(st({ paths: ["/a", "/b"] }), st({ paths: ["/b", "/a"] }))).toBe(
      false,
    );
    expect(sameIndexStatus(st({ paths: ["/a"] }), st({ paths: ["/a", "/b"] }))).toBe(
      false,
    );
  });

  it("visible 缺失时按「可见」算 —— 两边都缺 → 判「相同」", () => {
    const a = st();
    const b = st();
    delete (a as { visible?: boolean }).visible;
    delete (b as { visible?: boolean }).visible;
    expect(sameIndexStatus(a, b)).toBe(true);
  });

  it("一个缺 visible、一个有 visible=true → 判「相同」（兜底规则要一致）", () => {
    const a = st();
    delete (a as { visible?: boolean }).visible;
    expect(sameIndexStatus(a, st({ visible: true }))).toBe(true);
  });

  it("一个缺 visible、一个有 visible=false → 判「不同」", () => {
    const a = st();
    delete (a as { visible?: boolean }).visible;
    expect(sameIndexStatus(a, st({ visible: false }))).toBe(false);
  });
});
