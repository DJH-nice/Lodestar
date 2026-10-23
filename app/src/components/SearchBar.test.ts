// @vitest-environment happy-dom
/**
 * 搜索框的输入法（IME）组合期行为 —— **回归测试**。
 *
 *  【这个测试在修什么】Phase 3 验收（Task 6）发现：
 * 输入法组合期间，**正在打的拼音会被一次重渲染抹掉**。
 *
 *  【为什么以前没发现】Phase 2 的验收只验了「组合期能看到拼音」，
 * 没验「`compositionstart` 与写入落在【同一个任务】」这个时序 ——
 * 而真实输入法的第一个 `compositionupdate` 正好就落在同一个任务里。
 *
 *  【修法】`SearchBar.vue` 改用 `v-model`：
 * Vue 的组合期保护（`el.composing`）**只在 `v-model` 指令里** ——
 * 手写的 `:value` + `@input` 拿不到，Vue 会在每次重渲染时把
 * `el.value` 写回旧的查询串。
 *
 *  【怎么复现那个时序】下面第一个用例：
 * `compositionstart` → 写 `input.value` → `dispatchEvent("input")` → `await nextTick()`。
 * 组合期的那次重渲染就发生在 `nextTick()` 那一行 ——
 * 修之前，`input.value` 会被写回 "beta"（断言失败）；
 * 修之后，拼音留在输入框里。
 */

import { createApp, h, nextTick } from "vue";
import { afterEach, describe, expect, it, vi } from "vitest";

// `api.ts` 会去调 Tauri 的 IPC —— 测试环境里没有 `__TAURI_INTERNALS__`，
// 不 mock 的话 onMounted 里那个 await 会变成未处理的 rejection。
//
// 【用 importOriginal 而不是只列一个符号】—— 只列 `syntaxTables` 的话，
// 组件以后多 import 一个 api 符号就会「undefined is not a function」。
// 这里铺开真实模块、只替换要替换的那几个。
vi.mock("../lib/api", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../lib/api")>();
  return {
    ...actual,
    syntaxTables: async () => ({ modifiers: [], fields: [], operators: [] }),
  };
});

import SearchBar from "./SearchBar.vue";

/** 每个用例结束后要清理的挂载点。 */
const mounted: (() => void)[] = [];

/** 把 SearchBar 挂起来（没有 @vue/test-utils，用 createApp + h 凑）。 */
function mountSearchBar(initial: string) {
  const host = document.createElement("div");
  document.body.appendChild(host);
  const emitted: string[] = [];
  let value = initial;
  const app = createApp({
    render: () =>
      h(SearchBar, {
        modelValue: value,
        busy: false,
        "onUpdate:modelValue": (v: string) => {
          emitted.push(v);
          value = v;
        },
      }),
  });
  app.mount(host);
  // 【每个用例结束都要 unmount】—— 否则宿主 div 永久留在 document.body 里，
  // 后面的用例会看到上一次的残留 DOM。
  mounted.push(() => {
    app.unmount();
    host.remove();
  });
  const input = host.querySelector("input") as HTMLInputElement;
  return { app, host, input, emitted };
}

afterEach(() => {
  while (mounted.length) mounted.pop()!();
});

describe("SearchBar 的输入法组合期", () => {
  it("组合期重渲染不得把正在打的拼音抹掉", async () => {
    const { input, emitted } = mountSearchBar("beta");

    // 【复现真实输入法的时序】compositionstart 与写入在【同一个任务】里。
    input.dispatchEvent(new Event("compositionstart", { bubbles: true }));
    input.value = "zhongwen";
    input.dispatchEvent(new Event("input", { bubbles: true }));

    // 组合期的那次重渲染（composing 变了 → class 变了 + 高亮层 v-show）就在这一行
    await nextTick();

    // 拼音必须还在输入框里 —— 修之前这里会被写回 "beta"
    expect(input.value).toBe("zhongwen");
    // 组合期【不能】把值同步出去 —— 否则每敲一个拼音字母就触发一次搜索
    expect(emitted).toEqual([]);

    // 组合结束 → 同步一次
    input.dispatchEvent(new Event("compositionend", { bubbles: true }));
    await nextTick();
    expect(emitted).toEqual(["zhongwen"]);
  });

  it("组合期的高亮层要藏起来、输入框自己的文字要显出来", async () => {
    const { host, input } = mountSearchBar("beta");
    input.dispatchEvent(new Event("compositionstart", { bubbles: true }));
    await nextTick();

    const field = host.querySelector(".field") as HTMLElement;
    const hl = host.querySelector(".hl") as HTMLElement;
    expect(field.className).toContain("is-composing");
    // 高亮层藏起来（否则用户看到的是上一次的高亮文本）
    expect(hl.style.display).toBe("none");
  });

  it("非组合期的普通输入照常同步", async () => {
    const { input, emitted } = mountSearchBar("");
    input.value = "java";
    input.dispatchEvent(new Event("input", { bubbles: true }));
    await nextTick();
    expect(emitted).toEqual(["java"]);
  });
});

/**
 * 高亮层与输入框的**像素对齐**。
 *
 *  【这个测试在抓什么】搜索框是「高亮层（.hl）叠在透明 input 上面」的结构：
 * 可见的文字由 .hl 画，光标由 input 画 —— 两者**必须**从同一个 x 起点开始，
 * 否则用户看到的是「光标在左边、文字在右边，中间空一大段」。
 *
 *  【真实病因】.hl 原来是个 **<pre>**，而 <pre> 是 Vue 模板编译器的空白保留特例：
 * 模板里的换行 + 10 个缩进空格被**原样渲染**出来
 * （编译产物里能看到 `createTextVNode("          ")`）。
 * 加上 `white-space: pre-wrap`，那 10 个空格就成了可见缩进 → 文字偏移约 80px。
 *
 *  【为什么这条测试抓得住】happy-dom 没有布局引擎，量不了像素；
 * 但**空白是 DOM 层面的事实** —— .hl 的 textContent 必须**逐字等于**查询串。
 * 用 <pre> 时它是 `"          java\n        "` → 直接挂。
 */
describe("SearchBar 高亮层的空白", () => {
  /** 等语法表加载完 —— `.hl` 的内容来自 tokenize(query, tables)，
   *  tables 是 onMounted 里异步取的，没加载完时 `.hl` 是空的。 */
  async function flush() {
    await new Promise((r) => setTimeout(r, 0));
    await nextTick();
  }

  it("高亮层的内容必须逐字等于查询串（不得有多余的前后空白）", async () => {
    const { host } = mountSearchBar("java");
    await flush();
    const hl = host.querySelector(".hl") as HTMLElement;
    expect(hl.textContent).toBe("java");
  });

  it("查询串里的空格要保住，但不能多出别的空白", async () => {
    const { host } = mountSearchBar("size:>=0 ext:txt");
    await flush();
    const hl = host.querySelector(".hl") as HTMLElement;
    // 空格是查询串的一部分（tokenizer 把它当 token 产出）→ 必须原样保留
    expect(hl.textContent).toBe("size:>=0 ext:txt");
    // 但不得有多余的前导/尾随空白
    expect(hl.textContent).not.toMatch(/^\s/);
    expect(hl.textContent).not.toMatch(/\s$/);
  });

  it("连续空格也要原样保留（不能被折叠）", async () => {
    const { host } = mountSearchBar("a  b");
    await flush();
    const hl = host.querySelector(".hl") as HTMLElement;
    expect(hl.textContent).toBe("a  b");
  });
});
