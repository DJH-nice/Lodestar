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

/**
 * 查询的警告（`result.warning`）—— 这条链是「还没写完的查询不报错」的出口。
 *
 *  【为什么要有这个测试】引擎解析「还没写完」的查询时，不再报错，
 *  而是返回「已解析的部分 + 一句中文提示」——
 *  如果状态栏不显示这个提示，用户就会看到一份**看起来正常**的结果，
 *  完全不知道自己少写了一半（静默错答比报错更糟）。
 */
describe("StatusBar 的查询警告", () => {
  function resultWithWarning(warning: string): SearchResult {
    return {
      entries: [],
      total_entries: 3,
      total_files: 3,
      total_folders: 0,
      complete: true,
      elapsed_ms: 1,
      warning,
      group_boundary: null,
    };
  }

  it("有警告时把警告显示出来", () => {
    const { app, host } = mountStatusBar(
      null,
      resultWithWarning("AND 后面还没有条件（还没写完）"),
    );
    expect(host.textContent).toContain("AND 后面还没有条件");
    app.unmount();
  });

  it("没有警告时显示命中数", () => {
    const { app, host } = mountStatusBar(null, resultWithWarning(""));
    expect(host.textContent).toContain("3 项");
    app.unmount();
  });
});

/**
 * 外接卷的状态 —— 状态栏必须让用户看见「盘上的文件进索引了没有」。
 *
 *  【为什么要有这个测试】插上 U 盘后的首扫可能要几十秒，
 *  界面上一个字都不显示的话，用户会以为功能坏了（而且是**静默**地坏）。
 */
describe("StatusBar 的外接卷状态", () => {
  it("状态栏会显示外接卷的状态", () => {
    const { app, host } = mountStatusBar({
      scanning: false,
      indexed: 120,
      paths: ["/Users/me"],
      error: null,
      visible: true,
      volumes: [
        { path: "/Volumes/U盘", label: "U盘", phase: { state: "Ready" }, indexed: 42 },
        {
          path: "/Volumes/移动硬盘",
          label: "移动硬盘",
          phase: { state: "Indexing" },
          indexed: 0,
        },
      ],
    });
    expect(host.textContent).toContain("U盘");
    expect(host.textContent).toContain("42");
    expect(host.textContent).toContain("正在索引");
    app.unmount();
  });

  it("没有插盘时不显示卷那一行", () => {
    const { app, host } = mountStatusBar({
      scanning: false,
      indexed: 120,
      paths: ["/Users/me"],
      error: null,
    });
    expect(host.querySelector(".sb-volume")).toBeNull();
    app.unmount();
  });

  it("索引失败的卷要写明「索引失败」（不能只显示个卷名）", () => {
    const { app, host } = mountStatusBar({
      scanning: false,
      indexed: 120,
      paths: ["/Users/me"],
      error: null,
      volumes: [
        {
          path: "/Volumes/坏盘",
          label: "坏盘",
          phase: { state: "Failed", detail: "读不出来" },
          indexed: 0,
        },
      ],
    });
    expect(host.textContent).toContain("坏盘");
    expect(host.textContent).toContain("索引失败");
    app.unmount();
  });

  it("索引失败的卷要把**原因**显示出来（不能只写「索引失败」）", () => {
    // 【为什么】Rust 侧已经把原因传过来了（`detail`）→ 以前前端把它扔了 →
    //   用户只看到「坏盘 · 索引失败」，完全不知道为什么。
    //   这与项目自己的约定相悖（`.sb-error` 那条注释写着
    //   「索引出错的原因必须显示出来」）。
    const { app, host } = mountStatusBar({
      scanning: false,
      indexed: 120,
      paths: ["/Users/me"],
      error: null,
      volumes: [
        {
          path: "/Volumes/坏盘",
          label: "坏盘",
          phase: { state: "Failed", detail: "读不出来" },
          indexed: 0,
        },
      ],
    });
    expect(host.textContent).toContain("读不出来");
    app.unmount();
  });

  it("索引失败的卷悬停时也要看到原因（不只是路径）", () => {
    const { app, host } = mountStatusBar({
      scanning: false,
      indexed: 120,
      paths: ["/Users/me"],
      error: null,
      volumes: [
        {
          path: "/Volumes/坏盘",
          label: "坏盘",
          phase: { state: "Failed", detail: "读不出来" },
          indexed: 0,
        },
      ],
    });
    const el = host.querySelector(".sb-volume");
    const title = el?.getAttribute("title") ?? "";
    expect(title).toContain("读不出来");
    app.unmount();
  });

  it("就绪的卷显示的是**这个卷自己**的条目数", () => {
    // `volumes[].indexed` 的语义 = **这个卷自己**的条目数
    // （不是整个索引的总数 —— 那个数在 `status.indexed` 里）。
    const { app, host } = mountStatusBar({
      scanning: false,
      indexed: 8_150,
      paths: ["/Users/me"],
      error: null,
      volumes: [
        {
          path: "/Volumes/U盘",
          label: "U盘",
          phase: { state: "Ready" },
          indexed: 150,
        },
      ],
    });
    expect(host.textContent).toContain("U盘 · 150 项");
    expect(host.textContent).not.toContain("8,150 项");
    app.unmount();
  });
});
