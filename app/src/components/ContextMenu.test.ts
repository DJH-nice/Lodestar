// @vitest-environment happy-dom
/**
 * 右键菜单的测试。
 *
 *  【这个组件的契约】它只【显示】+【发意图】：
 *  - 收藏状态由 props（bookmarked）给 —— 组件自己不存，
 *    否则面板里删了书签、菜单还以为收藏着，再点一下又加回来；
 *  - 写入由 App.vue 做 —— 组件不碰设置文件；
 *  - 「收藏」点了【不关菜单】—— 写盘是异步的，
 *    成功/失败由 App.vue 回填（文案翻转 / 底部显示错误）。
 */

import { createApp, h, nextTick, reactive } from "vue";
import { afterAll, afterEach, beforeAll, describe, expect, it, vi } from "vitest";

import type { Entry } from "../lib/types";
import ContextMenu from "./ContextMenu.vue";

const FOLDER: Entry = {
  name: "docs",
  path: "/tmp/fsearch-demo/docs",
  dir: "/tmp/fsearch-demo",
  extension: "",
  size: 0,
  mtime: 1700000000,
  is_folder: true,
  highlights: [],
  path_highlights: [],
};

const FILE: Entry = {
  ...FOLDER,
  name: "notes.txt",
  path: "/tmp/fsearch-demo/notes.txt",
  extension: "txt",
  size: 12,
  is_folder: false,
};

interface MountOpts {
  entry?: Entry | null;
  bookmarked?: boolean;
  error?: string | null;
  x?: number;
  y?: number;
  onOpen?: (path: string) => void;
  onReveal?: (path: string) => void;
  onBookmark?: (path: string) => void;
  onPreview?: (entry: Entry, x: number, y: number) => void;
  onClose?: () => void;
}

function mountMenu(opts: MountOpts = {}) {
  const host = document.createElement("div");
  document.body.appendChild(host);
  const app = createApp({
    render: () =>
      h(ContextMenu, {
        entry: opts.entry === undefined ? FOLDER : opts.entry,
        x: opts.x ?? 100,
        y: opts.y ?? 100,
        bookmarked: opts.bookmarked ?? false,
        error: opts.error ?? null,
        onOpen: opts.onOpen,
        onReveal: opts.onReveal,
        onBookmark: opts.onBookmark,
        onPreview: opts.onPreview,
        onClose: opts.onClose,
      }),
  });
  app.mount(host);
  return { app, host };
}

/** 按文案找菜单项。 */
function item(host: HTMLElement, text: string): HTMLButtonElement {
  const btn = Array.from(host.querySelectorAll<HTMLButtonElement>(".ctx-item")).find(
    (b) => (b.textContent ?? "").trim() === text,
  );
  expect(btn, `菜单里应该有「${text}」这一项`).toBeTruthy();
  return btn!;
}

afterEach(() => {
  document.body.innerHTML = "";
});

/* ---- 贴边夹取：按【实测高度】而不是常数 ----
 *
 *  happy-dom 没有布局引擎 → offsetHeight 恒 0，量不到真实高度。
 *  这里给它造一个跟真机一致的假高度（复审在真机 DOM 里量到的数）：
 *  没有错误行 148px、有错误行 236px。
 *  不这么造，就只能验「常数夹取」—— 那正是要修的缺陷。 */
const REAL_H = 148;
const REAL_H_WITH_ERROR = 236;

const origOffsetHeight = Object.getOwnPropertyDescriptor(HTMLElement.prototype, "offsetHeight");

beforeAll(() => {
  Object.defineProperty(HTMLElement.prototype, "offsetHeight", {
    configurable: true,
    get(this: HTMLElement) {
      if (!this.classList.contains("ctx-menu")) return 0;
      return this.querySelector(".ctx-error") ? REAL_H_WITH_ERROR : REAL_H;
    },
  });
});

afterAll(() => {
  if (origOffsetHeight) {
    Object.defineProperty(HTMLElement.prototype, "offsetHeight", origOffsetHeight);
  } else {
    delete (HTMLElement.prototype as unknown as Record<string, unknown>).offsetHeight;
  }
});

/** 挂一个 error 可变的菜单（用来验「错误行后来才出现」）。 */
function mountReactiveMenu(y: number) {
  const host = document.createElement("div");
  document.body.appendChild(host);
  const state = reactive<{ error: string | null }>({ error: null });
  const app = createApp({
    render: () =>
      h(ContextMenu, {
        entry: FOLDER,
        x: 100,
        y,
        bookmarked: false,
        error: state.error,
      }),
  });
  app.mount(host);
  return { app, host, state };
}

describe("ContextMenu 的贴边夹取（用实测高度，不是常数）", () => {
  it("菜单在窗口底边 → 夹进去，不超出底边", async () => {
    const H = window.innerHeight;
    const { app, host } = mountMenu({ y: H - 10 });
    await nextTick();
    await nextTick();

    const menu = host.querySelector<HTMLElement>(".ctx-menu")!;
    const top = parseFloat(menu.style.top);
    expect(top + REAL_H, "菜单下沿不能超出窗口底边").toBeLessThanOrEqual(H - 8 + 0.001);
    app.unmount();
  });

  it("有错误行 → 按【更高的】实测高度夹取（按 180 那个常数会超出底边 48px）", async () => {
    const H = window.innerHeight;
    const { app, host } = mountMenu({ y: H - 10, error: "写盘失败：磁盘满了" });
    await nextTick();
    await nextTick();

    const menu = host.querySelector<HTMLElement>(".ctx-menu")!;
    const top = parseFloat(menu.style.top);
    // 按 236 夹 → top = H - 236 - 8；按常数 180 夹 → top = H - 180 - 8（下沿超出 48px）
    expect(top).toBeCloseTo(H - REAL_H_WITH_ERROR - 8, 1);
    expect(top + REAL_H_WITH_ERROR, "菜单下沿不能超出窗口底边").toBeLessThanOrEqual(
      H - 8 + 0.001,
    );
    app.unmount();
  });

  it("错误行后来才出现 → 要重量一次，位置跟着上移", async () => {
    const H = window.innerHeight;
    const { app, host, state } = mountReactiveMenu(H - 10);
    await nextTick();
    await nextTick();

    const menu = host.querySelector<HTMLElement>(".ctx-menu")!;
    const before = parseFloat(menu.style.top);

    state.error = "写盘失败：磁盘满了";
    await nextTick();
    await nextTick();

    const after = parseFloat(menu.style.top);
    expect(after, "错误行让菜单变高 → 位置必须上移").toBeLessThan(before);
    expect(after + REAL_H_WITH_ERROR).toBeLessThanOrEqual(H - 8 + 0.001);
    app.unmount();
  });
});

describe("ContextMenu 的菜单项", () => {
  it("打开 / 在访达中显示 / 收藏此目录 / 详情 —— 四项齐全", () => {
    const { app, host } = mountMenu();
    const texts = Array.from(host.querySelectorAll(".ctx-item")).map((b) =>
      (b.textContent ?? "").trim(),
    );
    expect(texts).toEqual(["打开", "在访达中显示", "收藏此目录", "详情"]);
    app.unmount();
  });

  it("还没收藏 → 文案是「收藏此目录」（目录）", () => {
    const { app, host } = mountMenu({ bookmarked: false });
    expect(host.textContent).toContain("收藏此目录");
    expect(host.textContent).not.toContain("取消收藏");
    app.unmount();
  });

  it("已经收藏 → 文案是「取消收藏」", () => {
    const { app, host } = mountMenu({ bookmarked: true });
    expect(host.textContent).toContain("取消收藏");
    expect(host.textContent).not.toContain("收藏此目录");
    app.unmount();
  });

  it("文件 → 文案是「收藏此文件」", () => {
    const { app, host } = mountMenu({ entry: FILE, bookmarked: false });
    expect(host.textContent).toContain("收藏此文件");
    expect(host.textContent).not.toContain("收藏此目录");
    app.unmount();
  });

  it("entry 为 null → 什么都不渲染", () => {
    const { app, host } = mountMenu({ entry: null });
    expect(host.querySelector(".ctx-menu")).toBeNull();
    app.unmount();
  });
});

describe("ContextMenu 的事件", () => {
  it("点「收藏」→ 发 bookmark 事件（参数是 entry 的路径），而且【菜单不关】", () => {
    const onBookmark = vi.fn();
    const onClose = vi.fn();
    const { app, host } = mountMenu({ entry: FILE, onBookmark, onClose });

    item(host, "收藏此文件").click();

    expect(onBookmark).toHaveBeenCalledTimes(1);
    expect(onBookmark).toHaveBeenCalledWith(FILE.path);
    /* 菜单不关 —— 用户能立刻反悔（再点一次变成「取消收藏」） */
    expect(onClose).not.toHaveBeenCalled();
    app.unmount();
  });

  it("点「打开」→ 发 open 事件【并且】关菜单", () => {
    const onOpen = vi.fn();
    const onClose = vi.fn();
    const { app, host } = mountMenu({ entry: FILE, onOpen, onClose });

    item(host, "打开").click();

    expect(onOpen).toHaveBeenCalledWith(FILE.path);
    expect(onClose).toHaveBeenCalledTimes(1);
    app.unmount();
  });

  it("点「在访达中显示」→ 发 reveal 事件【并且】关菜单", () => {
    const onReveal = vi.fn();
    const onClose = vi.fn();
    const { app, host } = mountMenu({ entry: FILE, onReveal, onClose });

    item(host, "在访达中显示").click();

    expect(onReveal).toHaveBeenCalledWith(FILE.path);
    expect(onClose).toHaveBeenCalledTimes(1);
    app.unmount();
  });

  it("点「详情」→ 发 preview 事件【并且】关菜单（预览卡的入口）", () => {
    const onPreview = vi.fn();
    const onClose = vi.fn();
    const { app, host } = mountMenu({ entry: FILE, x: 120, y: 140, onPreview, onClose });

    item(host, "详情").click();

    expect(onPreview).toHaveBeenCalledTimes(1);
    expect(onPreview).toHaveBeenCalledWith(FILE, 120, 140);
    expect(onClose).toHaveBeenCalledTimes(1);
    app.unmount();
  });
});

describe("ContextMenu 的关闭与错误显示", () => {
  it("写失败 → 菜单底部显示错误文字", () => {
    const { app, host } = mountMenu({ error: "写盘失败：磁盘满了" });
    const err = host.querySelector(".ctx-error");
    expect(err).not.toBeNull();
    expect(err?.textContent).toContain("写盘失败：磁盘满了");
    app.unmount();
  });

  it("没有错误 → 不显示错误行", () => {
    const { app, host } = mountMenu({ error: null });
    expect(host.querySelector(".ctx-error")).toBeNull();
    app.unmount();
  });

  it("点菜单外面 → 关闭", () => {
    const onClose = vi.fn();
    const { app } = mountMenu({ onClose });

    document.body.dispatchEvent(new PointerEvent("pointerdown", { bubbles: true }));

    expect(onClose).toHaveBeenCalledTimes(1);
    app.unmount();
  });

  it("点菜单里面 → 不关闭", () => {
    const onClose = vi.fn();
    const { app, host } = mountMenu({ onClose });

    item(host, "打开").dispatchEvent(new PointerEvent("pointerdown", { bubbles: true }));

    expect(onClose).not.toHaveBeenCalled();
    app.unmount();
  });

  it("按 Esc → 关闭", () => {
    const onClose = vi.fn();
    const { app } = mountMenu({ onClose });

    window.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape" }));

    expect(onClose).toHaveBeenCalledTimes(1);
    app.unmount();
  });

  it("按别的键 → 不关闭", () => {
    const onClose = vi.fn();
    const { app } = mountMenu({ onClose });

    window.dispatchEvent(new KeyboardEvent("keydown", { key: "a" }));

    expect(onClose).not.toHaveBeenCalled();
    app.unmount();
  });
});
