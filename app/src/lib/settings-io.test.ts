import { beforeEach, describe, expect, it, vi } from "vitest";
import { patchSettings } from "./settings-io";
import type { Settings } from "./types";

/** 假设置存储 —— 记住盘上的真值 + 记录调用次数。 */
const mock = vi.hoisted(() => {
  const state = {
    /** 盘上的设置（假存储里的那份真值）。 */
    value: null as Settings | null,
    loadCount: 0,
    saveCount: 0,
    /** 走过的 IPC 命令名 —— 验「命令名没写错」。 */
    commands: [] as string[],
  };
  const loadSettings = vi.fn(async () => {
    state.loadCount += 1;
    return state.value;
  });
  const saveSettings = vi.fn(async (s: Settings) => {
    state.saveCount += 1;
    state.value = s;
  });
  /* patch_settings 的语义**照抄 Rust 侧 settings::patch**（settings.rs:259）：
     「读盘上的真值 → 逐键覆盖 delta → 写回」。
     没出现在 delta 里的键**一律保留** —— 这正是「不把索引目录写没」的保证。
     （前端不再自己「整体写回」，差量交给 Rust 在进程内的写锁里合并，
       终审 M-10 —— 否则前端那份可能已旧的对象会把 Rust 侧的改动盖回去。） */
  const patchSettingsOnDisk = vi.fn(async (delta: Partial<Settings>) => {
    state.saveCount += 1;
    state.value = { ...(state.value as Settings), ...delta };
    return state.value;
  });
  /* mock 的是 IPC 层 —— 设置的读写现在都在 settings-io.ts 里，
     命令名写错的话这里会炸（正好当断言用）。 */
  const invoke = vi.fn(async (cmd: string, args?: Record<string, unknown>) => {
    state.commands.push(cmd);
    if (cmd === "get_settings") return loadSettings();
    if (cmd === "patch_settings") {
      return patchSettingsOnDisk(args?.delta as Partial<Settings>);
    }
    throw new Error(`未知命令 ${cmd}`);
  });
  return { state, loadSettings, saveSettings, patchSettingsOnDisk, invoke };
});

vi.mock("@tauri-apps/api/core", () => ({ invoke: mock.invoke }));

/** 一份完整的设置 —— 用来验「写入的是完整对象」。 */
function fullSettings(): Settings {
  return {
    indexPaths: ["/Users/x/Documents"],
    sortPref: "size",
    kindPref: "files",
    bookmarks: [{ kind: "query", value: "ext:rs" }],
    recentQueries: ["old"],
    windowWidth: 900,
    windowHeight: 600,
  };
}

beforeEach(() => {
  mock.state.value = fullSettings();
  mock.state.loadCount = 0;
  mock.state.saveCount = 0;
  mock.state.commands = [];
  mock.loadSettings.mockClear();
  mock.saveSettings.mockClear();
  mock.patchSettingsOnDisk.mockClear();
  mock.invoke.mockClear();
});

describe("patchSettings", () => {
  /* ---- 红线：写下去必须是【完整对象】，不是只改了一个字段 ----
     【口径变了】前端现在只送**差量**给 Rust（`patch_settings`），
       「合并成完整对象」这件事发生在 Rust 的写锁里 ——
       所以这里断言的是**合并之后**盘上的那份是完整的。 */
  it("只改一个字段，写下去的仍然是完整对象", async () => {
    await patchSettings((s) => ({ recentQueries: [...s.recentQueries, "new"] }));
    // 前端送出去的只是差量（没提的字段一个都不带）
    const delta = mock.patchSettingsOnDisk.mock.calls[0][0];
    expect(Object.keys(delta)).toEqual(["recentQueries"]);
    // 合并之后（Rust 那边做的事）—— 完整对象，没改的字段一个都不能少
    const written = mock.state.value!;
    expect(written.recentQueries).toEqual(["old", "new"]);
    expect(written.indexPaths).toEqual(["/Users/x/Documents"]);
    expect(written.sortPref).toBe("size");
    expect(written.kindPref).toBe("files");
    expect(written.bookmarks).toEqual([{ kind: "query", value: "ext:rs" }]);
    expect(written.windowWidth).toBe(900);
  });

  /* ---- 最严重的后果：把索引目录写没 ---- */
  it("改书签不会把索引目录写没", async () => {
    await patchSettings((s) => ({
      bookmarks: [...s.bookmarks, { kind: "query", value: "ext:md" }],
    }));
    const written = mock.state.value!;
    expect(written.indexPaths).toEqual(["/Users/x/Documents"]);
    expect(written.bookmarks).toHaveLength(2);
  });

  /* ---- 不能用调用方手里那份旧的（可能已经被别的面板改过）---- */
  it("写入前重新读盘上的真值", async () => {
    // 盘上的值已经被别的写入改过了
    mock.state.value = { ...fullSettings(), indexPaths: ["/Users/x/Downloads"] };
    const saved = await patchSettings(() => ({ recentQueries: ["r"] }));
    expect(saved.indexPaths).toEqual(["/Users/x/Downloads"]);
    expect(mock.loadSettings).toHaveBeenCalledTimes(1);
  });

  /* ---- 串行化：两个写入不能互相覆盖 ---- */
  it("两次并发写入不会互相覆盖", async () => {
    await Promise.all([
      patchSettings((s) => ({ recentQueries: [...s.recentQueries, "q1"] })),
      patchSettings((s) => ({
        bookmarks: [...s.bookmarks, { kind: "query", value: "q2" }],
      })),
    ]);
    const final = mock.state.value!;
    expect(final.recentQueries).toEqual(["old", "q1"]);
    expect(final.bookmarks).toHaveLength(2);
    expect(final.bookmarks[1]).toEqual({ kind: "query", value: "q2" });
  });

  it("并发写入时后一个看得到前一个的结果", async () => {
    // 两次都改 recentQueries：串行化之后应该是累积的，不是后一次覆盖前一次
    await Promise.all([
      patchSettings((s) => ({ recentQueries: [...s.recentQueries, "q1"] })),
      patchSettings((s) => ({ recentQueries: [...s.recentQueries, "q2"] })),
    ]);
    expect(mock.state.value!.recentQueries).toEqual(["old", "q1", "q2"]);
  });

  /* ---- 没有要改的 → 不写盘 ---- */
  it("差量为 null → 不写", async () => {
    const saved = await patchSettings(() => null);
    expect(mock.patchSettingsOnDisk).not.toHaveBeenCalled();
    expect(saved.recentQueries).toEqual(["old"]);
  });

  /* ---- 一次失败不能把队列卡死 ---- */
  it("一次写入失败之后，后面的写入照常能写", async () => {
    mock.patchSettingsOnDisk.mockRejectedValueOnce(new Error("boom"));
    await expect(patchSettings(() => ({ recentQueries: ["x"] }))).rejects.toThrow("boom");
    const saved = await patchSettings(() => ({ recentQueries: ["y"] }));
    expect(saved.recentQueries).toEqual(["y"]);
    expect(mock.state.value!.recentQueries).toEqual(["y"]);
  });

  it("写入失败会把错误抛给调用方（不能静默吞掉）", async () => {
    mock.patchSettingsOnDisk.mockRejectedValueOnce(new Error("磁盘满了"));
    await expect(patchSettings(() => ({ recentQueries: ["x"] }))).rejects.toThrow(
      "磁盘满了",
    );
  });

  /* ---- IPC 契约：命令名不能写错 ---- */
  it("走的是 get_settings / patch_settings 两个命令", async () => {
    await patchSettings(() => ({ recentQueries: ["x"] }));
    expect(mock.state.commands).toEqual(["get_settings", "patch_settings"]);
  });

  it("patch_settings 的参数名是 delta（Rust 侧按名字取）", async () => {
    await patchSettings(() => ({ recentQueries: ["x"] }));
    const call = mock.invoke.mock.calls.find((c) => c[0] === "patch_settings");
    expect(call).toBeTruthy();
    expect(Object.keys((call![1] ?? {}) as object)).toEqual(["delta"]);
  });
});
