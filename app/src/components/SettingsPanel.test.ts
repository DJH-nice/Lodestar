// @vitest-environment happy-dom
/**
 * 设置面板的「应用并重建索引」链路 —— 回归测试。
 *
 *  【这个测试在修什么】Phase 3 fix round 2 的评审发现：
 * `emit("reindexed")` 排在 `await patchSettings()` **之后** ——
 * 于是「写盘失败」时：
 *   - 引擎**已经换了**（`reindex` 成功 → commands.rs 里引擎已替换）
 *   - 但界面**没刷新**（没有 refreshKey++ → 结果表里还是旧目录的结果）
 *   - 盘上**还是旧路径**
 * → 又是「引擎 ≠ 界面」。修法：`reindex()` 一成功就 emit，
 *   写盘失败也要刷新（而且要把「哪一步坏了」说清楚）。
 *
 *  【怎么验】不需要真引擎 + 真 UI：把 `../lib/api` 与 `../lib/settings-io`
 *  mock 掉，组件自己的逻辑（emit 的时机与载荷、错误文案、抽屉关不关）就能断言。
 */

import { createApp, h, nextTick } from "vue";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { IndexStatus, Settings } from "../lib/types";

/* mock IPC 层 —— 面板**直接** invoke("get_global_shortcut") 去问「实际生效的键」。
 *
 *  【为什么必须 mock】
 *   happy-dom 里没有 Tauri 运行时 → invoke 抛错 → 面板问不到真相 →
 *   按 shortcutRuntimeView 的规则（lib/shortcut.ts:288）退回报「不确定」，
 *   渲染出一条 `读不到快捷键的实际生效状态` 的 .set-error。
 *
 *  【为什么它会【连累】别的断言】
 *   面板模板里有 **4 处** .set-error（shortcutError / launchAtLoginError /
 *   loadError / error），而这两条排在**最前面** ——
 *   `querySelector(".set-error")` 只拿**第一个**，
 *   于是后面要断言的「设置没有保存」「自动刷新开关没有保存」
 *   全部被它**顶掉**，断言的是另一条错误。
 *   → 这里把这两个命令都答成「一切正常」，
 *     让面板里只剩【待测的那一条】错误。 */
vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async (cmd: string) => {
    // 「实际生效的快捷键」—— 没有注册任何键，也没有报错
    if (cmd === "get_global_shortcut") return { effective: null, error: null };
    // 「开机自启」—— Rust 侧返回 Result<bool>，成功时就是那个 bool
    if (cmd === "get_launch_at_login") return false;
    if (cmd === "set_launch_at_login") return undefined;
    throw new Error(`未知命令 ${cmd}`);
  }),
}));

/** 「重建成功」之后的状态 —— 组件要原样把它带出去。 */
const NEW_STATUS: IndexStatus = {
  scanning: false,
  indexed: 5,
  paths: ["/tmp/fsearch-accept/dirB"],
  error: null,
};

/** 写盘失败时用的开关。 */
let patchShouldFail = false;
/** reindex 失败时用的开关。 */
let reindexShouldFail = false;
/** 「盘上真值」里的自动刷新开关 —— 断言「写盘之后变成 false」要用。 */
let autoRefreshOnDisk = true;
/** 每次 patchSettings 写盘时记下【改了哪个字段】。 */
let patchDeltas: (Partial<Settings> | null)[] = [];

vi.mock("../lib/api", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../lib/api")>();
  return {
    ...actual,
    excludeRules: async () => ["/.hidden"],
    reindex: async (paths: string[]) => {
      if (reindexShouldFail) throw new Error("invalid argument");
      return { ...NEW_STATUS, paths };
    },
  };
});

vi.mock("../lib/settings-io", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../lib/settings-io")>();
  return {
    ...actual,
    loadSettings: async (): Promise<Settings> => ({
      // 面板里已经有一条索引目录 —— 点「应用」时它就是传给 reindex 的那组路径
      indexPaths: ["/tmp/fsearch-accept/dirB"],
      sortPref: undefined,
      kindPref: undefined,
      bookmarks: [],
      recentQueries: [],
      autoRefresh: autoRefreshOnDisk,
    }),
    patchSettings: async (
      patch: (s: Settings) => Partial<Settings> | null,
    ): Promise<Settings> => {
      if (patchShouldFail) throw new Error("磁盘满了");
      const delta = patch({
        indexPaths: [],
        sortPref: undefined,
        kindPref: undefined,
        bookmarks: [],
        recentQueries: [],
        autoRefresh: autoRefreshOnDisk,
      });
      patchDeltas.push(delta);
      if (delta?.autoRefresh !== undefined) autoRefreshOnDisk = delta.autoRefresh;
      return {
        indexPaths: [],
        sortPref: undefined,
        kindPref: undefined,
        bookmarks: [],
        recentQueries: [],
        autoRefresh: autoRefreshOnDisk,
      };
    },
  };
});

import SettingsPanel from "./SettingsPanel.vue";

/** 挂起设置面板，把 emit 出来的事件收进数组。 */
function mountPanel() {
  const host = document.createElement("div");
  document.body.appendChild(host);
  const events: { name: string; payload: unknown }[] = [];
  const app = createApp({
    render: () =>
      h(SettingsPanel, {
        onReindexed: (st: IndexStatus) => events.push({ name: "reindexed", payload: st }),
        onClose: () => events.push({ name: "close", payload: null }),
      }),
  });
  app.mount(host);
  return { app, host, events };
}

/** 等若干个微任务 + 一个宏任务（组件里有几层 await）。 */
async function flush(times = 6): Promise<void> {
  for (let i = 0; i < times; i++) {
    await nextTick();
    await new Promise((r) => setTimeout(r, 0));
  }
}

afterEach(() => {
  patchShouldFail = false;
  reindexShouldFail = false;
  autoRefreshOnDisk = true;
  patchDeltas = [];
  document.body.innerHTML = "";
});

describe("设置面板的「应用并重建索引」", () => {
  it("重建成功后要把新的 IndexStatus 带出去（父组件靠它刷新）", async () => {
    const { app, host, events } = mountPanel();
    await flush();

    (host.querySelector(".set-btn-primary") as HTMLButtonElement).click();
    await flush();

    const reindexed = events.find((e) => e.name === "reindexed");
    expect(reindexed).toBeDefined();
    // 载荷就是 reindex() 返回的那份状态（父组件拿它更新状态栏）
    expect(reindexed?.payload).toEqual(NEW_STATUS);
    // 全部成功 → 关闭抽屉
    expect(events.some((e) => e.name === "close")).toBe(true);
    app.unmount();
  });

  it("【回归】写盘失败时也要 emit reindexed —— 引擎已经换了，界面必须刷新", async () => {
    patchShouldFail = true;
    const { app, host, events } = mountPanel();
    await flush();

    (host.querySelector(".set-btn-primary") as HTMLButtonElement).click();
    await flush();

    // 引擎换了 → 界面必须收到通知（否则旧目录的结果还留在屏上）
    expect(events.some((e) => e.name === "reindexed")).toBe(true);
    // 写盘失败 → 抽屉**不能关**（要让人看见错误）
    expect(events.some((e) => e.name === "close")).toBe(false);
    // 而且要说清楚是哪一步坏了
    const err = host.querySelector(".set-error")?.textContent ?? "";
    expect(err).toContain("设置没有保存");
    app.unmount();
  });

  it("重建失败时：不 emit、抽屉不关、显示错误", async () => {
    reindexShouldFail = true;
    const { app, host, events } = mountPanel();
    await flush();

    (host.querySelector(".set-btn-primary") as HTMLButtonElement).click();
    await flush();

    // 引擎没换 → 界面不该收到「换好了」的通知
    expect(events.some((e) => e.name === "reindexed")).toBe(false);
    expect(events.some((e) => e.name === "close")).toBe(false);
    const err = host.querySelector(".set-error")?.textContent ?? "";
    expect(err).toContain("invalid argument");
    app.unmount();
  });
});

describe("设置面板的「文件变化时自动刷新索引」开关", () => {
  /** 面板里的那个开关按钮。 */
  function switchButton(host: HTMLElement): HTMLButtonElement {
    const btn = host.querySelector<HTMLButtonElement>(".set-switch");
    expect(btn, "设置面板里应该有「自动刷新」这个开关").toBeTruthy();
    return btn!;
  }

  it("默认是【开】的（aria-checked=true）", async () => {
    const { app, host } = mountPanel();
    await flush();
    expect(
      switchButton(host).getAttribute("aria-checked"),
      "自动刷新默认必须开",
    ).toBe("true");
    app.unmount();
  });

  it("拨一下开关 → 立刻写盘（autoRefresh: false），不用点「应用」", async () => {
    const { app, host } = mountPanel();
    await flush();

    switchButton(host).click();
    await flush();

    // 写盘了，而且写的正是这一个字段
    expect(patchDeltas.length, "拨开关应该写一次盘").toBeGreaterThan(0);
    expect(patchDeltas[patchDeltas.length - 1]?.autoRefresh).toBe(false);
    // 盘上的值真的变成 false 了
    expect(autoRefreshOnDisk).toBe(false);
    // 界面上的开关也跟着翻过去
    expect(switchButton(host).getAttribute("aria-checked")).toBe("false");

    app.unmount();
  });

  it("再拨一下 → 又变回 true（toggle 语义）", async () => {
    const { app, host } = mountPanel();
    await flush();

    switchButton(host).click();
    await flush();
    switchButton(host).click();
    await flush();

    expect(autoRefreshOnDisk).toBe(true);
    expect(switchButton(host).getAttribute("aria-checked")).toBe("true");

    app.unmount();
  });

  it("写盘失败 → 显示错误，开关状态不变（不假装存上了）", async () => {
    patchShouldFail = true;
    const { app, host } = mountPanel();
    await flush();

    switchButton(host).click();
    await flush();

    const err = host.querySelector(".set-error")?.textContent ?? "";
    expect(err, "写盘失败要看得见错误").toContain("自动刷新开关没有保存");
    // 盘上没变 → 界面也不该变
    expect(autoRefreshOnDisk).toBe(true);
    expect(switchButton(host).getAttribute("aria-checked")).toBe("true");

    app.unmount();
  });
});
