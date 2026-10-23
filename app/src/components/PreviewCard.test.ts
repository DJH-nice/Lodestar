// @vitest-environment happy-dom
/**
 * 预览卡的「收藏此目录 / 收藏此文件 / 取消收藏」按钮 —— 测试。
 *
 *  【为什么要有这个按钮】路径书签原来【只有显示、删除，没有创建入口】——
 *  面板里那栏永远是空的。这个按钮就是入口 A（入口 B 是结果行的右键菜单）。
 *
 *  【契约】点收藏【不关卡片】——
 *  写盘是异步的，成功后 App.vue 更新书签状态 → 文案自动翻成「取消收藏」，
 *  用户想反悔可以马上再点一次。
 */

import { createApp, h } from "vue";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { Entry } from "../lib/types";
import PreviewCard from "./PreviewCard.vue";

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
  onClose?: () => void;
  onOpen?: (path: string) => void;
  onReveal?: (path: string) => void;
  onBookmark?: (path: string) => void;
}

function mountCard(opts: MountOpts = {}) {
  const host = document.createElement("div");
  document.body.appendChild(host);
  const app = createApp({
    render: () =>
      h(PreviewCard, {
        entry: opts.entry === undefined ? FOLDER : opts.entry,
        x: 100,
        y: 100,
        bookmarked: opts.bookmarked ?? false,
        error: opts.error ?? null,
        onClose: opts.onClose,
        onOpen: opts.onOpen,
        onReveal: opts.onReveal,
        onBookmark: opts.onBookmark,
      }),
  });
  app.mount(host);
  return { app, host };
}

/** 第三个按钮 = 收藏按钮（前面两个是「打开」「在访达中显示」）。 */
function bookmarkBtn(host: HTMLElement): HTMLButtonElement {
  const btns = host.querySelectorAll<HTMLButtonElement>(".pc-actions button");
  expect(btns.length, "动作条上应该有三个按钮").toBe(3);
  return btns[2];
}

afterEach(() => {
  document.body.innerHTML = "";
});

describe("PreviewCard 的收藏按钮", () => {
  it("目录 → 文案是「收藏此目录」", () => {
    const { app, host } = mountCard({ entry: FOLDER, bookmarked: false });
    expect(bookmarkBtn(host).textContent?.trim()).toBe("收藏此目录");
    app.unmount();
  });

  it("文件 → 文案是「收藏此文件」", () => {
    const { app, host } = mountCard({ entry: FILE, bookmarked: false });
    expect(bookmarkBtn(host).textContent?.trim()).toBe("收藏此文件");
    app.unmount();
  });

  it("已收藏 → 文案是「取消收藏」", () => {
    const { app, host } = mountCard({ entry: FOLDER, bookmarked: true });
    expect(bookmarkBtn(host).textContent?.trim()).toBe("取消收藏");
    app.unmount();
  });

  it("点它 → 发 bookmark 事件（参数是 entry 的路径）", () => {
    const onBookmark = vi.fn();
    const { app, host } = mountCard({ entry: FILE, onBookmark });

    bookmarkBtn(host).click();

    expect(onBookmark).toHaveBeenCalledTimes(1);
    expect(onBookmark).toHaveBeenCalledWith(FILE.path);
    app.unmount();
  });

  it("点它 → 【不关卡片】（成功后面板/App 更新 → 文案翻转，用户能反悔）", () => {
    const onClose = vi.fn();
    const { app, host } = mountCard({ entry: FILE, onClose });

    bookmarkBtn(host).click();

    expect(onClose).not.toHaveBeenCalled();
    app.unmount();
  });

  it("写失败 → 卡片上显示错误文字", () => {
    const { app, host } = mountCard({ error: "写盘失败：磁盘满了" });
    const err = host.querySelector(".pc-error");
    expect(err).not.toBeNull();
    expect(err?.textContent).toContain("写盘失败：磁盘满了");
    app.unmount();
  });

  it("没有错误 → 不显示错误行", () => {
    const { app, host } = mountCard({ error: null });
    expect(host.querySelector(".pc-error")).toBeNull();
    app.unmount();
  });
});
