/** 设置面板的【状态判定】—— 纯函数，组件只负责调用。
 *
 *  为什么不写在 .vue 里：仓库里没有组件测试环境（没有 @vue/test-utils），
 *  单元测试只能测纯函数（见 lib/settings.test.ts 的做法）。
 *  这几条判定又是【出事故会丢用户数据】的，必须有测试兜着。
 */

/** 设置的加载状态 —— 三态，不是「加载中 / 加载完」两态。
 *
 *  【为什么要「失败」这一态】原来只有一个 `loaded` 布尔，
 *  成功和失败都置 true。于是加载失败时：
 *    - 「应用」按钮可点
 *    - 而 `settings.value` 里的 indexPaths 还是**默认值**（空）
 *    - 一按「应用」→ 索引目录被写回空（= 索引家目录）
 *      → **用户配的索引目录没了**
 *  （书签 / 最近搜索不会再被写没 —— 写入走 patchSettings，
 *    它会先重新读盘上的真值；但 indexPaths 正是这次要写的字段，
 *    读失败时手里那份就是空的。）
 *  这是一条**静默丢用户数据**的路径，必须堵掉。 */
export type SettingsLoadState = "loading" | "ready" | "failed";

/** 「应用并重建索引」能不能点。
 *
 *  只有【真的拿到设置】才能点 —— 拿到默认值时写回去就是覆盖。 */
export function canApplySettings(state: SettingsLoadState, busy: boolean): boolean {
  return state === "ready" && !busy;
}

/** apply() 的【逻辑层守卫】—— 不能应用时给用户的一句话；null = 放行。
 *
 *  【为什么按钮的 disabled 之外还要有这道】`:disabled` 只挡住了「点不到」，
 *  那还是 UI 层的保险：组件的 disabled 绑定一改，这条
 *  【静默丢用户设置】的路径就又开了 —— 加载失败时 `settings.value` 里的
 *  indexPaths 还是默认值（空），一按「应用」就把它写回去，
 *  用户配的索引目录就没了（空 = 索引家目录）。
 *  这条判定出事故会丢用户数据，所以放在这里（单元测试兜着），
 *  组件只负责调用。 */
export function applyGuardReason(state: SettingsLoadState): string | null {
  if (state === "ready") return null;
  return "设置还没加载成功 —— 不能应用（否则会用默认值覆盖你现有的索引目录设置）。";
}

/** 按钮上的字 —— 状态要写在脸上，别让用户猜。
 *
 *  【为什么 busy 排在 state 前面】busy 只在 apply() 里置 true，
 *  而 apply() 只在 loadState === "ready" 时才可能触发（按钮 disabled +
 *  apply() 里的逻辑层守卫）—— 所以 busy === true 时 loadState 必然是
 *  "ready"，"failed" + busy 这个组合到不了，这里不必为它分支。 */
export function applyButtonText(state: SettingsLoadState, busy: boolean): string {
  if (busy) return "正在重建索引…";
  switch (state) {
    case "ready":
      return "应用并重建索引";
    case "failed":
      return "加载失败，无法应用";
    default:
      return "正在加载设置…";
  }
}

/** 加载失败时给用户看的话。
 *
 *  失败不能静悄悄过去 —— 用户看到的是「空列表 + 一颗能点的按钮」，
 *  根本不知道出了什么事。 */
export function loadFailedHint(error: unknown): string {
  return (
    `加载设置失败：${String(error)}。` +
    "为避免覆盖你现有的设置（书签 / 最近搜索 / 索引目录），「应用」已禁用。"
  );
}

/** 点遮罩要不要关抽屉。
 *
 *  【为什么重建期间不关】重建索引要 1~2 秒（扫全盘），这期间 reindex
 *   可能失败 —— 失败的原因显示在面板里。遮罩一点把抽屉关了，
 *  组件随之卸载，**错误信息跟着一起没了**：用户只看到抽屉凭空消失，
 *  不知道为什么。数据还是一致的（失败时旧引擎没动、设置也没写回），
 *  但用户看不到原因 —— 所以重建期间点遮罩不关抽屉。
 *
 *  （「关闭」按钮和右上角的 × 在重建期间也是禁用的，见 SettingsPanel.vue。） */
export function shouldCloseOnBackdrop(busy: boolean): boolean {
  return !busy;
}
