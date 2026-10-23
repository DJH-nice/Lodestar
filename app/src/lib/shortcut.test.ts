import { describe, expect, it } from "vitest";
import {
  isModifierCode,
  isSupportedMainKey,
  labelFromSpec,
  shortcutFromEvent,
  shortcutRuntimeView,
  shortcutViewFromChange,
  type KeyEventLike,
} from "./shortcut";

function ev(partial: Partial<KeyEventLike>): KeyEventLike {
  return {
    code: "KeyA",
    ctrlKey: false,
    altKey: false,
    shiftKey: false,
    metaKey: false,
    ...partial,
  };
}

describe("shortcutFromEvent", () => {
  it("把 ⌥⌘Space 转成后端规格", () => {
    const r = shortcutFromEvent(
      ev({ code: "Space", altKey: true, metaKey: true }),
    );
    expect(r?.spec).toBe("Option+Cmd+Space");
  });

  it("修饰键顺序固定（Ctrl → Option → Shift → Cmd）", () => {
    const r = shortcutFromEvent(
      ev({ code: "KeyF", ctrlKey: true, altKey: true, shiftKey: true, metaKey: true }),
    );
    expect(r?.spec).toBe("Ctrl+Option+Shift+Cmd+KeyF");
  });

  it("没有修饰键 → 拒绝（null）", () => {
    expect(shortcutFromEvent(ev({ code: "KeyA" }))).toBeNull();
    expect(shortcutFromEvent(ev({ code: "Space" }))).toBeNull();
  });

  it("只按了修饰键 → 拒绝（null），继续等用户按主键", () => {
    expect(shortcutFromEvent(ev({ code: "MetaLeft", metaKey: true }))).toBeNull();
    expect(shortcutFromEvent(ev({ code: "ShiftLeft", shiftKey: true }))).toBeNull();
    expect(shortcutFromEvent(ev({ code: "ControlRight", ctrlKey: true }))).toBeNull();
  });

  it("用 code 而不是 key —— 所以 ⌘ 出来是 MetaLeft 也不影响主键", () => {
    const r = shortcutFromEvent(ev({ code: "KeyA", metaKey: true }));
    expect(r?.spec).toBe("Cmd+KeyA");
  });

  it("给人看的标签用 macOS 符号", () => {
    const r = shortcutFromEvent(ev({ code: "Space", altKey: true, metaKey: true }));
    expect(r?.label).toBe("⌥⌘Space");
  });

  it("字母键的标签去掉 Key 前缀", () => {
    const r = shortcutFromEvent(ev({ code: "KeyK", metaKey: true }));
    expect(r?.label).toBe("⌘K");
  });

  it("数字键的标签去掉 Digit 前缀", () => {
    const r = shortcutFromEvent(ev({ code: "Digit7", ctrlKey: true }));
    expect(r?.label).toBe("⌃7");
  });
});

describe("isSupportedMainKey（前端预校验）", () => {
  it("后端认得的键 → 通过", () => {
    for (const code of [
      "KeyA",
      "KeyZ",
      "Digit0",
      "Digit9",
      "Space",
      "Enter",
      "Tab",
      "ArrowUp",
      "ArrowLeft",
      "F1",
      "F12",
      "F24",
      "Numpad0",
      "NumpadEnter",
      "Minus",
      "Slash",
    ]) {
      expect(isSupportedMainKey(code), `${code} 应该是能用的主键`).toBe(true);
    }
  });

  it("后端不认的键 → 拦下（当场告诉用户，别等后端报 Invalid）", () => {
    for (const code of [
      "Fn",
      "FnLeft",
      "Unidentified",
      "MediaPlayPause",
      "AudioVolumeUp",
      "ContextMenu",
      "F25",
      "Key",
      "Digit10",
    ]) {
      expect(isSupportedMainKey(code), `${code} 后端不认`).toBe(false);
    }
  });

  it("修饰键不是主键 —— 但要认得出它是修饰键", () => {
    expect(isSupportedMainKey("MetaLeft")).toBe(false);
    expect(isModifierCode("MetaLeft")).toBe(true);
    expect(isModifierCode("ShiftRight")).toBe(true);
    expect(isModifierCode("KeyA")).toBe(false);
  });

  it("后端不认的键 → 不生成规格（省得发过去再被拒）", () => {
    const r = shortcutFromEvent(ev({ code: "Fn", metaKey: true, altKey: true }));
    expect(r).toBeNull();
  });
});

describe("labelFromSpec", () => {
  it("从后端规格反推标签（面板打开时用它显示当前快捷键）", () => {
    expect(labelFromSpec("Option+Cmd+Space")).toBe("⌥⌘Space");
    expect(labelFromSpec("Cmd+KeyA")).toBe("⌘A");
    expect(labelFromSpec("Ctrl+Digit7")).toBe("⌃7");
  });

  it("空规格 → 空标签", () => {
    expect(labelFromSpec("")).toBe("");
  });

  it("不认识的修饰键 → 「未知」，不甩一串英文给用户", () => {
    // 设置文件被手改坏时（"Hyper+Cmd+Space"）不能原样显示。
    expect(labelFromSpec("Hyper+Cmd+Space")).toBe("未知⌘Space");
  });

  it("后端认的别名（Alt / Control / Command）也能显示成符号", () => {
    expect(labelFromSpec("Alt+Cmd+Space")).toBe("⌥⌘Space");
    expect(labelFromSpec("Control+KeyA")).toBe("⌃A");
    expect(labelFromSpec("Command+Space")).toBe("⌘Space");
  });
});

describe("shortcutViewFromChange", () => {
  it("显示的是【实际生效】的键 —— 不是用户刚录的那个", () => {
    // 注册失败：用户录了 ⌘K，但实际生效的还是旧的 ⌥⌘Space。
    // 面板若显示 ⌘K 就是在骗人（那个键压根没注册上）。
    const view = shortcutViewFromChange({
      effective: "Option+Cmd+Space",
      warning: "快捷键「Cmd+KeyK」注册失败（可能已被别的软件占用）：已被占用",
    });
    expect(view.spec).toBe("Option+Cmd+Space");
    expect(view.label).toBe("⌥⌘Space");
    expect(view.error).toContain("注册失败");
  });

  it("warning 非空 → 面板要把它显示出来", () => {
    const view = shortcutViewFromChange({
      effective: "Cmd+KeyK",
      warning: "旧快捷键「Option+Cmd+Space」注销失败（…）：注销失败",
    });
    expect(view.error).toContain("注销失败");
  });

  it("没有 warning → 不显示错误", () => {
    const view = shortcutViewFromChange({ effective: "Cmd+KeyK", warning: null });
    expect(view.spec).toBe("Cmd+KeyK");
    expect(view.label).toBe("⌘K");
    expect(view.error).toBeNull();
  });

  it("effective 为 null → 显示「未设置」", () => {
    const view = shortcutViewFromChange({ effective: null, warning: null });
    expect(view.spec).toBeNull();
    expect(view.label).toBe("");
  });
});

describe("shortcutRuntimeView", () => {
  it("启动时注册失败 → 显示「未生效」，并把原因说出来", () => {
    // 终审 I-1 的原始场景：
    //   设置里仍然写着 Option+Cmd+Space（偏好留着，好让用户重试），
    //   但注册失败 → **一个键都没生效**。
    //   面板若显示「⌥⌘Space 已生效」= 骗人。
    const view = shortcutRuntimeView(
      {
        effective: null,
        error: "快捷键「Option+Cmd+Space」注册失败（可能已被别的软件占用）：已被占用",
      },
      "Option+Cmd+Space",
    );
    expect(view.spec).toBeNull();
    expect(view.label).toBe("");
    expect(view.error).toContain("注册失败");
    // 偏好要留着 —— 用户重试时还要用它。
    expect(view.desired).toBe("Option+Cmd+Space");
  });

  it("真相与偏好一致 → 显示它、没有警告", () => {
    const view = shortcutRuntimeView(
      { effective: "Option+Cmd+Space", error: null },
      "Option+Cmd+Space",
    );
    expect(view.spec).toBe("Option+Cmd+Space");
    expect(view.label).toBe("⌥⌘Space");
    expect(view.error).toBeNull();
  });

  it("真相与偏好不一致（且后端没给原因）→ 两个都要说", () => {
    // 例如：换键成功但写盘失败 —— 生效的是新键，设置里还���旧键。
    const view = shortcutRuntimeView(
      { effective: "Cmd+KeyK", error: null },
      "Option+Cmd+Space",
    );
    expect(view.spec).toBe("Cmd+KeyK");
    expect(view.error).toContain("⌥⌘Space");
    expect(view.error).toContain("⌘K");
  });

  it("问不到真相 → 退回偏好，但**说清「不确定」**", () => {
    // 闷声显示偏好 = 又一次骗人（用户会以为它生效了）。
    const view = shortcutRuntimeView(null, "Option+Cmd+Space");
    expect(view.spec).toBe("Option+Cmd+Space");
    expect(view.error).toContain("读不到");
  });

  it("没设过快捷键 → 没有键、没有警告", () => {
    const view = shortcutRuntimeView({ effective: null, error: null }, null);
    expect(view.spec).toBeNull();
    expect(view.label).toBe("");
    expect(view.error).toBeNull();
  });
});
