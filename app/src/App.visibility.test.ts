// @vitest-environment happy-dom
/**
 * App.vue 的「窗口可见性 → 状态轮询启停」链路 —— 回归测试。
 *
 *  【这条链路在修什么】后台常驻时，应用每 800ms 轮询一次索引状态
 *   （`pollStatus` → 一次跨进程 IPC）。实测每次约 3.4ms CPU、1.25 次/秒 ——
 *   窗口**藏着**的时候用户根本看不见状态栏，这个轮询是纯浪费，
 *   而它恰恰是后台常驻时 CPU 占用的大头。
 *
 *  【怎么修的】Rust 侧把窗口可见性告诉前端（`app-visibility` 事件 +
 *   `index_status` 返回值里的 `visible`），前端据此启停那个轮询。
 *
 *  【为什么这几条测试重要】
 *   - 「停」和「启」必须**成对**：只停不启 = 窗口回来后状态栏再也不更新
 *     （用户看得见的故障）；只启不停 = 后台白烧 CPU（用户看不见的性能问题）。
 *   - 「启动时窗口就不可见」是**开机自启**那条路（`--hidden` 启动）——
 *     之前那里是无条件开轮询，等于登录项在后台一直白烧 CPU。
 */

import { createApp, nextTick } from "vue";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { IndexStatus, Settings } from "./lib/types";

/** `indexStatus` 被调用的次数 —— 「轮询还在跑吗」就看它涨不涨。 */
let statusCalls = 0;
/** mock 返回的「窗口可见吗」——
 *  - `"normal"`  = 正常返回 `visible` 字段
 *  - `"missing"` = **字段整个不返回**（模拟老版本 / 别的构造点漏了它）
 *
 *  【为什么必须有 "missing"】这条 mock 之前只有一种模式 → 「字段缺失」
 *  那条用例是**空的**：把实现改成 `if (s.visible)`（正是要防的错误方向）
 *  它也照样绿 —— 测试给了假安全感。
 *  （复审的变异验证实测过：改错了实现，测试依然全绿。）
 *  → 现在 mock 能返回「字段缺失」，那条用例才真正有区分力。 */
let visibleMode: "normal" | "missing" = "normal";
/** mock 返回的「窗口可见吗」的值（`"missing"` 模式下这个字段压根不返回）。 */
let windowVisible = true;
/** 监听注册表 —— 测试要能**模拟后端发事件**。 */
const eventHandlers: Record<string, (ev: { payload: unknown }) => void> = {};

vi.mock("./lib/api", async (importOriginal) => {
  const actual = await importOriginal<typeof import("./lib/api")>();
  return {
    ...actual,
    indexStatus: async (): Promise<IndexStatus> => {
      statusCalls += 1;
      const base = {
        scanning: false,
        indexed: 100,
        paths: ["/tmp"],
        error: null,
      };
      /* 【"missing" 模式：故意【不】返回 visible 字段】——
       *   模拟「字段缺失」—— Rust 侧的 `snapshot()` 给的是占位值，
       *   别的构造点万一漏填，前端拿到的就是这个形状。
       *   → 前端必须把它当成「可见」（缺字段 ≠ 不可见）。 */
      if (visibleMode === "missing") {
        return base as IndexStatus;
      }
      return { ...base, visible: windowVisible };
    },
    rescan: async (): Promise<IndexStatus> => ({
      scanning: false,
      indexed: 100,
      paths: ["/tmp"],
      error: null,
      visible: windowVisible,
    }),
    search: async () => ({
      total_entries: 0,
      total_files: 0,
      total_folders: 0,
      entries: [],
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

const SETTINGS: Settings = {
  indexPaths: [],
  sortPref: undefined,
  kindPref: undefined,
  bookmarks: [],
  recentQueries: [],
};

vi.mock("./lib/settings-io", async (importOriginal) => {
  const actual = await importOriginal<typeof import("./lib/settings-io")>();
  return {
    ...actual,
    loadSettings: async (): Promise<Settings> => ({ ...SETTINGS }),
    patchSettings: async (patch: (s: Settings) => Partial<Settings> | null) =>
      patch({ ...SETTINGS }) ?? { ...SETTINGS },
  };
});

/* 【文件选择框】—— 挂载时不会用到，mock 掉省事。 */
vi.mock("@tauri-apps/plugin-dialog", () => ({
  save: async (): Promise<string | null> => null,
  open: async (): Promise<string | null> => null,
}));

/* 【事件系统】—— 测试环境里没有 Tauri 的事件系统，
   而且这里**必须**能模拟后端发事件（整条链路的核心就是这个）。 */
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

/** 【假时钟下的 flush】——
 *  注意：不能照抄别处的 `new Promise(r => setTimeout(r, 0))` ——
 *  假时钟下那个 setTimeout 不会自己走 → Promise 永远不 resolve → 测试挂死。
 *  要用 `advanceTimersByTimeAsync(0)` 把「0ms 的定时器」放出来。 */
async function flush(times = 8): Promise<void> {
  for (let i = 0; i < times; i++) {
    await nextTick();
    await vi.advanceTimersByTimeAsync(0);
  }
}

beforeEach(() => {
  vi.useFakeTimers();
  statusCalls = 0;
  windowVisible = true;
  visibleMode = "normal";
  for (const k of Object.keys(eventHandlers)) delete eventHandlers[k];
});

afterEach(() => {
  for (const app of mountedApps) app.unmount();
  mountedApps = [];
  vi.useRealTimers();
});

describe("窗口可见性 → 状态轮询的启停", () => {
  it("窗口可见时，状态轮询一直在跑", async () => {
    windowVisible = true;
    mountApp();
    await flush();

    const before = statusCalls;
    await vi.advanceTimersByTimeAsync(800 * 3);
    expect(
      statusCalls - before,
      "窗口可见 → 800ms 一轮的状态轮询必须在跑",
    ).toBeGreaterThanOrEqual(2);
  });

  it("窗口隐藏后，状态轮询停下来（后台常驻时不白烧 CPU）", async () => {
    windowVisible = true;
    mountApp();
    await flush();

    // 后端说窗口没了（快捷键收起 / 点关闭按钮 → Rust 侧 hide() 发的事件）
    eventHandlers["app-visibility"]?.({ payload: false });
    await flush();

    const before = statusCalls;
    await vi.advanceTimersByTimeAsync(800 * 5);
    expect(
      statusCalls,
      "窗口隐藏 → 一次都不该再轮询（那每次都是一次跨进程 IPC）",
    ).toBe(before);
  });

  it("窗口重新可见后，状态轮询恢复", async () => {
    windowVisible = true;
    mountApp();
    await flush();

    eventHandlers["app-visibility"]?.({ payload: false });
    await flush();
    const stopped = statusCalls;
    await vi.advanceTimersByTimeAsync(800 * 3);
    expect(statusCalls, "先确认它真的停住了").toBe(stopped);

    // 用户把窗口叫回来了
    eventHandlers["app-visibility"]?.({ payload: true });
    await flush();
    await vi.advanceTimersByTimeAsync(800 * 3);
    expect(
      statusCalls,
      "窗口回来 → 轮询必须恢复（否则状态栏再也不更新）",
    ).toBeGreaterThan(stopped);
  });

  it("启动时窗口就不可见（开机自启那条路）→ 轮询根本不该开", async () => {
    // `--hidden` 启动：窗口从一开始就在后台，用户看不见
    windowVisible = false;
    mountApp();
    await flush();

    const before = statusCalls;
    await vi.advanceTimersByTimeAsync(800 * 5);
    expect(
      statusCalls,
      "开机自启时窗口一直不可见 → 轮询一次都不该发",
    ).toBe(before);
  });

  it("visible 字段缺失时按「可见」处理（宁可多跑一点，别让状态栏停更）", async () => {
    /* 【这条必须有区分力】——
     *   Rust 侧 `IndexState::snapshot()` 给的是占位值 `true`，
     *   别的构造点万一漏填，前端拿到的字段就可能是 undefined。
     *   → 缺省必须当成「可见」，否则状态栏会静默停止更新。
     *
     *   【为什么这里必须用 visibleMode = "missing"】
     *   如果 mock 照样返回 `visible: true`，那这条用例**测不出任何东西** ——
     *   实现写成 `if (s.visible)`（把缺字段当成不可见）它也照样绿。
     *   （复审的变异验证实测过这个假绿。）
     *   → mock 返回「字段缺失」时：
     *      `if (s.visible)`        → undefined 是假值 → 停轮询 → **本条变红**
     *      `if (s.visible !== false)` → undefined → 真 → 继续轮询 → 绿
     */
    visibleMode = "missing";
    mountApp();
    await flush();

    const before = statusCalls;
    await vi.advanceTimersByTimeAsync(800 * 3);
    expect(
      statusCalls - before,
      "visible 字段缺失 → 仍要继续轮询（缺字段 ≠ 不可见）",
    ).toBeGreaterThanOrEqual(2);
  });

  it("重复的可见性事件被去重（Rust 每秒都推，前端只在变化时动作）", async () => {
    // 【为什么】Rust 侧的看门狗**每秒**推一次当前可见性（不只在变化时推，
    //   否则「事件投递是单点的」→ 丢一次事件就永远补不回来 → 状态栏冻住）。
    //   → 前端必须自己去重，不然每秒都白跑一次 pollStatus。
    windowVisible = true;
    mountApp();
    await flush();

    const mid = statusCalls;
    eventHandlers["app-visibility"]?.({ payload: true });
    await flush();
    eventHandlers["app-visibility"]?.({ payload: true });
    await flush();
    eventHandlers["app-visibility"]?.({ payload: true });
    await flush();
    expect(
      statusCalls,
      "值没变 → 不该每次都重拉一次状态（mount 时已经把 lastVisible 设成 true 了）",
    ).toBe(mid);
  });
});
