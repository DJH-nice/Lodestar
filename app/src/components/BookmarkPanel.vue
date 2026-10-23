<script setup lang="ts">
import { computed } from "vue";
import { bookmarkKey, splitBookmarks } from "../lib/settings";
import type { SettingsLoadState } from "../lib/settings-panel";
import type { Bookmark } from "../lib/types";

/** 面板要显示的东西 —— 全部由 App.vue 给。
 *
 *  【为什么面板不自己读设置】设置有【两个写入口】：
 *  App.vue 的「收藏」按钮、面板里的删除。
 *  面板自己留一份拷贝，就会出现「面板里删了、
 *  标题栏的按钮还以为收藏着」这种【不同步】——
 *  而且那个不同步有后果：再点一次「收藏」会把刚删掉的书签**加回来**。
 *
 *  所以面板只管【显示】与【发意图】：
 *  真相（bookmarks / recentQueries）与写入都在 App.vue 那边，
 *  写完 App.vue 立刻更新 → 面板与标题栏按钮永远看到同一份。 */
const props = defineProps<{
  bookmarks: Bookmark[];
  recentQueries: string[];
  /** 加载状态 —— 三态，见 lib/settings-panel.ts 的说明。 */
  state: SettingsLoadState;
  /** 删除失败的原因 —— 写入方在 App.vue，面板只负责显示。 */
  error: string | null;
}>();

const emit = defineEmits<{
  (e: "close"): void;
  (e: "use-query", query: string): void;
  (e: "use-path", path: string): void;
  /** 「删掉这一条」—— 真正的写入由 App.vue 做（那边才有完整的设置对象）。 */
  (e: "remove-bookmark", b: Bookmark): void;
  /** 「导出书签」—— 面板只发意图，读写文件的事由 App.vue 做。 */
  (e: "export-bookmarks"): void;
  /** 「导入书签」—— 面板只发意图，读文件、校验、写入都由 App.vue 做。 */
  (e: "import-bookmarks"): void;
}>();

/** 两栏：查询书签 / 路径书签（用户已确认两种并存）。 */
const groups = computed(() => splitBookmarks(props.bookmarks));

/** 列表为空时显示什么 —— 加载中 / 加载失败 / 真的没有。 */
function emptyText(nothing: string): string {
  if (props.state === "loading") return "正在加载…";
  if (props.state === "failed") return "（设置加载失败 —— 列表不可用）";
  return nothing;
}
</script>

<template>
  <div class="panel-drawer-backdrop" @click.self="emit('close')">
    <section class="panel-drawer" @click.stop>
      <header class="panel-head">
        <h2>书签与最近</h2>
        <button class="panel-close" @click="emit('close')">×</button>
      </header>

      <div class="panel-body">
        <section class="set-block">
          <h3>最近搜索</h3>
          <p class="set-hint">回车提交过的搜索条件 —— 点一下重新跑这个搜索。</p>
          <ul v-if="recentQueries.length" class="bm-list">
            <li v-for="q in recentQueries" :key="q">
              <button class="bm-item" @click="emit('use-query', q)">{{ q }}</button>
            </li>
          </ul>
          <p v-else class="set-hint">{{ emptyText("还没有搜索记录。") }}</p>
        </section>

        <section class="set-block">
          <h3>查询书签</h3>
          <p class="set-hint">保存的是搜索条件 —— 点一下重新跑这个搜索。</p>
          <ul v-if="groups.query.length" class="bm-list">
            <li v-for="b in groups.query" :key="bookmarkKey(b)">
              <button class="bm-item" @click="emit('use-query', b.value)">
                {{ b.label || b.value }}
              </button>
              <button
                class="bm-del"
                title="删除这条书签"
                @click="emit('remove-bookmark', b)"
              >
                ×
              </button>
            </li>
          </ul>
          <p v-else class="set-hint">{{ emptyText("还没有查询书签。") }}</p>
        </section>

        <section class="set-block">
          <h3>路径书签</h3>
          <p class="set-hint">
            保存的是一个文件或目录 —— 点一下在访达里定位。
          </p>
          <ul v-if="groups.path.length" class="bm-list">
            <li v-for="b in groups.path" :key="bookmarkKey(b)">
              <button class="bm-item bm-path" @click="emit('use-path', b.value)">
                {{ b.label || b.value }}
              </button>
              <button
                class="bm-del"
                title="删除这条书签"
                @click="emit('remove-bookmark', b)"
              >
                ×
              </button>
            </li>
          </ul>
          <p v-else class="set-hint">{{ emptyText("还没有路径书签。") }}</p>
        </section>

        <p v-if="error" class="set-error">{{ error }}</p>
      </div>

      <footer class="panel-foot">
        <span class="help-foot-hint">按 ⌘B 打开或关闭</span>
        <!-- 【导入 / 导出】面板只发意图 —— 真正的读写在 App.vue。
             样式用现成的 .set-btn（页脚已有的那个），不新造 CSS。 -->
        <button class="set-btn" title="把书签导出成一个 JSON 文件" @click="emit('export-bookmarks')">
          导出
        </button>
        <button class="set-btn" title="从 JSON 文件导入书签（追加，不覆盖）" @click="emit('import-bookmarks')">
          导入
        </button>
        <button class="set-btn" @click="emit('close')">关闭</button>
      </footer>
    </section>
  </div>
</template>
