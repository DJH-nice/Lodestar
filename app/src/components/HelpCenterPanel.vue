<script setup lang="ts">
import HelpPanel from "./HelpPanel.vue";
import SyntaxCheatsheet from "./SyntaxCheatsheet.vue";
import UserGuidePanel from "./UserGuidePanel.vue";
import { helpTabs, type HelpTab } from "../lib/help-center";

/**
 * **帮助中心** —— 一个抽屉，三页内容：
 *
 *   1. 语法速查   给不会写查询语法的人（按「我想找什么」组织，可点）
 *   2. 完整语法表 引擎支持的每一个字段/修饰符/运算符
 *   3. 完整手册   整本使用手册
 *
 *  【为什么合成一个面板】速查和手册都是「查语法」这件事的答案，
 *  只是深度不同。分两个抽屉的话用户得先想「我现在要看哪个」——
 *  而这个问题本身对新手就是负担。
 *
 *  【页签归谁管】`tab` 是 prop，切换时 emit「tab」——
 *  真相在 App.vue（Rust 菜单的「完整手册」「搜索语法速查」两项
 *  要能直接落到不同的页上）。
 */
const props = defineProps<{
  /** 当前页签。 */
  tab: HelpTab;
  /** 搜索框当前的查询 —— 语法表页要用它判断引号闭合了没有。 */
  query: string;
}>();

const emit = defineEmits<{
  (e: "close"): void;
  (e: "tab", tab: HelpTab): void;
  /** 速查页：把这条写法填进搜索框。 */
  (e: "use-example", query: string): void;
  /** 语法表页：把关键字**追加**到搜索框。 */
  (e: "use-keyword", keyword: string): void;
}>();
</script>

<template>
  <div class="panel-drawer-backdrop" @click.self="emit('close')">
    <section class="panel-drawer panel-drawer-wide" @click.stop>
      <header class="panel-head">
        <h2>帮助中心</h2>
        <button class="panel-close" @click="emit('close')">×</button>
      </header>

      <!-- 页签：夹在标题与正文之间（正文是滚动容器，
           页签写在正文里会被滚走）。 -->
      <nav class="hc-tabs" role="tablist">
        <button
          v-for="t in helpTabs"
          :key="t.key"
          class="hc-tab"
          :class="{ 'is-on': props.tab === t.key }"
          :title="t.hint"
          role="tab"
          :aria-selected="props.tab === t.key"
          @click="emit('tab', t.key)"
        >
          {{ t.label }}
        </button>
      </nav>

      <div class="panel-body hc-body">
        <SyntaxCheatsheet
          v-if="props.tab === 'cheatsheet'"
          @use-example="(q) => emit('use-example', q)"
        />
        <HelpPanel
          v-else-if="props.tab === 'syntax'"
          :query="props.query"
          @use="(k) => emit('use-keyword', k)"
        />
        <UserGuidePanel v-else />
      </div>

      <footer class="panel-foot">
        <span class="help-foot-hint">按 Esc 关闭</span>
        <button class="set-btn" @click="emit('close')">关闭</button>
      </footer>
    </section>
  </div>
</template>
