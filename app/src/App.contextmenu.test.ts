// @vitest-environment happy-dom
/**
 * 右键菜单的三种行为 —— 回归测试。
 *
 *  【这个文件在钉什么】
 *   空白处 / 输入框 / 结果行，三处的右键**行为各不相同**，
 *   而且是**有意设计**的（见 `App.vue` 的 `onGlobalContextMenu`）：
 *
 *   | 右键在哪        | 期望                                              |
 *   |-----------------|---------------------------------------------------|
 *   | 空白处          | **不弹**任何菜单（`defaultPrevented === true`）     |
 *   | 输入框 / 可编辑区 | **放行**原生菜单（`defaultPrevented === false`）→ 能右键粘贴 |
 *   | 结果行          | 弹**自定义**菜单（打开 / 在访达中显示 / 收藏 / 详情） |
 *
 *  【为什么空白处要拦】
 *   WKWebView 在空白处给出的原生菜单里只有「重新载入」——
 *   一个原生 macOS 应用在空白处弹「重新载入」气质完全不对。
 *   （这是**一直存在**的行为，不是后台常驻模式带来的；
 *     引入全局拦截的是 commit f0739c6。）
 *
 *  【为什么输入框要放行】
 *   全局拦掉的话，右键粘贴就没了 —— 那比「弹个重新载入」更糟。
 *
 *  【这些断言是「行为」而不是「实现」】
 *   判据是事件的 `defaultPrevented`（浏览器会不会弹原生菜单），
 *   不是「有没有调 preventDefault」—— 换任何一种等效写法都该绿。
 */

import { createApp, nextTick } from "vue";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { Entry, IndexStatus, Settings } from "./lib/types";

/** 搜索结果里那一条 —— 右键它会弹自定义菜单。 */
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

const SETTINGS: Settings = {
  indexPaths: [],
  sortPref: undefined,
  kindPref: undefined,
  bookmarks: [],
  recentQueries: [],
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
      visible: true,
    }),
    rescan: async (): Promise<IndexStatus> => ({
      scanning: false,
      indexed: 100,
      paths: ["/tmp"],
      error: null,
      visible: true,
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
    readBookmarksFile: async (): Promise<string> => "{}",
    writeBookmarksFile: async (): Promise<void> => {},
  };
});

vi.mock("./lib/settings-io", async (importOriginal) => {
  const actual = await importOriginal<typeof import("./lib/settings-io")>();
  return {
    ...actual,
    loadSettings: async (): Promise<Settings> => ({ ...SETTINGS }),
    patchSettings: async (patch: (s: Settings) => Partial<Settings> | null) =>
      patch({ ...SETTINGS }) ?? { ...SETTINGS },
  };
});

vi.mock("@tauri-apps/plugin-dialog", () => ({
  save: async (): Promise<string | null> => null,
  open: async (): Promise<string | null> => null,
}));

vi.mock("@tauri-apps/api/event", () => ({
  listen: async (): Promise<() => void> => () => {},
}));

import App from "./App.vue";

let mountedApps: ReturnType<typeof createApp>[] = [];

function mountApp() {
  const host = document.createElement("div");
  document.body.appendChild(host);
  const app = createApp(App);
  app.mount(host);
  mountedApps.push(app);
  return host;
}

async function flush(times = 8): Promise<void> {
  for (let i = 0; i < times; i++) {
    await nextTick();
    await new Promise((r) => setTimeout(r, 0));
  }
}

/** 在某个元素上右键 —— 返回这个事件（看它有没有被 preventDefault）。 */
function rightClick(el: Element): MouseEvent {
  const ev = new MouseEvent("contextmenu", {
    bubbles: true,
    cancelable: true,
    clientX: 120,
    clientY: 120,
  });
  el.dispatchEvent(ev);
  return ev;
}

beforeEach(() => {
  document.body.innerHTML = "";
});

afterEach(() => {
  for (const app of mountedApps) app.unmount();
  mountedApps = [];
});

describe("右键的三种行为", () => {
  it("空白处右键 → 不弹原生菜单（WKWebView 那个「重新载入」要挡住）", async () => {
    const host = mountApp();
    await flush();

    // 组件的根元素 —— 那里就是「空白处」。
    const blank = host.firstElementChild!;
    expect(blank, "组件根元素应该在").toBeTruthy();

    const ev = rightClick(blank);
    expect(
      ev.defaultPrevented,
      "空白处右键必须被拦下 —— 否则会弹出只有「重新载入」的原生菜单",
    ).toBe(true);
  });

  it("输入框右键 → 放行原生菜单（否则右键粘贴没了）", async () => {
    const host = mountApp();
    await flush();

    const input = host.querySelector<HTMLInputElement>("input");
    expect(input, "界面上应该有输入框").toBeTruthy();

    const ev = rightClick(input!);
    expect(
      ev.defaultPrevented,
      "输入框右键必须放行 —— 用户要能右键粘贴",
    ).toBe(false);
  });

  it("结果行右键 → 弹自定义菜单（不是原生菜单）", async () => {
    const host = mountApp();
    await flush();
    // 等搜索结果渲染出来
    await flush(12);

    const row = host.querySelector<HTMLElement>(".rt-row");
    expect(row, "结果表里应该有一行").not.toBeNull();

    rightClick(row!);
    await flush();

    const menu = host.querySelector(".ctx-menu");
    expect(menu, "结果行右键应该弹出自定义菜单").not.toBeNull();
  });
});
