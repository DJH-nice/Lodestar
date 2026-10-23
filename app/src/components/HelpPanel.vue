<script setup lang="ts">
import { computed, onMounted, ref, watch } from "vue";
import { syntaxTables } from "../lib/api";
import { hasUnclosedQuote } from "../lib/settings";
import {
  buildLookup,
  clickToken,
  helpGroups,
  isRowSupported,
  supportNote,
  syntaxHelp,
} from "../lib/syntax-help";
import type { HelpGroup, HelpRow } from "../lib/syntax-help";
import type { SyntaxTables } from "../lib/types";

/**
 * **完整语法表** —— 引擎支持的每一个字段 / 修饰符 / 运算符。
 *
 *  【它只是「内容」，不自带抽屉外壳】
 *  外壳（标题、页签、页脚）由 `HelpCenterPanel.vue` 统一提供 ——
 *  速查、语法表、手册三页共用一个抽屉，页签一换就换内容。
 *
 *  【它和速查卡的分工】
 *  速查卡按「我想找什么」组织，给不会写语法的人；
 *  这一页按「语法怎么分类」组织，给要查全的人。
 */
const props = defineProps<{ query: string }>();

const emit = defineEmits<{
  (e: "close"): void;
  (e: "use", query: string): void;
}>();

const tables = ref<SyntaxTables | null>(null);
const error = ref<string | null>(null);
/** 拒绝追加时给用户的提示（面板不关，让用户当场改查询）。 */
const warning = ref<string | null>(null);

onMounted(async () => {
  try {
    tables.value = await syntaxTables();
  } catch (e) {
    /* 拿不到引擎的语法表 —— 面板照常显示，只是不标「引擎不支持」，
     * 总比整个面板打不开强。 */
    error.value = String(e);
  }
});

/** 引擎语法表的查找集合 —— 拿来标出「文档里有、引擎却不认」的项。 */
const lookup = computed(() => (tables.value ? buildLookup(tables.value) : null));

function supportedOf(group: HelpGroup, row: HelpRow): boolean {
  return isRowSupported(group, row, lookup.value);
}

function tokenOf(group: HelpGroup, row: HelpRow): string {
  return clickToken(group, row, lookup.value);
}

function titleOf(group: HelpGroup, row: HelpRow): string {
  const token = tokenOf(group, row);
  return supportedOf(group, row)
    ? `用这个查：${token}`
    : `当前引擎不支持这个写法：${token}`;
}

/* 点关键字 —— 拿不到可插入的 token 就什么都不做
 * （别塞空串，也别塞伪 token）。 */
function onUse(group: HelpGroup, row: HelpRow) {
  const token = tokenOf(group, row);
  if (!token) return;

  /* 查询里有未闭合的引号 → 【不追加】。
   *
   * 引擎的词法器（src/fsearch_query_lexer.c 的 parse_quoted_string）
   * 读到输入结束也没等到收尾的引号时，会把余下全部吞成一个字面词 ——
   * 追加的 `ext:` 会被吞进引号里，字段过滤完全失效，而且没有任何提示。
   * 这种「静默错答」比不追加坏得多，所以宁可拒绝 + 提示。 */
  if (hasUnclosedQuote(props.query)) {
    warning.value =
      '查询里有未闭合的引号（"）—— 引擎会把它后面的内容全吞成一个词，追加的字段会失效。先把引号补上再点关键字。';
    return;
  }
  warning.value = null;
  emit("use", token);
}

/* 用户改了查询（把引号补上了）→ 提示立刻消失。 */
watch(
  () => props.query,
  () => {
    warning.value = null;
  },
);
</script>

<template>
  <div class="help-content">
    <!-- 提示条【粘在正文顶上】—— 正文是外层那个滚动容器，
         普通的块级元素会被滚走，看不见；sticky 才留得住。 -->
    <p v-if="warning" class="help-warn help-warn-sticky">{{ warning }}</p>

    <p class="set-hint">
      说明改编自 FSearch 官方文档（CC BY-SA 4.0）。{{ supportNote(lookup) }}
    </p>
    <p v-if="error" class="set-error">{{ error }}</p>

    <section v-for="g in helpGroups" :key="g.key" class="help-group">
      <h3>{{ g.title }}</h3>
      <table class="help-table" :class="`help-table-${g.key}`">
        <thead>
          <tr>
            <th v-for="c in g.columns" :key="c">{{ c }}</th>
          </tr>
        </thead>
        <tbody>
          <tr v-for="(row, i) in syntaxHelp[g.key]" :key="i">
            <td
              v-for="(cell, j) in row"
              :key="j"
              :class="{
                'is-unsupported': j === g.keywordColumn && !supportedOf(g, row),
              }"
            >
              <!-- 关键字那一列才是可点的：点一下把关键字填进搜索框。
                   转义组（escaping）例外 —— 那一列是说明词，
                   插进搜索框是错的（见 syntax-help.ts 的 clickable）。 -->
              <button
                v-if="j === g.keywordColumn && g.clickable !== false"
                class="help-kw"
                :title="titleOf(g, row)"
                @click="onUse(g, row)"
              >
                {{ cell }}
              </button>
              <template v-else>{{ cell }}</template>
            </td>
          </tr>
        </tbody>
      </table>
    </section>
  </div>
</template>
