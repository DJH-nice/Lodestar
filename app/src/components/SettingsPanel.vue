<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref } from "vue";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import { excludeRules, reindex } from "../lib/api";
import { loadSettings, patchSettings } from "../lib/settings-io";
import {
  applyButtonText,
  applyGuardReason,
  canApplySettings,
  loadFailedHint,
  shouldCloseOnBackdrop,
} from "../lib/settings-panel";
import { labelFromSpec, isModifierCode, isSupportedMainKey, shortcutFromEvent, shortcutRuntimeView, shortcutViewFromChange, type RecordedShortcut } from "../lib/shortcut";
import type { SettingsLoadState } from "../lib/settings-panel";
import type { IndexStatus, Settings, ShortcutChange, ShortcutRuntimeState } from "../lib/types";

const emit = defineEmits<{
  (e: "close"): void;
  /** 重建成功了 —— **把新的索引状态带出去**。
   *
   *  【为什么必须带】父组件要靠它【立刻】重搜一次并更新状态栏。
   *  以前父组件只能靠「轮询恰好撞见 scanning 由 true 变 false」来触发重搜 ——
   *  换个小目录时 reindex 是毫秒级完成的，800ms 的轮询大概率错过那次翻转 →
   *  结果表里【旧目录的搜索结果还留在屏上】，状态栏还会同时显示
   *  「453 项（旧结果）… 已索引 5 项（新引擎）」。 */
  (e: "reindexed", status: IndexStatus): void;
}>();

/** 完整的设置对象 —— 改任何一个字段都要整体写回，所以必须一直持有完整对象。 */
const settings = ref<Settings>({
  indexPaths: [],
  sortPref: undefined,
  kindPref: undefined,
  bookmarks: [],
  recentQueries: [],
});
/** 「文件变化时自动刷新索引」—— **独立的一份**，不放在 `settings` 里。
 *
 *  【为什么不放在 settings 里】
 *   这个开关**不需要**「应用并重建索引」—— 它一改就该生效
 *   （Rust 侧的 watcher 每次要重扫前都会读一次设置）。
 *   而 `apply()` 走的是 `patchSettings(() => ({ indexPaths }))` ——
 *   面板里那份 `settings` 快照只是用来发 reindex 的，
 *   把开关混进去会让「改了开关、点应用」这种组合变得难以推理。
 *
 *  【为什么单独存一个 ref 而不是直接改 settings.value】
 *   `settings.value` 是**打开面板时**读的那份快照，可能已经旧了。
 *   开关的真相在盘上 —— 写完要用 `patchSettings` 返回的那份更新本地状态。 */
const autoRefresh = ref(true);
const rules = ref<string[]>([]);
/** 加载设置失败的原因（数据丢失路径相关）。 */
const loadError = ref<string | null>(null);
/** 其他失败（读排除规则 / 重建索引）。 */
const error = ref<string | null>(null);
const busy = ref(false);

/* ---- 全局快捷键（录制式） ---- */

/** 正在等用户按组合键。 */
const recording = ref(false);
/** 当前快捷键（后端规格）。null = 没设。 */
const shortcutSpec = ref<string | null>(null);
/** 设置里存的**偏好**（可能因为注册失败而**没生效**）。 */
const shortcutDesired = ref<string | null>(null);
/** 当前快捷键（给人看的）。 */
const shortcutLabel = ref("");
/** 快捷键相关的错误（注册失败、被占用…）。 */
const shortcutError = ref<string | null>(null);

/* ---- 开机自启 ---- */

/** 开机自启开关的当前状态。 */
const launchAtLogin = ref(false);
/** 开机自启相关的错误（写 plist 失败…）。 */
const launchAtLoginError = ref<string | null>(null);
/** 设置的加载状态 —— 三态，见 lib/settings-panel.ts 的说明。
 *
 *  【关键】加载失败时**不能**置成「可以应用」——
 *   那样 `settings.value` 里的 indexPaths 还是默认值（空），
 *   一按「应用」→ 索引目录被写回空（= 索引家目录）——
 *   用户配的索引目录就没了。
 *   （书签 / 最近搜索现在不会被写没 —— 写入走 patchSettings，
 *     它会先重新读盘上的真值。但 indexPaths 正是这次要写的字段，
 *     读失败时手里那份就是空的 —— 所以这道闸必须留着。） */
const loadState = ref<SettingsLoadState>("loading");

onMounted(async () => {
  try {
    settings.value = await loadSettings();
    /* 【缺省是「开」】老配置里没有这个字段时，Rust 会补成 true；
       万一读到的是 undefined（还没被 Rust 补过），也按「开」处理 ——
       与 Rust 侧的 `default_true()` 保持一致。 */
    autoRefresh.value = settings.value.autoRefresh ?? true;
    /* 【当前快捷键 —— 显示的是【实际生效】的那个】
     *
     *   终审 I-1：启动时快捷键注册失败 → 设置里**一个字都没改** →
     *   面板若直接显示设置里的值 → 面板说「⌥⌘Space 已生效」、
     *   用户按了没反应 = **骗人**。
     *   → 问后端要**真相**（`get_global_shortcut`），
     *     偏好几（设置里的值）留着 —— 用户重试时还要用它。 */
    shortcutDesired.value = settings.value.globalShortcut ?? null;
    await loadShortcutTruth();
    // 【开机自启读【系统】的真值，不是设置里的值】——
    //   设置里存的是「用户上次的选择」，而用户可能手动删掉了登录项。
    //   开关显示的必须是**实际状态**，否则面板说「开着」而实际上没有 = 骗人。
    //   （读失败 → 后端会**报错** → 这里沿用设置里的值，
    //     并且不让「读失败」被误当成「确实是关的」。）
    launchAtLogin.value = settings.value.launchAtLogin === true;
    try {
      launchAtLogin.value = await invoke<boolean>("get_launch_at_login");
    } catch (e) {
      console.warn("[lodestar] 读开机自启状态失败，沿用设置里的值", e);
      /* 【要摆到界面上，不能只写日志】（终审 M-8）——
         读失败时开关显示的是**设置里的旧值**，可能与系统里的真值不符。
         只 `console.warn` 的话，用户看着一个**可能不准**的开关去改设置，
         改的还是设置文件 —— 而系统那边根本没动。
         → 把原因显示出来（展示位 `launchAtLoginError` 本来就有）。 */
      launchAtLoginError.value = `读开机自启状态失败，开关显示的是设置里的值（可能不准）：${String(e)}`;
    }
    loadState.value = "ready";
  } catch (e) {
    /* 加载失败 —— 「应用」保持禁用，并把原因告诉用户。
     * （用户仍然可以「关闭」出去：关抽屉不受加载状态影响。） */
    loadError.value = loadFailedHint(e);
    loadState.value = "failed";
  }
  /* 排除规则是【另一件事】—— 它挂了不影响设置本身能不能写回，
   * 所以单独兜一份错误，不牵连 loadState。 */
  try {
    rules.value = await excludeRules();
  } catch (e) {
    error.value = `读取排除规则失败：${String(e)}`;
  }
});

/** 把「当前生效的快捷键」同步到界面。
 *
 *  【为什么标签要从规格反推】设置里存的是**规格**
 *   （"Option+Cmd+Space"，给后端 `global-hotkey` 解析用），
 *   而按钮上要显示的是**标签**（"⌥⌘Space"）——
 *   两者不是一回事，直接把规格显示出去用户认不出来。 */
function setShortcut(spec: string | null): void {
  shortcutSpec.value = spec;
  shortcutLabel.value = spec ? labelFromSpec(spec) : "";
}

/** 问后端要「**此刻生效的是哪个键**」，把面板显示成那个。
 *
 *  【为什么不能只读设置】
 *   设置里的是**偏好**，不是**事实** ——
 *   启动时注册失败时两者就不一致了（见 onMounted 里的说明）。
 *
 *  【问不到怎么办】
 *   退回偏好看，但**说清「不确定」** ——
 *   闷声显示偏好 = 又一次骗人。 */
async function loadShortcutTruth(): Promise<void> {
  let runtime: ShortcutRuntimeState | null = null;
  try {
    runtime = await invoke<ShortcutRuntimeState>("get_global_shortcut");
  } catch (e) {
    console.warn("[lodestar] 问不到快捷键的实际生效状态", e);
  }
  const view = shortcutRuntimeView(runtime, shortcutDesired.value);
  setShortcut(view.spec);
  shortcutError.value = view.error;
}

/** 把后端返回的变更结果铺到界面上。
 *
 *  【为什么走 `shortcutViewFromChange`】那是**前后端契约的落地点** ——
 *   显示的是 `effective`（**实际生效**的那个），不是用户刚录的那个；
 *   `warning` 非空就把警告显示出来。
 *   这段逻辑抽成了纯函数（见 lib/shortcut.ts）→ 有单测钉着。 */
function applyChange(res: ShortcutChange): void {
  const view = shortcutViewFromChange(res);
  shortcutSpec.value = view.spec;
  shortcutLabel.value = view.label;
  shortcutError.value = view.error;
  /* 【偏好也跟着更新】——
     后端那条命令已经按「设置 = 实际生效的键」把盘上的偏好改了
     （见 `commands::set_global_shortcut` 的硬规则），
     所以本地的偏好 = 实际生效的那个。 */
  shortcutDesired.value = view.spec;
}

/** 开始录制 —— 接下来按下的组合键就是要用的快捷键。 */
function startRecording(): void {
  recording.value = true;
  shortcutError.value = null;
  window.addEventListener("keydown", onRecordKey, true);
}

/** 停止录制。 */
function stopRecording(): void {
  recording.value = false;
  window.removeEventListener("keydown", onRecordKey, true);
}

/** 录制时的按键捕获。
 *
 *  【为什么要 `capture = true`】在**捕获阶段**拦下，
 *  才不会被页面自己的 keydown 处理先吃掉。 */
function onRecordKey(e: KeyboardEvent): void {
  // 录制期间把按键全吃掉 —— 否则 ⌘W 之类的会关掉应用。
  e.preventDefault();
  e.stopPropagation();
  if (e.key === "Escape") {
    stopRecording();
    return;
  }
  // 【只按了修饰键】→ 继续等主键（不是错误）。
  if (isModifierCode(e.code)) return;
  // 【前端预校验】（终审 M-7）——
  //   后端（global-hotkey）只认固定的一批键名。
  //   按了它不认的键（Fn、媒体键…）时**当场**说清，
  //   而不是等后端回一句「快捷键格式非法」。
  if (!isSupportedMainKey(e.code)) {
    shortcutError.value = `这个键不能用作快捷键（${e.code}）—— 请换一个键`;
    return;
  }
  const rec = shortcutFromEvent(e);
  // null = 没有修饰键 → 继续等
  if (!rec) return;
  stopRecording();
  void applyShortcut(rec);
}

/** 换一个全局快捷键。
 *
 *  【后端返回的不是一个简单的「成功 / 失败」】
 *   注册失败（被别的软件占着）→ **什么都没变**，旧键还在生效；
 *   注销失败 → **新键已经生效**，但旧键可能还在（两个都会唤起）。
 *   → 界面显示的快捷键要更新成后端告诉我们的 `effective`，
 *     **不能**用用户刚录的那个（那可能压根没生效）。 */
async function applyShortcut(rec: RecordedShortcut): Promise<void> {
  busy.value = true;
  shortcutError.value = null;
  try {
    const res = await invoke<ShortcutChange>("set_global_shortcut", { spec: rec.spec });
    applyChange(res);
  } catch (e) {
    shortcutError.value = String(e);
  } finally {
    busy.value = false;
  }
}

/** 关掉全局快捷键。 */
async function clearShortcut(): Promise<void> {
  busy.value = true;
  shortcutError.value = null;
  try {
    // 【必须传 null，不能传空串】——
    //   `spec: Option<String>` 收到 `Some("")` 会去注册一个空快捷键然后报错；
    //   `None` 的语义是「注销掉、不注册」。
    const res = await invoke<ShortcutChange>("set_global_shortcut", { spec: null });
    applyChange(res);
  } catch (e) {
    shortcutError.value = String(e);
  } finally {
    busy.value = false;
  }
}

/** 拨动「开机时自动在后台驻留」开关 —— **立即生效**。
 *
 *  【为什么不用 `patchSettings`】这个开关改的是**系统**（写/删 LaunchAgent），
 *   不是设置文件 —— 而且顺序要求是「**先改系统、成功才写盘**」
 *   （写盘成功但系统改失败 → 设置说开着、实际上没有 = 骗人）。
 *   这条顺序只能由后端保证，所以走单独的命令。 */
async function toggleLaunchAtLogin(): Promise<void> {
  busy.value = true;
  launchAtLoginError.value = null;
  const next = !launchAtLogin.value;
  try {
    await invoke("set_launch_at_login", { enabled: next });
    launchAtLogin.value = next;
  } catch (e) {
    launchAtLoginError.value = String(e);
  } finally {
    busy.value = false;
  }
}

/* 【面板关掉时要把录制监听摘掉】
   面板是 `v-if` 挂载的 —— 关闭时组件被销毁。
   监听器不摘掉的话它会一直活着（还会往已经销毁的组件上写状态）。 */
onBeforeUnmount(() => {
  stopRecording();
});

/** 「应用并重建索引」能不能点。 */
const canApply = computed(() => canApplySettings(loadState.value, busy.value));
/** 按钮上的字。 */
const applyLabel = computed(() => applyButtonText(loadState.value, busy.value));

/** 点遮罩关抽屉 —— 重建期间不关（否则错误信息随组件卸载一起没了）。 */
function onBackdrop() {
  if (!shouldCloseOnBackdrop(busy.value)) return;
  emit("close");
}

async function addDirs() {
  const picked = await open({
    directory: true,
    multiple: true,
    title: "选择要索引的目录",
  });
  if (!picked) return;
  const list = Array.isArray(picked) ? picked : [picked];
  for (const p of list) {
    if (!settings.value.indexPaths.includes(p)) {
      settings.value.indexPaths.push(p);
    }
  }
}

function removeDir(p: string) {
  settings.value.indexPaths = settings.value.indexPaths.filter((x) => x !== p);
}

/** 拨动「文件变化时自动刷新索引」开关 —— **立即生效**，不用点「应用」。
 *
 *  【为什么立即生效】
 *   Rust 侧的 watcher 每次「真的要重扫之前」会读一次设置 ——
 *   所以开关一写进盘上，下一个文件事件就按新的来。
 *   不需要重建索引（重建只跟索引目录有关）。
 *
 *  【为什么写入走 patchSettings】
 *   `set_settings` 是**整体替换** —— 直接写会把这个字段之外的东西
 *   （索引目录、书签…）一起写没。`patchSettings` 先重新读盘上的真值、
 *   只改这一个字段、再整体写回。
 *
 *  【为什么失败时不改本地状态】
 *   盘上没存上 → 本地状态必须跟盘上一致 → 开关拨回去，
 *   并把原因显示出来（不静默吞掉）。 */
async function toggleAutoRefresh() {
  const next = !autoRefresh.value;
  try {
    const saved = await patchSettings(() => ({ autoRefresh: next }));
    autoRefresh.value = saved.autoRefresh ?? next;
    error.value = null;
  } catch (e) {
    error.value = `自动刷新开关没有保存：${String(e)}`;
  }
}

async function apply() {
  /* 【逻辑层守卫 —— 双保险】
   *  按钮上的 :disabled 只挡住了「点不到」，那还是 UI 层的保险：
   *  组件的 disabled 绑定一改，这条【静默丢用户设置】的路径就又开了 ——
   *  加载失败时 settings.value 还是默认值，写回去就是覆盖用户真正的
   *  书签 / 最近搜索 / 索引目录。所以在逻辑层再挡一道。 */
  const reason = applyGuardReason(loadState.value);
  if (reason) {
    error.value = reason;
    return;
  }
  /* 重建中连点 —— 忽略（按钮本来就是禁着的，这里只是兜底）。 */
  if (busy.value) return;
  busy.value = true;
  error.value = null;
  // 【快照】—— 重建要 1~2 秒（扫全盘），这段时间用户还能改面板
  //   （加目录 / 移除目录）。
  //   如果 `reindex` 用调用瞬间的数组、而写回设置用的是 await 之后的
  //   最新对象，就会出现「设置里写了新目录、引擎只索引了旧的那组」——
  //   又一次「设置 ≠ 引擎」。
  //   快照一份，两次调用用同一份。
  const snapshot: Settings = {
    ...settings.value,
    indexPaths: [...settings.value.indexPaths],
  };
  /* 【顺序：先重建，再刷新界面，最后写盘】
   *
   *  ① reindex 成功 = **引擎已经换了** → 界面必须立刻反映新引擎
   *     （旧目录的搜索结果要清掉、状态栏要换成新条目数）。
   *  ② 写盘在后面：写盘失败只是「设置没存上」，
   *     **不能连带界面不刷新** —— 那正是「引擎 ≠ 界面」，
   *     屏上还是旧目录的结果、盘上还是旧路径，界面两头不靠。
   *  ③ 只有两步都成功才关抽屉 —— 写盘失败要在面板里把原因说清楚。
   *
   *  【空列表 = 索引家目录】—— 后端 `reindex` 会把空列表回落成 `$HOME`
   *  （与启动路径同一套回退），所以「移除最后一个目录」是合法的操作，
   *  不会报 "invalid argument"。 */
  let st: IndexStatus;
  try {
    st = await reindex(snapshot.indexPaths);
  } catch (e) {
    // 重建失败：旧引擎没动、设置也没写 → 状态一致，直接报错
    error.value = String(e);
    busy.value = false;
    return;
  }
  /* 【把新状态带出去】—— 父组件靠它立刻重搜 + 更新状态栏，
   *   不依赖「轮询撞见状态翻转」这种概率性机制。
   *   【放在写盘之前】—— 写盘失败也要刷新（引擎真的换了）。 */
  emit("reindexed", st);
  try {
    /* 【写回走 patchSettings —— 不能直接 saveSettings(snapshot)】
     *   `set_settings` 是**整体替换**：拿 snapshot 直接写，
     *   就把 snapshot 里【没有的字段】一并清空了 ——
     *   而 snapshot 是这个面板打开时读的，可能已经旧了：
     *   用户在这期间收藏了个书签（写进了盘上），这里一写就把它抹掉。
     *   patchSettings 会先重新读盘上的真值，在它上面只改 indexPaths，
     *   再整体写回 —— 别的字段（书签 / 最近搜索）原样保留。 */
    await patchSettings(() => ({ indexPaths: snapshot.indexPaths }));
    /* 全都成功了 → 关抽屉 */
    emit("close");
  } catch (e) {
    /* 引擎已经换了、界面也已经刷新了 —— 只有设置没存上。
     *   说清楚是哪一步坏了，别让用户以为「什么都没发生」。 */
    error.value = `索引已重建，但设置没有保存：${String(e)}`;
  } finally {
    busy.value = false;
  }
}
</script>

<template>
  <div class="panel-drawer-backdrop" @click.self="onBackdrop">
    <!-- 【tabindex="-1"】抽屉可以被**程序**聚焦（托盘「设置…」那条路要把焦点
         交给抽屉 —— 搜索框在抽屉**后面**，焦点留在那儿的话用户打的字
         会进到看不见的地方）。
         -1 = 不进 Tab 顺序（Tab 一下就进到里面的控件），
         也不会让任何按钮被「顺手」触发。 -->
    <section class="panel-drawer" tabindex="-1" @click.stop>
      <header class="panel-head">
        <h2>设置</h2>
        <button class="panel-close" :disabled="busy" @click="emit('close')">×</button>
      </header>

      <div class="panel-body">
        <section class="set-block">
          <h3>索引目录</h3>
          <p class="set-hint">
            留空 = 索引整个用户家目录（约 9000 项 / 约 0.3 秒）。
            换了目录会重新扫描一次。
          </p>
          <ul class="dir-list">
            <li v-for="p in settings.indexPaths" :key="p">
              <span class="dir-path">{{ p }}</span>
              <button class="dir-del" :disabled="busy" @click="removeDir(p)">
                移除
              </button>
            </li>
            <li v-if="loadState === 'loading'" class="dir-empty">正在加载设置…</li>
            <li v-else-if="loadState === 'failed'" class="dir-empty">
              （设置加载失败 —— 下面的目录列表不可用）
            </li>
            <li v-else-if="!settings.indexPaths.length" class="dir-empty">
              （未设置 → 使用用户家目录）
            </li>
          </ul>
          <button
            class="set-btn"
            :disabled="busy || loadState !== 'ready'"
            @click="addDirs"
          >
            添加目录…
          </button>

          <!-- 「文件变化时自动刷新索引」开关。
               【为什么放在「索引目录」这一节】它管的就是这些目录 ——
               只监听【被索引的】目录，目录换了它自己会跟上（Rust 侧每 1 秒对账一次）。 -->
          <div class="set-row">
            <div class="set-row-text">
              <span class="set-row-label">文件变化时自动刷新索引</span>
              <p class="set-hint">
                被索引的目录里有文件新建或修改时，自动重扫一遍索引
                （索引整个家目录时每扫一次约 2 秒）。<b>开关立即生效</b>，
                不需要重建索引。
              </p>
            </div>
            <button
              class="set-switch"
              role="switch"
              :aria-checked="autoRefresh"
              :aria-label="'文件变化时自动刷新索引'"
              :disabled="busy || loadState !== 'ready'"
              @click="toggleAutoRefresh"
            >
              <span class="set-switch-knob"></span>
            </button>
          </div>
        </section>

        <section class="set-block">
          <h3>后台驻留与唤起</h3>
          <p class="set-hint">
            关掉窗口后 Lodestar 不会退出，而是在后台待命（菜单栏图标常驻）——
            按全局快捷键就能把它叫回来。
          </p>

          <!-- 全局快捷键。 -->
          <div class="set-row">
            <div class="set-row-text">
              <span class="set-row-label">全局快捷键</span>
              <p class="set-hint">
                在任何软件里按这个组合键都能唤起 Lodestar。
                点「录制」后，直接按下想要的组合键就行（Esc 取消）。
              </p>
            </div>
            <div class="shortcut-recorder">
              <button
                class="set-btn"
                :disabled="busy || loadState !== 'ready'"
                @click="recording ? stopRecording() : startRecording()"
              >
                {{ recording ? "请按下组合键…" : (shortcutLabel || "未设置") }}
              </button>
              <button
                v-if="shortcutSpec || shortcutDesired"
                class="set-btn"
                :disabled="busy"
                @click="clearShortcut"
              >
                清除
              </button>
            </div>
          </div>
          <p v-if="shortcutError" class="set-error">{{ shortcutError }}</p>

          <!-- 开机自启。 -->
          <div class="set-row">
            <div class="set-row-text">
              <span class="set-row-label">开机时自动在后台驻留</span>
              <p class="set-hint">
                登录后 Lodestar 会在后台待命，按全局快捷键即可唤起。
              </p>
            </div>
            <button
              class="set-switch"
              role="switch"
              :aria-checked="launchAtLogin"
              aria-label="开机时自动在后台驻留"
              :disabled="busy || loadState !== 'ready'"
              @click="toggleLaunchAtLogin"
            >
              <span class="set-switch-knob"></span>
            </button>
          </div>
          <p v-if="launchAtLoginError" class="set-error">{{ launchAtLoginError }}</p>
        </section>

        <section class="set-block">
          <h3>当前生效的排除规则</h3>
          <p class="set-hint">
            来自引擎本身，列出当前排除了什么（隐藏文件、Library 等）。
          </p>
          <ul class="rule-list">
            <li v-for="r in rules" :key="r">{{ r }}</li>
          </ul>
        </section>

        <p v-if="loadError" class="set-error">{{ loadError }}</p>
        <p v-if="error" class="set-error">{{ error }}</p>
      </div>

      <footer class="panel-foot">
        <button class="set-btn set-btn-primary" :disabled="!canApply" @click="apply">
          {{ applyLabel }}
        </button>
        <button class="set-btn" :disabled="busy" @click="emit('close')">关闭</button>
      </footer>
    </section>
  </div>
</template>
