<script setup lang="ts">
import { onMounted, onBeforeUnmount, nextTick, computed, ref, watch } from "vue";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { revealItemInDir } from "@tauri-apps/plugin-opener";
/* 【书签导入 / 导出用的文件选择框】——
   `@tauri-apps/plugin-dialog` **已经装了**（见 package.json），
   不需要为了「读写文件」再加 `@tauri-apps/plugin-fs`。
   这里只用它拿**路径**（拿路径是 dialog 的活），落盘交给 Rust（见 lib/api.ts）。 */
import { open as openFileDialog, save as saveFileDialog } from "@tauri-apps/plugin-dialog";
import {
  ackVisibility,
  indexStatus,
  readBookmarksFile,
  rescan,
  writeBookmarksFile,
} from "./lib/api";
import {
  buildBookmarksFile,
  mergeBookmarks,
  parseBookmarksFile,
} from "./lib/bookmarks-file";
import {
  appendKeyword,
  isPathBookmarked,
  isQueryBookmarked,
  pushRecent,
  recentChanged,
  removeBookmark,
  toggleBookmark,
} from "./lib/settings";
import { loadSettings, patchSettings } from "./lib/settings-io";
import { sameIndexStatus } from "./lib/status";
import type { SettingsLoadState } from "./lib/settings-panel";
import type {
  Bookmark,
  Entry,
  IndexStatus,
  ListKind,
  SearchResult,
  SortField,
  SortOrder,
} from "./lib/types";
import AppIcons from "./components/AppIcons.vue";
import ResultTable from "./components/ResultTable.vue";
import SearchBar from "./components/SearchBar.vue";
import StatusBar from "./components/StatusBar.vue";
import PreviewCard from "./components/PreviewCard.vue";
import ContextMenu from "./components/ContextMenu.vue";
import SettingsPanel from "./components/SettingsPanel.vue";
import HelpCenterPanel from "./components/HelpCenterPanel.vue";
import BookmarkPanel from "./components/BookmarkPanel.vue";
import CreditsPanel from "./components/CreditsPanel.vue";
import type { HelpTab } from "./lib/help-center";

/* App.vue 只管【输入状态】；搜索与分页由 ResultTable 负责（见 Task 6 的说明），
   它把结果与加载中状态回传给我们。 */
const query = ref("");
const sort = ref<SortField>("name");
const order = ref<SortOrder>("asc");
const kind = ref<ListKind>("all");

const result = ref<SearchResult | null>(null);
const loading = ref(false);
const status = ref<IndexStatus | null>(null);
const preview = ref<{ entry: Entry; x: number; y: number } | null>(null);
/** 右键菜单 —— 右键点结果行弹出来的那个。
 *
 *  【两层浮层不能叠着】菜单与预览卡都是「点外面关闭」的浮层，
 *  同时开着的话两个监听会互相打架（点一下，两张都关）。
 *  所以开一个必关另一个。 */
const contextMenu = ref<{ entry: Entry; x: number; y: number } | null>(null);
/* ---- 抽屉面板：互斥 ---- */

/** 当前打开的面板 —— **同时最多只能有一个**。
 *
 *  【为什么要收成一个】独立布尔 → 可以同时开两个 →
 *  两层遮罩叠在一起、底下那个面板的文字整片透上来（终审实测确认属实）。
 *
 *  【为什么不用「多个布尔 + 一个 openPanel() 辅助函数」】
 *  那样只要有人直接写 `showHelp.value = true` 就又破功了 ——
 *  互斥必须是**结构上**保证的，不是靠大家记得调辅助函数。
 *
 *  【为什么「帮助」只有一个面板】帮助中心里有三页
 *  （语法速查 / 完整语法表 / 完整手册）—— 它们是**同一个抽屉的三页**，
 *  不是三个抽屉。分三个的话用户得先想「我要看哪个」，
 *  而这个问题本身对新手就是负担。 */
type Panel = "none" | "help" | "settings" | "bookmarks" | "credits";
const activePanel = ref<Panel>("none");

/** 帮助中心当前停在第几页。
 *
 *  【为什么归 App.vue 管】原生菜单有「搜索语法速查」和「完整手册」两项，
 *  两项要落到**不同的页**上 —— 真相只能有一个地方。 */
const helpTab = ref<HelpTab>("cheatsheet");

/** 模板里用的三个布尔 —— 现在是 activePanel 的**只读视图**。
 *
 *  【为什么留着它们】模板里 `v-if="showSettings"` 之类不用改，
 *  diff 更小。（它们是 computed —— 只读，**不能**在模板里赋值！） */
const showSettings = computed(() => activePanel.value === "settings");
const showHelp = computed(() => activePanel.value === "help");
const showBookmarks = computed(() => activePanel.value === "bookmarks");
const showCredits = computed(() => activePanel.value === "credits");

/** 打开某个面板（同时关掉其它所有面板）。 */
function openPanel(p: Panel) {
  activePanel.value = p;
  /* 抽屉是**模态层** —— 打开时把小浮层收起来。
     （层级上模态已经高于浮层，见 styles.css 的 --z-* 令牌；
       这里再收一次是**双保险**：万一将来出现「抽屉里弹菜单」的路径，
       收起它也更干净。） */
  preview.value = null;
  contextMenu.value = null;
}

/** 打开帮助中心的某一页。 */
function openHelp(tab: HelpTab) {
  helpTab.value = tab;
  openPanel("help");
}

/** 关掉当前面板。 */
function closePanel() {
  activePanel.value = "none";
}

/** 开/关切换 —— 快捷键用。
 *
 *  【为什么打开时也要走 openPanel】开抽屉要收起小浮层（见 openPanel）——
 *  快捷键这条路径也得走同一条，否则 ⌘B 开着预览卡时，
 *  抽屉开了、预览卡的状态还留着（层级上已被遮罩盖住，但状态没清干净）。 */
function togglePanel(p: Panel) {
  if (activePanel.value === p) closePanel();
  else openPanel(p);
}

/** 书签与最近搜索 —— 【唯一的真相】。
 *
 *  【为什么面板不自己存一份】设置有两个写入口（这里的「收藏」按钮、
 *  面板里的删除）。两边各留一份拷贝就会【不同步】——
 *  而且那个不同步有后果：面板里删了书签，标题栏按钮还以为收藏着，
 *  再点一下「收藏」会把刚删掉的那条**加回来**。
 *  所以真相只在这里，面板拿 props 显示。 */
const bookmarks = ref<Bookmark[]>([]);
const recentQueries = ref<string[]>([]);
/** 书签 / 最近搜索的加载状态 —— 三态。 */
const bookmarksState = ref<SettingsLoadState>("loading");
/** 收藏按钮写失败的原因（成功时为 null）。 */
const bookmarkError = ref<string | null>(null);
/** 路径书签写入失败的原因（成功时为 null）—— 给右键菜单和预览卡显示。 */
const pathBookmarkError = ref<string | null>(null);
/** 面板里的错误（删除失败等）—— 写入方在这儿，面板只负责显示。 */
const panelError = ref<string | null>(null);

/** 当前查询是不是已经收藏了。 */
const currentBookmarked = computed(() => isQueryBookmarked(bookmarks.value, query.value));

/** 右键菜单里那个路径是不是已经收藏了。
 *
 *  【从真实的书签列表算，不自己存】菜单要是自己记一份，
 *  面板里删了书签、菜单还以为收藏着，再点一下「取消收藏」
 *  反而会把刚删掉的那条加回来。 */
const contextBookmarked = computed(() =>
  contextMenu.value ? isPathBookmarked(bookmarks.value, contextMenu.value.entry.path) : false,
);

/** 预览卡里那个路径是不是已经收藏了（同上）。 */
const previewBookmarked = computed(() =>
  preview.value ? isPathBookmarked(bookmarks.value, preview.value.entry.path) : false,
);

/** 收藏按钮上的提示。 */
const bookmarkTitle = computed(() => {
  if (bookmarkError.value) return `收藏失败：${bookmarkError.value}`;
  return currentBookmarked.value
    ? "已收藏当前查询 —— 再点一次移除（⌘B 打开书签面板）"
    : "收藏当前查询（⌘B 打开书签面板）";
});

/** 搜索框组件的句柄 —— 追加关键字后要把焦点还回去（见 onUseKeyword）。 */
const searchBar = ref<InstanceType<typeof SearchBar> | null>(null);

/* 索引就绪后要让结果表重搜一次。
   首次挂载时 ResultTable 立刻搜了一次 —— 那时索引还在后台建，
   后端回「索引尚未就绪」，空态就一直停在那句错误上。
   索引一就绪就把这个计数器加一，ResultTable 看到它变了就重搜。 */
const refreshKey = ref(0);

function onTableState(s: { result: SearchResult | null; loading: boolean }) {
  result.value = s.result;
  loading.value = s.loading;
}

function onPreview(entry: Entry, x: number, y: number) {
  /* 卡片要夹在窗口里 —— 右键点在屏幕边缘时，卡片一半会跑到窗口外。
   * 尺寸跟 styles.css 的 .preview-card 对齐（宽 320，高约 260）。 */
  const w = window.innerWidth;
  const h = window.innerHeight;
  preview.value = {
    entry,
    x: Math.max(8, Math.min(x, w - 320 - 8)),
    y: Math.max(8, Math.min(y, h - 260 - 8)),
  };
  /* 两层浮层不能叠着 —— 开卡片就把右键菜单收掉。 */
  contextMenu.value = null;
  /* 换了一个目标，上一次的失败提示不再适用。 */
  pathBookmarkError.value = null;
}

/** 结果行上右键 → 弹右键菜单（菜单里的「详情」才开预览卡）。
 *
 *  【为什么右键不再直接弹预览卡】路径书签原来【没有创建入口】——
 *  面板里那栏永远是空的。右键是放「收藏」最自然的位置，
 *  预览卡则保留在菜单的「详情」里（入口没丢）。 */
function onContextMenu(entry: Entry, x: number, y: number) {
  contextMenu.value = { entry, x, y };
  /* 两层浮层不能叠着 —— 开菜单就把预览卡收掉。 */
  preview.value = null;
  /* 换了一个目标，上一次的失败提示不再适用。 */
  pathBookmarkError.value = null;
}

async function onOpen(path: string) {
  await invoke("open_path", { path });
}

async function onReveal(path: string) {
  await revealItemInDir(path);
}

/* 索引状态要【轮询】：建索引是在后台线程做的，
   onMounted 那一刻通常还没扫完，不轮询的话状态栏会一直卡在「正在扫描」。 */

/** 轮询间隔（毫秒）。 */
const POLL_INTERVAL_MS = 800;

/** 正在跑的轮询定时器 —— `undefined` = 没在跑。 */
let timer: number | undefined;

/** 【开轮询】已经在跑就别开第二个。 */
function startPolling(): void {
  if (timer !== undefined) return;
  timer = window.setInterval(() => void pollStatus(), POLL_INTERVAL_MS);
}

/** 【关轮询】—— 窗口看不见时用它把这条 IPC 掐掉。 */
function stopPolling(): void {
  if (timer === undefined) return;
  window.clearInterval(timer);
  timer = undefined;
}

/** 两个索引状态**是不是**同一份** —— 见 `lib/status.ts` 的说明。 */

/** 上次已知的窗口可见性 —— `null` = 还不知道。 */
let lastVisible: boolean | null = null;

/**
 * 按「窗口现在可见吗」启停状态轮询 —— **只在变化时动作**。
 *
 *  【为什么要有这个函数（而不是各处直接 start/stop）】
 *   Rust 侧的看门狗**每秒**都会推一次当前可见性 ——
 *   不是「变了才推」，因为**事件投递是单点的**：
 *   若某一次事件恰好发生在前端 `listen` 注册**之前**（启动期那几毫秒），
 *   前端就停在「停轮询」，而 Rust 侧认为「没变过」→ **永不补发**
 *   → → 窗口明明可见，**状态栏永远不更新**。
 *   → 所以 Rust 那边「每次都推」，**去重就得在前端做**。
 *
 *  【为什么去重是安全的】`startPolling` / `stopPolling` 本身幂等，
 *   但去重还省掉了「每次事件都白跑一次 `pollStatus`」的 IPC。
 */
function applyVisibility(visible: boolean): void {
  /* 【认知没变 → 什么都不用做】
   *  这条去重不只是省事 —— 它还是「后端稳态零 IPC」的前提：
   *  后端只在「实际状态 ≠ 我们的认知」时才推事件，
   *  而我们每次**真正改变**认知时都会 ack 一次 → 两边一致 → 它不再推。 */
  if (lastVisible === visible) return;
  lastVisible = visible;

  /* 【告诉后端「我知道了」】——
   *  后端据此认为「我们已经同步」→ 稳态下**零 IPC**。
   *  （Tauri 的事件 = 在 webview 里跑一段 JS；窗口隐藏时每次要把睡着的
   *    webview 叫醒 → 实测每次约 7ms CPU → 每秒推一次就是白花 0.7%。）
   *  事件万一丢了 → 我们没 ack → 后端知道「我们还不知道」→ 继续推 → 自愈。 */
  void ackVisibility(visible).catch(() => {
    /* 后端不在（单元测试 / 非 Tauri 环境）时忽略即可 */
  });

  if (visible) {
    // 刚可见 → 先立刻拉一次最新状态（别干等下一个 800ms）
    void pollStatus();
    startPolling();
  } else {
    stopPolling();
  }
}

async function pollStatus() {
  try {
    const s = await indexStatus();
    const wasScanning = status.value?.scanning ?? true;
    /* 【只有真的变了才更新 `status`】—— 见 `sameIndexStatus` 的说明。
     *
     *  【这不影响下面的翻转检测】它看的是 `s.scanning`，
     *  而 `scanning` 变了 → 上面那个比较判成「不相等」→ 这里照样会更新。 */
    if (!sameIndexStatus(status.value, s)) {
      status.value = s;
    }
    /* 扫描结束的那一刻：让结果表重搜一次（见 refreshKey 的说明）
     *
     *  【这条现在只负责「启动」那一种情况】——
     *  启动时索引是在后台线程里建的，没有事件可听，只能靠轮询看到状态翻转。
     *  （`wasScanning` 在 status 还是 null 时默认 true，
     *    所以「索引在第一次轮询之前就建好了」也能撞上。）
     *
     *  「换索引目录」那种重建**不走这条路** —— 它由 `onReindexed` 事件驱动，
     *  不依赖轮询能不能撞上那次毫秒级的状态翻转。 */
    if (wasScanning && !s.scanning && s.indexed > 0) {
      refreshKey.value += 1;
    }
    /* 【窗口看不见 → 把轮询掐掉】
     *
     *  【为什么这是省 CPU 的关键】窗口藏着的时候用户看不见状态栏，
     *    这个轮询是**纯浪费** —— 而且它不是小数目：
     *    实测每次约 3.4ms CPU、1.25 次/秒，
     *    后台常驻时它占了整个应用 CPU 的**大头**。
     *
     *  【为什么这里拿得到可见性】`index_status` 的返回值里带 `visible`，
     *    Rust 侧问的是**窗口服务器的真相**（不是我们自己的标志）——
     *    所以「窗口被 ⌘H 收走」也能如实反映出来。
     *
     *  【为什么写 `!== false` 而不是直接 `if (s.visible)`】
     *    字段缺失（老版本 / 别的构造点）时**一律当成「可见」**：
     *    猜「不可见」会让状态栏停止更新（用户看得见的故障），
     *    猜「可见」只是多跑一点轮询（性能问题）。
     *    → 宁可性能出问题，也不要功能出问题。
     *
     *  【光靠这里醒不过来】停了轮询就没人再问「可见性变没变」了。
     *    → Rust 侧的看门狗**每秒**都会推一次当前可见性（`app-visibility` 事件，
     *      见 `listenForVisibility`）。两条路合起来才完整。 */
    applyVisibility(s.visible !== false);
  } catch {
    /* 索引还没建好时会报错，忽略即可 */
  }
}

/** 换索引目录成功了 —— 【立刻】重搜一次 + 更新状态栏。
 *
 *  【为什么不关抽屉】关抽屉是**设置面板自己的事** ——
 *  它要先写盘（写盘失败要在面板里显示错误，抽屉不能关）。
 *  这里的职责只有两个：把状态更新成新引擎的、把结果重搜一次
 *  （旧目录的搜索结果这时才被清掉）。
 *
 *  【为什么必须有这条】以前重搜只靠 `pollStatus` 里的状态翻转 ——
 *  而 `reindex` 对小目录是毫秒级完成的，800ms 的轮询大概率错过 →
 *  结果表里【旧目录的搜索结果还留在屏上】，状态栏还会同时显示
 *  「453 项（旧结果）… 已索引 5 项（新引擎）」。
 *  现在改成【事件驱动】：reindex 成功 ⇒ 一定重搜一次，不靠概率。 */
function onReindexed(st: IndexStatus) {
  status.value = st; // 状态栏立刻显示新的索引目录与条目数
  refreshKey.value += 1; // 结果表重搜 —— 旧结果这时才被清掉
}

/* 全局快捷键。
 *
 *  ⌘/ 打开/关闭语法帮助面板。
 *  ⌘B 打开/关闭书签与最近面板。
 *
 *  【为什么是 ⌘/ 而不是裸 /】裸 "/" 在搜索框里是「路径」的语法 ——
 *  裸 "/" 一按就弹面板，用户就没法输路径了。
 *  ⌘/ 才是 Raycast / Alfred 的惯例。
 *
 *  修饰键不满足就【直接放行】—— 裸 "/" 必须原样进搜索框。
 *
 *  【为什么要看 e.code】e.key === "/" 在美式布局下成立，
 *  但别的布局 / 输入法可能报成别的值。e.code 是【物理键位】
 *  （Slash = 斜杠键），跟布局无关 —— 两个条件取或更稳。
 *  而这个面板是【唯一的入口】（标题栏里没有按钮），
 *  按键万一没送达，面板就彻底不可达。 */
function onGlobalKeydown(e: KeyboardEvent) {
  /* 【Esc 关掉当前打开的面板】—— 三个面板都支持。
     （菜单的 Esc 由 ContextMenu 自己处理 —— 那个监听器也会响应，
       两个都关掉是合理的：Esc 就是「关掉眼前这层」。）

     【为什么要放在修饰键判断【之前】】Esc 没有修饰键 ——
     放在下面的 `if (!mod) return;` 之后就永远走不到。 */
  if (e.key === "Escape") {
    closePanel();
    return;
  }

  const mod = e.metaKey || e.ctrlKey;
  if (!mod) return;
  if (e.key === "/" || e.code === "Slash") {
    e.preventDefault();
    /* 帮助中心默认落在**速查**那页 —— 那是给普通用户的那页。
       （完整语法表在同一面板里，页签一换就到。） */
    if (activePanel.value === "help") closePanel();
    else openHelp("cheatsheet");
  } else if (e.key === "b" || e.code === "KeyB") {
    /* e.code 认的是【物理键位】—— ⌘⇧B（e.key 是 "B"）也能命中，
       跟上面 Slash 那条一个套路。 */
    e.preventDefault();
    togglePanel("bookmarks");
  }
}

/**
 * 拦掉整个窗口的**原生右键菜单**（WKWebView 的 Reload / Back / Forward / Copy…）。
 *
 * 【为什么】结果行那里已经有 `@contextmenu.prevent`（见 ResultTable.vue），
 *   但**只有行内**不弹；点**空白区域**没人管 →
 *   会弹出 WKWebView 的原生菜单，里面只有个「Reload」——
 *   一个原生 macOS 应用在空白处弹「重新载入」气质完全不对。
 *   （这是**一直存在**的行为，不是后台常驻模式带来的。）
 *
 * 【输入框要放行】输入框/可编辑区域保留原生菜单 ——
 *   用户要能右键粘贴。全局拦掉的话，右键粘贴就没了。
 */
function onGlobalContextMenu(e: MouseEvent): void {
  const t = e.target as HTMLElement | null;
  if (
    t &&
    (t.tagName === "INPUT" || t.tagName === "TEXTAREA" || t.isContentEditable)
  ) {
    return;
  }
  e.preventDefault();
}

/* 语法面板里点了一个关键字 → 【追加】到搜索框，然后把焦点还回去。
 *
 *  【追加不是替换】用户是在自己敲好的条件上再加一条，
 *  不能把他原来那部分静默丢掉。
 *
 *  【要聚焦回去】不然用户点完关键字还得再点一下搜索框才能接着打字。
 *  用 focusEnd() 而不是 focus() —— 后者会选中全文，
 *  用户接着打的第一个字就把整条查询替换掉了。 */
function onUseKeyword(keyword: string) {
  const next = appendKeyword(query.value, keyword);
  // null = 拒绝追加（查询里有未闭合的引号，追加会让字段被吞进引号里）。
  // 面板那边已经给过提示了，这里只是兜底。
  if (next === null) return;
  query.value = next;
  closePanel();
  // 等 DOM 更新完（输入框的值要先是新的）再聚焦
  void nextTick(() => searchBar.value?.focusEnd());
}

/* 速查卡里点了一条写法 → **整条替换**进搜索框，然后把焦点还回去。
 *
 *  【为什么是替换，不是追加】
 *  速查给的是**一整条完整查询**（比如 `ext:pdf;doc;xlsx`），
 *  不是「再加一个条件」——
 *  追加到用户已有的查询后面会变成两个条件打架，结果不是他想要的那条。
 *  （语法表页给的是**关键字**，那边才是追加 —— 见上面的 onUseKeyword。）
 *
 *  【为什么替换前不确认】
 *  用户点速查 = 「我要用这条」，他看见的搜索框内容也在同一屏里 ——
 *  原来的查询被换掉是预期的、可预期的。
 *  真要更保险，可以把旧查询记进「最近搜索」——
 *  但那会让「最近搜索」变成一个垃圾堆，不做。 */
function onUseExample(example: string) {
  query.value = example;
  closePanel();
  // 等 DOM 更新完（输入框的值要先是新的）再聚焦
  void nextTick(() => searchBar.value?.focusEnd());
}

onMounted(() => {
  /* 【第一次轮询顺带决定「要不要开轮询」】
   *  `pollStatus` 拿到 `visible` 之后会自己 start / stop。
   *  → 开机自启（`--hidden`）那次窗口从一开始就是隐藏的，
   *    于是**一次都不会轮询**（这才对：用户看不见，白轮询什么）。
   *  → 手动启动时窗口可见 → 轮询照开。
   *  → 之前这里是无条件 `setInterval` —— 开机自启时它在后台白烧 CPU。 */
  void pollStatus();
  window.addEventListener("keydown", onGlobalKeydown);
  window.addEventListener("contextmenu", onGlobalContextMenu);
  void reloadBookmarks();
  void listenForUserGuide();
  void listenForSyntaxCheatsheet();
  void listenForCredits();
  void listenForAutoRefresh();
  void listenForAwake();
  void listenForVisibility();
  void listenForOpenSettings();
});

/* 面板打开时重读 —— 面板显示的必须是【当前】的设置，
   不能是几秒前那份快照（比如用户在别的窗口改过）。 */
watch(showBookmarks, (open) => {
  if (open) void reloadBookmarks();
});

onBeforeUnmount(() => {
  stopPolling();
  window.removeEventListener("keydown", onGlobalKeydown);
  window.removeEventListener("contextmenu", onGlobalContextMenu);
  unlistenUserGuide?.();
  unlistenSyntaxCheatsheet?.();
  unlistenCredits?.();
  unlistenAutoRefresh?.();
  unlistenAwake?.();
  unlistenVisibility?.();
  unlistenOpenSettings?.();
});

/* ---- 原生菜单栏「帮助 → 完整手册」 ---- */

/** 取消监听的函数 —— 卸载时调它。 */
let unlistenUserGuide: UnlistenFn | null = null;

/** 订阅「打开手册」事件 —— 原生菜单栏点了「完整手册」时 Rust 会发这个事件。
 *
 *  【事件名】必须是 Rust 侧 `OPEN_USER_GUIDE_EVENT` 的值 `"open-user-guide"`。
 *
 *  【为什么要钉死一个字符串】Rust 发事件、前端收事件，
 *  两边各写各的就会「事件发了但没人听」——
 *  而这**不会报错**，只是手册面板永远打不开。
 *
 *  【为什么走事件而不是直接调函数】菜单在 **Rust 侧**，
 *  面板在 **前端** —— 跨进程只能靠事件。
 *
 *  【为什么用 openPanel() 而不是直接改 activePanel】
 *  openPanel 会把小浮层（预览卡 / 右键菜单）也收掉 ——
 *  抽屉是模态层，不能浮在它们上面。
 *
 *  【为什么这里写【字面量】而不是常量】
 *   Rust 侧那道护栏（`app/src-tauri/src/event_names.rs`）断言的是
 *   「这个事件名必须出现在**含 `listen` 的那一行**上」。
 *   写成常量的话，字符串只在**声明处**出现 ——
 *   有人后来把 `listen(...)` 删了，护栏照样绿（它守的正是「发了没人听」这条缝，
 *   却在最该生效的方向上失效 —— 那道护栏的第一版就栽在这儿）。
 *   → 订阅点与事件名写在同一行，删掉订阅就一定会红。 */
async function listenForUserGuide(): Promise<void> {
  try {
    const un = await listen("open-user-guide", () => {
      openHelp("manual");
    });
    unlistenUserGuide = un;
  } catch (e) {
    /* 【什么时候会失败】不在 Tauri 环境里（单元测试、浏览器里直接开 vite dev）。
       那时没有原生菜单栏可点，事件也不会来 —— 不影响别的功能。
       （单元测试里这个模块被 mock 掉了，见 App.test.ts / UserGuide.test.ts。） */
    console.warn("[sextant] 监听「打开手册」事件失败（不在 Tauri 环境里？）", e);
  }
}

/** 取消监听的函数 —— 卸载时调它。 */
let unlistenSyntaxCheatsheet: UnlistenFn | null = null;

/** 订阅「打开语法速查」事件 —— 原生菜单栏点了「搜索语法速查」时 Rust 会发这个。
 *
 *  【为什么要有它】菜单里「搜索语法速查」和「完整手册」是**两项**，
 *  但落到同一个面板的**不同页** —— 事件把「落到哪一页」也带过来。
 *
 *  【事件名必须是字面量】理由同上面那条（Rust 侧那道护栏
 *  断言的是「这个事件名出现在【含 listen 的那一行】上」）。 */
async function listenForSyntaxCheatsheet(): Promise<void> {
  try {
    const un = await listen("open-syntax-cheatsheet", () => {
      openHelp("cheatsheet");
    });
    unlistenSyntaxCheatsheet = un;
  } catch (e) {
    console.warn("[sextant] 监听「打开语法速查」事件失败（不在 Tauri 环境里？）", e);
  }
}

/* ---- 原生菜单栏「帮助 → 致谢与许可」 ---- */

/** 取消监听的函数 —— 卸载时调它。 */
let unlistenCredits: UnlistenFn | null = null;

/** 订阅「打开致谢与许可」事件 —— 原生菜单栏点了那一项时 Rust 会发这个事件。
 *
 *  【事件名必须是字面量】理由同上面两条（Rust 侧那道护栏
 *  断言的是「这个事件名出现在【含 listen 的那一行】上」）。
 *
 *  【为什么需要它】GPL-2.0 要求分发时能看到「修改声明 + 许可证」——
 *  这一项是它在**应用内**的落点（仓库 README 里的那份，软件装到
 *  /Applications 之后没人看得到）。 */
async function listenForCredits(): Promise<void> {
  try {
    const un = await listen("open-credits", () => {
      openPanel("credits");
    });
    unlistenCredits = un;
  } catch (e) {
    console.warn("[sextant] 监听「打开致谢与许可」事件失败（不在 Tauri 环境里？）", e);
  }
}

/* ---- 后台唤起 ---- */

/** 取消监听的函数 —— 卸载时调它。 */
let unlistenAwake: UnlistenFn | null = null;

/** 订阅「已唤起」事件 —— 快捷键 / 托盘把窗口叫出来时 Rust 会发这个。
 *
 *  【事件名】必须是 Rust 侧 `lifecycle::AWAKE_EVENT` 的值 `"app-awake"`。
 *
 *  【为什么要钉死一个字符串】Rust 发事件、前端收事件，
 *   两边各写各的就会「事件发了但没人听」——
 *   而这**不会报错**，只是唤起后搜索框不聚焦。
 *
 *  【为什么这里写【字面量】而不是常量】
 *   Rust 侧那道护栏（`app/src-tauri/src/event_names.rs`）断言的是
 *   「这个事件名必须出现在**含 `listen` 的那一行**上」。
 *   写成常量的话，字符串只在**声明处**出现 ——
 *   有人后来把 `listen(...)` 删了，护栏照样绿。
 *   → 订阅点与事件名写在同一行，删掉订阅就一定会红。
 *
 *  【为什么用 `focus()` 而不是 `focusEnd()`】
 *   `focus()` 会**选中全文** —— 用户唤起后直接打字就是替换掉上次的查询，
 *   不用先手动清空（Raycast 就是这个手感）。
 *   `focusEnd()` 只是把光标放到末尾（那是「追加关键字」用的）。
 *
 *  【为什么这里【不】因为「抽屉开着」就不聚焦搜索框】
 *   「托盘 → 设置…」那条路上**根本没有这个事件** ——
 *   Rust 侧的 `open_settings()` 只发 `open-settings`、不发 `app-awake`
 *   （见 `lifecycle.rs`：一条路径只发一个事件，「谁先到」的竞态从根上不存在）。
 *   → 这里的「已唤起」只来自**快捷键 / 托盘左键 / 点 Dock 图标**，
 *     那几条路的语义就是「我要搜索」→ 聚焦搜索框是对的行为。 */
async function listenForAwake(): Promise<void> {
  try {
    const un = await listen("app-awake", () => {
      void nextTick(() => searchBar.value?.focus());
    });
    unlistenAwake = un;
  } catch (e) {
    /* 【什么时候会失败】不在 Tauri 环境里（单元测试、浏览器里直接开 vite dev）。
       那时不会有唤起事件 —— 不影响别的功能。 */
    console.warn("[sextant] 监听「已唤起」事件失败（不在 Tauri 环境里？）", e);
  }
}

/* ---- 窗口可见性 ---- */

/** 取消监听的函数 —— 卸载时调它。 */
let unlistenVisibility: UnlistenFn | null = null;

/** 订阅「窗口可见性」事件 —— Rust 侧在窗口显示/隐藏时推过来（布尔载荷）。
 *
 *  【事件名】必须是 Rust 侧 `lifecycle::VISIBILITY_EVENT` 的值 `"app-visibility"`。
 *
 *  【为什么需要它】前端的 800ms 状态轮询在窗口隐藏时是**纯浪费**
 *    （用户看不见状态栏，但每次轮询都是一次跨进程 IPC —— 实测约 3.4ms CPU）。
 *    → 窗口一隐藏就把它掐掉，一可见就开回来。
 *
 *  【为什么「可见性」不能只靠轮询自己发现】
 *    轮询停了，就没人再问「可见性变没变」了 —— 没人问就永远不会醒。
 *    → **变化**必须由 Rust 推过来；
 *      「当前是什么状态」由轮询的返回值带回来（`index_status` 的 `visible`）。
 *      两条路合起来才闭环。
 *
 *  【为什么这里写【字面量】而不是常量】
 *    Rust 侧那道护栏（`app/src-tauri/src/event_names.rs`）断言的是
 *    「这个事件名必须出现在**含 `listen` 的那一行**上」。
 *    写成常量的话字符串只在声明处 —— 删掉订阅护栏也不会红。
 *    → 订阅点与事件名同行，删订阅必红。 */
async function listenForVisibility(): Promise<void> {
  try {
    const un = await listen<boolean>("app-visibility", (e) => {
      /* 【用 `=== true` 而不是真值判断】——
       *   载荷是 Rust 发的 `bool`，序列化过来就是 `true` / `false`。
       *   万一哪天它变成了字符串 `"false"`，真值判断会把它当成 **true**
       *   （非空字符串都是真值）→ 窗口明明收起了却继续轮询（白烧 CPU）。
       *   → 严格比 `true`，其他一律当作「不可见」（保守：该停就停）。 */
      applyVisibility(e.payload === true);
    });
    unlistenVisibility = un;
  } catch (e) {
    /* 【什么时候会失败】不在 Tauri 环境里（单元测试、浏览器里直接开 vite dev）。
       那时不会有可见性事件 —— 不影响别的功能。 */
    console.warn("[sextant] 监听「窗口可见性」事件失败（不在 Tauri 环境里？）", e);
  }
}

/* ---- 托盘菜单的「设置…」 ---- */

/** 取消监听的函数 —— 卸载时调它。 */
let unlistenOpenSettings: UnlistenFn | null = null;

/** 订阅「打开设置」事件 —— 托盘右键 →「设置…」时 Rust 会发这个。
 *
 *  【事件名】必须是 Rust 侧 `lifecycle::OPEN_SETTINGS_EVENT` 的值 `"open-settings"`。
 *
 *  【这一行曾经漏掉过】Rust 侧发事件、前端没有接收者 →
 *  点「设置…」只会把窗口叫出来、**设置抽屉不开**，
 *  而且**不报错**（用户视角就是「这个菜单项坏了」）。
 *  → 事件名钉死 + Rust 侧有一道护栏测试
 *    （见 `app/src-tauri/src/event_names.rs`）断言每个事件常量
 *    都能在前端的**订阅行**上找到。
 *
 *  【为什么这里写【字面量】而不是常量】
 *   那道护栏判的是「事件名出现在含 `listen` 的那一行」。
 *   用常量的话字符串只在声明处 —— 删掉监听护栏也不会红。
 *   → 订阅点与事件名同行，删订阅必红。
 *
 *  【为什么抽屉打开后还要动焦点】
 *   抽屉是模态层，搜索框在它**后面**。
 *   如果焦点还留在搜索框上，用户打的字会进到看不见的地方，
 *   而抽屉里的按钮按下去「没反应」。
 *   → 焦点交给抽屉本身（`tabindex="-1"` 的抽屉容器）。
 *
 *  【为什么不是「聚焦抽屉里的某个按钮」】
 *   聚焦按钮的话，按一下空格/回车就会**触发**那个按钮
 *   （聚焦「关闭」按钮 → 空格 = 关掉抽屉；聚焦「录制」→ 空格 = 开始录制）——
 *   都是误伤。焦点落在**容器**上就没有这个问题，
 *   而 Tab 一下就能进到里面的控件。 */
async function listenForOpenSettings(): Promise<void> {
  try {
    const un = await listen("open-settings", () => {
      openPanel("settings");
      void nextTick(() => {
        document.querySelector<HTMLElement>(".panel-drawer")?.focus();
      });
    });
    unlistenOpenSettings = un;
  } catch (e) {
    /* 【什么时候会失败】不在 Tauri 环境里（单元测试、浏览器里直接开 vite dev）。
       那时没有托盘可点 —— 不影响别的功能。 */
    console.warn("[sextant] 监听「打开设置」事件失败（不在 Tauri 环境里？）", e);
  }
}

/* ---- 文件变化时自动刷新索引（Rust 侧的 watcher） ---- */

/** 取消监听的函数 —— 卸载时调它。 */
let unlistenAutoRefresh: UnlistenFn | null = null;

/** 订阅「索引自动刷新完成」事件。
 *
 *  【事件名】必须是 Rust 侧 `watcher::AUTO_REFRESH_EVENT` 的值 `"index-auto-refreshed"`。
 *
 *  【为什么要钉死一个字符串】Rust 发事件、前端收事件，
 *  两边各写各的就会「事件发了但没人听」—— 而这**不会报错**，
 *  只是结果表永远不刷新。Rust 侧有一道护栏测试断言这个名字
 *  出现在本文件的**订阅行**上（见 `app/src-tauri/src/event_names.rs`）。
 *
 *  【为什么这里写【字面量】而不是常量】—— 同上：
 *  护栏判的是「事件名出现在含 `listen` 的那一行」，
 *  用常量的话字符串只在声明处，删掉订阅护栏也不会红。
 *
 *  【为什么要有它】自动重扫是在**后台**发生的（watcher 线程里），
 *  前端不知道 → 结果表里留着**旧索引**的结果。
 *  事件一来就：更新状态栏 + `refreshKey++`（结果表重搜一次）。
 *
 *  【载荷就是当前的索引状态】—— 不用再等 800ms 的轮询去撞状态翻转。
 *
 *  【为什么用事件而不是轮询】轮询撞「scanning 由 true 变 false」是不可靠的
 *  —— 扫描窗口可能短到错过（这正是 Phase 4 Task 1 修过的老问题）。
 *  事件是**一定**会到的。 */
async function listenForAutoRefresh(): Promise<void> {
  try {
    const un = await listen<IndexStatus>("index-auto-refreshed", (ev) => {
      status.value = ev.payload;
      refreshKey.value += 1;
    });
    unlistenAutoRefresh = un;
  } catch (e) {
    /* 【什么时候会失败】不在 Tauri 环境里（单元测试、浏览器里直接开 vite dev）。
       那时没有后端、不会有自动刷新事件 —— 不影响别的功能。 */
    console.warn("[sextant] 监听「索引自动刷新」事件失败（不在 Tauri 环境里？）", e);
  }
}

/** 「刷新」是不是正在飞。
 *
 *  【为什么要这个】rescan 不是**瞬时**的（$HOME 量级实测约 300 毫秒）。
 *  以前按钮毫无变化：不变色、不禁用、能连点 ——
 *  用户不知道点了没，白跑一遍又是白跑一遍。
 *
 *  【为什么用 finally 复位】失败也必须复位 ——
 *  不复位的话按钮会**永久禁用**，用户再也点不了刷新。 */
const rescanBusy = ref(false);

async function onRescan() {
  /* 连点的第二道保险（第一道是按钮的 :disabled）。 */
  if (rescanBusy.value) return;
  rescanBusy.value = true;
  try {
    /* 【刷新要重搜一次】——
       以前这里只有 `await rescan(); await pollStatus();`，
       而 rescan 完全不碰 IndexState → scanning 恒 false →
       pollStatus 的「wasScanning → refreshKey++」永不成立 →
       结果表里留着 rescan 之前的行，状态栏的「已索引 N 项」也不更新。
       现在 rescan 直接把**新的索引状态**返回来：更新状态栏 + 重搜。 */
    const st = await rescan();
    status.value = st;
    refreshKey.value += 1;
  } catch (e) {
    /* 失败原因已经进了 index_status.error —— 下一个轮询周期状态栏会显示它。 */
    console.error("[sextant] 重新扫描失败", e);
  } finally {
    /* 成功、失败都要复位 —— 见上面 rescanBusy 的说明。 */
    rescanBusy.value = false;
  }
}

/* ---- 最近搜索 + 书签 ---- */

/** 重读书签与最近搜索。
 *
 *  什么时候调：面板打开时、任何一次写入之后。
 *  写完立刻更新 —— 面板与标题栏按钮看到的必须是同一份。 */
async function reloadBookmarks() {
  bookmarksState.value = "loading";
  try {
    const s = await loadSettings();
    bookmarks.value = s.bookmarks;
    recentQueries.value = s.recentQueries;
    bookmarksState.value = "ready";
  } catch (e) {
    bookmarksState.value = "failed";
    panelError.value = `加载设置失败：${String(e)}`;
  }
}

/** 搜索【提交】时（用户按回车）记进「最近搜索」。
 *
 *  【为什么不是每次输入都记】那会记下一堆没提交的半截查询 ——
 *  用户每敲一个字符就多一条，列表全是垃圾。提交 = 用户真的想搜这个。
 *
 *  【写入走 patchSettings】它会先重新读盘上的真值，在它上面改这一个字段，
 *  再整体写回 —— 不会把索引目录 / 书签写没，也不会覆盖掉别人刚写的东西。 */
async function onSearchSubmit() {
  const q = query.value.trim();
  if (!q) return;
  try {
    const saved = await patchSettings((s) => {
      const next = pushRecent(s.recentQueries, q);
      // 没变 → 不写（再搜一遍同样的词不用再动文件）
      if (!recentChanged(s.recentQueries, next)) return null;
      return { recentQueries: next };
    });
    bookmarks.value = saved.bookmarks;
    recentQueries.value = saved.recentQueries;
  } catch (e) {
    console.error("[sextant] 记录最近搜索失败", e);
  }
}

/** 收藏 / 取消收藏当前查询（toggle 语义）。
 *
 *  跟最近搜索一样走 patchSettings —— 增删书签都在【完整对象】上做。 */
async function toggleCurrentQueryBookmark() {
  const q = query.value.trim();
  if (!q) return;
  bookmarkError.value = null;
  try {
    const saved = await patchSettings((s) => ({
      bookmarks: toggleBookmark(s.bookmarks, { kind: "query", value: q }),
    }));
    bookmarks.value = saved.bookmarks;
    recentQueries.value = saved.recentQueries;
  } catch (e) {
    console.error("[sextant] 收藏失败", e);
    bookmarkError.value = String(e);
  }
}

/** 收藏 / 取消收藏一个【路径】（toggle 语义）。
 *
 *  跟查询书签一样走 patchSettings —— 增删书签都在【完整对象】上做。
 *
 *  【为什么不设 label】面板里路径书签那栏用的是等宽字体、
 *  【从开头截断】—— 显示全路径时文件名（结尾）才留在可见区。
 *  塞个 basename 当 label 反而把路径信息弄丢了。 */
async function togglePathBookmark(path: string) {
  if (!path) return;
  pathBookmarkError.value = null;
  try {
    const saved = await patchSettings((s) => ({
      bookmarks: toggleBookmark(s.bookmarks, { kind: "path", value: path }),
    }));
    /* 写完立刻更新 → 菜单/卡片的文案自动翻成「取消收藏」。 */
    bookmarks.value = saved.bookmarks;
    recentQueries.value = saved.recentQueries;
  } catch (e) {
    console.error("[sextant] 收藏路径失败", e);
    /* 不静默吞掉 —— 菜单与卡片底部会显示它。 */
    pathBookmarkError.value = String(e);
  }
}

/** 面板里点了删除 —— 写入在这里做，面板只发意图。
 *
 *  【为什么删除也在 App.vue 写】面板那边没有完整的设置对象 ——
 *  拿面板手里那份（可能旧的）写回去，会把别的字段写没。
 *  patchSettings 会先重新读盘上的真值，在它上面删这一条，再整体写回。 */
async function onRemoveBookmark(b: Bookmark) {
  panelError.value = null;
  try {
    const saved = await patchSettings((s) => ({
      bookmarks: removeBookmark(s.bookmarks, b),
    }));
    // 写完立刻更新 —— 标题栏的「收藏」按钮要立刻知道这条没了
    bookmarks.value = saved.bookmarks;
    recentQueries.value = saved.recentQueries;
  } catch (e) {
    console.error("[sextant] 删除书签失败", e);
    panelError.value = `删除书签失败：${String(e)}`;
  }
}

/** 用一条最近搜索 / 查询书签 —— 填进搜索框并重新搜。
 *
 *  改 query 会触发 ResultTable 的 watch → 自动重搜（见 ResultTable.vue）。 */
function onUseQuery(q: string) {
  query.value = q;
  closePanel();
}

/** 用一条路径书签 —— 在访达里定位。 */
async function onUsePath(path: string) {
  closePanel();
  try {
    await revealItemInDir(path);
  } catch (e) {
    console.error("[sextant] 在访达里定位失败", e);
  }
}

/* ---- 书签导入 / 导出 ----
 *
 *  【为什么写在 App.vue 而不是面板里】面板只管显示与发意图 ——
 *   设置有两个写入口时，各留一份拷贝就会不同步（见上面 bookmarks 的说明）。
 *   写入一律走 patchSettings（它会先重读盘上的真值再改这一个字段）。
 *
 *  【为什么用 dialog 拿路径 + Rust 落盘】前端没有文件系统的能力
 *   （没有 @tauri-apps/plugin-fs，也**不打算加**）——
 *   路径由**已装的** `@tauri-apps/plugin-dialog` 拿，
 *   落盘由 Rust 的 `write_bookmarks_file` / `read_bookmarks_file` 做（std::fs，零新依赖）。 */

/** 导出书签 —— 面板发意图，真正的活在这儿。 */
async function onExportBookmarks() {
  panelError.value = null;
  try {
    const path = await saveFileDialog({
      defaultPath: "fsearch-bookmarks.json",
      filters: [{ name: "JSON", extensions: ["json"] }],
    });
    // 用户取消选择 → 什么都不做（不是错误）
    if (!path) return;
    await writeBookmarksFile(path, buildBookmarksFile(bookmarks.value));
  } catch (e) {
    console.error("[sextant] 导出书签失败", e);
    panelError.value = `导出书签失败：${String(e)}`;
  }
}

/** 导入书签 —— **追加**（合并），不是替换。
 *
 *  【为什么追加】替换会静默丢掉用户已有的书签；追加更安全。
 *  【校验不过 → 什么都不改】连设置都不碰。
 */
async function onImportBookmarks() {
  panelError.value = null;
  try {
    const picked = await openFileDialog({
      multiple: false,
      filters: [{ name: "JSON", extensions: ["json"] }],
    });
    // 用户取消选择 → 什么都不做（不是错误）
    if (!picked) return;
    const path = Array.isArray(picked) ? picked[0] : picked;
    if (!path) return;

    const text = await readBookmarksFile(path);
    const parsed = parseBookmarksFile(text);
    if (!parsed.ok) {
      /* 【校验不过 → 一条都不导入】连设置都不碰。 */
      panelError.value = `导入书签失败：${parsed.error}`;
      return;
    }

    const saved = await patchSettings((s) => ({
      bookmarks: mergeBookmarks(s.bookmarks, parsed.bookmarks),
    }));
    // 写完立刻更新 → 面板（靠 props 显示）立刻看到新书签
    bookmarks.value = saved.bookmarks;
    recentQueries.value = saved.recentQueries;
  } catch (e) {
    console.error("[sextant] 导入书签失败", e);
    panelError.value = `导入书签失败：${String(e)}`;
  }
}
</script>

<template>
  <div class="app">
    <AppIcons />
    <!-- data-tauri-drag-region 必须写 "deep"：
         裸属性只对【元素自身】生效（脚本判的是 el === composedPath[0]），
         子元素（左侧留白、搜索框容器）点下去都拖不动。
         "deep" 才是整棵子树可拖。 -->
    <header class="titlebar" data-tauri-drag-region="deep">
      <!-- 左侧留出交通灯的位置（macOS 约 78pt） -->
      <div class="tb-spacer" />
      <SearchBar ref="searchBar" v-model="query" :busy="loading" @submit="onSearchSubmit" />
      <button
        class="tb-refresh"
        title="重新扫描索引"
        data-tauri-drag-region="false"
        :disabled="rescanBusy"
        @click="onRescan"
      >
        {{ rescanBusy ? "刷新中…" : "刷新" }}
      </button>
      <button
        class="tb-refresh"
        :class="{ 'is-on': currentBookmarked }"
        :disabled="!query.trim()"
        :title="bookmarkTitle"
        data-tauri-drag-region="false"
        @click="toggleCurrentQueryBookmark"
      >
        {{ currentBookmarked ? "已收藏" : "收藏" }}
      </button>
      <button
        class="tb-refresh"
        title="设置：索引目录等"
        data-tauri-drag-region="false"
        @click="openPanel('settings')"
      >
        设置
      </button>
    </header>
    <ResultTable
      :query="query"
      :kind="kind"
      :sort="sort"
      :order="order"
      :refresh-key="refreshKey"
      @open="onOpen"
      @reveal="onReveal"
      @sort="(f) => (sort = f)"
      @order="(o) => (order = o)"
      @preview="onPreview"
      @context-menu="onContextMenu"
      @state="onTableState"
    />
    <StatusBar :result="result" :status="status" :loading="loading" />
    <SettingsPanel
      v-if="showSettings"
      @close="closePanel()"
      @reindexed="onReindexed"
    />
    <HelpCenterPanel
      v-if="showHelp"
      :tab="helpTab"
      :query="query"
      @close="closePanel()"
      @tab="helpTab = $event"
      @use-example="onUseExample"
      @use-keyword="onUseKeyword"
    />
    <BookmarkPanel
      v-if="showBookmarks"
      :bookmarks="bookmarks"
      :recent-queries="recentQueries"
      :state="bookmarksState"
      :error="panelError"
      @close="closePanel()"
      @use-query="onUseQuery"
      @use-path="onUsePath"
      @remove-bookmark="onRemoveBookmark"
      @export-bookmarks="onExportBookmarks"
      @import-bookmarks="onImportBookmarks"
    />
    <CreditsPanel v-if="showCredits" @close="closePanel()" />
    <ContextMenu
      v-if="contextMenu"
      :entry="contextMenu.entry"
      :x="contextMenu.x"
      :y="contextMenu.y"
      :bookmarked="contextBookmarked"
      :error="pathBookmarkError"
      @close="contextMenu = null"
      @open="onOpen"
      @reveal="onReveal"
      @bookmark="togglePathBookmark"
      @preview="onPreview"
    />
    <PreviewCard
      :entry="preview?.entry ?? null"
      :x="preview?.x ?? 0"
      :y="preview?.y ?? 0"
      :bookmarked="previewBookmarked"
      :error="pathBookmarkError"
      @close="preview = null"
      @open="onOpen"
      @reveal="onReveal"
      @bookmark="togglePathBookmark"
    />
  </div>
</template>
