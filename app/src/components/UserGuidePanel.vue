<script setup lang="ts">
import { userGuideHtml } from "../lib/user-guide";

/**
 * **完整手册**的内容 —— 仓库里的 `docs/USER-GUIDE.md`，构建期内联。
 *
 *  【它只是「内容」，不自带抽屉外壳】
 *  外壳（标题、页签、页脚）由 `HelpCenterPanel.vue` 提供 ——
 *  速查、语法表、手册三页共用一个抽屉。
 *
 *  【它只负责显示】不读文件、不发请求、不改任何状态。
 */
</script>

<template>
  <!-- 【为什么可以 v-html】内容是仓库里的 docs/USER-GUIDE.md，
       构建期就内联进 bundle 了，不是用户输入 → 没有 XSS 面。
       样式在 styles.css 的「使用手册面板」一节里（v-html 的内容
       不受 <style scoped> 影响，所以那里必须是非 scoped 的）。 -->
  <div class="ug-body" v-html="userGuideHtml" />
</template>
