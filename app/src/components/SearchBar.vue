<script setup lang="ts">
import { computed, onMounted, ref } from "vue";
import { syntaxTables } from "../lib/api";
import { tokenize } from "../lib/tokenizer";
import type { SyntaxTables } from "../lib/types";

defineProps<{ busy: boolean }>();
const emit = defineEmits<{
  (e: "submit"): void;
}>();

/** 搜索框里的查询串 —— **必须用 `v-model` 绑定**。
 *
 *  【为什么不能手写 `:value` + `@input`】
 *  输入法组合期间，用户正在打的拼音**不能**被一次重渲染抹掉。
 *  Vue 的这层保护**只在 `v-model` 指令里**：
 *    - `vModelText.created`：给元素挂 `compositionstart` → `el.composing = true`
 *    - `vModelText.beforeUpdate`：`if (el.composing) return;` → 组合期【不】回写 `el.value`
 *    - `vModelText` 的 input 监听：`if (e.target.composing) return;` → 组合期【不】同步
 *  手写 `:value="modelValue"` 三层保护一个都拿不到：
 *    - `patchProps` 每次重渲染都会重新 patch `value`
 *    - `patchDOMProp` 发现 DOM 值与绑定值不同就 `el.value = newValue`
 *    → **正在打的拼音被写回旧查询串**
 *  而组合期【一定】会有一次重渲染（`composing` 变了 → class 变了 + 高亮层 `v-show`）。
 *
 *  【Phase 2 验收为什么没发现】当时的测法只验了「组合期能看到拼音」，
 *  没验「`compositionstart` 与写入落在【同一个任务】」这个时序 ——
 *  而真实输入法的第一个 `compositionupdate` 正好就落在同一个任务里。
 *  （回归测试见 `SearchBar.test.ts`。） */
const model = defineModel<string>({ required: true });

const tables = ref<SyntaxTables | null>(null);
const inputEl = ref<HTMLInputElement | null>(null);
const composing = ref(false);

onMounted(async () => {
  tables.value = await syntaxTables();
});

const tokens = computed(() =>
  tables.value ? tokenize(model.value, tables.value) : [],
);

/* 组合开始 —— 这里只管【样式】（高亮层藏起来、输入框自己的文字显出来）。
 * 值的保护由 `v-model` 指令负责（见上面 model 的说明）。 */
function onCompositionStart() {
  composing.value = true;
}

/* 组合结束 —— 样式复原。
 *  【值的同步不用我们管】`v-model` 指令自己会做：
 *  Vue 的 `onCompositionEnd`（runtime-dom/src/directives/vModel.ts）在组合结束时
 *  **补发一个 `input` 事件** —— 于是「有些浏览器组合结束时不发 input」那个坑
 *  也被 Vue 兜住了。我们再显式同步一次的话会**同步两次**（实测 emitted 里两条）。 */
function onCompositionEnd() {
  composing.value = false;
}

function onKeydown(e: KeyboardEvent) {
  // e.isComposing —— IME 组合时按 Enter 是「选词」，不是「提交搜索」
  if (e.key === "Enter" && !e.isComposing) {
    e.preventDefault();
    emit("submit");
  }
}

/* 暴露给父组件：全局快捷键 ⌘F 聚焦搜索框 */
function focus() {
  inputEl.value?.focus();
  inputEl.value?.select();
}

/* 暴露给父组件：聚焦并把光标放到【末尾】，【不】选中全文。
 *
 * ⌘F 用 focus()（选中全文好整体替换）；
 * 语法面板追加关键字后用这个 —— 选中文本的话，
 * 用户接着打的第一个字会把整条查询替换掉，
 * 正好毁掉「追加」想保住的东西。 */
function focusEnd() {
  const el = inputEl.value;
  if (!el) return;
  el.focus();
  const end = el.value.length;
  el.setSelectionRange(end, end);
}

defineExpose({ focus, focusEnd });
</script>

<template>
  <!-- 屏蔽标题栏的拖拽区：输入区点下去要能选字，不能拖窗口 -->
  <div class="field-wrap" data-tauri-drag-region="false">
    <div class="field" :class="{ 'is-composing': composing }">
      <svg class="field-ico"><use href="#i-search" /></svg>
      <div class="field-stack">
        <!-- 【为什么是 <div> 而不是 <pre>】
             <pre> 是 Vue 模板编译器的**空白保留特例**（跟 textarea 一样）——
             模板里的换行 + 缩进会被**原样渲染**：
                 _createTextVNode("          ", -1)   ← 10 个前导空格
                 _createTextVNode("\n        ", -1)   ← 尾部的换行 + 8 个空格
             加上 `white-space: pre-wrap`，这 10 个空格就变成了**可见的缩进**
             —— 高亮层的文字被推离输入框里的光标约 80px
             （用户看到的现象：光标在左边，文字在右边，中间空一大段）。

             换成 <div> 后 Vue 的空白 condense 会把那些「纯空白文本节点」删掉
             （编译产物里一个 createTextVNode 都不剩）。

             【查询串里的空格不会丢】它们不靠 <pre> 保：
               - tokenizer 把空格也当成一个 token 产出（lib/tokenizer.ts 的 push(c, "plain")）
               - `white-space: pre-wrap` 保住连续空格不被折叠
             回归测试见 SearchBar.test.ts 的「高亮层不得有多余空白」那条。 -->
        <div class="hl" aria-hidden="true" v-show="!composing">
          <span v-for="(t, i) in tokens" :key="i" :class="'tok-' + t.kind">{{ t.text }}</span>
        </div>
        <input
          ref="inputEl"
          class="field-input"
          v-model="model"
          spellcheck="false"
          placeholder="搜索文件名，或用 ext: / path: / size: 精确过滤"
          @keydown="onKeydown"
          @compositionstart="onCompositionStart"
          @compositionend="onCompositionEnd"
        />
      </div>
      <span v-if="busy" class="spinner" aria-label="搜索中" />
    </div>
  </div>
</template>
