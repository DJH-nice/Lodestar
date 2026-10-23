<script setup lang="ts">
import { computed } from "vue";
import type { IndexStatus, SearchResult } from "../lib/types";

const props = defineProps<{
  result: SearchResult | null;
  status: IndexStatus | null;
  loading: boolean;
}>();

const hitText = computed(() => {
  const r = props.result;
  if (!r) return "—";
  if (r.warning) return r.warning;
  return `${r.total_entries} 项（文件 ${r.total_files} · 目录 ${r.total_folders}）`;
});
</script>

<template>
  <footer class="statusbar">
    <span class="sb-left">{{ hitText }}</span>
    <span class="sb-right">
      <span v-if="status?.scanning">正在扫描索引…</span>
      <span v-else-if="status">已索引 {{ status.indexed }} 项</span>
      <!-- 【索引出错的原因必须显示出来】
           index_status.error 以前只是躺在状态里、界面上一个字都不提 ——
           用户看到的是「已索引 0 项」，完全不知道为什么没有结果。 -->
      <span
        v-if="status?.error"
        class="sb-error"
        :title="`索引失败：${status.error}`"
        >索引失败：{{ status.error }}</span
      >
      <span v-if="result && !loading">{{ result.elapsed_ms }} ms</span>
    </span>
  </footer>
</template>
