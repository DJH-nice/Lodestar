<script setup lang="ts">
import { computed } from "vue";
import type { IndexStatus, SearchResult, VolumeStatus } from "../lib/types";

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

/** 一个卷在状态栏上怎么写。
 *
 *  【为什么每个阶段都要写出来】
 *   插上 U 盘后的首扫可能要几十秒 —— 界面上什么都不显示的话，
 *   用户会以为功能坏了（而且是**静默**地坏）。
 */
function volumeText(v: VolumeStatus): string {
  switch (v.phase.state) {
    case "Indexing":
      return `${v.label} · 正在索引…`;
    case "Failed":
      // 【为什么要把原因显示出来】
      //   Rust 侧已经把原因传过来了（`detail`），以前前端把它扔了 →
      //   用户只看到「坏盘 · 索引失败」，鼠标悬停看到的还是**路径**而不是原因。
      //   这与项目自己的约定相悖（`.sb-error` 那条注释写着
      //   「索引出错的原因必须显示出来」）。
      return `${v.label} · 索引失败：${v.phase.detail}`;
    default:
      return `${v.label} · ${v.indexed.toLocaleString()} 项`;
  }
}

/** 悬停提示 —— 失败时要把**原因**也带上（路径只是「哪个盘」）。
 *
 *  【为什么要单独一个函数】模板里的三元表达式对 `v.phase` 的**收窄**
 *   在 `vue-tsc` 下不一定成立 → 放在脚本里写，类型一定收得住。
 */
function volumeTitle(v: VolumeStatus): string {
  return v.phase.state === "Failed"
    ? `${v.path} · ${v.phase.detail}`
    : v.path;
}
</script>

<template>
  <footer class="statusbar">
    <span class="sb-left">{{ hitText }}</span>
    <span class="sb-right">
      <!-- 【外接卷】
           用户插了 U 盘就得看得见「盘上的文件进索引了没有」——
           首扫可能要几十秒，没有反馈的话用户会以为功能坏了。
           `v-for` 的 key 用挂载点路径 —— 那是卷的唯一标识。 -->
      <span
        v-for="v in status?.volumes ?? []"
        :key="v.path"
        class="sb-volume"
        :class="{ 'is-failed': v.phase.state === 'Failed' }"
        :title="volumeTitle(v)"
        >{{ volumeText(v) }}</span
      >
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
