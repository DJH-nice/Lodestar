<script setup lang="ts">
/* SVG 图标集 —— 来自 prototype/index.html 的 symbol 定义。
   用一个隐藏的 <svg> 把 symbol 挂到文档上，各处用 <use href="#i-xxx" /> 引用。 */
</script>

<template>
  <svg class="svg-sprite" aria-hidden="true" focusable="false">
    <symbol id="i-search" viewBox="0 0 256 256">
      <circle cx="116" cy="116" r="84" fill="none" stroke="currentColor" stroke-width="16" />
      <path d="M175 175l56 56" fill="none" stroke="currentColor" stroke-width="16" stroke-linecap="round" />
    </symbol>
    <symbol id="i-folder" viewBox="0 0 256 256">
      <path
        d="M216 80h-83.7l-15.2-15.2A16 16 0 0 0 105.9 60H40a16 16 0 0 0-16 16v120a16 16 0 0 0 16 16h176a16 16 0 0 0 16-16V96a16 16 0 0 0-16-16Z"
        fill="none" stroke="currentColor" stroke-width="16" stroke-linejoin="round" />
    </symbol>
    <symbol id="i-file" viewBox="0 0 256 256">
      <path
        d="M213.7 82.4l-56-56A16 16 0 0 0 146.7 20H40a16 16 0 0 0-16 16v184a16 16 0 0 0 16 16h176a16 16 0 0 0 16-16V92.7a16 16 0 0 0-1.3-10.3Z"
        fill="none" stroke="currentColor" stroke-width="16" stroke-linejoin="round" />
      <path d="M152 20v64a8 8 0 0 0 8 8h56" fill="none" stroke="currentColor" stroke-width="16" stroke-linejoin="round" />
    </symbol>
    <symbol id="i-x" viewBox="0 0 256 256">
      <path d="M168 88l80 80M248 88l-80 80" fill="none" stroke="currentColor" stroke-width="20" stroke-linecap="round" />
    </symbol>
  </svg>
</template>

<style scoped>
/* 图标集本身不可见，只提供 symbol 定义 */
.svg-sprite {
  position: absolute;
  width: 0;
  height: 0;
  overflow: hidden;
}
</style>
