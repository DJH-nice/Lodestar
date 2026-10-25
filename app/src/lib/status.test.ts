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

  /* ---- 外接卷 ----
     【为什么 volumes 也要比】
       卷的状态变化（插上 → 正在索引 → 扫完 → 就绪）**不会**让
       `scanning` / `indexed` / `paths` 变 → 只比那几个字段的话，
       状态栏会停在「没有卷」上（用户插了盘却看不见任何提示）。 */

  it("volumes 变了（阶段 / 条数 / 卷增减）→ 判「不同」", () => {
    const ready = st({
      volumes: [
        { path: "/Volumes/U盘", label: "U盘", phase: { state: "Ready" }, indexed: 42 },
      ],
    });
    const indexing = st({
      volumes: [
        { path: "/Volumes/U盘", label: "U盘", phase: { state: "Indexing" }, indexed: 0 },
      ],
    });
    const otherCount = st({
      volumes: [
        { path: "/Volumes/U盘", label: "U盘", phase: { state: "Ready" }, indexed: 43 },
      ],
    });
    const more = st({
      volumes: [
        { path: "/Volumes/U盘", label: "U盘", phase: { state: "Ready" }, indexed: 42 },
        { path: "/Volumes/硬盘", label: "硬盘", phase: { state: "Ready" }, indexed: 7 },
      ],
    });
    expect(sameIndexStatus(ready, indexing)).toBe(false);
    expect(sameIndexStatus(ready, otherCount)).toBe(false);
    expect(sameIndexStatus(ready, more)).toBe(false);
  });

  it("失败原因变了 → 判「不同」", () => {
    const a = st({
      volumes: [
        { path: "/Volumes/坏盘", label: "坏盘", phase: { state: "Failed", detail: "读不出来" }, indexed: 0 },
      ],
    });
    const b = st({
      volumes: [
        { path: "/Volumes/坏盘", label: "坏盘", phase: { state: "Failed", detail: "权限不足" }, indexed: 0 },
      ],
    });
    expect(sameIndexStatus(a, b)).toBe(false);
  });

  it("volumes 内容一样 → 判「相同」（不能白重渲染）", () => {
    const a = st({
      volumes: [
        { path: "/Volumes/U盘", label: "U盘", phase: { state: "Ready" }, indexed: 42 },
      ],
    });
    const b = st({
      volumes: [
        { path: "/Volumes/U盘", label: "U盘", phase: { state: "Ready" }, indexed: 42 },
      ],
    });
    expect(sameIndexStatus(a, b)).toBe(true);
  });

  it("volumes 缺失时按「没有卷」算 —— 两边都缺 → 判「相同」", () => {
    const a = st();
    const b = st();
    delete (a as { volumes?: unknown }).volumes;
    delete (b as { volumes?: unknown }).volumes;
    expect(sameIndexStatus(a, b)).toBe(true);
  });

  it("一个缺 volumes、一个有 volumes → 判「不同」", () => {
    const a = st();
    delete (a as { volumes?: unknown }).volumes;
    expect(
      sameIndexStatus(
        a,
        st({
          volumes: [
            { path: "/Volumes/U盘", label: "U盘", phase: { state: "Ready" }, indexed: 42 },
          ],
        }),
      ),
    ).toBe(false);
  });
});
