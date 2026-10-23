<script setup lang="ts">
import { computed, onBeforeUnmount, ref, watch } from "vue";
import { search } from "../lib/api";
import { segmentize } from "../lib/highlight";
import { OVERSCAN, ROW_H, neededPages, separatorRow } from "../lib/paging";
import type { Entry, ListKind, SearchResult, SortField, SortOrder } from "../lib/types";

const PAGE_SIZE = 25;

const props = defineProps<{
  query: string;
  kind: ListKind;
  sort: SortField;
  order: SortOrder;
  /** 索引就绪 / 外部要求刷新时加一 —— 加一就重搜一次。
   *  首次挂载时索引还在后台建，搜不到东西；索引一就绪就该重搜。 */
  refreshKey?: number;
}>();
const emit = defineEmits<{
  (e: "open", path: string): void;
  (e: "reveal", path: string): void;
  (e: "sort", field: SortField): void;
  (e: "order", order: SortOrder): void;
  (e: "preview", entry: Entry, x: number, y: number): void;
  /** 右键 → 弹右键菜单（菜单里的「详情」才会打开预览卡）。 */
  (e: "context-menu", entry: Entry, x: number, y: number): void;
  (e: "state", s: { result: SearchResult | null; loading: boolean }): void;
}>();

/* 点表头换排序：同一个字段再点一次就翻方向。
   不做这个的话 `order` 永远是 asc —— 整套降序分支（group_boundary、
   separatorRow 的 desc、引擎的 FS_SORT_DESC）UI 上永远走不到。 */
function onSortClick(field: SortField) {
  if (props.sort === field) {
    emit("sort", field);
    emit("order", props.order === "asc" ? "desc" : "asc");
  } else {
    emit("sort", field);
    emit("order", "asc");
  }
}

const vp = ref<HTMLElement | null>(null);
const scrollTop = ref(0);
const viewportH = ref(400);
const loadedPages = ref(new Set<number>());
const pages = ref(new Map<number, SearchResult>());
const selected = ref(0);
const loading = ref(false);
const errorMessage = ref<string | null>(null);
/** 上一次的查询串 —— 用来判断这次变化是不是「用户在打字」 */
const lastQuery = ref<string>("");

/* 序列号：丢弃过期响应。
   后端搜索是串行的，但前端必须自己判断「这个响应还是我想要的吗」——
   否则慢的旧查询会覆盖快的新查询。 */
let seq = 0;

function resetCache() {
  pages.value = new Map();
  loadedPages.value = new Set();
  selected.value = 0;
  scrollTop.value = 0;
  if (vp.value) vp.value.scrollTop = 0;
}

/* query / sort / order / kind 任一变化都要重来：
   - 换了 query/sort → C 侧的 view 是旧的，fs_engine_page 会返回 FS_ERR_INVALID_ARG
   - 换了 kind    → 同一个 view 里取的是不同段，也要重新 search
   immediate: true —— 首次挂载也要搜一次（空查询会被 shim 短路成 0 条，正好是空态） */
/* 【防抖】query 每变一个字符就 resetCache + 全量搜索，而后端搜索是串行的
   （每次按键排一次队）。$HOME 量级下，最后一次搜索要等前面几次跑完 ——
   表现为「越打越卡」。

   120ms 是手感与延迟的平衡点：连续输入时只在停下来后搜一次。 */
const DEBOUNCE_MS = 120;
let debounceTimer: number | undefined;

watch(
  () => [props.query, props.sort, props.order, props.kind, props.refreshKey ?? 0],
  () => {
    // 换排序 / 换种类【不】防抖 —— 它们不是连续输入，立刻反映
    const isTyping = props.query !== lastQuery.value;
    lastQuery.value = props.query;
    resetCache();
    if (debounceTimer !== undefined) window.clearTimeout(debounceTimer);
    if (isTyping) {
      debounceTimer = window.setTimeout(() => {
        debounceTimer = undefined;
        void runSearch();
      }, DEBOUNCE_MS);
    } else {
      void runSearch();
    }
  },
  { immediate: true },
);

onBeforeUnmount(() => {
  if (debounceTimer !== undefined) window.clearTimeout(debounceTimer);
});

async function runSearch() {
  const mine = ++seq;
  loading.value = true;
  errorMessage.value = null;
  emit("state", { result: pages.value.get(0) ?? null, loading: true });
  try {
    // 第 0 页同时【建立引擎的 view】—— 之后翻页是游标切片，不重跑匹配
    const r = await search(props.query, props.sort, props.order, props.kind, 0, PAGE_SIZE);
    if (mine !== seq) return;
    pages.value.set(0, r);
    loadedPages.value = new Set([...loadedPages.value, 0]);
    emit("state", { result: r, loading: false });
  } catch (e) {
    if (mine !== seq) return;
    errorMessage.value = String(e);
    emit("state", { result: null, loading: false });
  } finally {
    if (mine === seq) loading.value = false;
  }
}

async function loadPage(page: number) {
  const mine = seq;
  try {
    const r = await search(props.query, props.sort, props.order, props.kind, page, PAGE_SIZE);
    if (mine !== seq) return; // 查询已经变了，丢弃这次响应
    pages.value.set(page, r);
    loadedPages.value = new Set([...loadedPages.value, page]);
  } catch {
    // 翻页失败 —— 常见原因是 view 被 rescan 掉了。重新 search 一次最省事。
    if (mine !== seq) return;
    resetCache();
    void runSearch();
  }
}

async function ensurePages() {
  const total = pages.value.get(0)?.total_entries ?? 0;
  const need = neededPages(
    scrollTop.value,
    viewportH.value,
    total,
    PAGE_SIZE,
    loadedPages.value,
  );
  for (const p of need) {
    if (loadedPages.value.has(p)) continue;
    // 先占位再发请求，避免同一次滚动里重复请求同一页
    loadedPages.value = new Set([...loadedPages.value, p]);
    void loadPage(p);
  }
}

function onScroll() {
  const el = vp.value;
  if (!el) return;
  scrollTop.value = el.scrollTop;
  viewportH.value = el.clientHeight;
  void ensurePages();
}

/* ---- 派生数据 ---- */

const totalRows = computed(() => pages.value.get(0)?.total_entries ?? 0);

/** 分隔行的【全局】行号：升序在目录段之后，降序在文件段之后。 */
const sepAt = computed(() => separatorRow(pages.value.get(0) ?? null, props.kind, props.order));

const sepLabel = computed(() => {
  const r = pages.value.get(0);
  if (!r) return "";
  return props.order === "asc"
    ? `文件 (${r.total_files})`
    : `文件夹 (${r.total_folders})`;
});

/* 虚拟滚动：总高度按【真实总数】算，所以滚动条长度立刻正确，
   不必等所有页都加载完。分隔行额外占一行。 */
const totalHeight = computed(
  () => (totalRows.value + (sepAt.value != null ? 1 : 0)) * ROW_H,
);

/** 第 g 行（全局行号）落在哪一页的哪一条。 */
function entryAt(g: number): Entry | null {
  const page = Math.floor(g / PAGE_SIZE);
  const local = g % PAGE_SIZE;
  return pages.value.get(page)?.entries[local] ?? null;
}

type Row =
  | { kind: "entry"; entry: Entry; segs: ReturnType<typeof segmentize>; top: number; index: number }
  | { kind: "separator"; label: string; top: number; index: number }
  | { kind: "skeleton"; top: number; index: number };

const visible = computed<Row[]>(() => {
  const out: Row[] = [];
  if (totalRows.value <= 0) return out;

  const first = Math.max(0, Math.floor(scrollTop.value / ROW_H));
  const last = Math.min(
    totalRows.value - 1,
    Math.ceil((scrollTop.value + viewportH.value) / ROW_H) + OVERSCAN,
  );
  const sep = sepAt.value;

  for (let g = first; g <= last; g++) {
    // 分隔行插在它前面，所以 g >= sep 的行整体下移一行
    const shift = sep != null && g >= sep ? 1 : 0;

    if (sep != null && g === sep) {
      out.push({ kind: "separator", label: sepLabel.value, top: g * ROW_H, index: g });
    }

    const e = entryAt(g);
    if (e) {
      out.push({
        kind: "entry",
        entry: e,
        segs: segmentize(e.name, e.highlights),
        top: (g + shift) * ROW_H,
        index: g,
      });
    } else {
      // 这一页还没加载回来 —— 画骨架行，避免大片空白
      out.push({ kind: "skeleton", top: (g + shift) * ROW_H, index: g });
    }
  }
  return out;
});

const empty = computed(
  () =>
    !loading.value &&
    totalRows.value === 0 &&
    (pages.value.has(0) || errorMessage.value !== null),
);

/* ---- 交互 ---- */

function scrollSelectedIntoView() {
  const el = vp.value;
  if (!el) return;
  const top = selected.value * ROW_H;
  const bottom = top + ROW_H;
  if (top < el.scrollTop) el.scrollTop = top;
  else if (bottom > el.scrollTop + el.clientHeight) {
    el.scrollTop = bottom - el.clientHeight;
  }
}

function onKeydown(e: KeyboardEvent) {
  const total = totalRows.value;
  if (!total) return;
  if (e.key === "ArrowDown") {
    e.preventDefault();
    selected.value = Math.min(total - 1, selected.value + 1);
    scrollSelectedIntoView();
  } else if (e.key === "ArrowUp") {
    e.preventDefault();
    selected.value = Math.max(0, selected.value - 1);
    scrollSelectedIntoView();
  } else if (e.key === "Enter") {
    e.preventDefault();
    const entry = entryAt(selected.value);
    if (!entry) return;
    if (e.metaKey) emit("reveal", entry.path);
    else emit("open", entry.path);
  }
}

function onRowClick(row: Row) {
  if (row.kind === "entry") selected.value = row.index;
}

/* 双击 = 在访达中定位（不是「用默认程序打开」）。
   项目目标写的就是「双击 Finder 定位」，验收标准 A4 也是这么写的 ——
   两边必须一致，否则 A4 必挂。 */
function onRowDblclick(row: Row) {
  if (row.kind === "entry") emit("reveal", row.entry.path);
}

/* 右键 → 弹【右键菜单】（不是直接弹预览卡）。
   菜单里有「打开 / 在访达中显示 / 收藏 / 详情」——
   预览卡改由菜单里的「详情」打开（否则右键就没法收藏路径了）。 */
function onContextMenu(row: Row, e: MouseEvent) {
  if (row.kind !== "entry") return;
  selected.value = row.index;
  emit("context-menu", row.entry, e.clientX, e.clientY);
}

function sizeText(e: Entry): string {
  if (e.is_folder) return "—";
  if (e.size < 1024) return `${e.size} B`;
  if (e.size < 1024 * 1024) return `${(e.size / 1024).toFixed(1)} KB`;
  return `${(e.size / 1024 / 1024).toFixed(1)} MB`;
}

function timeText(e: Entry): string {
  if (!e.mtime) return "—";
  const d = new Date(e.mtime * 1000);
  const p = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())} ${p(d.getHours())}:${p(d.getMinutes())}`;
}
</script>

<template>
  <div class="result-wrap">
    <div class="rt-head">
      <button class="rt-th rt-th-name" @click="onSortClick('name')">
        名称<span v-if="sort === 'name'" class="rt-arrow">{{ order === "asc" ? "↑" : "↓" }}</span>
      </button>
      <span class="rt-th rt-th-dir">位置</span>
      <button class="rt-th rt-th-size" @click="onSortClick('size')">
        大小<span v-if="sort === 'size'" class="rt-arrow">{{ order === "asc" ? "↑" : "↓" }}</span>
      </button>
      <button class="rt-th rt-th-time" @click="onSortClick('mtime')">
        修改时间<span v-if="sort === 'mtime'" class="rt-arrow">{{ order === "asc" ? "↑" : "↓" }}</span>
      </button>
    </div>

    <div
      ref="vp"
      class="rt-viewport"
      tabindex="0"
      @scroll.passive="onScroll"
      @keydown="onKeydown"
    >
      <div v-if="empty" class="rt-empty">
        {{ errorMessage ?? "没有匹配的结果" }}
      </div>
      <div v-else class="rt-canvas" :style="{ height: totalHeight + 'px' }">
        <div
          v-for="row in visible"
          :key="row.kind + ':' + row.index"
          class="rt-row"
          :class="{
            'is-selected': row.kind === 'entry' && row.index === selected,
            'is-separator': row.kind === 'separator',
            'is-skeleton': row.kind === 'skeleton',
          }"
          :style="{ transform: 'translateY(' + row.top + 'px)' }"
          @click="onRowClick(row)"
          @dblclick="onRowDblclick(row)"
          @contextmenu.prevent="onContextMenu(row, $event)"
        >
          <template v-if="row.kind === 'separator'">
            <span class="rt-sep-label">{{ row.label }}</span>
          </template>
          <template v-else-if="row.kind === 'entry'">
            <span class="rt-ic" :class="{ folder: row.entry.is_folder }">
              <svg><use :href="row.entry.is_folder ? '#i-folder' : '#i-file'" /></svg>
            </span>
            <span class="rt-name">
              <span
                v-for="(seg, k) in row.segs"
                :key="k"
                :class="{ 'is-hit': seg.hit }"
                >{{ seg.text }}</span
              >
            </span>
            <span class="rt-dir">{{ row.entry.dir }}</span>
            <span class="rt-size">{{ sizeText(row.entry) }}</span>
            <span class="rt-time">{{ timeText(row.entry) }}</span>
          </template>
          <template v-else>
            <span class="rt-skeleton-bar" />
          </template>
        </div>
      </div>
    </div>
  </div>
</template>
