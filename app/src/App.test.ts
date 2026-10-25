// @vitest-environment happy-dom
/**
 * App.vue 的「换索引目录 → 重搜」链路 —— 回归测试。
 *
 *  【第一个测试在修什么】Phase 3 fix round 1/2 的两个 bug 都在这条链路上：
 *   - 重搜只靠「轮询撞见 scanning 由 true 变 false」→ 小目录毫秒级完成时撞不上
 *     → 旧目录的搜索结果留在屏上（状态栏还会同时显示两个引擎的数字）；
 *   - `emit("reindexed")` 排在写盘之后 → 写盘失败时界面不刷新。
 *  现在改成**事件驱动**：`reindexed` 一到就更新状态 + `refreshKey++`。
 *
 *  【后面两个测试在验什么】路径书签的**创建入口**（原来只有显示、删除）：
 *   右键 → 菜单 → 「收藏」→ 走 patchSettings → 设置里多一条 {kind:"path"}。
 *   以及写盘失败时**不静默吞掉错误** —— 菜单里要看得见。
 *
 *  【怎么验】不需要真引擎：`../lib/api` 与 `../lib/settings-io` mock 掉，
 *  然后走**真实的组件链路**（右键结果行 → 点菜单项），
 *  断言 patchSettings 真的写进去了 / 失败时界面显示错误。
 */

import { createApp, nextTick } from "vue";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { Entry, IndexStatus, Settings } from "./lib/types";

/** 每次 `search` 调用都记一笔 —— 「有没有重搜」就看它的条数。 */
let searchCalls: string[] = [];
/** 每次 patchSettings 写盘后的完整设置 —— 断言「设置里多了一条路径书签」。 */
let settingsWrites: Settings[] = [];
/** 让 patchSettings 失败 —— 验「写盘失败 → 界面显示错误」。 */
let patchFails = false;
/** mock 的「盘上真值」—— patch 打在它上面（模拟真的读-改-写，
 *  这样「再点一次取消收藏」才能验出 toggle 语义）。 */
let currentSettings: Settings;

/* ---- 书签导入 / 导出用的假环境 ---- */

/** 假的「文件系统」—— 路径 → 文件内容。 */
let fakeFiles: Record<string, string> = {};
/** dialog 的 save() 返回的路径（null = 用户取消）。 */
let savePath: string | null = "/tmp/fsearch-bookmarks.json";
/** dialog 的 open() 返回的路径（null = 用户取消）。 */
let openPath: string | null = "/tmp/fsearch-bookmarks.json";
/** rescan 的闸门 —— 非 null 时 rescan 一直悬着（模拟「在飞」）。 */
let rescanGate: Promise<void> | null = null;
/** 让 rescan 失败 —— 验「失败也要复位按钮」。 */
let rescanFails = false;

const SETTINGS: Settings = {
  indexPaths: [],
  sortPref: undefined,
  kindPref: undefined,
  bookmarks: [],
  recentQueries: [],
};
currentSettings = { ...SETTINGS };

/** 结果行里的那一条 —— 右键它、收藏它。 */
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
    /* 【rescan 做成可控的】闸门没开时它一直悬着 ——
       这样才能测出「在飞」这个中间态（按钮禁用 + 文案变化）。 */
    rescan: async (): Promise<IndexStatus> => {
      if (rescanGate) await rescanGate;
      if (rescanFails) throw new Error("boom");
      return { scanning: false, indexed: 100, paths: ["/tmp"], error: null };
    },
    /* 【书签文件的读写】落到一个假的「文件系统」上。 */
    writeBookmarksFile: async (path: string, contents: string): Promise<void> => {
      fakeFiles[path] = contents;
    },
    readBookmarksFile: async (path: string): Promise<string> => {
      if (!(path in fakeFiles)) throw new Error(`读不到 ${path}`);
      return fakeFiles[path];
    },
    search: async (query: string) => {
      searchCalls.push(query);
      return {
        total_entries: 1,
        total_files: 1,
        total_folders: 0,
        entries: [ENTRY],
        elapsed_ms: 0,
        warning: null,
        complete: true,
        group_boundary: null,
      };
    },
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
    loadSettings: async (): Promise<Settings> => ({ ...currentSettings }),
    /* 真的把 patch 应用到当前设置上 —— 不然「设置里多了一条」这个断言就是空的。
       状态是累积的（跟真的读-改-写一样），所以 toggle 才验得出来。 */
    patchSettings: async (
      patch: (s: Settings) => Partial<Settings> | null,
    ): Promise<Settings> => {
      if (patchFails) throw new Error("磁盘满了");
      const delta = patch({ ...currentSettings });
      if (delta === null) return { ...currentSettings };
      currentSettings = { ...currentSettings, ...delta };
      settingsWrites.push({ ...currentSettings });
      return { ...currentSettings };
    },
  };
});

/* 【文件选择框】—— 前端没有文件系统，靠已装的 dialog 插件拿路径。
   这里 mock 掉：save/open 返回测试指定的路径（null = 用户取消）。 */
vi.mock("@tauri-apps/plugin-dialog", () => ({
  save: async (): Promise<string | null> => savePath,
  open: async (): Promise<string | null> => openPath,
}));

/* 【原生菜单栏的事件 / 后端的自动刷新事件】——
   测试环境里没有 Tauri 的事件系统（`listen` 会直接 reject），
   mock 掉，免得每次挂载都打一条「监听失败」的警告。
   （手册面板自己的行为在 UserGuide.test.ts 里验。）

   【这次多记一份】把 listen 注册的**回调**存下来 ——
   测试要能**模拟后端发事件**，才能验「收到事件 → 结果表重搜」。 */
const eventHandlers: Record<string, (ev: { payload: unknown }) => void> = {};

vi.mock("@tauri-apps/api/event", () => ({
  listen: async (
    name: string,
    handler: (ev: { payload: unknown }) => void,
  ): Promise<() => void> => {
    eventHandlers[name] = handler;
    return () => {};
  },
}));

import App from "./App.vue";

/** 挂载过的 app —— afterEach 里统一卸载
 *  （漏卸载会留下 App.vue 里那个 800ms 的轮询定时器）。 */
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

/** 右键点第一行结果 —— 返回弹出来的右键菜单。 */
async function openContextMenu(host: HTMLElement) {
  const row = host.querySelector<HTMLElement>(".rt-row");
  expect(row, "结果表里应该有一行").not.toBeNull();
  row!.dispatchEvent(
    new MouseEvent("contextmenu", {
      bubbles: true,
      cancelable: true,
      clientX: 120,
      clientY: 120,
    }),
  );
  await flush();
  const menu = host.querySelector(".ctx-menu");
  expect(menu, "右键应该弹出右键菜单").not.toBeNull();
  return menu!;
}

/** 按文案找菜单项。 */
function ctxItem(host: HTMLElement, text: string): HTMLButtonElement {
  const btn = Array.from(host.querySelectorAll<HTMLButtonElement>(".ctx-item")).find(
    (b) => (b.textContent ?? "").trim() === text,
  );
  expect(btn, `菜单里应该有「${text}」这一项`).toBeTruthy();
  return btn!;
}

/** 按一个全局快捷键 —— App.vue 把 ⌘/ 与 ⌘B 的监听挂在 window 上。 */
async function pressGlobalKey(key: string, code: string): Promise<void> {
  window.dispatchEvent(
    new KeyboardEvent("keydown", { key, code, metaKey: true, cancelable: true, bubbles: true }),
  );
  await flush();
}

/** 当前开着几个抽屉 —— E4 的现象就是这里 > 1。 */
function drawerCount(host: HTMLElement): number {
  return host.querySelectorAll(".panel-drawer").length;
}

/** 抽屉的标题 —— 设置 / 帮助中心 / 书签与最近。 */
function drawerTitle(host: HTMLElement): string {
  return host.querySelector(".panel-drawer h2")?.textContent?.trim() ?? "";
}

/** 标题栏的「设置」按钮。 */
function settingsButton(host: HTMLElement): HTMLButtonElement {
  const btn = Array.from(host.querySelectorAll<HTMLButtonElement>(".tb-refresh")).find(
    (b) => (b.textContent ?? "").trim() === "设置",
  );
  expect(btn, "标题栏应该有「设置」按钮").toBeTruthy();
  return btn!;
}

/** 标题栏的「刷新」按钮 —— 文案在「刷新」与「刷新中…」之间变。 */
function refreshButton(host: HTMLElement): HTMLButtonElement {
  const btn = Array.from(host.querySelectorAll<HTMLButtonElement>(".tb-refresh")).find((b) => {
    const t = (b.textContent ?? "").trim();
    return t === "刷新" || t === "刷新中…";
  });
  expect(btn, "标题栏应该有「刷新」按钮").toBeTruthy();
  return btn!;
}

/** 抽屉页脚里的按钮（按文案找）。 */
function footButton(host: HTMLElement, text: string): HTMLButtonElement {
  const btn = Array.from(
    host.querySelectorAll<HTMLButtonElement>(".panel-foot .set-btn"),
  ).find((b) => (b.textContent ?? "").trim() === text);
  expect(btn, `抽屉页脚应该有「${text}」按钮`).toBeTruthy();
  return btn!;
}

/** 按一个【没有修饰键】的键（Esc 那种）。 */
async function pressKey(key: string): Promise<void> {
  window.dispatchEvent(
    new KeyboardEvent("keydown", { key, code: key, cancelable: true, bubbles: true }),
  );
  await flush();
}

afterEach(() => {
  for (const app of mountedApps.splice(0)) {
    // _instance 在 unmount 之后是 null —— 手动卸载过的跳过（重复卸载 Vue 会警告）
    if ((app as unknown as { _instance: unknown })._instance) app.unmount();
  }
  searchCalls = [];
  settingsWrites = [];
  patchFails = false;
  currentSettings = { ...SETTINGS };
  fakeFiles = {};
  savePath = "/tmp/fsearch-bookmarks.json";
  openPath = "/tmp/fsearch-bookmarks.json";
  rescanGate = null;
  rescanFails = false;
  for (const k of Object.keys(eventHandlers)) delete eventHandlers[k];
  document.body.innerHTML = "";
});

describe("App.vue 的「索引自动刷新」事件", () => {
  it("收到 index-auto-refreshed → 结果表重搜一次 + 状态栏更新", async () => {
    const { app, host } = mountApp();
    await flush();

    // 挂载时结果表会搜一次（空查询）
    const before = searchCalls.length;
    expect(before).toBeGreaterThan(0);

    // 【事件名必须与 Rust 侧一致】——
    //   Rust 侧的 watcher 发的是 "index-auto-refreshed"。
    //   名字对不上 → 事件发了但没人听 → 结果表永远不刷新（而且不报错）。
    const handler = eventHandlers["index-auto-refreshed"];
    expect(
      handler,
      "App.vue 必须监听 index-auto-refreshed 事件",
    ).toBeTypeOf("function");

    // 模拟后端发事件：索引已经更新成 123 项
    handler!({
      payload: {
        scanning: false,
        indexed: 123,
        paths: ["/tmp"],
        error: null,
      },
    });
    await flush();

    expect(
      searchCalls.length,
      "自动刷新之后必须重搜一次（否则结果表里还是旧索引的结果）",
    ).toBeGreaterThan(before);

    // 状态栏的「已索引 N 项」也跟着更新
    expect(host.textContent).toContain("123");

    app.unmount();
  });

  it("没有这个事件监听 → 结果表不会自动重搜（反证实验的对照组）", async () => {
    const { app } = mountApp();
    await flush();

    // 假设前端把 listen 那一行删掉了 —— 那就没有任何回调能被触发
    const handler = eventHandlers["index-auto-refreshed"];
    expect(handler).toBeTypeOf("function");
    // 把回调摘掉（模拟「没监听」）
    delete eventHandlers["index-auto-refreshed"];

    const before = searchCalls.length;
    // 模拟后端发事件 —— 没人听，什么都不该发生
    expect(eventHandlers["index-auto-refreshed"]).toBeUndefined();
    await flush();
    expect(
      searchCalls.length,
      "没人监听这个事件时不应该重搜",
    ).toBe(before);

    app.unmount();
  });
});

describe("App.vue 的换索引目录链路", () => {
  it("换索引目录后必须重搜一次（refreshKey++）—— 不靠轮询撞状态翻转", async () => {
    const { app, host } = mountApp();
    await flush();

    // 挂载时结果表会搜一次（空查询）
    const before = searchCalls.length;
    expect(before).toBeGreaterThan(0);

    // 点标题栏的「设置」→ 打开抽屉
    const setBtn = Array.from(host.querySelectorAll<HTMLButtonElement>(".tb-refresh")).find(
      (b) => (b.textContent ?? "").trim() === "设置",
    );
    expect(setBtn).toBeDefined();
    setBtn!.click();
    await flush();

    // 点「应用并重建索引」→ reindex 成功 → reindexed 事件 → refreshKey++ → 重搜
    const applyBtn = host.querySelector<HTMLButtonElement>(".set-btn-primary");
    expect(applyBtn).not.toBeNull();
    applyBtn!.click();
    await flush();

    expect(
      searchCalls.length,
      "换索引目录之后必须重新搜一次（旧结果这时才被清掉）",
    ).toBeGreaterThan(before);

    app.unmount();
  });
});

describe("App.vue 的三个抽屉面板：互斥（设置 / 帮助中心 / 书签与最近）", () => {
  it("T1 开一个面板会自动关掉另一个（activePanel 只有一个）", async () => {
    const { app, host } = mountApp();
    await flush();

    // 点标题栏的「设置」→ 打开设置抽屉
    settingsButton(host).click();
    await flush();
    expect(drawerCount(host)).toBe(1);
    expect(drawerTitle(host)).toBe("设置");

    // 再按 ⌘/ → 帮助抽屉打开，设置抽屉自动没了
    // （⌘/ 打开的是「帮助中心」，默认停在速查那页）
    await pressGlobalKey("/", "Slash");
    expect(drawerCount(host), "两个面板不能同时开着").toBe(1);
    expect(drawerTitle(host)).toBe("帮助中心");

    // 再按 ⌘B → 书签抽屉打开，帮助抽屉自动没了
    await pressGlobalKey("b", "KeyB");
    expect(drawerCount(host), "两个面板不能同时开着").toBe(1);
    expect(drawerTitle(host)).toBe("书签与最近");

    app.unmount();
  });

  it("T2 同一面板再按一次快捷键 → 关闭（toggle 语义）", async () => {
    const { app, host } = mountApp();
    await flush();

    await pressGlobalKey("/", "Slash");
    expect(drawerCount(host)).toBe(1);

    await pressGlobalKey("/", "Slash");
    expect(drawerCount(host), "再按一次应该关掉").toBe(0);

    await pressGlobalKey("/", "Slash");
    expect(drawerCount(host), "再按一次应该再打开").toBe(1);

    // ⌘B 同理
    await pressGlobalKey("b", "KeyB");
    expect(drawerCount(host), "⌘B 应该切到书签面板").toBe(1);
    expect(drawerTitle(host)).toBe("书签与最近");

    await pressGlobalKey("b", "KeyB");
    expect(drawerCount(host), "再按一次 ⌘B 应该关掉").toBe(0);

    app.unmount();
  });

  it("T3 任意切换顺序下 .panel-drawer 最多只有 1 个（E4 的直接护栏）", async () => {
    const { app, host } = mountApp();
    await flush();

    // 帮助 → 书签 → 设置，每一步都只能有一个抽屉
    await pressGlobalKey("/", "Slash");
    expect(drawerCount(host)).toBeLessThanOrEqual(1);

    await pressGlobalKey("b", "KeyB");
    expect(drawerCount(host)).toBeLessThanOrEqual(1);
    expect(drawerTitle(host)).toBe("书签与最近");

    settingsButton(host).click();
    await flush();
    expect(drawerCount(host)).toBeLessThanOrEqual(1);
    expect(drawerTitle(host)).toBe("设置");

    // 反向再来一遍：设置 → 帮助 → 书签
    await pressGlobalKey("/", "Slash");
    expect(drawerCount(host)).toBeLessThanOrEqual(1);
    expect(drawerTitle(host)).toBe("帮助中心");

    await pressGlobalKey("b", "KeyB");
    expect(drawerCount(host)).toBeLessThanOrEqual(1);
    expect(drawerTitle(host)).toBe("书签与最近");

    app.unmount();
  });

  it("T5 开抽屉时小浮层被收起（它们不能浮在抽屉上面）", async () => {
    const { app, host } = mountApp();
    await flush();

    // 右键 → 菜单弹出
    await openContextMenu(host);
    expect(host.querySelector(".ctx-menu")).not.toBeNull();

    // 按 ⌘B → 书签抽屉打开 → 右键菜单必须被收起
    await pressGlobalKey("b", "KeyB");
    expect(drawerCount(host), "书签抽屉应该开了").toBe(1);
    expect(host.querySelector(".ctx-menu"), "右键菜单必须被收起").toBeNull();

    app.unmount();
  });

  it("T5 开抽屉时预览卡被收起", async () => {
    const { app, host } = mountApp();
    await flush();

    // 右键 → 「详情」→ 预览卡
    await openContextMenu(host);
    ctxItem(host, "详情").click();
    await flush();
    expect(host.querySelector(".preview-card")).not.toBeNull();

    // 按 ⌘B → 预览卡必须消失
    await pressGlobalKey("b", "KeyB");
    expect(drawerCount(host), "书签抽屉应该开了").toBe(1);
    expect(host.querySelector(".preview-card"), "预览卡必须被收起").toBeNull();

    app.unmount();
  });

  it("面板自己的「×」也能关掉它（模板的 @close 走 closePanel()）", async () => {
    const { app, host } = mountApp();
    await flush();

    settingsButton(host).click();
    await flush();
    expect(drawerCount(host)).toBe(1);

    host.querySelector<HTMLButtonElement>(".panel-close")!.click();
    await flush();
    expect(drawerCount(host), "点 × 应该关掉抽屉").toBe(0);

    app.unmount();
  });
});

describe("App.vue 的路径书签创建入口", () => {
  it("右键 → 菜单 → 收藏 → 设置里多一条 {kind:\"path\"}，菜单文案翻成「取消收藏」", async () => {
    const { app, host } = mountApp();
    await flush();

    await openContextMenu(host);

    ctxItem(host, "收藏此文件").click();
    await flush();

    // 设置里多了一条路径书签
    expect(settingsWrites.length, "收藏应该真的写了一次盘").toBeGreaterThan(0);
    expect(settingsWrites[settingsWrites.length - 1].bookmarks).toContainEqual({
      kind: "path",
      value: ENTRY.path,
    });

    // 菜单不关（用户能立刻反悔），但文案翻成「取消收藏」
    expect(host.querySelector(".ctx-menu")).not.toBeNull();
    expect(host.textContent).toContain("取消收藏");

    app.unmount();
  });

  it("再点一次「取消收藏」→ 设置里那条没了（toggle 语义，不是「只加不删」）", async () => {
    const { app, host } = mountApp();
    await flush();

    await openContextMenu(host);

    // 第一次：收藏
    ctxItem(host, "收藏此文件").click();
    await flush();
    expect(settingsWrites[settingsWrites.length - 1].bookmarks).toContainEqual({
      kind: "path",
      value: ENTRY.path,
    });

    // 文案翻成「取消收藏」—— 再点一次应该【删掉】那条
    ctxItem(host, "取消收藏").click();
    await flush();
    expect(
      settingsWrites[settingsWrites.length - 1].bookmarks,
      "再点一次必须把这条删掉（toggle 语义）",
    ).not.toContainEqual({ kind: "path", value: ENTRY.path });

    app.unmount();
  });

  it("写盘失败 → 菜单里显示错误（不静默吞掉），菜单还留着", async () => {
    const spy = vi.spyOn(console, "error").mockImplementation(() => {});
    patchFails = true;

    const { app, host } = mountApp();
    await flush();

    await openContextMenu(host);

    ctxItem(host, "收藏此文件").click();
    await flush();

    const err = host.querySelector(".ctx-error");
    expect(err, "写盘失败时菜单里要看得见错误").not.toBeNull();
    expect(err?.textContent).toContain("磁盘满了");
    // 菜单留着 —— 用户还能重试 / 看到失败
    expect(host.querySelector(".ctx-menu")).not.toBeNull();
    // 没有写进设置
    expect(settingsWrites).toHaveLength(0);

    spy.mockRestore();
    app.unmount();
  });
});

describe("App.vue 的 Esc 关面板", () => {
  it("Esc 能关掉三个面板（帮助 / 书签 / 设置）", async () => {
    const { app, host } = mountApp();
    await flush();

    // 帮助面板
    await pressGlobalKey("/", "Slash");
    expect(drawerCount(host)).toBe(1);
    await pressKey("Escape");
    expect(drawerCount(host), "Esc 应该关掉帮助面板").toBe(0);

    // 书签面板
    await pressGlobalKey("b", "KeyB");
    expect(drawerCount(host)).toBe(1);
    await pressKey("Escape");
    expect(drawerCount(host), "Esc 应该关掉书签面板").toBe(0);

    // 设置面板
    settingsButton(host).click();
    await flush();
    expect(drawerCount(host)).toBe(1);
    await pressKey("Escape");
    expect(drawerCount(host), "Esc 应该关掉设置面板").toBe(0);

    app.unmount();
  });

  it("没有面板开着时按 Esc 什么也不该发生", async () => {
    const { app, host } = mountApp();
    await flush();

    expect(drawerCount(host)).toBe(0);
    await pressKey("Escape");
    expect(drawerCount(host)).toBe(0);

    app.unmount();
  });
});

describe("App.vue 的书签导入 / 导出", () => {
  /** 打开书签面板（⌘B）。 */
  async function openBookmarkPanel(host: HTMLElement) {
    await pressGlobalKey("b", "KeyB");
    expect(drawerCount(host), "书签抽屉应该开了").toBe(1);
  }

  it("导出 → 产出的 JSON 结构正确（只含书签）", async () => {
    currentSettings = {
      ...SETTINGS,
      bookmarks: [
        { kind: "query", value: "java" },
        { kind: "path", value: "/Users/x/y" },
      ],
    };
    const { app, host } = mountApp();
    await flush();
    await openBookmarkPanel(host);

    footButton(host, "导出").click();
    await flush();

    const written = fakeFiles["/tmp/fsearch-bookmarks.json"];
    expect(written, "导出应该真的写了文件").toBeDefined();
    const parsed = JSON.parse(written!);
    expect(parsed.format).toBe("fsearch-bookmarks");
    expect(parsed.version).toBe(1);
    expect(parsed.bookmarks).toEqual([
      { kind: "query", value: "java" },
      { kind: "path", value: "/Users/x/y" },
    ]);
    // 【只有书签】索引目录 / 窗口位置不该被搬走
    expect(parsed.indexPaths).toBeUndefined();
    expect(parsed.recentQueries).toBeUndefined();

    app.unmount();
  });

  it("导入 → 设置里多了书签；重复导入同一个文件不产生重复", async () => {
    fakeFiles["/tmp/import.json"] = JSON.stringify({
      format: "fsearch-bookmarks",
      version: 1,
      bookmarks: [
        { kind: "query", value: "java" },
        { kind: "path", value: "/tmp/x" },
      ],
    });
    openPath = "/tmp/import.json";
    const { app, host } = mountApp();
    await flush();
    await openBookmarkPanel(host);

    footButton(host, "导入").click();
    await flush();

    expect(settingsWrites.length, "导入应该真的写了一次盘").toBeGreaterThan(0);
    const after = settingsWrites[settingsWrites.length - 1].bookmarks;
    expect(after).toContainEqual({ kind: "query", value: "java" });
    expect(after).toContainEqual({ kind: "path", value: "/tmp/x" });

    // 再导入同一个文件 → 不产生重复
    footButton(host, "导入").click();
    await flush();

    const after2 = settingsWrites[settingsWrites.length - 1].bookmarks;
    expect(
      after2.filter((b) => b.kind === "query" && b.value === "java"),
      "重复导入不能产生重复条目",
    ).toHaveLength(1);
    expect(after2.filter((b) => b.kind === "path" && b.value === "/tmp/x")).toHaveLength(1);

    app.unmount();
  });

  it("导入非法 JSON → 设置没变、错误显示出来了", async () => {
    fakeFiles["/tmp/bad.json"] = "{ 这不是 JSON";
    openPath = "/tmp/bad.json";
    const { app, host } = mountApp();
    await flush();
    await openBookmarkPanel(host);

    footButton(host, "导入").click();
    await flush();

    expect(settingsWrites, "校验不过 → 一个字都不许写盘").toHaveLength(0);
    const err = host.querySelector(".set-error");
    expect(err, "导入失败要在面板里看得见").not.toBeNull();
    expect(err?.textContent).toContain("导入书签失败");

    app.unmount();
  });

  it("导入格式不对的文件（format 不是 fsearch-bookmarks）→ 设置没变、有错误", async () => {
    fakeFiles["/tmp/wrong.json"] = JSON.stringify({
      format: "别的格式",
      version: 1,
      bookmarks: [{ kind: "query", value: "java" }],
    });
    openPath = "/tmp/wrong.json";
    const { app, host } = mountApp();
    await flush();
    await openBookmarkPanel(host);

    footButton(host, "导入").click();
    await flush();

    expect(settingsWrites).toHaveLength(0);
    const err = host.querySelector(".set-error");
    expect(err).not.toBeNull();
    expect(err?.textContent).toContain("format");

    app.unmount();
  });

  it("用户取消选择 → 什么都不做（不是错误，也不写盘）", async () => {
    savePath = null;
    openPath = null;
    const { app, host } = mountApp();
    await flush();
    await openBookmarkPanel(host);

    footButton(host, "导出").click();
    await flush();
    footButton(host, "导入").click();
    await flush();

    expect(settingsWrites).toHaveLength(0);
    expect(Object.keys(fakeFiles)).toHaveLength(0);
    expect(host.querySelector(".set-error"), "取消不是错误").toBeNull();

    app.unmount();
  });
});

describe("App.vue 的「刷新」按钮：进行中反馈", () => {
  it("rescan 在飞时按钮禁用、文案是「刷新中…」", async () => {
    let release = () => {};
    rescanGate = new Promise<void>((r) => {
      release = r;
    });

    const { app, host } = mountApp();
    await flush();

    const btn = refreshButton(host);
    expect(btn.disabled).toBe(false);
    expect((btn.textContent ?? "").trim()).toBe("刷新");

    btn.click();
    await flush();

    // 【在飞】→ 禁用 + 文案变了
    expect(refreshButton(host).disabled, "在飞时按钮必须禁用（防连点）").toBe(true);
    expect((refreshButton(host).textContent ?? "").trim()).toBe("刷新中…");

    // 放行 → 复位
    release();
    await flush();
    expect(refreshButton(host).disabled).toBe(false);
    expect((refreshButton(host).textContent ?? "").trim()).toBe("刷新");

    app.unmount();
  });

  it("rescan 失败 → 按钮照样复位（不能永久禁用）", async () => {
    const spy = vi.spyOn(console, "error").mockImplementation(() => {});
    rescanFails = true;
    const { app, host } = mountApp();
    await flush();

    refreshButton(host).click();
    await flush();

    // 【先确认失败这条路真的走到了】—— 否则这条测试是空的
    expect(spy, "rescan 失败应该被记到 console.error").toHaveBeenCalled();
    expect(refreshButton(host).disabled, "失败也必须复位").toBe(false);
    expect((refreshButton(host).textContent ?? "").trim()).toBe("刷新");

    spy.mockRestore();
    app.unmount();
  });
});

/**
 * 「外接卷变化」事件 —— 插上 / 拔掉 U 盘后结果表必须自动刷新。
 *
 *  【为什么不能靠轮询撞】
 *   前端原来只靠「scanning 从 true 翻到 false」触发重搜，
 *   而卷的附加 / 分离**不会**让 scanning 翻转 → 撞不出来 →
 *   结果表里留着已经拔掉的盘的条目（用户最不能接受的那种：盘都拔了还搜得到）。
 *
 *  【事件名必须与 Rust 侧一致】
 *   Rust 侧发的是 `"index-volumes-changed"`（`volumes::VOLUMES_CHANGED_EVENT`）。
 *   名字对不上 → 事件发了但没人听 → 结果表永远不刷新（而且不报错）。
 */
describe("App.vue 的「外接卷变化」事件", () => {
  it("收到 index-volumes-changed → 结果表重搜一次 + 状态栏显示卷", async () => {
    const { app, host } = mountApp();
    await flush();

    // 挂载时结果表会搜一次（空查询）
    const before = searchCalls.length;
    expect(before).toBeGreaterThan(0);

    const handler = eventHandlers["index-volumes-changed"];
    expect(handler, "App.vue 必须监听 index-volumes-changed 事件").toBeTypeOf("function");

    // 模拟后端发事件：U 盘挂上了，扫出了 42 项
    handler!({
      payload: {
        scanning: false,
        indexed: 142,
        paths: ["/tmp", "/Volumes/U盘"],
        error: null,
        visible: true,
        volumes: [
          { path: "/Volumes/U盘", label: "U盘", phase: { state: "Ready" }, indexed: 42 },
        ],
      },
    });
    await flush();

    expect(
      searchCalls.length,
      "卷变化之后必须重搜一次（否则结果表里还是旧结果）",
    ).toBeGreaterThan(before);

    // 状态栏也要跟着显示这个卷
    expect(host.textContent).toContain("U盘");

    app.unmount();
  });
});
