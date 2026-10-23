<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from "vue";
import type { Entry } from "../lib/types";

/** 右键菜单 —— 结果行上右键弹出的浮动菜单。
 *
 *  【它只负责显示 + 发意图】收藏状态由 App.vue 给（bookmarked prop），
 *  写入也在 App.vue 做 —— 本组件**不碰**设置文件，也不自己存一份收藏状态
 *  （否则面板里删了书签、菜单还以为收藏着，再点一下又加回来）。 */
const props = defineProps<{
  /** null → 不渲染。 */
  entry: Entry | null;
  /** 鼠标位置（clientX / clientY）。 */
  x: number;
  y: number;
  /** 这个路径是不是已经收藏了 —— 由 App.vue 从真实的书签列表算出来。 */
  bookmarked: boolean;
  /** 写入失败的原因（成功时 null）—— 菜单底部显示它。 */
  error: string | null;
}>();

const emit = defineEmits<{
  (e: "close"): void;
  (e: "open", path: string): void;
  (e: "reveal", path: string): void;
  /** 收藏 / 取消收藏这个路径 —— 真正的写入由 App.vue 做。 */
  (e: "bookmark", path: string): void;
  /** 「详情」→ 打开预览卡。 */
  (e: "preview", entry: Entry, x: number, y: number): void;
}>();

/* 菜单自己的尺寸 —— 【不跟别的组件耦合】
   （预览卡的 320px 是它自己的事，菜单按自己的项数估）。 */
const MENU_W = 200;
/** 兜底高度 —— 量不到时用（见下面 menuH 的说明）。 */
const MENU_H = 180;
/* 离窗口边缘至少留这么远。 */
const EDGE = 8;

const menuEl = ref<HTMLElement | null>(null);

/** 菜单的【实测高度】—— 夹取时用它，不用常数。
 *
 *  【为什么不能用常数】菜单底部会多一行 .ctx-error（写盘失败时），
 *  高度从 148px 涨到 236px（真机实测的数）。
 *  用常数（180）夹取的话，在窗口底边右键时菜单会被夹到
 *  `top = 窗口高 - 180 - 8`，而下沿是 `top + 236`
 *  → **超出窗口底边 48px**，错误消息的下半截看不到。
 *  （这个缺陷是 Task 7 复审在真机 DOM 里量出来的。）
 *
 *  【怎么量】挂载后量一次 `offsetHeight`；
 *  错误行出现/消失会改高度 → error 变了再量一次。
 *  量不到（还没渲染 / 环境没有布局引擎）就保留兜底值。 */
const menuH = ref(MENU_H);

function measure() {
  const h = menuEl.value?.offsetHeight;
  if (h) menuH.value = h;
}

/* 错误行出现/消失会改高度 → 重量一次。
   flush:"post" —— 必须在 DOM 更新【之后】量，
   否则量到的还是旧的（错误行还没插进去）。 */
watch(() => props.error, measure, { flush: "post" });

/** 菜单的位置 —— 必须【夹在窗口里】：
 *  右键点在屏幕/窗口边缘时，菜单一半会跑到窗口外面去。
 *  跟 App.vue 里 onPreview() 夹预览卡是同一个算法，
 *  区别是这里用的是**实测高度**（见 menuH 的说明）。 */
const pos = computed(() => {
  const w = window.innerWidth;
  const h = window.innerHeight;
  return {
    left: Math.max(EDGE, Math.min(props.x, w - MENU_W - EDGE)) + "px",
    top: Math.max(EDGE, Math.min(props.y, h - menuH.value - EDGE)) + "px",
  };
});

/** 收藏项的文案 —— 目录 / 文件 / 已收藏，三种。 */
const bookmarkLabel = computed(() => {
  if (props.bookmarked) return "取消收藏";
  return props.entry?.is_folder ? "收藏此目录" : "收藏此文件";
});

/* 点菜单外面就关掉。浮层没有关闭入口的话，弹出来就关不掉。 */
function onWindowPointerDown(e: PointerEvent) {
  const el = menuEl.value;
  if (el && !el.contains(e.target as Node)) emit("close");
}

/* Esc 关掉。 */
function onWindowKeydown(e: KeyboardEvent) {
  if (e.key === "Escape") emit("close");
}

onMounted(() => {
  window.addEventListener("pointerdown", onWindowPointerDown);
  window.addEventListener("keydown", onWindowKeydown);
  /* 量一次真实高度（挂载后 DOM 已经在，量得到）。
     字体度量 / 父组件数据到达可能让高度再变一点 —— 下一拍再量一次兜底。 */
  measure();
  void nextTick(measure);
});
onBeforeUnmount(() => {
  window.removeEventListener("pointerdown", onWindowPointerDown);
  window.removeEventListener("keydown", onWindowKeydown);
});

/** 这三个动作做完就收菜单 —— 它们的目标界面不在菜单里。
 *
 *  【「收藏」不收菜单】写盘是异步的，成功/失败由 App.vue 决定：
 *  成功 → App.vue 更新 bookmarks → 文案自动翻成「取消收藏」，菜单留着
 *  （用户能立刻反悔）；失败 → App.vue 把错误写进 error → 菜单底部显示错误，
 *  菜单也留着。 */
function onOpen() {
  const e = props.entry;
  if (!e) return;
  emit("open", e.path);
  emit("close");
}

function onReveal() {
  const e = props.entry;
  if (!e) return;
  emit("reveal", e.path);
  emit("close");
}

function onDetails() {
  const e = props.entry;
  if (!e) return;
  emit("preview", e, props.x, props.y);
  emit("close");
}

function onBookmark() {
  const e = props.entry;
  if (!e) return;
  emit("bookmark", e.path);
}
</script>

<template>
  <div
    v-if="entry"
    ref="menuEl"
    class="ctx-menu"
    :style="pos"
    @click.stop
    @contextmenu.prevent
  >
    <button class="ctx-item" @click="onOpen">打开</button>
    <button class="ctx-item" @click="onReveal">在访达中显示</button>
    <div class="ctx-sep" />
    <button class="ctx-item" @click="onBookmark">{{ bookmarkLabel }}</button>
    <div class="ctx-sep" />
    <button class="ctx-item" @click="onDetails">详情</button>
    <div v-if="error" class="ctx-error">{{ error }}</div>
  </div>
</template>
