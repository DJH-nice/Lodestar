/** 全局快捷键的「录制」—— 把一次按键变成后端能认的规格字符串。
 *
 *  【为什么用 `event.code` 而不是 `event.key`】
 *   `key` 受输入法 / 键盘布局影响（同一个物理键在不同布局下 `key` 不同），
 *   `code` 是**物理位置**，稳。后端（global-hotkey）认的也是物理键。
 *
 *  【后端语法】修饰键在前、只能有一个主键：
 *   "Option+Cmd+Space" / "Ctrl+Option+Shift+Cmd+KeyF"
 *   主键名用 `KeyboardEvent.code`（KeyA / Digit1 / Space / F1 / ArrowUp …），
 *   它们跟 global-hotkey 的 `parse_key` 认的名字是对得上的。
 */

import type { ShortcutChange, ShortcutRuntimeState } from "./types";

/** 修饰键的标志位。 */
export interface ModState {
  ctrlKey: boolean;
  altKey: boolean;
  shiftKey: boolean;
  metaKey: boolean;
}

/** 一次 keydown 事件里我们关心的部分。 */
export interface KeyEventLike extends ModState {
  /** `KeyboardEvent.code` */
  code: string;
}

export interface RecordedShortcut {
  /** 后端规格，如 "Option+Cmd+Space" */
  spec: string;
  /** 给人看的，如 "⌥⌘Space" */
  label: string;
}

/** 修饰键 → 后端语法里的名字。**顺序固定**（后端解析要求修饰键在前）。 */
const MOD_ORDER: ReadonlyArray<readonly [keyof ModState, string]> = [
  ["ctrlKey", "Ctrl"],
  ["altKey", "Option"],
  ["shiftKey", "Shift"],
  ["metaKey", "Cmd"],
];

/** 修饰键本身 —— 不能当主键（只按了它就结束录制是不对的）。 */
const MODIFIER_CODES = new Set([
  "ControlLeft",
  "ControlRight",
  "AltLeft",
  "AltRight",
  "ShiftLeft",
  "ShiftRight",
  "MetaLeft",
  "MetaRight",
  "CapsLock",
  "OSLeft",
  "OSRight",
]);

/** 这个键码是**修饰键**吗（修饰键不能当主键）。 */
export function isModifierCode(code: string): boolean {
  return MODIFIER_CODES.has(code);
}

/** 后端（`global-hotkey` 的 `parse_key`）认得的**具名主键**。
 *
 *  【这份白名单是怎么来的】
 *   逐条对着 `global-hotkey 0.8.0/src/hotkey.rs` 的 `parse_key` 抄的
 *   —— 那是后端**唯一**认得的那批名字。
 *   （它认的是 `KeyboardEvent.code` 的写法：`KeyA` / `Digit1` / `Space` /
 *     `ArrowUp` / `F1` …）
 */
const SUPPORTED_MAIN_KEYS = new Set([
  "Backquote",
  "Backslash",
  "BracketLeft",
  "BracketRight",
  "Pause",
  "Comma",
  "Equal",
  "Minus",
  "Period",
  "Quote",
  "Semicolon",
  "Slash",
  "Backspace",
  "Enter",
  "Space",
  "Tab",
  "Delete",
  "End",
  "Home",
  "Insert",
  "PageDown",
  "PageUp",
  "PrintScreen",
  "ScrollLock",
  "ArrowDown",
  "ArrowLeft",
  "ArrowRight",
  "ArrowUp",
  "NumLock",
  "NumpadAdd",
  "NumpadSubtract",
  "NumpadMultiply",
  "NumpadDivide",
  "NumpadDecimal",
  "NumpadEnter",
  "NumpadEqual",
  "Escape",
]);

/** 成组的主键（字母 / 数字 / 小键盘数字 / 功能键）。 */
const SUPPORTED_MAIN_KEY_PATTERNS: RegExp[] = [
  /^Key[A-Z]$/, // 字母
  /^Digit[0-9]$/, // 顶排数字
  /^Numpad[0-9]$/, // 小键盘数字
  /^F([1-9]|1[0-9]|2[0-4])$/, // 功能键 F1~F24
];

/** 这个键码能当**主键**吗（后端认得）。
 *
 *  【为什么要前端先验一道】（终审 M-7）
 *   后端只认固定的一批键名。用户按了 `Fn`、媒体键之类它不认的键时，
 *   以前要**等后端回一句「快捷键格式非法」**才知道 ——
 *   用户已经看到按钮变了，才被告知不能用。
 *   → 录制时**当场**校验并说清「这个键不能用」。
 */
export function isSupportedMainKey(code: string): boolean {
  if (SUPPORTED_MAIN_KEYS.has(code)) return true;
  return SUPPORTED_MAIN_KEY_PATTERNS.some((re) => re.test(code));
}

/** 修饰键的 macOS 符号。
 *
 *  【键名必须与 `MOD_ORDER` 里产出的**一模一样**】
 *   `MOD_ORDER` 把 altKey 映射成后端语法里的 **"Option"**（global-hotkey
 *   认的是 Option / Ctrl / Shift / Cmd），所以这里的键也得是 "Option"。
 *   （写成 "Alt" 的话 → `MOD_LABEL["Option"]` 取不到 → 标签变成
 *     "Option⌘Space" —— 符号没生效，界面上一半英文一半符号。）
 *
 *  【为什么还有几个别名】后端（`global-hotkey`）认的名字不止一种：
 *   "OPTION"|"ALT"、"CONTROL"|"CTRL"、"COMMAND"|"CMD"|"SUPER"、"SHIFT"
 *   —— 设置文件被手改过（或者别的软件写进去的）时可能用的是别名，
 *   那些也要能显示成符号。 */
const MOD_LABEL: Record<string, string> = {
  Ctrl: "⌃",
  Control: "⌃",
  Option: "⌥",
  Alt: "⌥",
  Shift: "⇧",
  Cmd: "⌘",
  Command: "⌘",
  Super: "⌘",
};

/** **不认识**的修饰键 —— 显示成「未知」。
 *
 *  【为什么不能原样输出】设置文件被手改坏时（"Hyper+Cmd+Space"），
 *   原样输出就是一串英文甩在按钮上 —— 用户看不懂，还以为那是键名。
 *   「未知」才说得清「这里有个看不懂的东西」。
 *   （面板的「清除」按钮能把它清掉。） */
const UNKNOWN_MOD_LABEL = "未知";

/** 一些主键的专属写法。
 *
 *  【为什么没有 `Escape`】
 *   录制时 `Escape` 被当作「取消」拦掉了（见 `SettingsPanel.vue`），
 *   这个 label 永远用不上 —— 是**死数据**，别留着（终审 M-1）。 */
const KEY_LABEL: Record<string, string> = {
  Space: "Space",
  Enter: "↩",
  Tab: "⇥",
  Backspace: "⌫",
  Delete: "⌦",
  ArrowUp: "↑",
  ArrowDown: "↓",
  ArrowLeft: "←",
  ArrowRight: "→",
};

/** 把一次 keydown 变成快捷键规格。
 *
 *  返回 `null` = 这一下**还不能**结束录制：
 *   - 只按了修饰键（还在等主键）
 *   - 没有任何修饰键（会跟正常打字打架，拒绝）
 *   - 主键后端**不认**（如 `Fn`、媒体键）—— 见 [`isSupportedMainKey`]
 */
export function shortcutFromEvent(e: KeyEventLike): RecordedShortcut | null {
  if (isModifierCode(e.code)) return null;
  // 【前端预校验】后端不认的主键**不生成规格** ——
  //   与其把一个注定被拒的规格发过去等后端报错，不如压根不生成。
  if (!isSupportedMainKey(e.code)) return null;

  const mods = MOD_ORDER.filter(([k]) => e[k]).map(([, name]) => name);
  if (mods.length === 0) return null;

  const spec = [...mods, e.code].join("+");
  return { spec, label: labelFor(mods, e.code) };
}

/** 从**后端规格**反推给人看的标签 —— "Option+Cmd+Space" → "⌥⌘Space"。
 *
 *  【为什么需要它】设置面板显示的是**标签**（⌥⌘Space），
 *   而设置里存的是**规格**（Option+Cmd+Space）——
 *   两者不是一回事。面板打开时手里只有规格，得反推成标签才能显示。
 *
 *  【为什么不能直接显示规格】那玩意儿是给 `global-hotkey` 解析用的，
 *   出现在用户眼前既不统一（录完一遍之后又变成符号了）、也不好认。
 *
 *  【规格的形状】修饰键在前、**主键在最后** —— 所以最后一个段是主键，
 *   前面几段都是修饰键。（与 `MOD_ORDER` 产出的形状一致。） */
export function labelFromSpec(spec: string): string {
  const parts = spec.split("+").filter((p) => p.length > 0);
  if (parts.length === 0) return "";
  const key = parts[parts.length - 1];
  return labelFor(parts.slice(0, -1), key);
}

/** 修饰键 + 主键 → 标签。 */
function labelFor(mods: string[], code: string): string {
  const m = mods.map((x) => MOD_LABEL[x] ?? UNKNOWN_MOD_LABEL).join("");
  const k =
    KEY_LABEL[code] ??
    code.replace(/^Key/, "").replace(/^Digit/, "");
  return m + k;
}

/** 快捷键变更之后，界面该显示成什么样 —— **纯函数**（好测）。 */
export interface ShortcutView {
  /** 当前快捷键（后端规格）。`null` = 确实没有键生效。 */
  spec: string | null;
  /** 当前快捷键（给人看的）。 */
  label: string;
  /** 要显示的错误 / 警告（`null` = 没有）。 */
  error: string | null;
}

/** 把后端返回的变更结果变成界面该显示的样子。
 *
 *  【为什么必须是纯函数】这是**前后端契约的落地点** ——
 *   面板「显示哪个快捷键」「什么时候显示警告」全在这里。
 *   抽成纯函数才能测（组件本身在 vitest 里测起来很别扭）。
 *
 *  【关键】显示的快捷键是 **`effective`（实际生效的那个）**，
 *   **不是**用户刚录的那个 ——
 *   录的那个可能压根没注册上（被别的软件占着），
 *   这时显示它就是在骗人。
 */
export function shortcutViewFromChange(res: ShortcutChange): ShortcutView {
  return {
    spec: res.effective,
    label: res.effective ? labelFromSpec(res.effective) : "",
    error: res.warning,
  };
}

/** 面板**打开时**该显示成什么样 —— **纯函数**（好测）。
 *
 *  【为什么要有这个函数】
 *   设置里存的是**用户的偏好**（「我想用 ⌥⌘Space」），
 *   而面板要显示的是「**此刻按下去会不会唤起 Lodestar**」。
 *   两者会分叉 —— 终审 I-1 的那条证据链：
 *     启动时 ⌥⌘Space 被别的软件占着 → 注册失败 →
 *     设置里**一个字都没改**（偏好留着，好让用户重试）→
 *     面板若直接显示设置里的值 = **骗人说「它已生效」**。
 *
 *  【两条输入】
 *   - `runtime` —— 后端问出来的**真相**（`null` = 问不到）
 *   - `desired` —— 设置里存的**偏好**（可能 `null` = 没设过）
 */
export interface ShortcutRuntimeView extends ShortcutView {
  /** 设置里存的偏好（面板还要用它决定「清除」按钮该不该出现）。 */
  desired: string | null;
}

/** 面板打开时该显示成什么样 —— 由「真相 + 偏好」算出来。
 *
 *  【关键】显示的永远是 **`effective`（实际生效的那个）**，
 *   不是设置里存的那个 —— 与 [`shortcutViewFromChange`] 同一个原则：
 *   「设置 = 实际生效的键」。
 */
export function shortcutRuntimeView(
  runtime: ShortcutRuntimeState | null,
  desired: string | null,
): ShortcutRuntimeView {
  // 问不到真相 → 退回偏好，但**必须说清「不确定」**
  // （闷声显示偏好 = 又一次骗人）。
  if (!runtime) {
    return {
      spec: desired,
      label: desired ? labelFromSpec(desired) : "",
      error: "读不到快捷键的实际生效状态（下面的显示可能不准）",
      desired,
    };
  }
  const spec = runtime.effective;
  const label = spec ? labelFromSpec(spec) : "";
  // ① 后端说出了问题 → 用它的话（它知道**为什么**）
  if (runtime.error) {
    return { spec, label, error: runtime.error, desired };
  }
  // ② 偏好与真相不一致 → **两个都要说**
  //   （只说一半 = 用户不知道自己按的到底会不会唤起。）
  if (desired && desired !== spec) {
    return {
      spec,
      label,
      error: `设置的快捷键「${labelFromSpec(desired)}」与实际生效的不一致（现在生效的是「${
        label || "无"
      }」）`,
      desired,
    };
  }
  return { spec, label, error: null, desired };
}
