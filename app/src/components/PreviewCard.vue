<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref } from "vue";
import type { Entry } from "../lib/types";

const props = defineProps<{
  entry: Entry | null;
  x: number;
  y: number;
  /** 这个路径是不是已经收藏了 —— 由 App.vue 从真实的书签列表算出来。
   *  （卡片自己不存 —— 面板里删了书签，卡片还以为收藏着，再点一下就加回来。） */
  bookmarked: boolean;
  /** 收藏写失败的原因（成功时 null）—— 卡片上显示它。 */
  error: string | null;
}>();
const cardEl = ref<HTMLElement | null>(null);
const emit = defineEmits<{
  (e: "close"): void;
  (e: "open", path: string): void;
  (e: "reveal", path: string): void;
  /** 收藏 / 取消收藏这个路径 —— 真正的写入由 App.vue 做。 */
  (e: "bookmark", path: string): void;
}>();

/* 点卡片外面就关掉。预览卡是浮层，没有关闭入口的话弹出来就关不掉。 */
function onWindowPointerDown(e: PointerEvent) {
  const el = cardEl.value;
  if (el && !el.contains(e.target as Node)) emit("close");
}

onMounted(() => window.addEventListener("pointerdown", onWindowPointerDown));
onBeforeUnmount(() => window.removeEventListener("pointerdown", onWindowPointerDown));

const sizeText = computed(() => {
  const e = props.entry;
  if (!e) return "";
  if (e.is_folder) return "文件夹";
  if (e.size < 1024) return `${e.size} B`;
  if (e.size < 1024 * 1024) return `${(e.size / 1024).toFixed(1)} KB`;
  return `${(e.size / 1024 / 1024).toFixed(1)} MB`;
});

const timeText = computed(() => {
  const e = props.entry;
  if (!e || !e.mtime) return "";
  return new Date(e.mtime * 1000).toLocaleString("zh-CN");
});

/** 收藏按钮的文案 —— 目录 / 文件 / 已收藏，三种。 */
const bookmarkLabel = computed(() => {
  const e = props.entry;
  if (!e) return "收藏";
  if (props.bookmarked) return "取消收藏";
  return e.is_folder ? "收藏此目录" : "收藏此文件";
});
</script>

<template>
  <div
    v-if="entry"
    ref="cardEl"
    class="preview-card"
    :style="{ left: x + 'px', top: y + 'px' }"
    @click.stop
  >
    <button class="pc-close" title="关闭" @click="emit('close')">×</button>
    <div class="pc-name">{{ entry.name }}</div>
    <div class="pc-dir">{{ entry.dir }}</div>
    <dl class="pc-meta">
      <dt>大小</dt>
      <dd>{{ sizeText }}</dd>
      <dt>修改</dt>
      <dd>{{ timeText }}</dd>
      <dt>类型</dt>
      <dd>{{ entry.extension || (entry.is_folder ? "文件夹" : "文件") }}</dd>
    </dl>
    <div v-if="error" class="pc-error">{{ error }}</div>
    <div class="pc-actions">
      <button @click="emit('open', entry.path)">打开</button>
      <button @click="emit('reveal', entry.path)">在访达中显示</button>
      <!-- 收藏：不关卡片 —— 写盘成功后文案会自己翻成「取消收藏」，
           用户想反悔可以马上再点一次。 -->
      <button @click="emit('bookmark', entry.path)">{{ bookmarkLabel }}</button>
    </div>
  </div>
</template>
