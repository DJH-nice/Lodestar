<script setup lang="ts">
import { computed, ref } from "vue";
import { cheatGroups, filterCheatsheet } from "../lib/cheatsheet";

/**
 * 搜索语法**速查卡**。
 *
 *  【它给谁看】给不会写查询语法的人 —— 所以组织方式是
 *  「我想找什么 → 这么写」，而不是「这个字段是什么意思」。
 *
 *  【每条写法都能点】点了就填进搜索框（父组件负责填）——
 *  照着抄比记语法容易得多。
 *
 *  【它只负责显示与发意图】数据在 `lib/cheatsheet.ts`，
 *  筛选也是那里的纯函数 —— 组件里没有一行判断逻辑。
 */
const emit = defineEmits<{
  /** 「把这条写法填进搜索框」 */
  (e: "use-example", query: string): void;
}>();

/** 速查里的搜索框 —— 记不清怎么写的时候按关键词找。 */
const keyword = ref("");

/** 筛完之后的分组（空组已经被丢掉了）。 */
const groups = computed(() => filterCheatsheet(cheatGroups, keyword.value));

/** 现在显示了几条 / 一共几条。 */
const shown = computed(() => groups.value.reduce((n, g) => n + g.items.length, 0));
const total = cheatGroups.reduce((n, g) => n + g.items.length, 0);
</script>

<template>
  <div class="cheat">
    <div class="cheat-search-row">
      <input
        class="cheat-search"
        type="text"
        v-model="keyword"
        placeholder="搜速查：大文件 / 排除 / 时间 / 路径…"
        spellcheck="false"
        data-tauri-drag-region="false"
      />
      <span class="cheat-count">{{ shown }} / {{ total }} 条</span>
    </div>

    <p v-if="!groups.length" class="cheat-empty">
      没有跟「{{ keyword }}」匹配的写法。<br />
      换个词试试，或者切到「完整语法表」「完整手册」那两页看看。
    </p>

    <section v-for="g in groups" :key="g.key" class="cheat-group">
      <h3>{{ g.title }}</h3>
      <p v-if="g.hint" class="cheat-hint">{{ g.hint }}</p>

      <ul class="cheat-list">
        <li v-for="it in g.items" :key="it.intent" class="cheat-item">
          <div class="cheat-head">
            <span class="cheat-intent">{{ it.intent }}</span>
            <button
              class="cheat-query"
              :title="`点一下填进搜索框：${it.query}`"
              @click="emit('use-example', it.query)"
            >
              {{ it.query }}
            </button>
          </div>

          <!-- 错误写法：**不能点** —— 点了就把错的填进搜索框了。 -->
          <p v-if="it.wrong" class="cheat-wrong">
            <span class="cheat-wrong-tag">别写成</span>
            <code>{{ it.wrong }}</code>
          </p>

          <p v-if="it.note" class="cheat-note">{{ it.note }}</p>
        </li>
      </ul>
    </section>
  </div>
</template>
