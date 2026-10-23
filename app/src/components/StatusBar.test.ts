// @vitest-environment happy-dom
/**
 * 状态栏的索引错误提示 —— 回归测试。
 *
 *  【这个测试在修什么】`index_status.error` 以前有值但界面上一个字都不提 ——
 *  用户只看到「已索引 0 项」，完全不知道为什么没有结果
 *  （Phase 3 fix round 1 加的 `.sb-error`，本轮补上单测）。
 */

import { createApp, h } from "vue";
import { describe, expect, it } from "vitest";

import type { IndexStatus, SearchResult } from "../lib/types";
import StatusBar from "./StatusBar.vue";

function mountStatusBar(status: IndexStatus | null, result: SearchResult | null = null) {
  const host = document.createElement("div");
  document.body.appendChild(host);
  const app = createApp({
    render: () => h(StatusBar, { status, result, loading: false }),
  });
  app.mount(host);
  return { app, host };
}

describe("StatusBar 的索引错误提示", () => {
  it("索引出错时要把原因显示出来", () => {
    const { app, host } = mountStatusBar({
      scanning: false,
      indexed: 0,
      paths: [],
      error: "invalid argument",
    });
    const err = host.querySelector(".sb-error");
    expect(err).not.toBeNull();
    expect(err?.textContent).toContain("invalid argument");
    // 已索引的条目数也要还在（不能因为有错就什么都不显示）
    expect(host.textContent).toContain("已索引 0 项");
    app.unmount();
  });

  it("没有错误时不该出现错误提示", () => {
    const { app, host } = mountStatusBar({
      scanning: false,
      indexed: 42,
      paths: ["/tmp"],
      error: null,
    });
    expect(host.querySelector(".sb-error")).toBeNull();
    expect(host.textContent).toContain("已索引 42 项");
    app.unmount();
  });

  it("正在扫描时也显示得出扫描状态", () => {
    const { app, host } = mountStatusBar({
      scanning: true,
      indexed: 0,
      paths: [],
      error: null,
    });
    expect(host.textContent).toContain("正在扫描索引");
    app.unmount();
  });
});
