// @vitest-environment happy-dom
/**
 * 帮助中心（速查 / 完整语法表 / 完整手册 三页）的回归测试。
 *
 *  【验什么】
 *   1. 原生菜单的两项（完整手册 / 搜索语法速查）分别落到**不同的页**上；
 *   2. 页签能切，内容跟着换；
 *   3. 速查里的写法**点一下就填进搜索框**（这是速查最大的用处）；
 *   4. 速查页自己的搜索框能过滤条目；
 *   5. 面板与其它抽屉互斥，Esc 能关。
 *
 *  【怎么验】不需要真引擎：`../lib/api` 与 `../lib/settings-io` mock 掉，
 * `@tauri-apps/api/event` 也 mock 掉（把 listen 的回调抓在手里，自己触发）。
 */

import { createApp, nextTick } from "vue";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { Entry, IndexStatus, Settings } from "./lib/types";

/** listen 抓到的回调 —— 测试自己触发它，模拟 Rust 发了事件。 */
type Listener = (e: unknown) => void;
let listeners: { event: string; cb: Listener }[] = [];

vi.mock("@tauri-apps/api/event", () => ({
  listen: async (event: string, cb: Listener) => {
    listeners.push({ event, cb });
    return () => {
      listeners = listeners.filter((l) => l.cb !== cb);
    };
  },
}));

const SETTINGS: Settings = {
  indexPaths: [],
  sortPref: undefined,
  kindPref: undefined,
  bookmarks: [],
  recentQueries: [],
};

const ENTRY: Entry = {
  name: "notes.txt",
  path: "/tmp/fsearch-demo/notes.txt",
  dir: "/tmp/fsearch-demo",
  extension: "txt",
  size: 12,
  mtime: 1700000000,
  is_folder: false,
  highlights: [],
  path_highlights: [],
};

vi.mock("./lib/api", async (importOriginal) => {
  const actual = await importOriginal<typeof import("./lib/api")>();
  return {
    ...actual,
    indexStatus: async (): Promise<IndexStatus> => ({
      scanning: false,
      indexed: 100,
      paths: ["/tmp"],
      error: null,
    }),
    rescan: async (): Promise<IndexStatus> => ({
      scanning: false,
      indexed: 100,
      paths: ["/tmp"],
      error: null,
    }),
    search: async () => ({
      total_entries: 1,
      total_files: 1,
      total_folders: 0,
      entries: [ENTRY],
      elapsed_ms: 0,
      warning: null,
      complete: true,
      group_boundary: null,
    }),
    syntaxTables: async () => ({ modifiers: [], fields: [], operators: [] }),
    excludeRules: async () => [],
    reindex: async (paths: string[]): Promise<IndexStatus> => ({
      scanning: false,
      indexed: 5,
      paths,
      error: null,
    }),
  };
});

vi.mock("./lib/settings-io", async (importOriginal) => {
  const actual = await importOriginal<typeof import("./lib/settings-io")>();
  return {
    ...actual,
    loadSettings: async (): Promise<Settings> => ({ ...SETTINGS }),
    patchSettings: async (): Promise<Settings> => ({ ...SETTINGS }),
  };
});

import App from "./App.vue";

let mountedApps: ReturnType<typeof createApp>[] = [];

function mountApp() {
  const host = document.createElement("div");
  document.body.appendChild(host);
  const app = createApp(App);
  app.mount(host);
  mountedApps.push(app);
  return { app, host };
}

async function flush(times = 8): Promise<void> {
  for (let i = 0; i < times; i++) {
    await nextTick();
    await new Promise((r) => setTimeout(r, 0));
  }
}

/** 当前开着几个抽屉。 */
function drawerCount(host: HTMLElement): number {
  return host.querySelectorAll(".panel-drawer").length;
}

/** 抽屉的标题。 */
function drawerTitle(host: HTMLElement): string {
  return host.querySelector(".panel-drawer h2")?.textContent?.trim() ?? "";
}

/** 三个页签上的字。 */
function tabLabels(host: HTMLElement): string[] {
  return Array.from(host.querySelectorAll(".hc-tab")).map(
    (b) => (b.textContent ?? "").trim(),
  );
}

/** 点某个页签。 */
function clickTab(host: HTMLElement, label: string): void {
  const tab = Array.from(host.querySelectorAll<HTMLButtonElement>(".hc-tab")).find(
    (b) => (b.textContent ?? "").trim() === label,
  );
  expect(tab, `页签里应该有「${label}」`).toBeTruthy();
  tab!.click();
}

/** 模拟 Rust 发了某个事件。 */
async function fire(event: string): Promise<void> {
  const hit = listeners.find((l) => l.event === event);
  expect(hit, `应该监听了 ${event} 事件`).toBeTruthy();
  hit!.cb({});
  await flush();
}

/** 主搜索框里的内容。 */
function searchBoxValue(host: HTMLElement): string {
  return host.querySelector<HTMLInputElement>(".field-input")?.value ?? "";
}

afterEach(() => {
  for (const app of mountedApps.splice(0)) {
    if ((app as unknown as { _instance: unknown })._instance) app.unmount();
  }
  listeners = [];
  document.body.innerHTML = "";
});

describe("帮助中心：三个页签", () => {
  it("H1 监听了两个菜单事件", async () => {
    mountApp();
    await flush();
    const events = listeners.map((l) => l.event);
    expect(events, "应该监听 open-user-guide").toContain("open-user-guide");
    expect(events, "应该监听 open-syntax-cheatsheet").toContain("open-syntax-cheatsheet");
  });

  it("H2 菜单「完整手册」→ 打开帮助中心并停在手册页", async () => {
    const { host } = mountApp();
    await flush();
    await fire("open-user-guide");

    expect(drawerCount(host)).toBe(1);
    expect(drawerTitle(host)).toBe("帮助中心");
    expect(host.querySelector(".ug-body"), "手册正文应该在").not.toBeNull();
    expect(host.querySelector(".cheat"), "速查那页不该同时显示").toBeNull();
  });

  it("H3 菜单「搜索语法速查」→ 打开帮助中心并停在速查页", async () => {
    const { host } = mountApp();
    await flush();
    await fire("open-syntax-cheatsheet");

    expect(drawerCount(host)).toBe(1);
    expect(drawerTitle(host)).toBe("帮助中心");
    expect(host.querySelector(".cheat"), "速查那页应该在").not.toBeNull();
    expect(host.querySelector(".ug-body"), "手册正文不该同时显示").toBeNull();
  });

  it("H4 页签就是那三个", async () => {
    const { host } = mountApp();
    await flush();
    await fire("open-syntax-cheatsheet");
    expect(tabLabels(host)).toEqual(["语法速查", "完整语法表", "完整手册"]);
  });

  it("H5 点页签能换内容", async () => {
    const { host } = mountApp();
    await flush();
    await fire("open-syntax-cheatsheet");

    clickTab(host, "完整手册");
    await flush();
    expect(host.querySelector(".ug-body"), "切到手册页应该有手册正文").not.toBeNull();
    expect(host.querySelector(".cheat"), "速查那页应该收起来").toBeNull();

    clickTab(host, "语法速查");
    await flush();
    expect(host.querySelector(".cheat"), "切回速查页应该有速查内容").not.toBeNull();
    expect(host.querySelector(".ug-body"), "手册那页应该收起来").toBeNull();
  });
});

describe("帮助中心：速查卡", () => {
  it("H6 每条写法都能点 —— 点一下整条填进搜索框", async () => {
    const { host } = mountApp();
    await flush();
    await fire("open-syntax-cheatsheet");

    const queryBtn = host.querySelector<HTMLButtonElement>(".cheat-query");
    expect(queryBtn, "速查里应该有可点的写法").toBeTruthy();
    const want = (queryBtn!.textContent ?? "").trim();
    expect(want.length, "写法不能是空的").toBeGreaterThan(0);

    queryBtn!.click();
    await flush();

    expect(searchBoxValue(host), "写法应该被填进搜索框").toBe(want);
    expect(drawerCount(host), "填完应该把抽屉关掉").toBe(0);
  });

  it("H7 速查页自己的搜索框能筛条目", async () => {
    const { host } = mountApp();
    await flush();
    await fire("open-syntax-cheatsheet");

    const before = host.querySelectorAll(".cheat-item").length;
    expect(before, "速查条目应该不止一条").toBeGreaterThan(1);

    const search = host.querySelector<HTMLInputElement>(".cheat-search");
    expect(search, "速查页应该有搜索框").toBeTruthy();
    search!.value = "排除";
    search!.dispatchEvent(new Event("input"));
    await flush();

    const after = host.querySelectorAll(".cheat-item").length;
    expect(after, "筛完应该更少").toBeLessThan(before);
    expect(after, "但不能一条都不剩").toBeGreaterThan(0);
  });

  it("H8 错误写法不可点（点了就是把错的填进搜索框）", async () => {
    const { host } = mountApp();
    await flush();
    await fire("open-syntax-cheatsheet");

    const wrong = host.querySelector<HTMLElement>(".cheat-wrong code");
    expect(wrong, "坑那一组应该有错误写法").not.toBeNull();
    // 错误写法是纯文本，不该是按钮
    expect(wrong!.closest("button"), "错误写法不该能被点").toBeNull();
  });
});

describe("帮助中心：互斥与关闭", () => {
  it("H9 与其它抽屉互斥（同时最多一个）", async () => {
    const { host } = mountApp();
    await flush();

    await fire("open-user-guide");
    expect(drawerCount(host)).toBe(1);

    // 按 ⌘B → 切到书签面板，帮助中心自动没了
    window.dispatchEvent(
      new KeyboardEvent("keydown", {
        key: "b",
        code: "KeyB",
        metaKey: true,
        cancelable: true,
        bubbles: true,
      }),
    );
    await flush();
    expect(drawerCount(host), "两个面板不能同时开着").toBe(1);
    expect(drawerTitle(host)).toBe("书签与最近");
  });

  it("H10 Esc 能关掉", async () => {
    const { host } = mountApp();
    await flush();
    await fire("open-syntax-cheatsheet");
    expect(drawerCount(host)).toBe(1);

    window.dispatchEvent(
      new KeyboardEvent("keydown", { key: "Escape", code: "Escape", cancelable: true, bubbles: true }),
    );
    await flush();
    expect(drawerCount(host)).toBe(0);
  });
});
