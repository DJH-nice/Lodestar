import { describe, expect, it } from "vitest";
import {
  applyButtonText,
  applyGuardReason,
  canApplySettings,
  loadFailedHint,
  shouldCloseOnBackdrop,
} from "./settings-panel";

describe("canApplySettings", () => {
  /* ---- 加载失败时【不能】让「应用」可点 ----
     加载失败时 settings 还是默认值，一按就把用户真正的设置覆盖掉。 */
  it("加载失败 → 不能应用", () => {
    expect(canApplySettings("failed", false)).toBe(false);
  });

  it("还在加载 → 不能应用", () => {
    expect(canApplySettings("loading", false)).toBe(false);
  });

  it("加载好了 → 能应用", () => {
    expect(canApplySettings("ready", false)).toBe(true);
  });

  it("重建中 → 不能应用（避免连点两次）", () => {
    expect(canApplySettings("ready", true)).toBe(false);
  });
});

describe("applyGuardReason", () => {
  /* ---- apply() 的逻辑层守卫（UI 的 disabled 之外的那道）----
     这条路径出事故 = 用户真正的设置被默认值覆盖（丢书签 / 最近搜索）。 */
  it("加载失败 → 挡住，并说清楚为什么", () => {
    const reason = applyGuardReason("failed");
    expect(reason).toContain("不能应用");
    expect(reason).toContain("覆盖");
  });

  it("还在加载 → 同样挡住", () => {
    expect(applyGuardReason("loading")).toContain("不能应用");
  });

  it("加载好了 → 放行（null）", () => {
    expect(applyGuardReason("ready")).toBeNull();
  });
});

describe("applyButtonText", () => {
  it("加载中", () => {
    expect(applyButtonText("loading", false)).toBe("正在加载设置…");
  });

  it("加载好了", () => {
    expect(applyButtonText("ready", false)).toBe("应用并重建索引");
  });

  it("加载失败 —— 按钮上要写明失败，不能还是「应用并重建索引」", () => {
    expect(applyButtonText("failed", false)).toBe("加载失败，无法应用");
  });

  it("重建中优先显示重建状态", () => {
    expect(applyButtonText("ready", true)).toBe("正在重建索引…");
    /* 【删掉了一个不可达组合】原来这里还有一条
     *   expect(applyButtonText("failed", true)).toBe("正在重建索引…")
     * —— 它测的是一个【到不了】的状态组合：
     * busy 只在 apply() 里置 true，而 apply() 只在 loadState === "ready"
     * 时才可能触发（按钮 disabled + 逻辑层守卫），loadState 之后也不会
     * 再变 —— 所以 busy === true 时 loadState 必然是 "ready"。
     * 留着它等于给一条死路径写断言：既测不出真问题，
     * 又把「失败时按钮该显示什么」这件事糊过去（万一将来
     * 组合变得可达，这条断言还会锁死一个骗人的文案）。 */
  });
});

describe("loadFailedHint", () => {
  it("说清楚了失败，也说清楚了为什么不能应用", () => {
    const hint = loadFailedHint("boom");
    expect(hint).toContain("加载设置失败");
    expect(hint).toContain("boom");
    expect(hint).toContain("已禁用");
  });
});

describe("shouldCloseOnBackdrop", () => {
  /* 重建进行中点遮罩会把抽屉关掉 → 组件卸载 → 错误信息跟着没了。 */
  it("重建中 → 点遮罩不关抽屉", () => {
    expect(shouldCloseOnBackdrop(true)).toBe(false);
  });

  it("空闲时 → 点遮罩关抽屉", () => {
    expect(shouldCloseOnBackdrop(false)).toBe(true);
  });
});
